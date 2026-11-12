//! Direct Media Foundation transforms: codec discovery, encoding the owned
//! NV12 GPU texture into raw access units (no container, no sink writer), and
//! decoding them back through an OS decoder for a pixel check. No FFmpeg.

use crate::devices;
use crate::encoder::{ContextProtection, Runtime, convert, media_type};
use anyhow::{Context, Result, anyhow, bail, ensure};
use mirage_native_capture_probe::{
    bitstream::{Codec, av1_canonical, inspect},
    rtp::{RTP_HEADER_LEN, Reassembler, annexb_canonical, depacketize, payloads, rtp_packet},
    selection::{self, AdapterChoice, Backend, Target},
};
use std::{
    io::Write,
    mem::ManuallyDrop,
    net::{Ipv4Addr, SocketAddr, UdpSocket},
    path::Path,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::VARIANT_BOOL,
        Graphics::Direct3D11::*,
        Media::MediaFoundation::*,
        System::{
            Com::CoTaskMemFree,
            Variant::{VARIANT, VT_BOOL, VT_UI4},
        },
    },
    core::{GUID, Interface},
};

const FILE_FRAMES: i64 = 20;
const FRAME_100NS: i64 = 500_000; // 20 FPS
const MAX_UNIT_BYTES: u32 = 8 * 1024 * 1024;
const RTP_MTU: usize = 1200;

fn subtype(codec: Codec) -> GUID {
    match codec {
        Codec::H264 => MFVideoFormat_H264,
        Codec::Hevc => MFVideoFormat_HEVC,
        Codec::Av1 => MFVideoFormat_AV1,
    }
}

fn extension(codec: Codec) -> &'static str {
    match codec {
        Codec::H264 => "h264",
        Codec::Hevc => "h265",
        Codec::Av1 => "obu",
    }
}

/// Enumerate transforms; the returned activations own their references.
fn enumerate(
    category: GUID,
    flags: MFT_ENUM_FLAG,
    input: Option<GUID>,
    output: Option<GUID>,
) -> Vec<IMFActivate> {
    let info = |sub: GUID| MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: sub,
    };
    let (input, output) = (input.map(info), output.map(info));
    let mut array: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    let mut found = Vec::new();
    // SAFETY: MF allocates `array` with CoTaskMemAlloc; each element is read
    // exactly once (taking ownership of its reference) before the array is freed.
    unsafe {
        if MFTEnumEx(
            category,
            flags,
            input.as_ref().map(|t| t as *const _),
            output.as_ref().map(|t| t as *const _),
            &mut array,
            &mut count,
        )
        .is_err()
            || array.is_null()
        {
            return found;
        }
        found = take_activates(array, count);
    }
    found
}

/// Take ownership of an MF-allocated activation array and free it.
///
/// # Safety
/// `array` must come from `MFTEnumEx`/`MFTEnum2` with `count` elements.
pub(crate) unsafe fn take_activates(
    array: *mut Option<IMFActivate>,
    count: u32,
) -> Vec<IMFActivate> {
    let mut found = Vec::new();
    if array.is_null() {
        return found;
    }
    // SAFETY: Each element is read exactly once, then the array is freed.
    unsafe {
        for i in 0..count as usize {
            if let Some(activate) = std::ptr::read(array.add(i)) {
                found.push(activate);
            }
        }
        CoTaskMemFree(Some(array.cast()));
    }
    found
}

const HARDWARE: MFT_ENUM_FLAG =
    MFT_ENUM_FLAG(MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_SORTANDFILTER.0);
const SOFTWARE: MFT_ENUM_FLAG = MFT_ENUM_FLAG(
    MFT_ENUM_FLAG_SYNCMFT.0 | MFT_ENUM_FLAG_ASYNCMFT.0 | MFT_ENUM_FLAG_SORTANDFILTER.0,
);

/// Counts only: vendor and adapter names stay out of shared reports.
pub fn discover() -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for codec in Codec::ALL {
        let sub = subtype(codec);
        let n = |category, flags, input, output| enumerate(category, flags, input, output).len();
        out.insert(
            codec.as_str().into(),
            serde_json::json!({
                "hardware_encoders": n(MFT_CATEGORY_VIDEO_ENCODER, HARDWARE, Some(MFVideoFormat_NV12), Some(sub)),
                "software_encoders": n(MFT_CATEGORY_VIDEO_ENCODER, SOFTWARE, Some(MFVideoFormat_NV12), Some(sub)),
                "hardware_decoders": n(MFT_CATEGORY_VIDEO_DECODER, HARDWARE, Some(sub), None),
                "software_decoders": n(MFT_CATEGORY_VIDEO_DECODER, SOFTWARE, Some(sub), None),
            }),
        );
    }
    serde_json::Value::Object(out)
}

/// Copy a sample's bytes, bounded.
fn sample_bytes(sample: &IMFSample) -> Result<Vec<u8>> {
    // SAFETY: Lock/Unlock are paired; the copy stays within the reported length.
    unsafe {
        let buffer = sample.ConvertToContiguousBuffer()?;
        let mut data = std::ptr::null_mut();
        let mut length = 0u32;
        buffer.Lock(&mut data, None, Some(&mut length))?;
        let copied = if data.is_null() || length > MAX_UNIT_BYTES {
            None
        } else {
            Some(std::slice::from_raw_parts(data, length as usize).to_vec())
        };
        buffer.Unlock()?;
        copied.context("output buffer missing or exceeds bound")
    }
}

enum Pulled {
    /// Encoded or decoded bytes with the sample time (100 ns units, -1 if absent).
    Unit(Vec<u8>, i64),
    NeedInput,
    Changed,
}

/// One ProcessOutput call, allocating a sample only when the MFT does not.
fn pull(transform: &IMFTransform) -> Result<Pulled> {
    // SAFETY: The MFT_OUTPUT_DATA_BUFFER fields are ManuallyDrop by binding
    // design; both are released on every path below.
    unsafe {
        let info = transform.GetOutputStreamInfo(0)?;
        let provides = info.dwFlags
            & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0)
                as u32
            != 0;
        let sample = if provides {
            None
        } else {
            let sample = MFCreateSample()?;
            sample.AddBuffer(&MFCreateMemoryBuffer(
                info.cbSize.clamp(1 << 20, MAX_UNIT_BYTES),
            )?)?;
            Some(sample)
        };
        let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: ManuallyDrop::new(sample),
            dwStatus: 0,
            pEvents: ManuallyDrop::new(None),
        }];
        let mut status = 0;
        let result = transform.ProcessOutput(0, &mut buffers, &mut status);
        let sample = ManuallyDrop::take(&mut buffers[0].pSample);
        ManuallyDrop::drop(&mut buffers[0].pEvents);
        match result {
            Ok(()) => {
                let sample = sample.context("MFT returned no sample")?;
                let time = sample.GetSampleTime().unwrap_or(-1);
                Ok(Pulled::Unit(sample_bytes(&sample)?, time))
            }
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(Pulled::NeedInput),
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => Ok(Pulled::Changed),
            Err(e) => Err(e.into()),
        }
    }
}

fn timed_sample(buffer: &IMFMediaBuffer, frame: i64) -> Result<IMFSample> {
    // SAFETY: Plain sample construction around a live buffer.
    unsafe {
        let sample = MFCreateSample()?;
        sample.AddBuffer(buffer)?;
        sample.SetSampleTime(frame * FRAME_100NS)?;
        sample.SetSampleDuration(FRAME_100NS)?;
        Ok(sample)
    }
}

fn gpu_sample(texture: &ID3D11Texture2D, frame: i64) -> Result<IMFSample> {
    // SAFETY: Wraps a live GPU texture; the buffer keeps its own reference.
    unsafe {
        let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, texture, 0, false)?;
        let buffer2d: IMF2DBuffer = buffer.cast()?;
        buffer.SetCurrentLength(buffer2d.GetContiguousLength()?)?;
        timed_sample(&buffer, frame)
    }
}

/// System-memory NV12 sample for CPU (software) encoders.
fn memory_sample(data: &[u8], frame: i64) -> Result<IMFSample> {
    // SAFETY: Lock/Unlock paired; the copy fits the allocated length.
    unsafe {
        let buffer = MFCreateMemoryBuffer(data.len() as u32)?;
        let mut target = std::ptr::null_mut();
        buffer.Lock(&mut target, None, None)?;
        std::ptr::copy_nonoverlapping(data.as_ptr(), target, data.len());
        buffer.Unlock()?;
        buffer.SetCurrentLength(data.len() as u32)?;
        timed_sample(&buffer, frame)
    }
}

/// Synthetic motion: a 32x32 black block moves across the top band of the
/// captured fixture each frame. Each frame gets its own NV12 texture, so an
/// async encoder may hold earlier samples safely.
struct Motion<'a> {
    device: &'a ID3D11Device,
    context: &'a ID3D11DeviceContext,
    pristine: &'a ID3D11Texture2D,
    work: ID3D11Texture2D,
    width: u32,
    block: Vec<u8>,
}

impl<'a> Motion<'a> {
    fn new(
        device: &'a ID3D11Device,
        context: &'a ID3D11DeviceContext,
        pristine: &'a ID3D11Texture2D,
    ) -> Result<Self> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        let mut work = None;
        // SAFETY: Same-device texture with the captured texture's description.
        unsafe {
            pristine.GetDesc(&mut desc);
            device.CreateTexture2D(&desc, None, Some(&mut work))?;
        }
        ensure!(
            desc.Width > 64 && desc.Height > 64,
            "fixture too small for motion"
        );
        let mut block = Vec::with_capacity(32 * 32 * 4);
        for _ in 0..32 * 32 {
            block.extend_from_slice(&[0, 0, 0, 255]);
        }
        Ok(Self {
            device,
            context,
            pristine,
            work: work.context("missing motion texture")?,
            width: desc.Width,
            block,
        })
    }

    /// The frame as a fresh NV12 texture on the capture device.
    fn nv12(&self, frame: i64) -> Result<ID3D11Texture2D> {
        let x = (frame as u32 * 12) % (self.width - 32);
        let area = D3D11_BOX {
            left: x,
            top: 16,
            front: 0,
            right: x + 32,
            bottom: 48,
            back: 1,
        };
        // SAFETY: The box lies inside the work texture; the source covers 32x32 BGRA.
        unsafe {
            self.context.CopyResource(&self.work, self.pristine);
            self.context.UpdateSubresource(
                &self.work,
                0,
                Some(&area),
                self.block.as_ptr().cast(),
                32 * 4,
                0,
            );
        }
        convert(self.device, self.context, &self.work)
    }
}

struct Configured {
    transform: IMFTransform,
    is_async: bool,
    gop_applied: bool,
    low_latency_applied: bool,
    zero_b_frames_applied: bool,
}

fn set_codec_bool(transform: &IMFTransform, api: &GUID, value: bool) -> bool {
    let Ok(codec_api) = transform.cast::<ICodecAPI>() else {
        return false;
    };
    let mut variant = VARIANT::default();
    // SAFETY: A VT_BOOL VARIANT needs no cleanup; the union field matches the tag.
    unsafe {
        let inner = &mut *variant.Anonymous.Anonymous;
        inner.vt = VT_BOOL;
        inner.Anonymous.boolVal = VARIANT_BOOL(if value { -1 } else { 0 });
        codec_api.SetValue(api, &variant).is_ok()
    }
}

fn set_codec_u32(transform: &IMFTransform, api: &GUID, value: u32) -> bool {
    let Ok(codec_api) = transform.cast::<ICodecAPI>() else {
        return false;
    };
    let mut variant = VARIANT::default();
    // SAFETY: A VT_UI4 VARIANT needs no cleanup; the union field matches the tag.
    unsafe {
        let inner = &mut *variant.Anonymous.Anonymous;
        inner.vt = VT_UI4;
        inner.Anonymous.ulVal = value;
        codec_api.SetValue(api, &variant).is_ok()
    }
}

/// Configure one candidate encoder. Hardware MFTs may belong to another
/// adapter on multi-GPU systems; such a mismatch fails here and the caller
/// moves to the next candidate.
fn configure_encoder(
    activate: &IMFActivate,
    codec: Codec,
    manager: &IMFDXGIDeviceManager,
    hardware: bool,
    gop: Option<u32>,
) -> Result<Configured> {
    // SAFETY: COM calls on a freshly activated transform owned by this thread.
    unsafe {
        let transform: IMFTransform = activate.ActivateObject()?;
        let attributes = transform.GetAttributes().ok();
        let is_async = attributes
            .as_ref()
            .is_some_and(|a| a.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) == 1);
        if let Some(a) = &attributes {
            if is_async {
                a.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
            }
            let _ = a.SetUINT32(&MF_LOW_LATENCY, 1);
        }
        if hardware {
            transform
                .ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)
                .context("encoder rejected the capture device manager")?;
        }
        // Interactive streaming: ask for low-latency mode and no B-frames
        // (B-frames add reordering delay). Encoders may decline; reported.
        let low_latency_applied = set_codec_bool(&transform, &CODECAPI_AVLowLatencyMode, true);
        let zero_b_frames_applied =
            set_codec_u32(&transform, &CODECAPI_AVEncMPVDefaultBPictureCount, 0);
        let gop_applied =
            gop.is_some_and(|size| set_codec_u32(&transform, &CODECAPI_AVEncMPVGOPSize, size));
        let output = media_type(&subtype(codec))?;
        output.SetUINT32(&MF_MT_AVG_BITRATE, 800_000)?;
        transform
            .SetOutputType(0, &output, 0)
            .context("set encoder output type")?;
        let input = media_type(&MFVideoFormat_NV12)?;
        if transform.SetInputType(0, &input, 0).is_err() {
            // Fall back to the encoder's own NV12 offer, constrained to our format.
            let mut set = false;
            for index in 0..32 {
                let Ok(offer) = transform.GetInputAvailableType(0, index) else {
                    break;
                };
                if offer.GetGUID(&MF_MT_SUBTYPE).ok() != Some(MFVideoFormat_NV12) {
                    continue;
                }
                offer.SetUINT64(&MF_MT_FRAME_SIZE, (640_u64 << 32) | 360)?;
                offer.SetUINT64(&MF_MT_FRAME_RATE, (20_u64 << 32) | 1)?;
                if transform.SetInputType(0, &offer, 0).is_ok() {
                    set = true;
                    break;
                }
            }
            ensure!(set, "encoder accepted no NV12 input type");
        }
        transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
        transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        Ok(Configured {
            transform,
            is_async,
            gop_applied,
            low_latency_applied,
            zero_b_frames_applied,
        })
    }
}

struct Selected {
    encoder: Configured,
    activate: IMFActivate,
    hardware: bool,
    index: usize,
}

/// Hardware candidates first, then software; falls over only on configuration
/// failure so a started stream is never silently replaced.
fn select_encoder(
    codec: Codec,
    manager: &IMFDXGIDeviceManager,
    gop: Option<u32>,
) -> (Option<Selected>, Vec<String>) {
    let mut attempts = Vec::new();
    for (hardware, flags) in [(true, HARDWARE), (false, SOFTWARE)] {
        let candidates = enumerate(
            MFT_CATEGORY_VIDEO_ENCODER,
            flags,
            Some(MFVideoFormat_NV12),
            Some(subtype(codec)),
        );
        for (index, activate) in candidates.into_iter().enumerate() {
            match configure_encoder(&activate, codec, manager, hardware, gop) {
                Ok(encoder) => {
                    let selected = Selected {
                        encoder,
                        activate,
                        hardware,
                        index,
                    };
                    return (Some(selected), attempts);
                }
                Err(error) => {
                    // SAFETY: Release the failed activation's object.
                    unsafe {
                        let _ = activate.ShutdownObject();
                    }
                    attempts.push(format!(
                        "{}#{index}: {error:#}",
                        if hardware { "hw" } else { "sw" }
                    ));
                }
            }
        }
    }
    (None, attempts)
}

/// Feed `frames` input samples (optionally paced in real time) and hand each
/// encoded unit to `output` as soon as the encoder releases it.
fn run_encoder(
    encoder: &Configured,
    frames: i64,
    paced: bool,
    input: &mut dyn FnMut(i64) -> Result<IMFSample>,
    output: &mut dyn FnMut(Vec<u8>, i64) -> Result<()>,
) -> Result<usize> {
    let transform = &encoder.transform;
    let start = Instant::now();
    let deadline = start + Duration::from_secs(15) + Duration::from_millis(50 * frames as u64);
    let mut produced = 0;
    let renegotiate = || -> Result<()> {
        // SAFETY: Re-apply the MFT's preferred output type after a stream change.
        unsafe {
            let offer = transform.GetOutputAvailableType(0, 0)?;
            transform.SetOutputType(0, &offer, 0)?;
        }
        Ok(())
    };
    let pace = |frame: i64| {
        if paced {
            let due = start + Duration::from_millis(50 * frame as u64);
            if let Some(wait) = due.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
        }
    };
    let mut handle = |pulled: Pulled, produced: &mut usize| -> Result<bool> {
        match pulled {
            Pulled::Unit(unit, time) => {
                *produced += 1;
                output(unit, time)?;
                Ok(true)
            }
            Pulled::Changed => {
                renegotiate()?;
                Ok(true)
            }
            Pulled::NeedInput => Ok(false),
        }
    };
    // SAFETY: Single-threaded use of one transform; events are polled without
    // blocking so the overall deadline always holds.
    unsafe {
        if encoder.is_async {
            let events: IMFMediaEventGenerator = transform.cast()?;
            let (mut sent, mut draining) = (0, false);
            loop {
                ensure!(Instant::now() < deadline, "encoder event deadline exceeded");
                let event = match events.GetEvent(MF_EVENT_FLAG_NO_WAIT) {
                    Ok(event) => event,
                    Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => {
                        std::thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    Err(e) => return Err(e.into()),
                };
                let kind = event.GetType()?;
                if kind == METransformNeedInput.0 as u32 {
                    if sent < frames {
                        pace(sent);
                        transform.ProcessInput(0, &input(sent)?, 0)?;
                        sent += 1;
                    } else if !draining {
                        transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)?;
                        transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)?;
                        draining = true;
                    }
                } else if kind == METransformHaveOutput.0 as u32 {
                    handle(pull(transform)?, &mut produced)?;
                } else if kind == METransformDrainComplete.0 as u32 {
                    break;
                }
            }
        } else {
            let mut drain = |produced: &mut usize| -> Result<()> {
                loop {
                    ensure!(
                        Instant::now() < deadline,
                        "encoder output deadline exceeded"
                    );
                    if !handle(pull(transform)?, produced)? {
                        return Ok(());
                    }
                }
            };
            for frame in 0..frames {
                pace(frame);
                let sample = input(frame)?;
                loop {
                    match transform.ProcessInput(0, &sample, 0) {
                        Ok(()) => break,
                        Err(e) if e.code() == MF_E_NOTACCEPTING => drain(&mut produced)?,
                        Err(e) => return Err(e.into()),
                    }
                }
                drain(&mut produced)?;
            }
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)?;
            drain(&mut produced)?;
        }
        let _ = transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
    }
    ensure!(produced > 0, "encoder produced no access units");
    Ok(produced)
}

/// Parameter sets some encoders publish only on the output media type.
fn sequence_header(transform: &IMFTransform) -> Option<Vec<u8>> {
    // SAFETY: Blob size is queried first and bounded before allocation.
    unsafe {
        let current = transform.GetOutputCurrentType(0).ok()?;
        let size = current.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER).ok()?;
        if size == 0 || size > 64 * 1024 {
            return None;
        }
        let mut blob = vec![0; size as usize];
        current
            .GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut blob, None)
            .ok()?;
        Some(blob)
    }
}

/// What the receiver measures in each decoded NV12 frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodedFrame {
    pub center_yuv: [u8; 3],
    /// FNV-1a over the luma of the top 64 rows, where the motion block moves.
    pub band_hash: u64,
}

pub fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Decode units with an OS decoder. `Ok(None)` means no decoder is installed.
fn decode_frames(codec: Codec, units: &[Vec<u8>]) -> Result<Option<Vec<DecodedFrame>>> {
    let mut candidates = enumerate(
        MFT_CATEGORY_VIDEO_DECODER,
        SOFTWARE,
        Some(subtype(codec)),
        Some(MFVideoFormat_NV12),
    );
    if candidates.is_empty() {
        candidates = enumerate(
            MFT_CATEGORY_VIDEO_DECODER,
            SOFTWARE,
            Some(subtype(codec)),
            None,
        );
    }
    let Some(activate) = candidates.first() else {
        return Ok(None);
    };
    // SAFETY: Synchronous use of one software decoder on this thread.
    let frames = unsafe {
        let decoder: IMFTransform = activate.ActivateObject()?;
        if let Ok(attributes) = decoder.GetAttributes()
            && attributes.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) == 1
        {
            attributes.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
        }
        decoder
            .SetInputType(0, &media_type(&subtype(codec))?, 0)
            .context("decoder input type")?;
        let choose_nv12 = || -> Result<IMFMediaType> {
            for index in 0..64 {
                let offer = decoder.GetOutputAvailableType(0, index)?;
                if offer.GetGUID(&MF_MT_SUBTYPE)? == MFVideoFormat_NV12 {
                    decoder.SetOutputType(0, &offer, 0)?;
                    return Ok(offer);
                }
            }
            bail!("decoder offers no NV12 output")
        };
        let mut output_type = choose_nv12()?;
        decoder.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut frames = Vec::new();
        let mut feed = units.iter().enumerate();
        let mut ended = false;
        loop {
            ensure!(Instant::now() < deadline, "decoder deadline exceeded");
            match pull(&decoder)? {
                Pulled::Unit(bytes, _) => frames.push(measure(&bytes, &output_type)?),
                Pulled::Changed => output_type = choose_nv12()?,
                Pulled::NeedInput => match feed.next() {
                    Some((index, unit)) => {
                        let buffer = MFCreateMemoryBuffer(unit.len() as u32)?;
                        let mut data = std::ptr::null_mut();
                        buffer.Lock(&mut data, None, None)?;
                        std::ptr::copy_nonoverlapping(unit.as_ptr(), data, unit.len());
                        buffer.Unlock()?;
                        buffer.SetCurrentLength(unit.len() as u32)?;
                        decoder.ProcessInput(0, &timed_sample(&buffer, index as i64)?, 0)?;
                    }
                    None if !ended => {
                        decoder.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)?;
                        decoder.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)?;
                        ended = true;
                    }
                    None => break,
                },
            }
        }
        frames
    };
    ensure!(!frames.is_empty(), "decoder produced no frame");
    Ok(Some(frames))
}

fn measure(frame: &[u8], output_type: &IMFMediaType) -> Result<DecodedFrame> {
    // SAFETY: Attribute reads on a live media type.
    let (size, stride) = unsafe {
        (
            output_type.GetUINT64(&MF_MT_FRAME_SIZE)?,
            output_type.GetUINT32(&MF_MT_DEFAULT_STRIDE).ok(),
        )
    };
    let (width, height) = ((size >> 32) as usize, (size & 0xffff_ffff) as usize);
    let stride = stride
        .map(|s| (s as i32).unsigned_abs() as usize)
        .unwrap_or(width)
        .max(width);
    // NV12 planes may be padded beyond the display height; infer the luma
    // plane height from the buffer length (Y + Y/2).
    let plane_rows = frame.len() * 2 / 3 / stride;
    ensure!(
        plane_rows >= height && width >= 640 && height >= 360,
        "unexpected decoded layout"
    );
    let chroma = plane_rows * stride + 90 * stride + 320;
    Ok(DecodedFrame {
        center_yuv: [
            frame[180 * stride + 320],
            *frame.get(chroma).context("chroma")?,
            *frame.get(chroma + 1).context("chroma")?,
        ],
        band_hash: fnv1a(&frame[..64 * stride]),
    })
}

/// BT.709 limited-range NV12 for the fixture BGR (80,180,20): Y≈135, U≈100, V≈62.
pub fn is_fixture_yuv(yuv: [u8; 3]) -> bool {
    yuv[0].abs_diff(135) <= 14 && yuv[1].abs_diff(100) <= 14 && yuv[2].abs_diff(62) <= 14
}

fn decode_report(codec: Codec, units: &[Vec<u8>]) -> serde_json::Value {
    match decode_frames(codec, units) {
        Ok(Some(frames)) => {
            let mut hashes: Vec<u64> = frames.iter().map(|f| f.band_hash).collect();
            hashes.sort_unstable();
            hashes.dedup();
            serde_json::json!({
                "status": "decoded",
                "frames": frames.len(),
                "center_yuv": frames[0].center_yuv,
                "fixture_matches": frames.iter().all(|f| is_fixture_yuv(f.center_yuv)),
                "distinct_motion_bands": hashes.len(),
            })
        }
        Ok(None) => serde_json::json!({"status":"no_os_decoder"}),
        Err(error) => serde_json::json!({"status":"failed","error":format!("{error:#}")}),
    }
}

fn mf_device_manager(device: &ID3D11Device) -> Result<IMFDXGIDeviceManager> {
    // SAFETY: Creates and binds a manager to the live capture device.
    unsafe {
        let mut token = 0;
        let mut manager = None;
        MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
        let manager = manager.ok_or_else(|| anyhow!("missing DXGI manager"))?;
        manager.ResetDevice(device, token)?;
        Ok(manager)
    }
}

fn encode_codec(
    codec: Codec,
    manager: &IMFDXGIDeviceManager,
    nv12: &ID3D11Texture2D,
    directory: &Path,
) -> serde_json::Value {
    let (selected, attempts) = select_encoder(codec, manager, None);
    let Some(Selected {
        encoder,
        activate,
        hardware,
        index,
    }) = selected
    else {
        return serde_json::json!({"encoded":false,"failed_attempts":attempts});
    };
    let mut units = Vec::new();
    let result = run_encoder(
        &encoder,
        FILE_FRAMES,
        false,
        &mut |frame| gpu_sample(nv12, frame),
        &mut |unit, _| {
            units.push(unit);
            Ok(())
        },
    );
    let header = sequence_header(&encoder.transform);
    drop(encoder);
    // SAFETY: Release the activated object after use.
    unsafe {
        let _ = activate.ShutdownObject();
    }
    result
        .and_then(|_| report(codec, hardware, index, &attempts, units, header, directory))
        .unwrap_or_else(|error| serde_json::json!({"encoded":false,"error":format!("{error:#}")}))
}

fn report(
    codec: Codec,
    hardware: bool,
    index: usize,
    attempts: &[String],
    mut units: Vec<Vec<u8>>,
    header: Option<Vec<u8>>,
    directory: &Path,
) -> Result<serde_json::Value> {
    let first = inspect(codec, &units[0]).context("first unit is malformed")?;
    let mut header_from_type = false;
    if !first.parameter_sets
        && let Some(header) = header
    {
        let mut joined = header;
        joined.extend_from_slice(&units[0]);
        units[0] = joined;
        header_from_type = true;
    }
    let summaries = units
        .iter()
        .map(|unit| inspect(codec, unit))
        .collect::<Option<Vec<_>>>()
        .context("malformed access unit")?;
    let mut sequence = 0u16;
    let (mut packets, mut max_packet, mut lossless) = (0, 0, true);
    for (frame, unit) in units.iter().enumerate() {
        let parts =
            payloads(codec, unit, RTP_MTU - RTP_HEADER_LEN).context("packetization failed")?;
        let timestamp = (frame as u32) * 4500; // 90 kHz at 20 FPS
        for (i, part) in parts.iter().enumerate() {
            let packet = rtp_packet(
                96,
                sequence,
                timestamp,
                0x4d49_5241,
                i + 1 == parts.len(),
                part,
            );
            max_packet = max_packet.max(packet.len());
            sequence = sequence.wrapping_add(1);
            packets += 1;
        }
        let expected = match codec {
            Codec::Av1 => av1_canonical(unit).context("AV1 canonical form")?,
            _ => annexb_canonical(unit),
        };
        lossless &= depacketize(codec, &parts).as_ref() == Some(&expected);
    }
    let path = directory.join(format!("fixture.{}", extension(codec)));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    for unit in &units {
        file.write_all(unit)?;
    }
    Ok(serde_json::json!({
        "encoded": true,
        "encoder_kind": if hardware { "hardware" } else { "software" },
        "candidate_index": index,
        "skipped_candidates": attempts,
        "access_units": units.len(),
        "bytes": units.iter().map(Vec::len).sum::<usize>(),
        "keyframes": summaries.iter().filter(|s| s.keyframe).count(),
        "first_unit_keyframe": summaries[0].keyframe,
        "parameter_sets_in_first_unit": summaries[0].parameter_sets,
        "parameter_sets_from_media_type": header_from_type,
        "bitstream_dims": summaries.iter().find_map(|s| s.dims),
        "rtp": {"mtu": RTP_MTU, "packets": packets, "max_packet": max_packet, "roundtrip_lossless": lossless},
        "os_decode": decode_report(codec, &units),
    }))
}

/// Encode the owned texture with every available codec.
pub fn stream(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    texture: &ID3D11Texture2D,
    directory: &Path,
) -> Result<serde_json::Value> {
    ensure!(
        !directory.exists(),
        "stream output directory must not already exist"
    );
    std::fs::create_dir_all(directory)?;
    let _runtime = Runtime::start()?;
    let _protection = ContextProtection::enable(context)?;
    let nv12 = convert(device, context, texture).context("convert owned texture")?;
    let manager = mf_device_manager(device)?;
    let mut codecs = serde_json::Map::new();
    for codec in Codec::ALL {
        codecs.insert(
            codec.as_str().into(),
            encode_codec(codec, &manager, &nv12, directory),
        );
    }
    Ok(serde_json::json!({"capabilities": discover(), "codecs": codecs}))
}

const RTP_PAYLOAD_TYPE: u8 = 96;
const RTP_SSRC: u32 = 0x4d49_5241;
pub const GOP_FRAMES: u32 = 20;

/// The user's encoder choice: backend (GPU/CPU) and which GPU.
#[derive(Debug, Clone, Copy, Default)]
pub struct Preference {
    pub backend: Backend,
    pub adapter: AdapterChoice,
}

/// How each frame reaches the chosen encoder.
enum Delivery {
    /// Encoder on the capture GPU: the NV12 texture is used as is.
    ZeroCopy,
    /// Encoder on another GPU: GPU -> system memory -> other GPU per frame.
    CrossAdapter {
        device: ID3D11Device,
        _protection: ContextProtection,
    },
    /// CPU encoder: GPU -> system memory per frame.
    SystemMemory,
}

struct Pipeline {
    // Field order is drop order: encoder before its device manager and device.
    selected: Selected,
    delivery: Delivery,
    _manager: IMFDXGIDeviceManager,
    target: Target,
    adapter: Option<(u32, &'static str, &'static str, bool)>,
    plan: Vec<Target>,
    attempts: Vec<String>,
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        // SAFETY: Release the activated encoder object once.
        unsafe {
            let _ = self.selected.activate.ShutdownObject();
        }
    }
}

fn target_name(target: Target) -> String {
    match target {
        Target::Gpu(index) => format!("gpu{index}"),
        Target::Cpu => "cpu".into(),
    }
}

impl Pipeline {
    fn describe(&self) -> serde_json::Value {
        let (index, kind, vendor, capture) = match self.adapter {
            Some((index, kind, vendor, capture)) => (Some(index), kind, vendor, capture),
            None => (None, "cpu", "cpu", false),
        };
        serde_json::json!({
            "selected": target_name(self.target),
            "adapter_index": index,
            "adapter_kind": kind,
            "adapter_vendor": vendor,
            "adapter_is_capture_gpu": capture,
            "delivery": match self.delivery {
                Delivery::ZeroCopy => "zero_copy_gpu",
                Delivery::CrossAdapter { .. } => "cross_adapter_copy",
                Delivery::SystemMemory => "system_memory",
            },
            "plan": self.plan.iter().map(|t| target_name(*t)).collect::<Vec<_>>(),
        })
    }
}

/// Resolve the user's preference to a configured encoder, trying planned
/// targets in order and recording why any were skipped.
fn build_pipeline(
    codec: Codec,
    device: &ID3D11Device,
    _context: &ID3D11DeviceContext,
    preference: Preference,
) -> Result<Pipeline> {
    let sub = subtype(codec);
    let capture = devices::device_luid(device)?;
    let gpus = devices::adapters(Some(capture), sub)?;
    let software = enumerate(
        MFT_CATEGORY_VIDEO_ENCODER,
        SOFTWARE,
        Some(MFVideoFormat_NV12),
        Some(sub),
    );
    let infos: Vec<_> = gpus.iter().map(|g| g.info.clone()).collect();
    let plan = selection::plan(
        preference.backend,
        preference.adapter,
        &infos,
        !software.is_empty(),
    );
    ensure!(
        !plan.is_empty(),
        "no encoder satisfies backend={:?} adapter={:?} for {} (GPU encoders per adapter: {:?}; CPU encoders: {})",
        preference.backend,
        preference.adapter,
        codec.as_str(),
        infos
            .iter()
            .map(|a| (a.index, a.kind.as_str(), a.hardware_encoders))
            .collect::<Vec<_>>(),
        software.len()
    );
    let mut attempts = Vec::new();
    for &target in &plan {
        match target {
            Target::Gpu(index) => {
                let Some(gpu) = gpus.iter().find(|g| g.info.index == index) else {
                    continue;
                };
                let zero_copy = devices::luid_key(gpu.luid) == devices::luid_key(capture);
                let built = (|| -> Result<(IMFDXGIDeviceManager, Delivery)> {
                    if zero_copy {
                        Ok((mf_device_manager(device)?, Delivery::ZeroCopy))
                    } else {
                        let (other, other_context) = devices::create_device(&gpu.adapter)?;
                        let protection = ContextProtection::enable(&other_context)?;
                        Ok((
                            mf_device_manager(&other)?,
                            Delivery::CrossAdapter {
                                device: other,
                                _protection: protection,
                            },
                        ))
                    }
                })();
                let (manager, delivery) = match built {
                    Ok(built) => built,
                    Err(error) => {
                        attempts.push(format!("gpu{index}: device setup failed: {error:#}"));
                        continue;
                    }
                };
                for (slot, activate) in devices::hardware_encoders_on(gpu.luid, sub)
                    .into_iter()
                    .enumerate()
                {
                    match configure_encoder(&activate, codec, &manager, true, Some(GOP_FRAMES)) {
                        Ok(encoder) => {
                            return Ok(Pipeline {
                                selected: Selected {
                                    encoder,
                                    activate,
                                    hardware: true,
                                    index: slot,
                                },
                                delivery,
                                _manager: manager,
                                target,
                                adapter: Some((
                                    index,
                                    gpu.info.kind.as_str(),
                                    gpu.vendor,
                                    zero_copy,
                                )),
                                plan: plan.clone(),
                                attempts,
                            });
                        }
                        Err(error) => {
                            // SAFETY: Release the failed activation's object.
                            unsafe {
                                let _ = activate.ShutdownObject();
                            }
                            attempts.push(format!("gpu{index}#{slot}: {error:#}"));
                        }
                    }
                }
            }
            Target::Cpu => {
                let manager = mf_device_manager(device)?;
                for (slot, activate) in software.iter().enumerate() {
                    match configure_encoder(activate, codec, &manager, false, Some(GOP_FRAMES)) {
                        Ok(encoder) => {
                            return Ok(Pipeline {
                                selected: Selected {
                                    encoder,
                                    activate: activate.clone(),
                                    hardware: false,
                                    index: slot,
                                },
                                delivery: Delivery::SystemMemory,
                                _manager: manager,
                                target,
                                adapter: None,
                                plan: plan.clone(),
                                attempts,
                            });
                        }
                        Err(error) => {
                            // SAFETY: Release the failed activation's object.
                            unsafe {
                                let _ = activate.ShutdownObject();
                            }
                            attempts.push(format!("cpu#{slot}: {error:#}"));
                        }
                    }
                }
            }
        }
    }
    bail!("every planned encoder failed: {attempts:?}")
}

/// `--devices`: adapters, Windows' preference ranks and encoders per codec.
pub fn devices_report() -> Result<serde_json::Value> {
    let _runtime = Runtime::start()?;
    let mut adapters: Vec<serde_json::Value> = Vec::new();
    let mut software = serde_json::Map::new();
    for codec in Codec::ALL {
        let sub = subtype(codec);
        software.insert(
            codec.as_str().into(),
            enumerate(
                MFT_CATEGORY_VIDEO_ENCODER,
                SOFTWARE,
                Some(MFVideoFormat_NV12),
                Some(sub),
            )
            .len()
            .into(),
        );
        for gpu in devices::adapters(None, sub)? {
            let index = gpu.info.index as usize;
            if adapters.len() <= index {
                adapters.push(serde_json::json!({
                    "index": gpu.info.index,
                    "name": gpu.name,
                    "vendor": gpu.vendor,
                    "kind": gpu.info.kind.as_str(),
                    "dedicated_mib": gpu.dedicated_mib,
                    "high_performance_rank": gpu.info.high_performance_rank,
                    "minimum_power_rank": gpu.info.minimum_power_rank,
                    "default_capture_gpu": gpu.info.is_capture,
                    "hardware_encoders": {},
                }));
            }
            adapters[index]["hardware_encoders"][codec.as_str()] =
                gpu.info.hardware_encoders.into();
        }
    }
    Ok(serde_json::json!({"adapters": adapters, "software_encoders": software}))
}

/// Live sender: paced 20 FPS motion stream, each unit packetized and sent the
/// moment the encoder releases it. Loopback destinations only.
/// What to stream and where, as chosen on the command line.
#[derive(Debug, Clone, Copy)]
pub struct SendRequest {
    pub destination: SocketAddr,
    pub codec: Codec,
    pub frames: u32,
    /// Deliberately drop this packet index (loss-recovery test).
    pub drop_packet: Option<u64>,
    pub preference: Preference,
}

pub fn rtp_send(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    texture: &ID3D11Texture2D,
    request: SendRequest,
) -> Result<serde_json::Value> {
    let SendRequest {
        destination,
        codec,
        frames,
        drop_packet,
        preference,
    } = request;
    ensure!(
        destination.ip().is_loopback(),
        "the RTP diagnostic sends to loopback only"
    );
    ensure!((1..=600).contains(&frames), "frames must be 1..=600");
    let _runtime = Runtime::start()?;
    let _protection = ContextProtection::enable(context)?;
    let motion = Motion::new(device, context, texture)?;
    let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
    socket.connect(destination)?;
    let pipeline = build_pipeline(codec, device, context, preference)?;
    let encoder = &pipeline.selected.encoder;
    let hardware = pipeline.selected.hardware;
    let attempts = pipeline.attempts.clone();
    let submitted = std::cell::RefCell::new(vec![None::<Instant>; frames as usize]);
    let mut copy_ms = Vec::new();
    let mut latency_ms = Vec::new();
    let header = sequence_header(&encoder.transform);
    let gop_applied = encoder.gop_applied;
    let low_latency_applied = encoder.low_latency_applied;
    let zero_b_frames_applied = encoder.zero_b_frames_applied;
    let (mut sequence, mut packets, mut bytes, mut units, mut keyframes) =
        (0u16, 0u64, 0u64, 0u64, 0u64);
    let mut header_prepended = false;
    let started = Instant::now();
    let result = run_encoder(
        encoder,
        i64::from(frames),
        true,
        &mut |frame| {
            let nv12 = motion.nv12(frame)?;
            let copy_started = Instant::now();
            let sample = match &pipeline.delivery {
                Delivery::ZeroCopy => gpu_sample(&nv12, frame)?,
                Delivery::CrossAdapter { device: target, .. } => {
                    let bytes = devices::readback_nv12(device, context, &nv12)?;
                    let uploaded = devices::upload_nv12(target, &bytes)?;
                    copy_ms.push(copy_started.elapsed().as_secs_f64() * 1000.0);
                    gpu_sample(&uploaded, frame)?
                }
                Delivery::SystemMemory => {
                    let bytes = devices::readback_nv12(device, context, &nv12)?;
                    copy_ms.push(copy_started.elapsed().as_secs_f64() * 1000.0);
                    memory_sample(&bytes, frame)?
                }
            };
            if let Some(slot) = submitted.borrow_mut().get_mut(frame as usize) {
                *slot = Some(Instant::now());
            }
            Ok(sample)
        },
        &mut |mut unit, time| {
            if time >= 0
                && let Some(Some(at)) = submitted.borrow().get((time / FRAME_100NS) as usize)
            {
                latency_ms.push(at.elapsed().as_secs_f64() * 1000.0);
            }
            let mut au = inspect(codec, &unit).context("encoder emitted a malformed unit")?;
            if units == 0
                && !au.parameter_sets
                && let Some(header) = &header
            {
                let mut joined = header.clone();
                joined.extend_from_slice(&unit);
                unit = joined;
                au = inspect(codec, &unit).context("malformed unit after header")?;
                header_prepended = true;
            }
            keyframes += u64::from(au.keyframe);
            let timestamp = (time.max(0) as u64 * 9 / 1000) as u32; // 100 ns -> 90 kHz
            let parts =
                payloads(codec, &unit, RTP_MTU - RTP_HEADER_LEN).context("packetization")?;
            for (i, part) in parts.iter().enumerate() {
                let packet = rtp_packet(
                    RTP_PAYLOAD_TYPE,
                    sequence,
                    timestamp,
                    RTP_SSRC,
                    i + 1 == parts.len(),
                    part,
                );
                // Optional deliberate loss for the recovery test; the sequence
                // number still advances so the receiver can detect the gap.
                if drop_packet != Some(packets) {
                    socket.send(&packet)?;
                }
                sequence = sequence.wrapping_add(1);
                packets += 1;
                bytes += packet.len() as u64;
            }
            units += 1;
            Ok(())
        },
    );
    let elapsed = started.elapsed().as_secs_f64();
    let target = pipeline.describe();
    drop(pipeline);
    result?;
    copy_ms.sort_by(f64::total_cmp);
    latency_ms.sort_by(f64::total_cmp);
    Ok(serde_json::json!({
        "codec": codec.as_str(),
        "encoder_kind": if hardware { "hardware" } else { "software" },
        "target": target,
        "copy_ms": {"p50": percentile(&copy_ms, 0.5), "p95": percentile(&copy_ms, 0.95)},
        "encode_latency_ms": {
            "p50": percentile(&latency_ms, 0.5),
            "p95": percentile(&latency_ms, 0.95),
            "max": latency_ms.last().copied().unwrap_or(0.0),
        },
        "skipped_candidates": attempts,
        "frames_in": frames,
        "units_sent": units,
        "keyframes_sent": keyframes,
        "packets_sent": packets,
        "bytes_sent": bytes,
        "gop_requested": GOP_FRAMES,
        "gop_accepted_by_encoder": gop_applied,
        "low_latency_accepted": low_latency_applied,
        "zero_b_frames_accepted": zero_b_frames_applied,
        "deliberately_dropped_packet": drop_packet,
        "header_prepended_from_media_type": header_prepended,
        "elapsed_seconds": elapsed,
        "send_bitrate_kbps": bytes as f64 * 8.0 / elapsed / 1000.0,
    }))
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

/// Live receiver on 127.0.0.1:`port`: reassembles units as packets arrive,
/// inspects each one, then decodes the stream with an OS decoder.
pub fn rtp_receive(
    codec: Codec,
    port: u16,
    frames: u32,
    directory: &Path,
) -> Result<serde_json::Value> {
    ensure!(
        !directory.exists(),
        "receive directory must not already exist"
    );
    ensure!((1..=600).contains(&frames), "frames must be 1..=600");
    let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, port))?;
    socket.set_read_timeout(Some(Duration::from_millis(100)))?;
    println!(
        "{}",
        serde_json::json!({"ready": true, "port": socket.local_addr()?.port()})
    );
    std::io::stdout().flush()?;
    let mut reassembler = Reassembler::new(codec);
    let mut units: Vec<Vec<u8>> = Vec::new();
    let mut arrivals = Vec::new();
    let mut buffer = vec![0u8; 65_536];
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut last_packet: Option<Instant> = None;
    while units.len() < frames as usize && Instant::now() < deadline {
        if last_packet.is_some_and(|at| at.elapsed() > Duration::from_secs(3)) {
            break; // sender finished or stalled
        }
        let length = match socket.recv(&mut buffer) {
            Ok(length) => length,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        last_packet = Some(Instant::now());
        if let Some(unit) = reassembler.push(&buffer[..length]) {
            arrivals.push(Instant::now());
            units.push(unit);
        }
    }
    ensure!(!units.is_empty(), "no complete access unit received");
    let summaries = units
        .iter()
        .map(|unit| inspect(codec, unit))
        .collect::<Option<Vec<_>>>()
        .context("received a malformed unit")?;
    let mut gaps: Vec<f64> = arrivals
        .windows(2)
        .map(|w| (w[1] - w[0]).as_secs_f64() * 1000.0)
        .collect();
    gaps.sort_by(f64::total_cmp);
    let span = (arrivals[arrivals.len() - 1] - arrivals[0]).as_secs_f64();
    let bytes: usize = units.iter().map(Vec::len).sum();
    std::fs::create_dir_all(directory)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join(format!("received.{}", extension(codec))))?;
    for unit in &units {
        file.write_all(unit)?;
    }
    let _runtime = Runtime::start()?;
    let stats = reassembler.stats;
    Ok(serde_json::json!({
        "codec": codec.as_str(),
        "units_received": units.len(),
        "frames_expected": frames,
        "packets": stats.packets,
        "lost_packets": stats.lost,
        "late_or_duplicate": stats.late_or_duplicate,
        "malformed_packets": stats.malformed,
        "foreign_ssrc": stats.foreign_ssrc,
        "dropped_units": stats.dropped_units,
        "withheld_awaiting_keyframe": stats.awaiting_keyframe,
        "keyframes": summaries.iter().filter(|s| s.keyframe).count(),
        "keyframe_positions": summaries.iter().enumerate().filter(|(_, s)| s.keyframe).map(|(i, _)| i).collect::<Vec<_>>(),
        "first_unit_keyframe": summaries[0].keyframe,
        "bitstream_dims": summaries.iter().find_map(|s| s.dims),
        "payload_bytes": bytes,
        "media_bitrate_kbps": bytes as f64 * 8.0 / (units.len() as f64 / 20.0) / 1000.0,
        "arrival_span_seconds": span,
        "interarrival_ms": {"p50": percentile(&gaps, 0.5), "p95": percentile(&gaps, 0.95), "max": gaps.last().copied().unwrap_or(0.0)},
        "os_decode": decode_report(codec, &units),
    }))
}

/// `--codecs`: capability discovery only, no capture.
pub fn codecs() -> Result<()> {
    let _runtime = Runtime::start()?;
    println!("{}", discover());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{fnv1a, is_fixture_yuv, percentile};

    #[test]
    fn fixture_yuv_tolerates_rounding_but_rejects_swaps() {
        assert!(is_fixture_yuv([135, 100, 62]));
        assert!(is_fixture_yuv([128, 108, 70]));
        assert!(!is_fixture_yuv([135, 62, 100]), "swapped chroma");
        assert!(!is_fixture_yuv([16, 128, 128]), "black");
    }

    #[test]
    fn hash_and_percentile_helpers() {
        assert_ne!(fnv1a(&[1, 2, 3]), fnv1a(&[1, 2, 4]));
        assert_eq!(fnv1a(&[]), 0xcbf2_9ce4_8422_2325);
        let sorted = [10.0, 20.0, 30.0, 40.0, 50.0];
        assert_eq!(
            (percentile(&sorted, 0.5), percentile(&sorted, 0.95)),
            (30.0, 50.0)
        );
        assert_eq!(percentile(&[], 0.5), 0.0);
    }
}
