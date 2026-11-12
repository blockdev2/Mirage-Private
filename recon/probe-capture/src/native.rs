//! WGC -> owned GPU texture -> one post-stop validation readback.

use crate::fixture::Fixture;
use anyhow::{Context as _, Result, anyhow, bail};
use mirage_native_capture_probe::{center_offset, is_fixture_pixel};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicI32, Ordering},
    },
    time::{Duration, Instant},
};
use windows::Win32::Graphics::{Direct3D11::*, Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM};
use windows_capture::{
    capture::{Context, GraphicsCaptureApiHandler},
    frame::Frame,
    graphics_capture_api::InternalCaptureControl,
    settings::*,
    window::Window,
};

#[derive(Default)]
struct State {
    frames: u64,
    allocations: u64,
    width: u32,
    height: u32,
    texture: Option<ID3D11Texture2D>,
    device: Option<ID3D11Device>,
    context: Option<ID3D11DeviceContext>,
    submission_ns: u128,
}

struct Capture {
    state: Arc<Mutex<State>>,
}

struct Target {
    window: Window,
    error: Arc<AtomicI32>,
}

impl TryInto<GraphicsCaptureItemType> for Target {
    type Error = windows::core::Error;
    fn try_into(self) -> std::result::Result<GraphicsCaptureItemType, Self::Error> {
        self.window
            .try_into()
            .inspect_err(|error: &windows::core::Error| {
                self.error.store(error.code().0, Ordering::Relaxed);
            })
    }
}

impl GraphicsCaptureApiHandler for Capture {
    type Flags = Arc<Mutex<State>>;
    type Error = anyhow::Error;

    fn new(ctx: Context<Self::Flags>) -> Result<Self> {
        Ok(Self { state: ctx.flags })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        control: InternalCaptureControl,
    ) -> Result<()> {
        let start = Instant::now();
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("capture state lock poisoned"))?;
        let mut desc = *frame.desc();
        if desc.Width == 0
            || desc.Height == 0
            || desc.Width > 1024
            || desc.Height > 1024
            || desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM
            || desc.SampleDesc.Count != 1
        {
            bail!("unexpected fixture texture layout");
        }
        if state.texture.is_none() || state.width != desc.Width || state.height != desc.Height {
            desc.Usage = D3D11_USAGE_DEFAULT;
            // Video processor input views require a render/decoder/encoder/UAV
            // binding (or no bindings); shader-resource-only is insufficient.
            desc.BindFlags = (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32;
            desc.CPUAccessFlags = 0;
            desc.MiscFlags = 0;
            let mut texture = None;
            // SAFETY: The description has bounded dimensions and matches the
            // live capture device. The returned COM texture is independently owned.
            unsafe {
                frame
                    .device()
                    .CreateTexture2D(&desc, None, Some(&mut texture))?;
            }
            state.texture = Some(texture.context("GPU texture creation returned no resource")?);
            state.width = desc.Width;
            state.height = desc.Height;
            state.allocations += 1;
            state.device = Some(frame.device().clone());
            state.context = Some(frame.device_context().clone());
        }
        // SAFETY: Both textures have matching descriptions and belong to the
        // same device. Only the owned copy survives the callback, not the pooled frame.
        unsafe {
            frame
                .device_context()
                .CopyResource(state.texture.as_ref().unwrap(), frame.as_raw_texture());
        }
        state.frames += 1;
        state.submission_ns += start.elapsed().as_nanos();
        if state.frames >= 40 {
            control.stop();
        }
        Ok(())
    }
}

struct Mapped<'a> {
    context: &'a ID3D11DeviceContext,
    texture: &'a ID3D11Texture2D,
}
impl Drop for Mapped<'_> {
    fn drop(&mut self) {
        // SAFETY: This guard exists only after a successful Map, and releases it once.
        unsafe {
            self.context.Unmap(self.texture, 0);
        }
    }
}

fn validate_owned_texture(state: &State) -> Result<bool> {
    let device = state.device.as_ref().context("missing capture device")?;
    let context = state.context.as_ref().context("missing capture context")?;
    let texture = state.texture.as_ref().context("missing owned texture")?;
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: The capture thread was joined before accessing its context here.
    unsafe {
        texture.GetDesc(&mut desc);
    }
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
    desc.MiscFlags = 0;
    let mut staging = None;
    // SAFETY: Copy only our owned synthetic resource into one bounded staging resource.
    unsafe {
        device.CreateTexture2D(&desc, None, Some(&mut staging))?;
    }
    let staging = staging.context("missing validation staging texture")?;
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    unsafe {
        context.CopyResource(&staging, texture);
        context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
    }
    let _guard = Mapped {
        context,
        texture: &staging,
    };
    let offset = center_offset(desc.Width, desc.Height, mapped.RowPitch)
        .context("invalid validation row pitch")?;
    if mapped.pData.is_null() {
        bail!("validation Map returned no data");
    }
    let mut pixel = [0_u8; 4];
    // SAFETY: The checked offset covers four bytes inside RowPitch * Height.
    // Map keeps the allocation valid until the guard unmaps it after this read.
    unsafe {
        std::ptr::copy_nonoverlapping(mapped.pData.cast::<u8>().add(offset), pixel.as_mut_ptr(), 4);
    }
    Ok(is_fixture_pixel(pixel))
}

pub enum Mode {
    Capture,
    Mp4(std::path::PathBuf),
    Stream(std::path::PathBuf),
    RtpSend {
        destination: std::net::SocketAddr,
        codec: mirage_native_capture_probe::bitstream::Codec,
        frames: u32,
        drop_packet: Option<u64>,
    },
}

pub fn run(mode: Mode) -> Result<()> {
    if let Mode::Mp4(path) | Mode::Stream(path) = &mode {
        anyhow::ensure!(!path.exists(), "output must not already exist");
    }
    let fixture = Fixture::create()?;
    let state = Arc::new(Mutex::new(State::default()));
    let conversion_error = Arc::new(AtomicI32::new(0));
    let window = Window::from_raw_hwnd(fixture.hwnd().0);
    if !window.is_valid() {
        bail!("owned fixture is not an eligible capture window");
    }
    let settings = Settings::new(
        Target {
            window,
            error: conversion_error.clone(),
        },
        CursorCaptureSettings::WithoutCursor,
        DrawBorderSettings::Default,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        state.clone(),
    );
    let (started, startup) = std::sync::mpsc::sync_channel(1);
    let initializing = std::thread::spawn(move || {
        let result = Capture::start_free_threaded(settings);
        if started.send(result).is_err() {
            eprintln!("capture startup receiver closed");
        }
    });
    let startup_deadline = Instant::now() + Duration::from_secs(8);
    let result = loop {
        match startup.try_recv() {
            Ok(result) => break result,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                bail!("capture startup owner ended")
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
        if Instant::now() >= startup_deadline || !fixture.pump()? {
            bail!("capture startup deadline exceeded or fixture closed");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    initializing
        .join()
        .map_err(|_| anyhow!("capture startup thread panicked"))?;
    let capture = result.map_err(|error| {
        let code = conversion_error.load(Ordering::Relaxed);
        if code == 0 {
            anyhow!("WGC startup failed: {error:?}")
        } else {
            anyhow!(
                "WGC startup failed: {error:?}; conversion_hresult=0x{:08X}",
                code as u32
            )
        }
    })?;
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut resized = false;
    let pumping: Result<()> = (|| {
        while Instant::now() < deadline && fixture.pump()? {
            let frames = state
                .lock()
                .map_err(|_| anyhow!("capture state lock poisoned"))?
                .frames;
            if frames >= 40 {
                return Ok(());
            }
            if frames >= 15 && !resized {
                fixture.resize()?;
                resized = true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        bail!("fixture closed or native capture deadline exceeded")
    })();
    let stopped = capture
        .stop()
        .map_err(|error| anyhow!("WGC cleanup failed: {error:?}"));
    if let Err(error) = &stopped {
        eprintln!("{error}");
    }
    pumping?;
    stopped?;
    let state = state
        .lock()
        .map_err(|_| anyhow!("capture state lock poisoned"))?;
    let matches = validate_owned_texture(&state)?;
    if !matches || state.allocations < 2 {
        bail!("owned texture or resize validation failed");
    }
    fixture.close()?;
    let device = state.device.as_ref().context("missing device")?;
    let context = state.context.as_ref().context("missing context")?;
    let texture = state.texture.as_ref().context("missing texture")?;
    let (encoding, streaming) = match &mode {
        Mode::Capture => (None, None),
        Mode::Mp4(path) => (
            Some(crate::encoder::encode(device, context, texture, path)?),
            None,
        ),
        Mode::Stream(directory) => (
            None,
            Some(crate::mft::stream(device, context, texture, directory)?),
        ),
        Mode::RtpSend {
            destination,
            codec,
            frames,
            drop_packet,
        } => (
            None,
            Some(crate::mft::rtp_send(
                device,
                context,
                texture,
                *codec,
                *destination,
                *frames,
                *drop_packet,
            )?),
        ),
    };
    println!(
        "{}",
        serde_json::json!({"captured_frames":state.frames,"owned_texture_allocations":state.allocations,
        "final_width":state.width,"final_height":state.height,"resize_observed":resized,
        "fixture_pixel_matches":matches,"gpu_copy_submission_mean_us":state.submission_ns as f64 / state.frames as f64 / 1000.0,
        "validation_readbacks":1,"user_windows_captured":false,"network_used":false,"encoded_video":encoding.is_some(),"encoding":encoding,"streaming":streaming})
    );
    Ok(())
}
