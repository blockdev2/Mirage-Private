//! Streaming H.264 encoder over a Media Foundation hardware transform.
//!
//! The encoder consumes NV12 GPU textures without a CPU readback. Hardware
//! MFTs are asynchronous: they announce `NeedInput` and `HaveOutput` events,
//! which are polled without blocking so a stuck driver can never hang the
//! caller. Synchronous MFTs use the plain input/output loop. Only hardware
//! encoders are used; a machine without one gets an error naming that.

use crate::config::EncodedUnit;
use crate::nal::{analyze, with_header};
use anyhow::{anyhow, bail, ensure, Context, Result};
use std::collections::VecDeque;
use std::mem::ManuallyDrop;
use std::time::{Duration, Instant};
use windows::{
    core::{Interface, GUID},
    Win32::{
        Foundation::VARIANT_BOOL,
        Graphics::Direct3D11::*,
        Media::MediaFoundation::*,
        System::{
            Com::{CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_MULTITHREADED},
            Variant::{VARIANT, VT_BOOL, VT_UI4},
        },
    },
};

const MAX_UNIT_BYTES: u32 = 8 * 1024 * 1024;
/// How long one `encode` waits for an async encoder to ask for input.
const INPUT_WAIT: Duration = Duration::from_millis(250);
/// Distance between keyframes, in seconds.
const GOP_SECONDS: u32 = 2;
/// `eAVEncH264VProfile_High`.
const H264_PROFILE_HIGH: u32 = 100;

/// COM and Media Foundation for the current thread; drop it last.
pub struct MfRuntime;

impl MfRuntime {
    pub fn start() -> Result<Self> {
        // SAFETY: paired with `Drop`; failure paths undo what they started.
        unsafe {
            // S_FALSE (already initialised on this thread) is fine; a different
            // apartment mode is not, and `.ok()` reports it.
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
            if let Err(error) = MFStartup(MF_VERSION, MFSTARTUP_FULL) {
                CoUninitialize();
                return Err(error.into());
            }
        }
        Ok(Self)
    }
}

impl Drop for MfRuntime {
    fn drop(&mut self) {
        // SAFETY: every Media Foundation object is dropped before this guard.
        unsafe {
            let _ = MFShutdown();
            CoUninitialize();
        }
    }
}

/// Keeps D3D11 multithread protection on while Media Foundation worker
/// threads may use the immediate context; restores the previous mode.
pub struct ContextProtection {
    lock: ID3D11Multithread,
    previous: bool,
}

impl ContextProtection {
    pub fn enable(context: &ID3D11DeviceContext) -> Result<Self> {
        let lock: ID3D11Multithread = context.cast().context("query context protection")?;
        // SAFETY: plain state toggle on a live context.
        let previous = unsafe { lock.SetMultithreadProtected(true).as_bool() };
        Ok(Self { lock, previous })
    }
}

impl Drop for ContextProtection {
    fn drop(&mut self) {
        // SAFETY: restores the mode found at `enable`.
        unsafe {
            let _ = self.lock.SetMultithreadProtected(self.previous);
        }
    }
}

fn set_codec_u32(transform: &IMFTransform, api: &GUID, value: u32) -> bool {
    let Ok(codec_api) = transform.cast::<ICodecAPI>() else {
        return false;
    };
    let mut variant = VARIANT::default();
    // SAFETY: a VT_UI4 VARIANT needs no cleanup; the union field matches the tag.
    unsafe {
        let inner = &mut *variant.Anonymous.Anonymous;
        inner.vt = VT_UI4;
        inner.Anonymous.ulVal = value;
        codec_api.SetValue(api, &variant).is_ok()
    }
}

fn set_codec_bool(transform: &IMFTransform, api: &GUID, value: bool) -> bool {
    let Ok(codec_api) = transform.cast::<ICodecAPI>() else {
        return false;
    };
    let mut variant = VARIANT::default();
    // SAFETY: a VT_BOOL VARIANT needs no cleanup; the union field matches the tag.
    unsafe {
        let inner = &mut *variant.Anonymous.Anonymous;
        inner.vt = VT_BOOL;
        inner.Anonymous.boolVal = VARIANT_BOOL(if value { -1 } else { 0 });
        codec_api.SetValue(api, &variant).is_ok()
    }
}

/// Activations of hardware H.264 encoders that take NV12.
fn hardware_candidates() -> Vec<IMFActivate> {
    let info = |subtype: GUID| MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: subtype,
    };
    let (input, output) = (info(MFVideoFormat_NV12), info(MFVideoFormat_H264));
    let mut array: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    let mut found = Vec::new();
    // SAFETY: MF allocates `array` with CoTaskMemAlloc; each element is read
    // exactly once (taking its reference) before the array is freed.
    unsafe {
        if MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG(MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_SORTANDFILTER.0),
            Some(&input),
            Some(&output),
            &mut array,
            &mut count,
        )
        .is_err()
            || array.is_null()
        {
            return found;
        }
        for index in 0..count as usize {
            if let Some(activate) = std::ptr::read(array.add(index)) {
                found.push(activate);
            }
        }
        CoTaskMemFree(Some(array.cast()));
    }
    found
}

/// The PCI device path a hardware encoder announces (`PCI\VEN_1002&DEV_7590…`).
/// Not exported by the `windows` crate.
const MFT_ENUM_HARDWARE_DEVICE_PATH: GUID = GUID::from_u128(0x556155e0_8b27_44ac_8dbf_2547b9bd7112);

/// With two GPUs of one vendor, the other GPU's encoder accepts the capture
/// device, takes one picture and then goes quiet, so a viewer gets no video.
/// Keep the encoders whose device path names the capture GPU, or every
/// encoder when none can be matched.
fn on_capture_gpu(candidates: Vec<IMFActivate>, device: &ID3D11Device) -> Vec<IMFActivate> {
    // SAFETY: read-only queries on a live device.
    let gpu = unsafe {
        device
            .cast::<windows::Win32::Graphics::Dxgi::IDXGIDevice>()
            .and_then(|d| d.GetAdapter())
            .and_then(|a| a.GetDesc())
            .map(|desc| (desc.VendorId, desc.DeviceId))
    };
    let Ok(gpu) = gpu else { return candidates };
    let paths: Vec<Option<String>> = candidates.iter().map(device_path).collect();
    keep_matching(candidates, &paths, gpu)
}

fn device_path(activate: &IMFActivate) -> Option<String> {
    let mut text = [0u16; 512];
    let mut len = 0;
    // SAFETY: the buffer is large enough for a PCI path; a longer one fails.
    unsafe {
        activate
            .GetString(&MFT_ENUM_HARDWARE_DEVICE_PATH, &mut text, Some(&mut len))
            .ok()?;
    }
    Some(String::from_utf16_lossy(&text[..len as usize]))
}

fn keep_matching<T>(candidates: Vec<T>, paths: &[Option<String>], gpu: (u32, u32)) -> Vec<T> {
    let wanted = format!("VEN_{:04X}&DEV_{:04X}", gpu.0, gpu.1);
    let matches = |path: &Option<String>| {
        path.as_deref()
            .is_some_and(|p| p.to_ascii_uppercase().contains(&wanted))
    };
    if !paths.iter().any(matches) {
        return candidates;
    }
    candidates
        .into_iter()
        .zip(paths)
        .filter(|(_, path)| matches(path))
        .map(|(candidate, _)| candidate)
        .collect()
}

fn media_type(subtype: &GUID, size: (u32, u32), fps: u32) -> Result<IMFMediaType> {
    // SAFETY: plain attribute writes on a fresh media type.
    unsafe {
        let media = MFCreateMediaType()?;
        media.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        media.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        media.SetUINT64(
            &MF_MT_FRAME_SIZE,
            (u64::from(size.0) << 32) | u64::from(size.1),
        )?;
        media.SetUINT64(&MF_MT_FRAME_RATE, (u64::from(fps) << 32) | 1)?;
        media.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1_u64 << 32) | 1)?;
        media.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        media.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32)?;
        media.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32)?;
        media.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
        media.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
        Ok(media)
    }
}

enum Pulled {
    Unit(Vec<u8>, i64),
    NeedInput,
    Changed,
}

/// One `ProcessOutput`, allocating a sample only when the MFT does not.
fn pull(transform: &IMFTransform) -> Result<Pulled> {
    // SAFETY: the ManuallyDrop fields of the output buffer are released on
    // every path below.
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
                let sample = sample.context("encoder returned no sample")?;
                let time = sample.GetSampleTime().unwrap_or(-1);
                Ok(Pulled::Unit(sample_bytes(&sample)?, time))
            }
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(Pulled::NeedInput),
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => Ok(Pulled::Changed),
            Err(e) => Err(e.into()),
        }
    }
}

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
        copied.context("encoder output is missing or exceeds the size bound")
    }
}

fn gpu_sample(texture: &ID3D11Texture2D, time: i64, duration: i64) -> Result<IMFSample> {
    // SAFETY: wraps a live GPU texture; the buffer keeps its own reference.
    unsafe {
        let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, texture, 0, false)?;
        let flat: IMF2DBuffer = buffer.cast()?;
        buffer.SetCurrentLength(flat.GetContiguousLength()?)?;
        let sample = MFCreateSample()?;
        sample.AddBuffer(&buffer)?;
        sample.SetSampleTime(time)?;
        sample.SetSampleDuration(duration)?;
        Ok(sample)
    }
}

pub struct H264Encoder {
    // Field order is drop order: the transform goes before its device manager.
    transform: IMFTransform,
    activate: IMFActivate,
    _manager: IMFDXGIDeviceManager,
    events: Option<IMFMediaEventGenerator>,
    frame_100ns: i64,
    /// Async: `NeedInput` events not yet answered.
    need_input: u32,
    queued: VecDeque<(Vec<u8>, i64)>,
    header: Option<Vec<u8>>,
    force_keyframe: bool,
}

impl Drop for H264Encoder {
    fn drop(&mut self) {
        // SAFETY: ends the stream and releases the activated object once.
        unsafe {
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            let _ = self.activate.ShutdownObject();
        }
    }
}

impl H264Encoder {
    /// Opens the first hardware H.264 encoder that accepts the capture device.
    /// `size` must be even; the stream is low-latency with no B-frames and a
    /// two second keyframe interval.
    pub fn new(
        device: &ID3D11Device,
        size: (u32, u32),
        fps: u32,
        bitrate_kbps: u32,
    ) -> Result<Self> {
        let manager = device_manager(device)?;
        let candidates = on_capture_gpu(hardware_candidates(), device);
        ensure!(
            !candidates.is_empty(),
            "no hardware H.264 encoder is available on this machine"
        );
        let mut failures = Vec::new();
        for (index, activate) in candidates.into_iter().enumerate() {
            match configure(&activate, &manager, size, fps, bitrate_kbps) {
                Ok(configured) => return Ok(configured.finish(activate, manager, fps)),
                Err(error) => {
                    // SAFETY: releases the failed activation's object.
                    unsafe {
                        let _ = activate.ShutdownObject();
                    }
                    failures.push(format!("#{index}: {error:#}"));
                }
            }
        }
        bail!(
            "no hardware H.264 encoder accepted the capture device ({})",
            failures.join("; ")
        )
    }

    /// The next unit will be an IDR picture (a late viewer, a lost packet).
    pub fn request_keyframe(&mut self) {
        self.force_keyframe = true;
    }

    /// Encodes one NV12 texture and returns every unit the encoder has ready,
    /// which may be none (pipeline delay) or, rarely, more than one.
    pub fn encode(&mut self, nv12: &ID3D11Texture2D, time_100ns: i64) -> Result<Vec<EncodedUnit>> {
        if std::mem::take(&mut self.force_keyframe) {
            set_codec_u32(&self.transform, &CODECAPI_AVEncVideoForceKeyFrame, 1);
        }
        let sample = gpu_sample(nv12, time_100ns, self.frame_100ns)?;
        // SAFETY: single-threaded use of one transform; every wait is bounded.
        unsafe {
            if self.events.is_some() {
                self.pump(true)?;
                if self.need_input == 0 {
                    // The encoder is behind: skip this picture instead of queueing.
                    return self.take_units();
                }
                self.need_input -= 1;
                self.transform.ProcessInput(0, &sample, 0)?;
                self.pump(false)?;
            } else {
                loop {
                    match self.transform.ProcessInput(0, &sample, 0) {
                        Ok(()) => break,
                        Err(e) if e.code() == MF_E_NOTACCEPTING => self.drain()?,
                        Err(e) => return Err(e.into()),
                    }
                }
                self.drain()?;
            }
        }
        self.take_units()
    }

    /// Reads async events until input is wanted (when `wait_for_input`) or no
    /// event is pending, collecting finished units on the way.
    unsafe fn pump(&mut self, wait_for_input: bool) -> Result<()> {
        let events = self.events.clone().ok_or_else(|| anyhow!("not async"))?;
        let deadline = Instant::now() + INPUT_WAIT;
        loop {
            // SAFETY: the caller guarantees single-threaded use of the transform.
            let event = match unsafe { events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(event) => event,
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => {
                    if wait_for_input && self.need_input == 0 && Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    return Ok(());
                }
                Err(e) => return Err(e.into()),
            };
            // SAFETY: reading the type of a live event.
            let kind = unsafe { event.GetType()? };
            if kind == METransformNeedInput.0 as u32 {
                self.need_input += 1;
                if wait_for_input {
                    return Ok(());
                }
            } else if kind == METransformHaveOutput.0 as u32 {
                self.collect()?;
            }
        }
    }

    fn collect(&mut self) -> Result<()> {
        match pull(&self.transform)? {
            Pulled::Unit(bytes, time) => self.queued.push_back((bytes, time)),
            Pulled::Changed => self.renegotiate()?,
            Pulled::NeedInput => {}
        }
        Ok(())
    }

    /// Re-applies the encoder's preferred output type after a stream change.
    fn renegotiate(&mut self) -> Result<()> {
        // SAFETY: plain type negotiation on the live transform.
        unsafe {
            let offer = self.transform.GetOutputAvailableType(0, 0)?;
            self.transform.SetOutputType(0, &offer, 0)?;
        }
        Ok(())
    }

    /// Synchronous encoders: reads output until the encoder wants more input.
    fn drain(&mut self) -> Result<()> {
        let deadline = Instant::now() + INPUT_WAIT;
        loop {
            ensure!(
                Instant::now() < deadline,
                "encoder output deadline exceeded"
            );
            match pull(&self.transform)? {
                Pulled::Unit(bytes, time) => self.queued.push_back((bytes, time)),
                Pulled::Changed => self.renegotiate()?,
                Pulled::NeedInput => return Ok(()),
            }
        }
    }

    fn take_units(&mut self) -> Result<Vec<EncodedUnit>> {
        if self.header.is_none() && !self.queued.is_empty() {
            // Some encoders publish the parameter sets only once output flows.
            self.header = sequence_header(&self.transform);
        }
        let mut units = Vec::new();
        while let Some((data, time)) = self.queued.pop_front() {
            let info = analyze(&data);
            // A late viewer starts on a keyframe, so every keyframe carries
            // the parameter sets even if the encoder emitted them only once.
            let data = match (&self.header, info.keyframe && !info.parameter_sets) {
                (Some(header), true) => with_header(header, &data),
                _ => data,
            };
            units.push(EncodedUnit {
                data,
                keyframe: info.keyframe,
                time_100ns: time,
            });
        }
        Ok(units)
    }
}

/// SPS and PPS some encoders publish only on the output media type.
fn sequence_header(transform: &IMFTransform) -> Option<Vec<u8>> {
    // SAFETY: the blob size is queried first and bounded before allocation.
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

fn device_manager(device: &ID3D11Device) -> Result<IMFDXGIDeviceManager> {
    // SAFETY: creates a manager and binds it to the live capture device.
    unsafe {
        let mut token = 0;
        let mut manager = None;
        MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
        let manager = manager.ok_or_else(|| anyhow!("missing DXGI device manager"))?;
        manager.ResetDevice(device, token)?;
        Ok(manager)
    }
}

struct Configured {
    transform: IMFTransform,
    events: Option<IMFMediaEventGenerator>,
}

impl Configured {
    fn finish(self, activate: IMFActivate, manager: IMFDXGIDeviceManager, fps: u32) -> H264Encoder {
        let header = sequence_header(&self.transform);
        H264Encoder {
            transform: self.transform,
            activate,
            _manager: manager,
            events: self.events,
            frame_100ns: 10_000_000 / i64::from(fps),
            need_input: 0,
            queued: VecDeque::new(),
            header,
            force_keyframe: false,
        }
    }
}

/// Configures one candidate. Hardware encoders of another adapter reject the
/// capture device here, and the caller moves to the next one.
fn configure(
    activate: &IMFActivate,
    manager: &IMFDXGIDeviceManager,
    size: (u32, u32),
    fps: u32,
    bitrate_kbps: u32,
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
        transform
            .ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)
            .context("encoder rejected the capture device manager")?;
        // Interactive streaming: low latency, no B-frame reordering. Encoders
        // may decline either request.
        set_codec_bool(&transform, &CODECAPI_AVLowLatencyMode, true);
        set_codec_u32(&transform, &CODECAPI_AVEncMPVDefaultBPictureCount, 0);
        set_codec_u32(&transform, &CODECAPI_AVEncMPVGOPSize, fps * GOP_SECONDS);

        let output = media_type(&MFVideoFormat_H264, size, fps)?;
        output.SetUINT32(&MF_MT_AVG_BITRATE, bitrate_kbps.saturating_mul(1000))?;
        // The official client decodes only H.264 High (profile-level-id 640c1f),
        // so ask for it; an encoder that declines keeps its own default profile.
        let _ = output.SetUINT32(&MF_MT_MPEG2_PROFILE, H264_PROFILE_HIGH);
        if transform.SetOutputType(0, &output, 0).is_err() {
            output.DeleteItem(&MF_MT_MPEG2_PROFILE)?;
            transform
                .SetOutputType(0, &output, 0)
                .context("set encoder output type")?;
        }
        let input = media_type(&MFVideoFormat_NV12, size, fps)?;
        if transform.SetInputType(0, &input, 0).is_err() {
            // Fall back to the encoder's own NV12 offer at our size and rate.
            let mut accepted = false;
            for index in 0..32 {
                let Ok(offer) = transform.GetInputAvailableType(0, index) else {
                    break;
                };
                if offer.GetGUID(&MF_MT_SUBTYPE).ok() != Some(MFVideoFormat_NV12) {
                    continue;
                }
                offer.SetUINT64(
                    &MF_MT_FRAME_SIZE,
                    (u64::from(size.0) << 32) | u64::from(size.1),
                )?;
                offer.SetUINT64(&MF_MT_FRAME_RATE, (u64::from(fps) << 32) | 1)?;
                if transform.SetInputType(0, &offer, 0).is_ok() {
                    accepted = true;
                    break;
                }
            }
            ensure!(accepted, "encoder accepted no NV12 input type");
        }
        transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
        transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        let events = if is_async {
            Some(transform.cast::<IMFMediaEventGenerator>()?)
        } else {
            None
        };
        Ok(Configured { transform, events })
    }
}

#[cfg(test)]
mod gpu_tests {
    use super::keep_matching;

    #[test]
    fn keeps_only_the_capture_gpus_encoder() {
        // Seen on a Ryzen with a Radeon card: iGPU first, card second, one untagged.
        let paths = [
            Some(r"PCI\VEN_1002&DEV_164E&SUBSYS_88771043&REV_C3\4&1".to_string()),
            Some(r"PCI\VEN_1002&DEV_7590&SUBSYS_86011EAE&REV_C0\6&3".to_string()),
            None,
        ];
        let picked = keep_matching(vec!["igpu", "card", "untagged"], &paths, (0x1002, 0x7590));
        assert_eq!(picked, vec!["card"]);
    }

    #[test]
    fn keeps_every_encoder_when_none_names_the_gpu() {
        let paths = [None, Some(r"PCI\VEN_10DE&DEV_2684".to_string())];
        let picked = keep_matching(vec!["a", "b"], &paths, (0x1002, 0x7590));
        assert_eq!(picked, vec!["a", "b"]);
    }
}
