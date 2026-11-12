//! Direct Media Foundation transforms: codec discovery, encoding the owned
//! NV12 GPU texture into raw access units (no container, no sink writer), and
//! decoding them back through an OS decoder for a pixel check. No FFmpeg.

use crate::encoder::{ContextProtection, Runtime, convert, media_type};
use anyhow::{Context, Result, anyhow, bail, ensure};
use mirage_native_capture_probe::{
    bitstream::{Codec, av1_canonical, inspect},
    rtp::{RTP_HEADER_LEN, annexb_canonical, depacketize, payloads, rtp_packet},
};
use std::{
    io::Write,
    mem::ManuallyDrop,
    path::Path,
    time::{Duration, Instant},
};
use windows::{
    Win32::{Graphics::Direct3D11::*, Media::MediaFoundation::*, System::Com::CoTaskMemFree},
    core::{GUID, Interface},
};

const FRAMES: i64 = 20;
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
    Unit(Vec<u8>),
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
            Ok(()) => Ok(Pulled::Unit(sample_bytes(
                &sample.context("MFT returned no sample")?,
            )?)),
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(Pulled::NeedInput),
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => Ok(Pulled::Changed),
            Err(e) => Err(e.into()),
        }
    }
}

fn input_sample(buffer: &IMFMediaBuffer, frame: i64) -> Result<IMFSample> {
    // SAFETY: Plain sample construction around a live buffer.
    unsafe {
        let sample = MFCreateSample()?;
        sample.AddBuffer(buffer)?;
        sample.SetSampleTime(frame * FRAME_100NS)?;
        sample.SetSampleDuration(FRAME_100NS)?;
        Ok(sample)
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
) -> Result<(IMFTransform, bool)> {
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
        Ok((transform, is_async))
    }
}

/// Feed the repeated GPU frame and collect access units.
fn run_encoder(
    transform: &IMFTransform,
    is_async: bool,
    buffer: &IMFMediaBuffer,
) -> Result<Vec<Vec<u8>>> {
    let mut units = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(15);
    let renegotiate = || -> Result<()> {
        // SAFETY: Re-apply the MFT's preferred output type after a stream change.
        unsafe {
            let offer = transform.GetOutputAvailableType(0, 0)?;
            transform.SetOutputType(0, &offer, 0)?;
        }
        Ok(())
    };
    // SAFETY: Single-threaded use of one transform; events are polled without
    // blocking so the overall deadline always holds.
    unsafe {
        if is_async {
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
                    if sent < FRAMES {
                        transform.ProcessInput(0, &input_sample(buffer, sent)?, 0)?;
                        sent += 1;
                    } else if !draining {
                        transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)?;
                        transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)?;
                        draining = true;
                    }
                } else if kind == METransformHaveOutput.0 as u32 {
                    match pull(transform)? {
                        Pulled::Unit(unit) => units.push(unit),
                        Pulled::Changed => renegotiate()?,
                        Pulled::NeedInput => {}
                    }
                } else if kind == METransformDrainComplete.0 as u32 {
                    break;
                }
            }
        } else {
            let drain = |units: &mut Vec<Vec<u8>>| -> Result<()> {
                loop {
                    ensure!(
                        Instant::now() < deadline,
                        "encoder output deadline exceeded"
                    );
                    match pull(transform)? {
                        Pulled::Unit(unit) => units.push(unit),
                        Pulled::Changed => renegotiate()?,
                        Pulled::NeedInput => return Ok(()),
                    }
                }
            };
            for frame in 0..FRAMES {
                let sample = input_sample(buffer, frame)?;
                loop {
                    match transform.ProcessInput(0, &sample, 0) {
                        Ok(()) => break,
                        Err(e) if e.code() == MF_E_NOTACCEPTING => drain(&mut units)?,
                        Err(e) => return Err(e.into()),
                    }
                }
                drain(&mut units)?;
            }
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)?;
            drain(&mut units)?;
        }
        let _ = transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
    }
    ensure!(!units.is_empty(), "encoder produced no access units");
    Ok(units)
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

/// Decode the first unit(s) with an OS decoder and return the center NV12
/// (Y, U, V) sample. `Ok(None)` means no decoder is installed for the codec.
fn decode_center(codec: Codec, units: &[Vec<u8>]) -> Result<Option<[u8; 3]>> {
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
    unsafe {
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
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut frame = None;
        let mut feed = units.iter().enumerate();
        let mut ended = false;
        while frame.is_none() {
            ensure!(Instant::now() < deadline, "decoder deadline exceeded");
            match pull(&decoder)? {
                Pulled::Unit(bytes) => frame = Some(bytes),
                Pulled::Changed => output_type = choose_nv12()?,
                Pulled::NeedInput => match feed.next() {
                    Some((index, unit)) => {
                        let buffer = MFCreateMemoryBuffer(unit.len() as u32)?;
                        let mut data = std::ptr::null_mut();
                        buffer.Lock(&mut data, None, None)?;
                        std::ptr::copy_nonoverlapping(unit.as_ptr(), data, unit.len());
                        buffer.Unlock()?;
                        buffer.SetCurrentLength(unit.len() as u32)?;
                        let sample = input_sample(&buffer, index as i64)?;
                        decoder.ProcessInput(0, &sample, 0)?;
                    }
                    None if !ended => {
                        decoder.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)?;
                        decoder.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)?;
                        ended = true;
                    }
                    None => bail!("decoder produced no frame"),
                },
            }
        }
        let frame = frame.context("no decoded frame")?;
        let size = output_type.GetUINT64(&MF_MT_FRAME_SIZE)?;
        let (width, height) = ((size >> 32) as usize, (size & 0xffff_ffff) as usize);
        let stride = output_type
            .GetUINT32(&MF_MT_DEFAULT_STRIDE)
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
        let y = frame[180 * stride + 320];
        let chroma = plane_rows * stride + 90 * stride + 320;
        let (u, v) = (
            *frame.get(chroma).context("chroma")?,
            *frame.get(chroma + 1).context("chroma")?,
        );
        Ok(Some([y, u, v]))
    }
}

/// BT.709 limited-range NV12 for the fixture BGR (80,180,20): Y≈135, U≈100, V≈62.
pub fn is_fixture_yuv(yuv: [u8; 3]) -> bool {
    yuv[0].abs_diff(135) <= 14 && yuv[1].abs_diff(100) <= 14 && yuv[2].abs_diff(62) <= 14
}

fn encode_codec(
    codec: Codec,
    manager: &IMFDXGIDeviceManager,
    buffer: &IMFMediaBuffer,
    directory: &Path,
) -> serde_json::Value {
    let sub = subtype(codec);
    let mut attempts = Vec::new();
    for (hardware, flags) in [(true, HARDWARE), (false, SOFTWARE)] {
        for (index, activate) in enumerate(
            MFT_CATEGORY_VIDEO_ENCODER,
            flags,
            Some(MFVideoFormat_NV12),
            Some(sub),
        )
        .iter()
        .enumerate()
        {
            let result = configure_encoder(activate, codec, manager, hardware).and_then(
                |(transform, is_async)| {
                    let units = run_encoder(&transform, is_async, buffer)?;
                    Ok((units, sequence_header(&transform)))
                },
            );
            // SAFETY: Release the activated object regardless of outcome.
            unsafe {
                let _ = activate.ShutdownObject();
            }
            match result {
                Ok((units, header)) => {
                    return report(codec, hardware, index, &attempts, units, header, directory)
                        .unwrap_or_else(|error| serde_json::json!({"encoded":false,"error":format!("{error:#}")}));
                }
                Err(error) => attempts.push(format!(
                    "{}#{index}: {error:#}",
                    if hardware { "hw" } else { "sw" }
                )),
            }
        }
    }
    serde_json::json!({"encoded":false,"failed_attempts":attempts})
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
    let decoded = decode_center(codec, &units);
    let decode = match decoded {
        Ok(Some(yuv)) => {
            serde_json::json!({"status":"decoded","center_yuv":yuv,"fixture_matches":is_fixture_yuv(yuv)})
        }
        Ok(None) => serde_json::json!({"status":"no_os_decoder"}),
        Err(error) => serde_json::json!({"status":"failed","error":format!("{error:#}")}),
    };
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
        "os_decode": decode,
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
    // SAFETY: The manager and GPU buffer reference live COM objects that are
    // dropped before the runtime guard.
    let (manager, buffer) = unsafe {
        let mut token = 0;
        let mut manager = None;
        MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
        let manager = manager.ok_or_else(|| anyhow!("missing DXGI manager"))?;
        manager.ResetDevice(device, token)?;
        let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &nv12, 0, false)?;
        let buffer2d: IMF2DBuffer = buffer.cast()?;
        buffer.SetCurrentLength(buffer2d.GetContiguousLength()?)?;
        (manager, buffer)
    };
    let mut codecs = serde_json::Map::new();
    for codec in Codec::ALL {
        codecs.insert(
            codec.as_str().into(),
            encode_codec(codec, &manager, &buffer, directory),
        );
    }
    Ok(serde_json::json!({"capabilities": discover(), "codecs": codecs}))
}

/// `--codecs`: capability discovery only, no capture.
pub fn codecs() -> Result<()> {
    let _runtime = Runtime::start()?;
    println!("{}", discover());
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::is_fixture_yuv;

    #[test]
    fn fixture_yuv_tolerates_rounding_but_rejects_swaps() {
        assert!(is_fixture_yuv([135, 100, 62]));
        assert!(is_fixture_yuv([128, 108, 70]));
        assert!(!is_fixture_yuv([135, 62, 100]), "swapped chroma");
        assert!(!is_fixture_yuv([16, 128, 128]), "black");
    }
}
