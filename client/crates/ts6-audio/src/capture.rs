//! Microphone capture: device -> mono 48 kHz -> 20 ms frames -> Opus packets.
//!
//! Runs on its own OS thread because `cpal::Stream` is not `Send` on Windows
//! (WASAPI requires the stream to live on the thread that created it). Encoded
//! packets are handed to the network loop over a bounded `tokio::mpsc` channel, so
//! a slow network never blocks the audio callback: excess frames are dropped and
//! counted.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::SampleFormat;

use crate::codec::{VoiceEncoder, FRAME_SAMPLES, SAMPLE_RATE};
use crate::denoise::{Denoiser, NoiseSuppression};
use crate::devices::pick_input;
use crate::playback::Monitor;
use crate::resample::{downmix_to_mono, LinearResampler};
use crate::vad::{level_db, Decision, VoiceActivation, DEFAULT_THRESHOLD_DB, SILENCE_DB};

#[derive(Debug, Clone)]
pub struct CaptureConfig {
    /// Exact device name, or `None` for the system default.
    pub device: Option<String>,
    /// Opus target bitrate in bits per second.
    pub bitrate: i32,
    /// Transmit only while the voice activation gate is open. When false every
    /// frame is sent (push-to-talk gates downstream, or continuous transmission).
    pub voice_activation: bool,
    /// Initial voice activation threshold in dBFS; adjustable while running.
    pub threshold_db: f32,
    /// Noise suppression level; adjustable while running.
    pub noise_suppression: NoiseSuppression,
    /// Receives the processed audio for local "hear yourself" playback.
    pub monitor: Monitor,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            device: None,
            bitrate: 64_000,
            voice_activation: true,
            threshold_db: DEFAULT_THRESHOLD_DB,
            noise_suppression: NoiseSuppression::Standard,
            monitor: Monitor::default(),
        }
    }
}

pub struct Capture {
    pub device_name: String,
    pub sample_rate: u32,
    pub channels: u16,
    stop: Arc<AtomicBool>,
    frames: Arc<AtomicU64>,
    dropped: Arc<AtomicU64>,
    live: Arc<Live>,
    thread: Option<JoinHandle<()>>,
}

/// State shared between the capture callback and the rest of the app.
struct Live {
    /// f32 bits: level of the last frame in dBFS.
    level_db: AtomicU32,
    /// f32 bits: voice activation threshold in dBFS.
    threshold_db: AtomicU32,
    /// Whether frames are currently being sent.
    transmitting: AtomicBool,
    /// [NoiseSuppression::level].
    noise: AtomicU8,
}

impl Capture {
    /// Frames encoded and handed to the network loop.
    pub fn frames_encoded(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    /// Level of the latest captured frame, in dBFS.
    pub fn level_db(&self) -> f32 {
        f32::from_bits(self.live.level_db.load(Ordering::Relaxed))
    }

    /// Whether the microphone is currently transmitting (voice detected, or
    /// always when voice activation is off).
    pub fn transmitting(&self) -> bool {
        self.live.transmitting.load(Ordering::Relaxed)
    }

    /// Change the voice activation threshold without restarting the device.
    pub fn set_threshold_db(&self, threshold_db: f32) {
        self.live
            .threshold_db
            .store(threshold_db.to_bits(), Ordering::Relaxed);
    }

    /// Change noise suppression without restarting the device.
    pub fn set_noise_suppression(&self, mode: NoiseSuppression) {
        self.live.noise.store(mode.level(), Ordering::Relaxed);
    }

    /// Frames dropped because the network loop did not keep up.
    pub fn frames_dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Stop capturing and wait for the thread to finish.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Everything the audio callback needs, moved into the callback closure.
struct Processor {
    encoder: VoiceEncoder,
    resampler: LinearResampler,
    mono: Vec<f32>,
    resampled: Vec<f32>,
    pending: Vec<f32>,
    sink: tokio::sync::mpsc::Sender<Vec<u8>>,
    frames: Arc<AtomicU64>,
    dropped: Arc<AtomicU64>,
    live: Arc<Live>,
    denoiser: Denoiser,
    vad: Option<VoiceActivation>,
    /// The last encoded frame, sent first when the gate opens (pre-roll).
    previous: Option<Vec<u8>>,
    monitor: Monitor,
}

impl Processor {
    fn new(
        sample_rate: u32,
        bitrate: i32,
        sink: tokio::sync::mpsc::Sender<Vec<u8>>,
        frames: Arc<AtomicU64>,
        dropped: Arc<AtomicU64>,
        live: Arc<Live>,
        voice_activation: bool,
    ) -> Result<Self> {
        Ok(Self {
            encoder: VoiceEncoder::new(bitrate)?,
            resampler: LinearResampler::new(sample_rate, SAMPLE_RATE),
            mono: Vec::with_capacity(4096),
            resampled: Vec::with_capacity(4096),
            pending: Vec::with_capacity(FRAME_SAMPLES * 2),
            sink,
            frames,
            dropped,
            vad: voice_activation.then(|| {
                VoiceActivation::new(f32::from_bits(live.threshold_db.load(Ordering::Relaxed)))
            }),
            live,
            denoiser: Denoiser::new(),
            previous: None,
            monitor: Monitor::default(),
        })
    }

    /// The device was (re)opened with a new sample rate: drop stale audio.
    fn restart(&mut self, sample_rate: u32) {
        self.resampler = LinearResampler::new(sample_rate, SAMPLE_RATE);
        self.pending.clear();
        self.previous = None;
    }

    /// The device went away: end any open voice session so listeners do not
    /// hear a stuck transmission, and reset the voice gate.
    fn interrupted(&mut self) {
        if self.live.transmitting.swap(false, Ordering::Relaxed) {
            self.send(Vec::new());
        }
        let threshold = f32::from_bits(self.live.threshold_db.load(Ordering::Relaxed));
        self.vad = self.vad.as_ref().map(|_| VoiceActivation::new(threshold));
        self.pending.clear();
        self.previous = None;
    }

    fn send(&mut self, packet: Vec<u8>) {
        match self.sink.try_send(packet) {
            Ok(()) => {
                self.frames.fetch_add(1, Ordering::Relaxed);
            }
            Err(_) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// `interleaved` is a device buffer with `channels` interleaved channels.
    fn feed(&mut self, interleaved: &[f32], channels: usize) {
        if interleaved.is_empty() {
            return;
        }

        self.mono.clear();
        downmix_to_mono(interleaved, channels, &mut self.mono);

        self.resampled.clear();
        self.resampler.process(&self.mono, &mut self.resampled);
        self.pending.extend_from_slice(&self.resampled);

        while self.pending.len() >= FRAME_SAMPLES {
            let mut frame: Vec<f32> = self.pending.drain(..FRAME_SAMPLES).collect();
            // Denoise first, so the level (and voice activation) sees clean audio.
            let noise = NoiseSuppression::from_level(self.live.noise.load(Ordering::Relaxed));
            self.denoiser.process(&mut frame, noise);
            self.monitor.push(&frame);
            let level = level_db(&frame);
            self.live.level_db.store(level.to_bits(), Ordering::Relaxed);
            // Every frame is encoded, sent or not, so the encoder state stays
            // continuous and the pre-roll frame is ready when speech starts.
            let encoded = match self.encoder.encode(&frame) {
                Ok(encoded) => encoded.to_vec(),
                Err(error) => {
                    tracing::warn!(%error, "failed to encode a captured frame");
                    continue;
                }
            };
            let threshold = f32::from_bits(self.live.threshold_db.load(Ordering::Relaxed));
            let decision = match self.vad.as_mut() {
                None => Decision::Send,
                Some(vad) => {
                    vad.set_threshold_db(threshold);
                    vad.next(level)
                }
            };
            match decision {
                Decision::Silent => {}
                Decision::Start => {
                    if let Some(previous) = self.previous.take() {
                        self.send(previous);
                    }
                    self.send(encoded.clone());
                }
                Decision::Send => self.send(encoded.clone()),
                // An empty payload ends the voice session for every listener.
                Decision::End => self.send(Vec::new()),
            }
            let transmitting = self.vad.as_ref().is_none_or(VoiceActivation::is_open);
            self.live
                .transmitting
                .store(transmitting, Ordering::Relaxed);
            self.previous = Some(encoded);
        }
    }
}

/// No callback for this long means the device vanished without an error.
const STALL: Duration = Duration::from_millis(1500);
const RETRY: Duration = Duration::from_millis(200);

fn input<T: cpal::SizedSample + Send + 'static>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    processor: &Arc<Mutex<Processor>>,
    lost: &Arc<AtomicBool>,
    beat: &Arc<AtomicU64>,
    to_f32: fn(T) -> f32,
) -> Result<cpal::Stream> {
    let (processor, beat, lost) = (processor.clone(), beat.clone(), lost.clone());
    let mut scratch: Vec<f32> = Vec::new();
    Ok(device.build_input_stream(
        config,
        move |data: &[T], _| {
            beat.fetch_add(1, Ordering::Relaxed);
            scratch.clear();
            scratch.extend(data.iter().map(|s| to_f32(*s)));
            if let Ok(mut processor) = processor.lock() {
                processor.feed(&scratch, channels);
            }
        },
        move |error: cpal::Error| {
            // Xruns and reroutes are glitches the stream survives; reopening on
            // them would loop forever on a busy system.
            use cpal::ErrorKind::{DeviceChanged, Xrun};
            if !matches!(error.kind(), Xrun | DeviceChanged) {
                tracing::warn!(%error, "capture stream lost");
                lost.store(true, Ordering::Relaxed);
            }
        },
        None,
    )?)
}

fn open_stream(
    device: &cpal::Device,
    processor: &Arc<Mutex<Processor>>,
    lost: &Arc<AtomicBool>,
    beat: &Arc<AtomicU64>,
) -> Result<cpal::Stream> {
    let supported = device.default_input_config()?;
    let channels = supported.channels() as usize;
    if let Ok(mut processor) = processor.lock() {
        processor.restart(supported.sample_rate());
    }
    let (config, format) = (supported.config(), supported.sample_format());
    let stream = match format {
        SampleFormat::F32 => input::<f32>(device, config, channels, processor, lost, beat, |s| s)?,
        SampleFormat::I16 => input::<i16>(device, config, channels, processor, lost, beat, |s| {
            s as f32 / 32768.0
        })?,
        SampleFormat::U16 => input::<u16>(device, config, channels, processor, lost, beat, |s| {
            (s as f32 - 32768.0) / 32768.0
        })?,
        other => return Err(anyhow!("unsupported capture sample format {other:?}")),
    };
    stream.play()?;
    Ok(stream)
}

/// Keeps the microphone alive: when the device disappears (even for a few
/// milliseconds, as with a flaky USB cable) the stream is dropped and reopened
/// until it works, instead of staying dead until the app restarts. Runs until
/// `stop` is set.
fn supervise(
    first: cpal::Device,
    wanted: &Option<String>,
    processor: &Arc<Mutex<Processor>>,
    stop: &AtomicBool,
) {
    let lost = Arc::new(AtomicBool::new(false));
    let beat = Arc::new(AtomicU64::new(0));
    let mut first = Some(first);
    // The open stream, the name of its device and the last heartbeat seen.
    let mut open: Option<(cpal::Stream, String)> = None;
    let mut seen = 0;
    let mut seen_at = Instant::now();
    let mut checked_at = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        if let Some((_, name)) = &open {
            let now = beat.load(Ordering::Relaxed);
            if now != seen {
                (seen, seen_at) = (now, Instant::now());
            }
            // On a fallback device, switch back as soon as the chosen one returns.
            let back = wanted.is_some()
                && wanted.as_deref() != Some(name.as_str())
                && checked_at.elapsed() > Duration::from_secs(1)
                && {
                    checked_at = Instant::now();
                    pick_input(wanted.clone())
                        .is_some_and(|d| &crate::devices::device_name(&d) != name)
                };
            if lost.load(Ordering::Relaxed) || seen_at.elapsed() > STALL || back {
                tracing::warn!("microphone interrupted, reopening");
                open = None;
                if let Ok(mut processor) = processor.lock() {
                    processor.interrupted();
                }
            }
        }
        if open.is_none() {
            lost.store(false, Ordering::Relaxed);
            let device = first.take().or_else(|| pick_input(wanted.clone()));
            match device {
                Some(device) => match open_stream(&device, processor, &lost, &beat) {
                    Ok(stream) => {
                        let name = crate::devices::device_name(&device);
                        tracing::info!(device = %name, "capture started");
                        (seen, seen_at) = (beat.load(Ordering::Relaxed), Instant::now());
                        open = Some((stream, name));
                    }
                    Err(error) => {
                        tracing::warn!(%error, "cannot open the capture stream, retrying")
                    }
                },
                None => tracing::warn!("no input device, retrying"),
            }
            if open.is_none() {
                std::thread::sleep(RETRY);
                continue;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Open the microphone and start feeding encoded frames into `sink`.
pub fn start_capture(
    config: CaptureConfig,
    sink: tokio::sync::mpsc::Sender<Vec<u8>>,
) -> Result<Capture> {
    let device = pick_input(config.device.clone()).ok_or_else(|| anyhow!("no input device"))?;
    let device_name = crate::devices::device_name(&device);
    let supported = device.default_input_config()?;
    let channels = supported.channels() as usize;
    let sample_rate = supported.sample_rate();

    let stop = Arc::new(AtomicBool::new(false));
    let frames = Arc::new(AtomicU64::new(0));
    let dropped = Arc::new(AtomicU64::new(0));

    let live = Arc::new(Live {
        level_db: AtomicU32::new(SILENCE_DB.to_bits()),
        threshold_db: AtomicU32::new(config.threshold_db.to_bits()),
        transmitting: AtomicBool::new(false),
        noise: AtomicU8::new(config.noise_suppression.level()),
    });
    let (stop_thread, frames_thread, dropped_thread, live_thread) =
        (stop.clone(), frames.clone(), dropped.clone(), live.clone());
    let bitrate = config.bitrate;
    let voice_activation = config.voice_activation;
    let device_for_thread = device_name.clone();

    let wanted = config.device.clone();
    let monitor = config.monitor.clone();
    let thread = std::thread::Builder::new()
        .name("ts6-capture".into())
        .spawn(move || {
            let processor = match Processor::new(
                sample_rate,
                bitrate,
                sink,
                frames_thread,
                dropped_thread,
                live_thread.clone(),
                voice_activation,
            ) {
                Ok(processor) => Arc::new(Mutex::new(Processor { monitor, ..processor })),
                Err(error) => {
                    tracing::error!(%error, device = %device_for_thread, "cannot create the Opus encoder");
                    return;
                }
            };
            supervise(device, &wanted, &processor, &stop_thread);
            live_thread.transmitting.store(false, Ordering::Relaxed);
            tracing::info!(device = %device_for_thread, "capture stopped");
        })?;

    Ok(Capture {
        device_name,
        sample_rate,
        channels: channels as u16,
        stop,
        frames,
        dropped,
        live,
        thread: Some(thread),
    })
}
