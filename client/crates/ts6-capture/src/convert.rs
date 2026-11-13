//! GPU scaling and BGRA to NV12 conversion through the D3D11 video processor.
//!
//! Everything stays on the GPU. The converter owns one BGRA texture that each
//! captured frame is copied into (video processor inputs need a render-target
//! binding, which pooled capture surfaces lack) and a small ring of NV12
//! output textures, so an encoder that still holds an earlier sample is never
//! overwritten.

use anyhow::{ensure, Context, Result};
use std::mem::ManuallyDrop;
use windows::{
    core::Interface,
    Win32::{
        Foundation::RECT,
        Graphics::{Direct3D11::*, Dxgi::Common::*},
    },
};

/// Output textures in rotation. The encoder copies a sample's surface quickly;
/// eight frames of slack is far more than it ever needs.
const RING: usize = 8;

struct Slot {
    texture: ID3D11Texture2D,
    view: ID3D11VideoProcessorOutputView,
}

pub struct Converter {
    context: ID3D11DeviceContext,
    video_context: ID3D11VideoContext,
    processor: ID3D11VideoProcessor,
    _enumerator: ID3D11VideoProcessorEnumerator,
    input: ID3D11Texture2D,
    input_view: ID3D11VideoProcessorInputView,
    ring: Vec<Slot>,
    next: usize,
}

impl Converter {
    /// `source` is the capture size, `target` the (even) encoded size.
    pub fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        source: (u32, u32),
        target: (u32, u32),
        fps: u32,
    ) -> Result<Self> {
        ensure!(
            target.0.is_multiple_of(2) && target.1.is_multiple_of(2),
            "NV12 needs even dimensions"
        );
        // SAFETY: every resource below is created from `device` and kept alive
        // by this struct; descriptions are fully initialised.
        unsafe {
            let video: ID3D11VideoDevice = device.cast().context("no D3D11 video device")?;
            let video_context: ID3D11VideoContext = context.cast()?;
            let color: ID3D11VideoContext1 = context.cast()?;
            let rate = DXGI_RATIONAL {
                Numerator: fps,
                Denominator: 1,
            };
            let enumerator = video
                .CreateVideoProcessorEnumerator(&D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                    InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                    InputFrameRate: rate,
                    InputWidth: source.0,
                    InputHeight: source.1,
                    OutputFrameRate: rate,
                    OutputWidth: target.0,
                    OutputHeight: target.1,
                    Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
                })
                .context("create video processor enumerator")?;
            ensure!(
                enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_B8G8R8A8_UNORM)?
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

            let mut desc = D3D11_TEXTURE2D_DESC {
                Width: source.0,
                Height: source.1,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut input = None;
            device
                .CreateTexture2D(&desc, None, Some(&mut input))
                .context("create BGRA input texture")?;
            let input = input.context("missing BGRA input texture")?;
            let mut input_view = None;
            video
                .CreateVideoProcessorInputView(
                    &input,
                    &enumerator,
                    &D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                        ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                        ..Default::default()
                    },
                    Some(&mut input_view),
                )
                .context("create BGRA processor view")?;
            let input_view = input_view.context("missing BGRA processor view")?;

            desc.Width = target.0;
            desc.Height = target.1;
            desc.Format = DXGI_FORMAT_NV12;
            desc.BindFlags = D3D11_BIND_RENDER_TARGET.0 as u32;
            let mut ring = Vec::with_capacity(RING);
            for _ in 0..RING {
                let mut texture = None;
                device
                    .CreateTexture2D(&desc, None, Some(&mut texture))
                    .context("create NV12 texture")?;
                let texture = texture.context("missing NV12 texture")?;
                let mut view = None;
                video
                    .CreateVideoProcessorOutputView(
                        &texture,
                        &enumerator,
                        &D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                            ..Default::default()
                        },
                        Some(&mut view),
                    )
                    .context("create NV12 output view")?;
                ring.push(Slot {
                    texture,
                    view: view.context("missing NV12 output view")?,
                });
            }

            let source_rect = RECT {
                left: 0,
                top: 0,
                right: source.0 as i32,
                bottom: source.1 as i32,
            };
            let target_rect = RECT {
                left: 0,
                top: 0,
                right: target.0 as i32,
                bottom: target.1 as i32,
            };
            video_context.VideoProcessorSetStreamFrameFormat(
                &processor,
                0,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            );
            video_context.VideoProcessorSetStreamSourceRect(
                &processor,
                0,
                true,
                Some(&source_rect),
            );
            video_context.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&target_rect));
            video_context.VideoProcessorSetOutputTargetRect(&processor, true, Some(&target_rect));
            video_context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            // Full-range sRGB-ish screen content in, studio-range BT.709 out;
            // the encoder's media types carry the same description.
            color.VideoProcessorSetStreamColorSpace1(
                &processor,
                0,
                DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
            );
            color.VideoProcessorSetOutputColorSpace1(
                &processor,
                DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
            );

            Ok(Self {
                context: context.clone(),
                video_context,
                processor,
                _enumerator: enumerator,
                input,
                input_view,
                ring,
                next: 0,
            })
        }
    }

    /// Copies `frame` (a BGRA texture of the source size on the same device),
    /// scales and converts it, and returns the NV12 texture holding the result.
    /// The texture stays valid until `RING` more frames have been converted.
    pub fn convert(&mut self, frame: &ID3D11Texture2D) -> Result<&ID3D11Texture2D> {
        let slot = self.next;
        self.next = (self.next + 1) % self.ring.len();
        // SAFETY: both textures belong to this device; the view reference taken
        // for the stream is released on every path.
        unsafe {
            self.context.CopyResource(&self.input, frame);
            let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: ManuallyDrop::new(Some(self.input_view.clone())),
                ..Default::default()
            };
            let result = self.video_context.VideoProcessorBlt(
                &self.processor,
                &self.ring[slot].view,
                0,
                std::slice::from_ref(&stream),
            );
            ManuallyDrop::drop(&mut stream.pInputSurface);
            result.context("GPU BGRA to NV12 conversion")?;
        }
        Ok(&self.ring[slot].texture)
    }
}
