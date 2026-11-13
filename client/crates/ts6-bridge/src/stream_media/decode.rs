//! H.264 access units to RGBA frames through the Windows Media Foundation
//! decoder (ported from `recon/probe-capture/src/mft.rs`). One decoder lives
//! on one thread: Media Foundation and COM are started for that thread.

/// Decoded frames wider than this are scaled down to it.
pub const MAX_FRAME_WIDTH: u32 = 1920;

/// One decoded picture, RGBA8, rows packed (`width * 4` bytes each).
#[derive(Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Frame({}x{})", self.width, self.height)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// No H.264 decoder is installed (or Media Foundation is missing).
    Unavailable,
    /// The decoder rejected the data; wait for the next keyframe.
    Failed,
}

/// BT.709 limited-range NV12 to RGBA, scaled (nearest) to at most
/// `MAX_FRAME_WIDTH` wide. `stride` is the luma row pitch and `rows` the
/// number of luma rows stored before the chroma plane.
pub fn nv12_to_rgba(
    data: &[u8],
    width: u32,
    height: u32,
    stride: usize,
    rows: usize,
) -> Option<Frame> {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || stride < w || rows < h || data.len() < stride * rows + stride * h / 2 {
        return None;
    }
    let (out_w, out_h) = if width > MAX_FRAME_WIDTH {
        (
            MAX_FRAME_WIDTH as usize,
            (h * MAX_FRAME_WIDTH as usize / w).max(1),
        )
    } else {
        (w, h)
    };
    let chroma = stride * rows;
    let xs: Vec<usize> = (0..out_w).map(|ox| ox * w / out_w).collect();
    let mut rgba = vec![0u8; out_w * out_h * 4];
    // A 1080p picture takes about 70 ms in one float loop, which caps the
    // stream near 14 fps; fixed point split over a few threads is far faster.
    let workers = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .clamp(1, 4);
    let band = out_h.div_ceil(workers).max(1);
    std::thread::scope(|scope| {
        for (index, rows_out) in rgba.chunks_mut(band * out_w * 4).enumerate() {
            let xs = &xs;
            scope.spawn(move || {
                for (n, row) in rows_out.chunks_exact_mut(out_w * 4).enumerate() {
                    let y = (index * band + n) * h / out_h;
                    let luma_row = &data[y * stride..y * stride + w];
                    let chroma_row = &data[chroma + (y / 2) * stride..];
                    for (px, &x) in row.as_chunks_mut::<4>().0.iter_mut().zip(xs) {
                        let c = (i32::from(luma_row[x]) - 16) * 298 + 128;
                        let u = i32::from(chroma_row[x & !1]) - 128;
                        let v = i32::from(chroma_row[(x & !1) + 1]) - 128;
                        px[0] = ((c + 459 * v) >> 8).clamp(0, 255) as u8;
                        px[1] = ((c - 55 * u - 136 * v) >> 8).clamp(0, 255) as u8;
                        px[2] = ((c + 541 * u) >> 8).clamp(0, 255) as u8;
                        px[3] = 255;
                    }
                }
            });
        }
    });
    Some(Frame {
        width: out_w as u32,
        height: out_h as u32,
        rgba,
    })
}

#[cfg(windows)]
pub use imp::H264Decoder;

#[cfg(windows)]
mod imp {
    use super::{nv12_to_rgba, DecodeError, Frame};
    use std::mem::ManuallyDrop;
    use windows::core::GUID;
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::{
        CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_MULTITHREADED,
    };

    const MAX_UNIT_BYTES: u32 = 32 * 1024 * 1024;

    /// COM + Media Foundation for the current thread, undone on drop.
    pub(crate) struct Runtime;

    impl Runtime {
        pub(crate) fn start() -> Result<Self, DecodeError> {
            // SAFETY: Paired with the teardown in Drop on the same thread.
            unsafe {
                CoInitializeEx(None, COINIT_MULTITHREADED)
                    .ok()
                    .map_err(|_| DecodeError::Unavailable)?;
                if MFStartup(MF_VERSION, MFSTARTUP_FULL).is_err() {
                    CoUninitialize();
                    return Err(DecodeError::Unavailable);
                }
            }
            Ok(Self)
        }
    }

    impl Drop for Runtime {
        fn drop(&mut self) {
            // SAFETY: Every MF object is dropped before this guard (field order).
            unsafe {
                let _ = MFShutdown();
                CoUninitialize();
            }
        }
    }

    /// Enumerate transforms; the returned activations own their references.
    pub(crate) fn enumerate(
        category: GUID,
        input: Option<GUID>,
        output: Option<GUID>,
    ) -> Vec<IMFActivate> {
        let info = |sub: GUID| MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: sub,
        };
        let (input, output) = (input.map(info), output.map(info));
        let flags = MFT_ENUM_FLAG(MFT_ENUM_FLAG_SYNCMFT.0 | MFT_ENUM_FLAG_SORTANDFILTER.0);
        let mut array: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        let mut found = Vec::new();
        // SAFETY: MF allocates `array` with CoTaskMemAlloc; each element is
        // read exactly once (taking its reference) before the array is freed.
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

    pub(crate) enum Pulled {
        Unit(Vec<u8>),
        NeedInput,
        Changed,
    }

    /// One ProcessOutput call, allocating a sample only when the MFT does not.
    pub(crate) fn pull(transform: &IMFTransform) -> windows::core::Result<Pulled> {
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
                Ok(()) => match sample {
                    Some(sample) => Ok(Pulled::Unit(sample_bytes(&sample)?)),
                    None => Ok(Pulled::NeedInput),
                },
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(Pulled::NeedInput),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => Ok(Pulled::Changed),
                Err(e) => Err(e),
            }
        }
    }

    fn sample_bytes(sample: &IMFSample) -> windows::core::Result<Vec<u8>> {
        // SAFETY: Lock/Unlock are paired; the copy stays within the reported length.
        unsafe {
            let buffer = sample.ConvertToContiguousBuffer()?;
            let mut data = std::ptr::null_mut();
            let mut length = 0u32;
            buffer.Lock(&mut data, None, Some(&mut length))?;
            let copied = if data.is_null() || length > MAX_UNIT_BYTES {
                Vec::new()
            } else {
                std::slice::from_raw_parts(data, length as usize).to_vec()
            };
            buffer.Unlock()?;
            Ok(copied)
        }
    }

    pub(crate) fn memory_sample(data: &[u8], time: i64) -> windows::core::Result<IMFSample> {
        // SAFETY: Lock/Unlock paired; the copy fits the allocated length.
        unsafe {
            let buffer = MFCreateMemoryBuffer(data.len() as u32)?;
            let mut target = std::ptr::null_mut();
            buffer.Lock(&mut target, None, None)?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), target, data.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(data.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(time)?;
            sample.SetSampleDuration(333_333)?;
            Ok(sample)
        }
    }

    /// Decoded picture layout from the current output type.
    #[derive(Clone, Copy)]
    struct Layout {
        width: u32,
        height: u32,
        stride: usize,
    }

    pub struct H264Decoder {
        transform: IMFTransform,
        layout: Option<Layout>,
        time: i64,
        // Last: Media Foundation shuts down after the transform is released.
        _runtime: Runtime,
    }

    impl H264Decoder {
        pub fn new() -> Result<Self, DecodeError> {
            let runtime = Runtime::start()?;
            let activate = enumerate(
                MFT_CATEGORY_VIDEO_DECODER,
                Some(MFVideoFormat_H264),
                Some(MFVideoFormat_NV12),
            )
            .into_iter()
            .next()
            .ok_or(DecodeError::Unavailable)?;
            // SAFETY: One decoder used synchronously on this thread.
            let transform = unsafe {
                let transform: IMFTransform = activate
                    .ActivateObject()
                    .map_err(|_| DecodeError::Unavailable)?;
                if let Ok(attributes) = transform.GetAttributes() {
                    let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);
                }
                let input = MFCreateMediaType().map_err(|_| DecodeError::Unavailable)?;
                input
                    .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                    .and_then(|()| input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264))
                    .and_then(|()| transform.SetInputType(0, &input, 0))
                    .map_err(|_| DecodeError::Unavailable)?;
                transform
            };
            let mut decoder = Self {
                transform,
                layout: None,
                time: 0,
                _runtime: runtime,
            };
            decoder.choose_output();
            // SAFETY: Starting the stream on the configured transform.
            unsafe {
                let _ = decoder
                    .transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
                let _ = decoder
                    .transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
            }
            Ok(decoder)
        }

        /// Selects NV12 output and reads the picture layout; None until the
        /// decoder has seen a sequence header.
        fn choose_output(&mut self) {
            // SAFETY: Attribute reads/writes on live media types.
            unsafe {
                for index in 0..64 {
                    let Ok(offer) = self.transform.GetOutputAvailableType(0, index) else {
                        break;
                    };
                    if offer.GetGUID(&MF_MT_SUBTYPE).ok() != Some(MFVideoFormat_NV12) {
                        continue;
                    }
                    if self.transform.SetOutputType(0, &offer, 0).is_err() {
                        continue;
                    }
                    let Ok(size) = offer.GetUINT64(&MF_MT_FRAME_SIZE) else {
                        return;
                    };
                    let (mut width, mut height) = ((size >> 32) as u32, size as u32);
                    let mut aperture = MFVideoArea::default();
                    if offer
                        .GetBlob(
                            &MF_MT_MINIMUM_DISPLAY_APERTURE,
                            std::slice::from_raw_parts_mut(
                                (&mut aperture as *mut MFVideoArea).cast::<u8>(),
                                std::mem::size_of::<MFVideoArea>(),
                            ),
                            None,
                        )
                        .is_ok()
                        && aperture.Area.cx > 0
                        && aperture.Area.cy > 0
                    {
                        width = aperture.Area.cx as u32;
                        height = aperture.Area.cy as u32;
                    }
                    let stride = offer
                        .GetUINT32(&MF_MT_DEFAULT_STRIDE)
                        .map(|s| (s as i32).unsigned_abs() as usize)
                        .unwrap_or((size >> 32) as usize);
                    self.layout = Some(Layout {
                        width,
                        height,
                        stride,
                    });
                    return;
                }
            }
        }

        /// Feeds one Annex B access unit; returns the newest picture it completed.
        pub fn decode(&mut self, access_unit: &[u8]) -> Result<Option<Frame>, DecodeError> {
            if access_unit.is_empty() || access_unit.len() as u64 > u64::from(MAX_UNIT_BYTES) {
                return Err(DecodeError::Failed);
            }
            let sample = memory_sample(access_unit, self.time).map_err(|_| DecodeError::Failed)?;
            self.time += 333_333;
            let mut newest = None;
            for _ in 0..8 {
                // SAFETY: Synchronous transform owned by this thread.
                match unsafe { self.transform.ProcessInput(0, &sample, 0) } {
                    Ok(()) => break,
                    Err(e) if e.code() == MF_E_NOTACCEPTING => self.drain(&mut newest)?,
                    Err(_) => return Err(DecodeError::Failed),
                }
            }
            self.drain(&mut newest)?;
            Ok(newest)
        }

        fn drain(&mut self, newest: &mut Option<Frame>) -> Result<(), DecodeError> {
            for _ in 0..16 {
                match pull(&self.transform).map_err(|_| DecodeError::Failed)? {
                    Pulled::NeedInput => return Ok(()),
                    Pulled::Changed => self.choose_output(),
                    Pulled::Unit(bytes) => {
                        let Some(layout) = self.layout else { continue };
                        // NV12 planes may be padded beyond the display height.
                        let rows = bytes.len() * 2 / 3 / layout.stride.max(1);
                        if let Some(frame) =
                            nv12_to_rgba(&bytes, layout.width, layout.height, layout.stride, rows)
                        {
                            *newest = Some(frame);
                        }
                    }
                }
            }
            Ok(())
        }
    }
}

/// Test-only H.264 source: encodes solid-color frames with the Windows
/// software encoder into Annex B access units.
#[cfg(all(windows, test))]
pub mod test_support {
    use super::imp::{enumerate, memory_sample, pull, Pulled, Runtime};
    use windows::Win32::Media::MediaFoundation::*;

    fn video_type(subtype: &windows::core::GUID, width: u32, height: u32) -> IMFMediaType {
        // SAFETY: Plain attribute writes on a new media type.
        unsafe {
            let mt = MFCreateMediaType().unwrap();
            mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).unwrap();
            mt.SetGUID(&MF_MT_SUBTYPE, subtype).unwrap();
            mt.SetUINT64(
                &MF_MT_FRAME_SIZE,
                (u64::from(width) << 32) | u64::from(height),
            )
            .unwrap();
            mt.SetUINT64(&MF_MT_FRAME_RATE, (30_u64 << 32) | 1).unwrap();
            mt.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1_u64 << 32) | 1)
                .unwrap();
            mt.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
                .unwrap();
            mt
        }
    }

    /// `frames` access units of a `width`x`height` picture of color `bgr`.
    pub fn encode_fixture(width: u32, height: u32, bgr: [u8; 3], frames: usize) -> Vec<Vec<u8>> {
        let _runtime = Runtime::start().expect("media foundation");
        let [b, g, r] = bgr.map(f32::from);
        let y = (16.0 + 0.183 * r + 0.614 * g + 0.062 * b).round() as u8;
        let u = (128.0 - 0.101 * r - 0.339 * g + 0.439 * b).round() as u8;
        let v = (128.0 + 0.439 * r - 0.399 * g - 0.040 * b).round() as u8;
        let (w, h) = (width as usize, height as usize);
        let mut nv12 = vec![y; w * h];
        for _ in 0..(w * h / 4) {
            nv12.extend_from_slice(&[u, v]);
        }
        let activate = enumerate(
            MFT_CATEGORY_VIDEO_ENCODER,
            Some(MFVideoFormat_NV12),
            Some(MFVideoFormat_H264),
        )
        .into_iter()
        .next()
        .expect("a software H.264 encoder");
        let mut units = Vec::new();
        // SAFETY: One synchronous encoder used on this thread, then dropped
        // before the runtime guard.
        unsafe {
            let encoder: IMFTransform = activate.ActivateObject().unwrap();
            let output = video_type(&MFVideoFormat_H264, width, height);
            output.SetUINT32(&MF_MT_AVG_BITRATE, 2_000_000).unwrap();
            encoder.SetOutputType(0, &output, 0).unwrap();
            encoder
                .SetInputType(0, &video_type(&MFVideoFormat_NV12, width, height), 0)
                .unwrap();
            encoder
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .unwrap();
            encoder
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .unwrap();
            let drain = |units: &mut Vec<Vec<u8>>| loop {
                match pull(&encoder).unwrap() {
                    Pulled::Unit(bytes) => units.push(bytes),
                    Pulled::NeedInput => break,
                    Pulled::Changed => {
                        let offer = encoder.GetOutputAvailableType(0, 0).unwrap();
                        encoder.SetOutputType(0, &offer, 0).unwrap();
                    }
                }
            };
            for frame in 0..frames {
                let sample = memory_sample(&nv12, frame as i64 * 333_333).unwrap();
                while encoder.ProcessInput(0, &sample, 0).is_err() {
                    drain(&mut units);
                }
                drain(&mut units);
            }
            encoder
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)
                .unwrap();
            encoder
                .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)
                .unwrap();
            drain(&mut units);
            // Parameter sets some encoders publish only on the output type.
            let current = encoder.GetOutputCurrentType(0).unwrap();
            if let Ok(size) = current.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER) {
                if size > 0 && size < 64 * 1024 && !units.is_empty() {
                    let mut header = vec![0; size as usize];
                    current
                        .GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut header, None)
                        .unwrap();
                    header.extend_from_slice(&units[0]);
                    units[0] = header;
                }
            }
        }
        units
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::test_support::encode_fixture;
    use super::*;

    const GREEN_BGR: [u8; 3] = [80, 180, 20];

    fn center(frame: &Frame) -> [u8; 3] {
        let at = ((frame.height / 2 * frame.width + frame.width / 2) * 4) as usize;
        [frame.rgba[at], frame.rgba[at + 1], frame.rgba[at + 2]]
    }

    fn decode_all(units: &[Vec<u8>]) -> Vec<Frame> {
        let mut decoder = H264Decoder::new().expect("decoder");
        units
            .iter()
            .filter_map(|unit| decoder.decode(unit).expect("decode"))
            .collect()
    }

    #[test]
    fn decodes_solid_green() {
        let frames = decode_all(&encode_fixture(640, 360, GREEN_BGR, 10));
        let last = frames.last().expect("at least one frame");
        assert_eq!((last.width, last.height), (640, 360));
        let [r, g, b] = center(last);
        assert!(
            r.abs_diff(20) <= 14 && g.abs_diff(180) <= 14 && b.abs_diff(80) <= 14,
            "{r},{g},{b}"
        );
    }

    #[test]
    fn follows_resolution_change() {
        let mut units = encode_fixture(640, 360, GREEN_BGR, 5);
        units.extend(encode_fixture(1280, 720, GREEN_BGR, 5));
        let frames = decode_all(&units);
        let last = frames.last().expect("frames");
        assert_eq!((last.width, last.height), (1280, 720));
    }

    #[test]
    fn scales_wide_frames() {
        let frames = decode_all(&encode_fixture(2560, 1440, GREEN_BGR, 3));
        let last = frames.last().expect("frames");
        assert_eq!((last.width, last.height), (1920, 1080));
    }
}

#[cfg(test)]
mod convert_tests {
    use super::*;

    /// The original per-pixel float conversion, kept as the reference.
    fn reference(data: &[u8], w: usize, h: usize, stride: usize, rows: usize) -> Vec<u8> {
        let chroma = stride * rows;
        let mut out = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let luma = f32::from(data[y * stride + x]);
                let at = chroma + (y / 2) * stride + (x & !1);
                let u = f32::from(data[at]) - 128.0;
                let v = f32::from(data[at + 1]) - 128.0;
                let l = (luma - 16.0) * 1.164;
                let clamp = |c: f32| c.round().clamp(0.0, 255.0) as u8;
                out.extend_from_slice(&[
                    clamp(l + 1.793 * v),
                    clamp(l - 0.213 * u - 0.533 * v),
                    clamp(l + 2.112 * u),
                    255,
                ]);
            }
        }
        out
    }

    #[test]
    fn fixed_point_matches_the_float_reference() {
        let (w, h, stride, rows) = (66usize, 38usize, 70usize, 40usize);
        let data: Vec<u8> = (0..stride * rows * 3 / 2)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        let frame = nv12_to_rgba(&data, w as u32, h as u32, stride, rows).unwrap();
        let want = reference(&data, w, h, stride, rows);
        assert_eq!(frame.rgba.len(), want.len());
        let worst = frame
            .rgba
            .iter()
            .zip(&want)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        assert!(worst <= 2, "worst channel difference {worst}");
    }

    #[test]
    #[ignore = "timing probe, run with --release --nocapture"]
    fn convert_1080p_timing() {
        let (w, h) = (1920usize, 1080usize);
        let data = vec![100u8; w * h * 3 / 2];
        let start = std::time::Instant::now();
        for _ in 0..20 {
            std::hint::black_box(nv12_to_rgba(&data, w as u32, h as u32, w, h));
        }
        println!("nv12_to_rgba 1080p: {:?} per frame", start.elapsed() / 20);
    }
}
