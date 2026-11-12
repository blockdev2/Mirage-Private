//! Diagnostic GPU conversion and file encoding of a repeated owned fixture.

use anyhow::{Context, Result, ensure};
use std::{mem::ManuallyDrop, os::windows::ffi::OsStrExt, path::Path};
use windows::{
    Win32::{
        Foundation::RECT,
        Graphics::{Direct3D11::*, Dxgi::Common::*},
        Media::MediaFoundation::*,
        System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize},
    },
    core::{Interface, PCWSTR},
};

struct Runtime;
struct ContextProtection {
    lock: ID3D11Multithread,
    previous: bool,
}
impl Drop for ContextProtection {
    fn drop(&mut self) {
        // SAFETY: Restore the prior mode after writer resources have dropped.
        unsafe {
            // Return value is the old mode, not an error status.
            let _previous_mode = self.lock.SetMultithreadProtected(self.previous);
        }
    }
}
impl Runtime {
    fn start() -> Result<Self> {
        // SAFETY: Runs on the post-capture owner thread; paired with teardown.
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
            if let Err(error) = MFStartup(MF_VERSION, MFSTARTUP_FULL) {
                CoUninitialize();
                return Err(error.into());
            }
        }
        Ok(Self)
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        // SAFETY: All dependent COM resources are scoped after this guard.
        unsafe {
            if MFShutdown().is_err() {
                eprintln!("Media Foundation shutdown failed");
            }
            CoUninitialize();
        }
    }
}

fn convert(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    input: &ID3D11Texture2D,
) -> Result<ID3D11Texture2D> {
    // SAFETY: Capture has stopped and joined. All views/resources belong to the
    // same device; no pooled capture surfaces or CPU-mapped buffers are used.
    unsafe {
        let video: ID3D11VideoDevice = device.cast()?;
        let vctx: ID3D11VideoContext = context.cast()?;
        let color: ID3D11VideoContext1 = context.cast()?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        input.GetDesc(&mut desc);
        let source = RECT {
            left: 0,
            top: 0,
            right: desc.Width as i32,
            bottom: desc.Height as i32,
        };
        let destination = RECT {
            left: 0,
            top: 0,
            right: 640,
            bottom: 360,
        };
        let rate = DXGI_RATIONAL {
            Numerator: 20,
            Denominator: 1,
        };
        let enumerator = video
            .CreateVideoProcessorEnumerator(&D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: rate,
                InputWidth: desc.Width,
                InputHeight: desc.Height,
                OutputFrameRate: rate,
                OutputWidth: 640,
                OutputHeight: 360,
                Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
            })
            .context("create video processor enumerator")?;
        ensure!(
            enumerator.CheckVideoProcessorFormat(desc.Format)?
                & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0 as u32
                != 0,
            "BGRA processor input unsupported"
        );
        ensure!(
            enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_NV12)?
                & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0 as u32
                != 0,
            "NV12 processor output unsupported"
        );
        let processor = video.CreateVideoProcessor(&enumerator, 0)?;
        let mut iview = None;
        video
            .CreateVideoProcessorInputView(
                input,
                &enumerator,
                &D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                    ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                    ..Default::default()
                },
                Some(&mut iview),
            )
            .context("create BGRA processor view")?;
        desc.Width = 640;
        desc.Height = 360;
        desc.Format = DXGI_FORMAT_NV12;
        desc.Usage = D3D11_USAGE_DEFAULT;
        desc.BindFlags = D3D11_BIND_RENDER_TARGET.0 as u32;
        desc.CPUAccessFlags = 0;
        desc.MiscFlags = 0;
        let mut output = None;
        device
            .CreateTexture2D(&desc, None, Some(&mut output))
            .context("create GPU NV12 texture")?;
        let output = output.context("missing NV12 resource")?;
        let mut oview = None;
        video
            .CreateVideoProcessorOutputView(
                &output,
                &enumerator,
                &D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                    ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                    ..Default::default()
                },
                Some(&mut oview),
            )
            .context("create NV12 processor output view")?;
        let oview = oview.context("missing NV12 output view")?;
        vctx.VideoProcessorSetStreamFrameFormat(
            &processor,
            0,
            D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
        );
        vctx.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&source));
        vctx.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&destination));
        vctx.VideoProcessorSetOutputTargetRect(&processor, true, Some(&destination));
        vctx.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
        color.VideoProcessorSetStreamColorSpace1(
            &processor,
            0,
            DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
        );
        color.VideoProcessorSetOutputColorSpace1(
            &processor,
            DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
        );
        let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            pInputSurface: ManuallyDrop::new(Some(iview.context("missing input view")?)),
            ..Default::default()
        };
        let result = vctx.VideoProcessorBlt(&processor, &oview, 0, std::slice::from_ref(&stream));
        // Release the COM reference hidden by the binding's ManuallyDrop field,
        // including on Blt failure. The borrowed view remains valid during Blt.
        ManuallyDrop::drop(&mut stream.pInputSurface);
        result.context("GPU BGRA to NV12 conversion")?;
        context.Flush();
        Ok(output)
    }
}

fn media_type(subtype: &windows::core::GUID) -> Result<IMFMediaType> {
    // SAFETY: Attributes describe the fixed, bounded diagnostic format.
    unsafe {
        let mt = MFCreateMediaType()?;
        mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        mt.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        mt.SetUINT64(&MF_MT_FRAME_SIZE, (640_u64 << 32) | 360)?;
        mt.SetUINT64(&MF_MT_FRAME_RATE, (20_u64 << 32) | 1)?;
        mt.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1_u64 << 32) | 1)?;
        mt.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        mt.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32)?;
        mt.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32)?;
        mt.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
        mt.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
        Ok(mt)
    }
}

pub fn encode(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    texture: &ID3D11Texture2D,
    path: &Path,
) -> Result<serde_json::Value> {
    let _runtime = Runtime::start()?;
    let lock: ID3D11Multithread = context.cast().context("query context protection")?;
    // SAFETY: The capture owner is joined; MF may use the immediate context on
    // its own worker threads while consuming GPU samples.
    let previous = unsafe { lock.SetMultithreadProtected(true).as_bool() };
    let _protection = ContextProtection { lock, previous };
    let nv12 = convert(device, context, texture).context("convert owned texture")?;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    ensure!(
        !wide[..wide.len() - 1].contains(&0),
        "output path contains NUL"
    );
    // SAFETY: All MF resources outlive their calls and are dropped before the
    // runtime guard. Exclusive creation prevents overwriting existing output.
    let encoder_kind = unsafe {
        let mut token = 0;
        let mut manager = None;
        MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
        let manager = manager.context("missing DXGI manager")?;
        manager
            .ResetDevice(device, token)
            .context("reset DXGI device manager")?;
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 5)?;
        let attrs = attrs.context("missing writer attributes")?;
        attrs.SetUnknown(&MF_SINK_WRITER_D3D_MANAGER, &manager)?;
        attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
        attrs.SetUINT32(&MF_LOW_LATENCY, 1)?;
        attrs.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;
        attrs.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)?;
        let bytes = MFCreateFile(
            MF_ACCESSMODE_WRITE,
            MF_OPENMODE_FAIL_IF_EXIST,
            MF_FILEFLAGS_NONE,
            PCWSTR(wide.as_ptr()),
        )
        .context("exclusive diagnostic output creation")?;
        let writer = MFCreateSinkWriterFromURL(PCWSTR::null(), &bytes, &attrs)
            .context("create MP4 sink writer")?;
        let target = media_type(&MFVideoFormat_H264)?;
        target
            .SetUINT32(&MF_MT_AVG_BITRATE, 800_000)
            .context("set H264 bitrate")?;
        let index = writer.AddStream(&target).context("configure H264 output")?;
        writer
            .SetInputMediaType(index, &media_type(&MFVideoFormat_NV12)?, None)
            .context("configure GPU NV12 input")?;
        writer.BeginWriting().context("begin H264 writing")?;
        let encoder_kind = inspect_encoder(&writer, index);
        let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &nv12, 0, false)
            .context("wrap GPU NV12 buffer")?;
        let buffer2d: IMF2DBuffer = buffer.cast().context("query GPU 2D buffer")?;
        buffer
            .SetCurrentLength(
                buffer2d
                    .GetContiguousLength()
                    .context("query contiguous buffer length")?,
            )
            .context("set GPU buffer length")?;
        for frame in 0..20 {
            let sample = MFCreateSample().context("create media sample")?;
            sample.AddBuffer(&buffer).context("attach GPU buffer")?;
            sample
                .SetSampleTime(frame * 500_000)
                .context("set sample timestamp")?;
            sample
                .SetSampleDuration(500_000)
                .context("set sample duration")?;
            writer
                .WriteSample(index, &sample)
                .context("submit GPU sample")?;
        }
        writer.Finalize().context("finalize MP4 diagnostic")?;
        drop(writer);
        // The sink manages the stream lifecycle. Release our remaining COM
        // reference after successful synchronous finalization and writer teardown.
        drop(bytes);
        encoder_kind
    };
    Ok(
        serde_json::json!({"codec":"h264","width":640,"height":360,"submitted_samples":20,
        "fps":20,"gpu_input":true,"gpu_color_conversion":true,"hardware_transforms_allowed":true,
        "encoder_kind":encoder_kind.as_str(),
        "hardware_encoder_confirmed":encoder_kind == EncoderKind::Hardware,
        "repeated_final_texture":true}),
    )
}

/// What the sink writer actually inserted, deliberately without vendor or
/// adapter names so shared reports carry no workstation inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderKind {
    Hardware,
    Software,
    /// An encoder exists but did not expose its attribute store.
    Unknown,
    /// No video-encoder transform appeared in the writer chain.
    Missing,
}

impl EncoderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hardware => "hardware",
            Self::Software => "software",
            Self::Unknown => "unknown",
            Self::Missing => "missing",
        }
    }

    /// Classify a writer chain from `(is_video_encoder, hardware_url_present)`
    /// pairs. `None` means the transform's attributes were unavailable.
    pub fn classify(chain: &[(bool, Option<bool>)]) -> Self {
        match chain.iter().find(|(encoder, _)| *encoder) {
            None => Self::Missing,
            Some((_, None)) => Self::Unknown,
            Some((_, Some(true))) => Self::Hardware,
            Some((_, Some(false))) => Self::Software,
        }
    }
}

/// Hardware MFTs publish `MFT_ENUM_HARDWARE_URL_Attribute`; software MFTs do not.
fn inspect_encoder(writer: &IMFSinkWriter, stream: u32) -> EncoderKind {
    let Ok(extended) = writer.cast::<IMFSinkWriterEx>() else {
        return EncoderKind::Missing;
    };
    let mut chain = Vec::new();
    // A writer chain is short; the bound only guards against a misbehaving sink.
    for position in 0..16 {
        let mut category = windows::core::GUID::zeroed();
        let mut transform = None;
        // SAFETY: Out-pointers are valid locals; an index past the end returns
        // MF_E_INVALIDINDEX, which ends the walk.
        let found = unsafe {
            extended.GetTransformForStream(stream, position, Some(&mut category), &mut transform)
        };
        let (Ok(()), Some(transform)) = (found, transform) else {
            break;
        };
        // SAFETY: Read-only attribute queries on a live transform.
        let hardware = unsafe {
            transform.GetAttributes().ok().map(|attributes| {
                attributes
                    .GetStringLength(&MFT_ENUM_HARDWARE_URL_Attribute)
                    .is_ok()
            })
        };
        chain.push((category == MFT_CATEGORY_VIDEO_ENCODER, hardware));
    }
    EncoderKind::classify(&chain)
}

#[cfg(test)]
mod tests {
    use super::EncoderKind;

    #[test]
    fn classifies_the_first_encoder_in_the_writer_chain() {
        assert_eq!(EncoderKind::classify(&[]), EncoderKind::Missing);
        assert_eq!(
            EncoderKind::classify(&[(false, Some(true))]),
            EncoderKind::Missing
        );
        assert_eq!(
            EncoderKind::classify(&[(false, Some(false)), (true, Some(true))]),
            EncoderKind::Hardware
        );
        assert_eq!(
            EncoderKind::classify(&[(true, Some(false))]),
            EncoderKind::Software
        );
        assert_eq!(EncoderKind::classify(&[(true, None)]), EncoderKind::Unknown);
    }
}
