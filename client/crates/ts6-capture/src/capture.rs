//! Monitor capture with Windows Graphics Capture, feeding the GPU encoder.
//!
//! Everything runs inside the capture callback, on the capture thread: copy,
//! scale and convert on the GPU, encode, and push the units into a bounded
//! channel. The callback never blocks on the consumer; when the channel is
//! full the unit is dropped and the next one is forced to be a keyframe so a
//! viewer recovers by itself.
//!
//! Windows Graphics Capture delivers a frame only when the screen changes, so
//! a perfectly still screen produces no new units until something moves.

use crate::config::{fit_size, CaptureConfig, CaptureSource, EncodedUnit};
use crate::convert::Converter;
use crate::encoder::{ContextProtection, H264Encoder, MfRuntime};
use anyhow::{anyhow, Context as _, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows_capture::{
    capture::{CaptureControl, Context, GraphicsCaptureApiHandler},
    frame::Frame,
    graphics_capture_api::InternalCaptureControl,
    monitor::Monitor,
    settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    },
    window::Window,
};

/// Encoded units waiting for the consumer.
const QUEUE: usize = 8;

struct Flags {
    config: CaptureConfig,
    units: SyncSender<EncodedUnit>,
    keyframe: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    failure: Arc<Mutex<Option<String>>>,
}

/// Confines COM objects to the capture thread.
///
/// The capture handler must be `Send` to be handed to Windows Graphics Capture,
/// but the pipeline inside it is created lazily on the first frame and used
/// only from the frame callback, which runs on the capture thread's own
/// dispatcher. It is never moved or shared after that. The apartment is
/// multithreaded and the D3D11 context has multithread protection on.
struct Confined<T>(T);

// SAFETY: see the type's documentation; the value never crosses threads once
// it exists.
unsafe impl<T> Send for Confined<T> {}

/// What exists once the first frame has told us the source size.
struct Pipeline {
    source: (u32, u32),
    converter: Converter,
    encoder: H264Encoder,
    _protection: ContextProtection,
}

struct Handler {
    flags: Flags,
    // Field order is drop order: encoder resources go before Media Foundation.
    pipeline: Option<Confined<Pipeline>>,
    _runtime: MfRuntime,
    started: Instant,
    last: Option<Instant>,
}

impl Handler {
    fn frame(&mut self, frame: &mut Frame) -> Result<()> {
        let config = self.flags.config;
        let interval = Duration::from_secs(1) / config.fps;
        if self.last.is_some_and(|at| at.elapsed() < interval) {
            return Ok(());
        }
        let desc = *frame.desc();
        let source = (desc.Width, desc.Height);
        if self.pipeline.as_ref().is_none_or(|p| p.0.source != source) {
            // First frame, or the monitor changed size: build everything anew.
            self.pipeline = None;
            let target = fit_size(source, (config.max_width, config.max_height));
            let device = frame.device();
            let context = frame.device_context();
            let protection = ContextProtection::enable(context)?;
            let converter = Converter::new(device, context, source, target, config.fps)?;
            let encoder = H264Encoder::new(device, target, config.fps, config.bitrate_kbps)?;
            self.pipeline = Some(Confined(Pipeline {
                source,
                converter,
                encoder,
                _protection: protection,
            }));
        }
        let Some(Confined(pipeline)) = self.pipeline.as_mut() else {
            return Ok(());
        };
        if self.flags.keyframe.swap(false, Ordering::AcqRel) {
            pipeline.encoder.request_keyframe();
        }
        let time = i64::try_from(self.started.elapsed().as_nanos() / 100).unwrap_or(i64::MAX);
        let nv12 = pipeline.converter.convert(frame.as_raw_texture())?;
        let units = pipeline.encoder.encode(nv12, time)?;
        self.last = Some(Instant::now());
        for unit in units {
            match self.flags.units.try_send(unit) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    // The consumer is behind: skip, and restart on a keyframe.
                    pipeline.encoder.request_keyframe();
                }
                Err(TrySendError::Disconnected(_)) => {
                    self.flags.stop.store(true, Ordering::Release);
                }
            }
        }
        Ok(())
    }
}

impl GraphicsCaptureApiHandler for Handler {
    type Flags = Flags;
    type Error = anyhow::Error;

    fn new(ctx: Context<Self::Flags>) -> Result<Self> {
        Ok(Self {
            flags: ctx.flags,
            pipeline: None,
            _runtime: MfRuntime::start()?,
            started: Instant::now(),
            last: None,
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        control: InternalCaptureControl,
    ) -> Result<()> {
        if self.flags.stop.load(Ordering::Acquire) {
            control.stop();
            return Ok(());
        }
        if let Err(error) = self.frame(frame) {
            let mut failure = self.flags.failure.lock().unwrap_or_else(|e| e.into_inner());
            *failure = Some(format!("{error:#}"));
            control.stop();
        }
        Ok(())
    }
}

fn launch<T>(item: T, flags: Flags) -> Result<CaptureControl<Handler, anyhow::Error>>
where
    T: TryInto<windows_capture::settings::GraphicsCaptureItemType> + Send + 'static,
{
    let settings = Settings::new(
        item,
        CursorCaptureSettings::WithCursor,
        DrawBorderSettings::Default,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        flags,
    );
    Handler::start_free_threaded(settings)
        .map_err(|error| anyhow!("cannot start the capture: {error}"))
}

/// A running capture. Dropping it stops the capture.
pub struct ScreenCapture {
    control: Option<CaptureControl<Handler, anyhow::Error>>,
    units: Receiver<EncodedUnit>,
    keyframe: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    failure: Arc<Mutex<Option<String>>>,
}

impl ScreenCapture {
    /// Starts capturing the configured monitor. The picture of the user's
    /// screen stays in this process: it is encoded and handed to `units`.
    pub fn start(config: CaptureConfig) -> Result<Self> {
        config.validate().map_err(|reason| anyhow!(reason))?;
        let (sender, units) = sync_channel(QUEUE);
        let keyframe = Arc::new(AtomicBool::new(true));
        let stop = Arc::new(AtomicBool::new(false));
        let failure = Arc::new(Mutex::new(None));
        let flags = Flags {
            config,
            units: sender,
            keyframe: Arc::clone(&keyframe),
            stop: Arc::clone(&stop),
            failure: Arc::clone(&failure),
        };
        let control = match config.source {
            CaptureSource::Monitor(index) => {
                let monitor = match index {
                    Some(index) => Monitor::from_index(index + 1),
                    None => Monitor::primary(),
                }
                .map_err(|error| anyhow!("cannot open the monitor: {error}"))?;
                launch(monitor, flags)?
            }
            CaptureSource::Window(handle) => {
                let window = Window::from_raw_hwnd(handle as isize as *mut std::ffi::c_void);
                if !window.is_valid() {
                    return Err(anyhow!("that window cannot be captured (closed or hidden)"));
                }
                launch(window, flags)?
            }
        };
        Ok(Self {
            control: Some(control),
            units,
            keyframe,
            stop,
            failure,
        })
    }

    /// Encoded units, oldest first. Use `recv_timeout`: a still screen is quiet.
    pub fn units(&self) -> &Receiver<EncodedUnit> {
        &self.units
    }

    /// The next unit will be a keyframe (a viewer joined, a packet was lost).
    pub fn request_keyframe(&self) {
        self.keyframe.store(true, Ordering::Release);
    }

    /// Why the capture ended on its own, if it did.
    pub fn error(&self) -> Option<String> {
        self.failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn is_running(&self) -> bool {
        self.control.as_ref().is_some_and(|c| !c.is_finished())
    }

    /// Stops the capture and waits for its thread.
    pub fn stop(mut self) -> Result<()> {
        self.shutdown()
    }

    fn shutdown(&mut self) -> Result<()> {
        self.stop.store(true, Ordering::Release);
        if let Some(control) = self.control.take() {
            control
                .stop()
                .map_err(|error| anyhow!("capture did not stop cleanly: {error}"))
                .context("stopping the screen capture")?;
        }
        Ok(())
    }
}

impl Drop for ScreenCapture {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}
