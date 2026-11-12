//! Playback: received voice packets -> jitter buffer/decoder (owned by this
//! thread) -> mono 48 kHz -> device rate -> speakers.
//!
//! Runs on its own OS thread (a `cpal::Stream` is not `Send` on Windows). The
//! [`Mixer`] is created *inside* that thread and never crosses it, which keeps the
//! Opus decoder single-threaded; the network loop only posts packets through a
//! bounded channel.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::SampleFormat;
use tsproto_packets::packets::InAudioBuf;

use crate::codec::SAMPLE_RATE;
use crate::devices::pick_output;
use crate::mixer::Mixer;
use crate::resample::LinearResampler;

/// Notification sounds waiting to be mixed in, 48 kHz mono.
type Cues = Arc<std::sync::Mutex<std::collections::VecDeque<f32>>>;

/// Per-peer playback volume, shared with the audio callback.
type Volumes = Arc<std::sync::Mutex<std::collections::HashMap<u16, f32>>>;

/// Our own processed microphone, played back locally while enabled
/// ("hear yourself"). Capture pushes into it, playback mixes it in.
#[derive(Clone, Default, Debug)]
pub struct Monitor(Arc<MonitorInner>);

#[derive(Default, Debug)]
struct MonitorInner {
    on: AtomicBool,
    samples: std::sync::Mutex<std::collections::VecDeque<f32>>,
}

/// Older monitor audio is dropped past this (100 ms at 48 kHz). shortcut:
/// mic and speaker clocks drift, so this drops or pads samples now and then
/// instead of resampling; upgrade if the monitor is used for long sessions.
const MONITOR_MAX: usize = (SAMPLE_RATE as usize) / 10;

impl Monitor {
    pub fn set_enabled(&self, on: bool) {
        self.0.on.store(on, Ordering::Relaxed);
        if !on {
            self.0
                .samples
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clear();
        }
    }

    pub(crate) fn push(&self, frame: &[f32]) {
        if !self.0.on.load(Ordering::Relaxed) {
            return;
        }
        let mut samples = self.0.samples.lock().unwrap_or_else(|p| p.into_inner());
        samples.extend(frame);
        let excess = samples.len().saturating_sub(MONITOR_MAX);
        samples.drain(..excess);
    }

    fn mix_into(&self, mono: &mut [f32]) {
        if !self.0.on.load(Ordering::Relaxed) {
            return;
        }
        if let Ok(mut samples) = self.0.samples.try_lock() {
            let n = mono.len().min(samples.len());
            for (out, own) in mono.iter_mut().zip(samples.drain(..n)) {
                *out += own;
            }
        }
    }
}

/// Highest per-user volume (400%).
pub const MAX_PEER_VOLUME: f32 = 4.0;

pub struct Playback {
    pub device_name: String,
    pub sample_rate: u32,
    pub channels: u16,
    packets: SyncSender<(u16, InAudioBuf)>,
    volumes: Volumes,
    cues: Cues,
    mixer: Arc<std::sync::Mutex<Mixer>>,
    dropped: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    frames: Arc<AtomicU64>,
    thread: Option<JoinHandle<()>>,
}

impl Playback {
    /// Interleaved frames handed to the device.
    pub fn frames_played(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    /// Voice packets dropped because the playback thread fell behind.
    pub fn packets_dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Set how loud one peer plays: 1.0 is unchanged, 0.0 silent, 4.0 four times as loud.
    pub fn set_volume(&self, peer: u16, volume: f32) {
        let volume = if volume.is_finite() {
            volume.clamp(0.0, MAX_PEER_VOLUME)
        } else {
            1.0
        };
        let mut volumes = self
            .volumes
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if (volume - 1.0).abs() < f32::EPSILON {
            volumes.remove(&peer);
        } else {
            volumes.insert(peer, volume);
        }
    }

    /// Voice level (RMS, 0..1) of each peer currently talking.
    pub fn peer_levels(&self) -> std::collections::HashMap<u16, f32> {
        // A real lock: the audio callback holds the mixer most of the time, so
        // try_lock usually failed and the levels came back empty. Cloning a
        // few entries keeps the wait in microseconds.
        self.mixer
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .levels()
    }

    /// Play a notification sound over whatever is playing.
    pub fn play_cue(&self, cue: crate::cues::Cue) {
        let samples = crate::cues::render(cue);
        let mut cues = self.cues.lock().unwrap_or_else(|p| p.into_inner());
        // A burst of joins mixes into one sound instead of queuing seconds of them.
        for (i, sample) in samples.into_iter().enumerate() {
            match cues.get_mut(i) {
                Some(existing) => *existing += sample,
                None => cues.push_back(sample),
            }
        }
    }

    /// Queue one received voice packet for playback.
    pub fn enqueue(&self, from: u16, packet: InAudioBuf) {
        match self.packets.try_send((from, packet)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => {}
        }
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Pulls mono 48 kHz audio from the mixer and converts it to the device format.
struct Renderer {
    mixer: Arc<std::sync::Mutex<Mixer>>,
    volumes: Volumes,
    resampler: LinearResampler,
    resampled: Vec<f32>,
    /// Device-rate samples produced but not yet played. Kept across callbacks:
    /// dropping them, as an earlier version did, cut a few samples of voice on
    /// every callback, audible as small clicks.
    ready: std::collections::VecDeque<f32>,
    frames: Arc<AtomicU64>,
    monitor: Monitor,
    cues: Cues,
}

/// Mixer audio pulled per step: 5 ms, so at most 5 ms of extra latency.
const RENDER_CHUNK: usize = (SAMPLE_RATE as usize) / 200;

impl Renderer {
    fn new(
        target_rate: u32,
        mixer: Arc<std::sync::Mutex<Mixer>>,
        volumes: Volumes,
        frames: Arc<AtomicU64>,
        monitor: Monitor,
        cues: Cues,
    ) -> Self {
        Self {
            mixer,
            volumes,
            resampler: LinearResampler::new(SAMPLE_RATE, target_rate),
            resampled: Vec::new(),
            ready: std::collections::VecDeque::new(),
            frames,
            monitor,
            cues,
        }
    }

    /// Fill `out` (interleaved, `channels` channels) from the mixer.
    fn fill(&mut self, out: &mut [f32], channels: usize) {
        let out_frames = out.len() / channels;
        if out_frames == 0 {
            return;
        }

        // Pull small chunks until enough device-rate audio is ready; whatever is
        // left over waits for the next callback.
        while self.ready.len() < out_frames {
            let mut mono = {
                let mut mixer = self
                    .mixer
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                if let Ok(volumes) = self.volumes.try_lock() {
                    mixer.apply_volumes(&volumes);
                }
                mixer.render_mono(RENDER_CHUNK)
            };
            self.monitor.mix_into(&mut mono);
            if let Ok(mut cues) = self.cues.try_lock() {
                let n = mono.len().min(cues.len());
                for (out, cue) in mono.iter_mut().zip(cues.drain(..n)) {
                    *out += cue;
                }
            }
            self.resampled.clear();
            self.resampler.process(&mono, &mut self.resampled);
            if self.resampled.is_empty() {
                // Never spin inside the audio callback.
                break;
            }
            self.ready.extend(self.resampled.iter().copied());
        }

        for frame in 0..out_frames {
            let value = self.ready.pop_front().unwrap_or(0.0).clamp(-1.0, 1.0);
            for channel in 0..channels {
                out[frame * channels + channel] = value;
            }
        }

        self.frames.fetch_add(out_frames as u64, Ordering::Relaxed);
    }
}

/// Open the speakers. Received voice packets must be posted with
/// [`Playback::enqueue`].
pub fn start_playback(device: Option<String>, monitor: Monitor) -> Result<Playback> {
    let output = pick_output(device).ok_or_else(|| anyhow!("no output device"))?;
    let device_name = crate::devices::device_name(&output);
    let supported = output.default_output_config()?;
    let channels = supported.channels() as usize;
    let sample_rate = supported.sample_rate();
    let sample_format = supported.sample_format();
    let stream_config = supported.config();

    let stop = Arc::new(AtomicBool::new(false));
    let frames = Arc::new(AtomicU64::new(0));
    let dropped = Arc::new(AtomicU64::new(0));
    let (packets, packet_rx) = sync_channel::<(u16, InAudioBuf)>(128);
    let volumes: Volumes = Default::default();
    let cues: Cues = Default::default();
    let cues_thread = cues.clone();
    let mixer = Arc::new(std::sync::Mutex::new(Mixer::new()));
    let mixer_thread = mixer.clone();
    let volumes_thread = volumes.clone();

    let (stop_thread, frames_thread, dropped_thread) =
        (stop.clone(), frames.clone(), dropped.clone());
    let device_for_thread = device_name.clone();

    let thread = std::thread::Builder::new()
        .name("ts6-playback".into())
        .spawn(move || {
            let mixer = mixer_thread;

            // Pump: move queued packets into the jitter buffer. Runs on this thread,
            // so the decoder and its queues never leave it.
            let pump_mixer = mixer.clone();
            let pump_stop = stop_thread.clone();
            let pump_dropped = dropped_thread.clone();
            let pump = std::thread::Builder::new()
                .name("ts6-playback-pump".into())
                .spawn(move || {
                    while !pump_stop.load(Ordering::Relaxed) {
                        match packet_rx.recv_timeout(std::time::Duration::from_millis(50)) {
                            Ok((from, packet)) => {
                                let mut mixer = pump_mixer
                                    .lock()
                                    .unwrap_or_else(|poison| poison.into_inner());
                                if let Err(error) = mixer.push(from, packet) {
                                    tracing::debug!(%error, from, "jitter buffer rejected a packet");
                                }
                            }
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                        }
                    }
                    let _ = pump_dropped;
                });

            let on_error = |error| tracing::error!(%error, "playback stream error");

            let stream = match sample_format {
                SampleFormat::F32 => {
                    let mut renderer = Renderer::new(sample_rate, mixer.clone(), volumes_thread.clone(), frames_thread.clone(), monitor.clone(), cues_thread.clone());
                    output.build_output_stream(
                        stream_config,
                        move |data: &mut [f32], _| renderer.fill(data, channels),
                        on_error,
                        None,
                    )
                }
                SampleFormat::I16 => {
                    let mut renderer = Renderer::new(sample_rate, mixer.clone(), volumes_thread.clone(), frames_thread.clone(), monitor.clone(), cues_thread.clone());
                    let mut scratch: Vec<f32> = vec![0.0; 8192];
                    output.build_output_stream::<i16, _, _>(
                        stream_config,
                        move |data: &mut [i16], _| {
                            if scratch.len() < data.len() {
                                scratch.resize(data.len(), 0.0);
                            }
                            let slice = &mut scratch[..data.len()];
                            renderer.fill(slice, channels);
                            for (dst, src) in data.iter_mut().zip(slice.iter()) {
                                *dst = (*src * 32767.0) as i16;
                            }
                        },
                        on_error,
                        None,
                    )
                }
                SampleFormat::U16 => {
                    let mut renderer = Renderer::new(sample_rate, mixer.clone(), volumes_thread.clone(), frames_thread.clone(), monitor.clone(), cues_thread.clone());
                    let mut scratch: Vec<f32> = vec![0.0; 8192];
                    output.build_output_stream::<u16, _, _>(
                        stream_config,
                        move |data: &mut [u16], _| {
                            if scratch.len() < data.len() {
                                scratch.resize(data.len(), 0.0);
                            }
                            let slice = &mut scratch[..data.len()];
                            renderer.fill(slice, channels);
                            for (dst, src) in data.iter_mut().zip(slice.iter()) {
                                *dst = (*src * 32767.0 + 32768.0) as u16;
                            }
                        },
                        on_error,
                        None,
                    )
                }
                other => {
                    tracing::error!(?other, "unsupported playback sample format");
                    return;
                }
            };

            let stream = match stream {
                Ok(stream) => stream,
                Err(error) => {
                    tracing::error!(%error, device = %device_for_thread, "cannot open the playback stream");
                    return;
                }
            };
            if let Err(error) = stream.play() {
                tracing::error!(%error, "cannot start playback");
                return;
            }
            tracing::info!(device = %device_for_thread, sample_rate, channels, ?sample_format, "playback started");

            while !stop_thread.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            drop(stream);
            if let Ok(pump) = pump {
                let _ = pump.join();
            }
        })?;

    Ok(Playback {
        device_name,
        sample_rate,
        channels: channels as u16,
        packets,
        volumes,
        cues,
        mixer,
        dropped,
        stop,
        frames,
        thread: Some(thread),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitor_is_silent_when_off_and_bounded_when_on() {
        let monitor = Monitor::default();
        monitor.push(&[0.5; 10]);
        let mut out = [0.0; 4];
        monitor.mix_into(&mut out);
        assert_eq!(out, [0.0; 4]);

        monitor.set_enabled(true);
        monitor.push(&vec![0.25; MONITOR_MAX * 2]);
        assert_eq!(monitor.0.samples.lock().unwrap().len(), MONITOR_MAX);
        monitor.mix_into(&mut out);
        assert_eq!(out, [0.25; 4]);
    }
}
