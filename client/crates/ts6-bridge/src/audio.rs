//! Local audio pipeline for the bridge: microphone (or synthetic tone) into the
//! session, received voice into the speakers, optional push-to-talk gate.
//!
//! Everything hardware-facing lives on its own thread inside `ts6-audio`; this
//! module only wires the pieces together and keeps the counters the UI shows.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ts6_core::{AudioData, InAudioBuf};

/// How long a client keeps its "speaking" ring after the last voice packet,
/// for senders that never send the end-of-voice marker.
const SPEAKING_HOLD: Duration = Duration::from_millis(300);
/// How often audio counters reach the UI. Short, so the speaking rings and the
/// input meter follow the voice closely.
const STATS_INTERVAL: Duration = Duration::from_millis(80);
/// Captured frames allowed to wait for the network loop (20 ms each).
const MIC_QUEUE_FRAMES: usize = 4;
/// Voice gate while sharing sound with push-to-talk: below ordinary speech and
/// quiet game audio, above digital silence.
const STREAM_KEY_THRESHOLD_DB: f32 = -55.0;

#[derive(Debug, Clone, PartialEq)]
pub struct AudioConfig {
    pub enabled: bool,
    pub tone: bool,
    pub ptt: Option<String>,
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    /// Without push-to-talk: transmit only while voice is detected (true) or
    /// continuously (false).
    pub voice_activation: bool,
    pub threshold_db: f32,
    pub noise_suppression: ts6_audio::denoise::NoiseSuppression,
    /// Play our own processed microphone back locally.
    pub monitor: bool,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            tone: false,
            ptt: None,
            input_device: None,
            output_device: None,
            voice_activation: true,
            threshold_db: ts6_audio::vad::DEFAULT_THRESHOLD_DB,
            noise_suppression: ts6_audio::denoise::NoiseSuppression::Standard,
            monitor: false,
        }
    }
}

pub struct AudioEngine {
    playback: Option<ts6_audio::Playback>,
    capture: Option<ts6_audio::Capture>,
    gate: Option<ts6_input::Gate>,
    ptt: Option<ts6_input::PttMonitor>,
    mic_tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    mic_rx: tokio::sync::mpsc::Receiver<Vec<u8>>,
    tone_stop: Option<Arc<AtomicBool>>,
    tone_thread: Option<std::thread::JoinHandle<()>>,
    pub tone: bool,
    pub ptt_binding: Option<String>,
    rx_packets: u64,
    speaking: std::collections::HashMap<u16, Instant>,
    last_publish: Instant,
    /// The configuration currently applied, to tell threshold-only changes apart.
    applied: Option<AudioConfig>,
    /// Per-client playback volume, reapplied when playback restarts.
    volumes: std::collections::HashMap<u16, f32>,
    monitor: ts6_audio::Monitor,
    /// The computer's sound, captured while a share sends it (Windows only).
    #[cfg(windows)]
    system_audio: Option<ts6_audio::loopback::Loopback>,
    /// A cue asked for before playback started (our own join: the UI turns
    /// audio on a moment after the session connects).
    pending_cue: Option<ts6_audio::cues::Cue>,
}

impl AudioEngine {
    pub fn new() -> Self {
        // At most a few frames may wait for the network loop. A larger queue
        // turned any brief stall into permanent latency: the backlog went out in
        // a burst and then sat in the listener's jitter buffer.
        let (mic_tx, mic_rx) = tokio::sync::mpsc::channel(MIC_QUEUE_FRAMES);
        Self {
            playback: None,
            capture: None,
            gate: None,
            ptt: None,
            mic_tx,
            mic_rx,
            tone_stop: None,
            tone_thread: None,
            tone: false,
            ptt_binding: None,
            rx_packets: 0,
            speaking: Default::default(),
            last_publish: Instant::now(),
            applied: None,
            volumes: Default::default(),
            monitor: Default::default(),
            #[cfg(windows)]
            system_audio: None,
            pending_cue: None,
        }
    }

    /// The session polls this for microphone frames.
    pub fn mic_rx_mut(&mut self) -> &mut tokio::sync::mpsc::Receiver<Vec<u8>> {
        &mut self.mic_rx
    }

    /// A received voice packet: note who is speaking and queue it for playback.
    pub fn on_audio(&mut self, packet: InAudioBuf) {
        let from = match packet.data().data() {
            AudioData::S2C { from, .. } => Some(*from),
            AudioData::S2CWhisper { from, .. } => Some(*from),
            _ => None,
        };
        let ended = match packet.data().data() {
            AudioData::S2C { data, .. } | AudioData::S2CWhisper { data, .. } => data.is_empty(),
            _ => false,
        };
        if let Some(from) = from {
            self.rx_packets += 1;
            // The empty end-of-voice packet clears the ring at once.
            if ended {
                self.speaking.remove(&from);
            } else {
                self.speaking.insert(from, Instant::now());
            }
            if let Some(playback) = &self.playback {
                playback.enqueue(from, packet);
            }
        }
    }

    pub fn configure(&mut self, config: &AudioConfig) {
        // Moving the voice activation slider must not restart the microphone.
        if let (Some(applied), Some(capture)) = (&self.applied, &self.capture) {
            let same = AudioConfig {
                threshold_db: config.threshold_db,
                noise_suppression: config.noise_suppression,
                monitor: config.monitor,
                ..applied.clone()
            };
            if &same == config {
                capture.set_threshold_db(config.threshold_db);
                capture.set_noise_suppression(config.noise_suppression);
                self.monitor.set_enabled(config.monitor);
                self.applied = Some(config.clone());
                return;
            }
        }
        self.teardown_input();
        self.applied = Some(config.clone());
        self.monitor.set_enabled(config.enabled && config.monitor);

        if !config.enabled {
            if let Some(playback) = self.playback.take() {
                playback.stop();
            }
            self.tone = false;
            self.ptt_binding = None;
            return;
        }

        // While a share sends the computer's sound the microphone path runs in
        // stream mode: the capture itself holds the microphone closed unless
        // the key is down, so the sound is not gated by push-to-talk.
        let stream_mode = self.stream_audio_on();
        let mut mic_open: Option<Arc<AtomicBool>> = None;
        // Push-to-talk, when bound: the gate sits between the source and the session.
        let binding = config.ptt.clone().filter(|key| !key.trim().is_empty());
        let mut monitor = None;
        let target = match &binding {
            Some(key) => match ts6_input::bind_ptt(key) {
                Ok(started) if stream_mode => {
                    mic_open = Some(started.pressed());
                    monitor = Some(started);
                    self.mic_tx.clone()
                }
                Ok(started) => {
                    let (source_tx, source_rx) =
                        tokio::sync::mpsc::channel::<Vec<u8>>(MIC_QUEUE_FRAMES);
                    let gate =
                        ts6_input::Gate::start(source_rx, self.mic_tx.clone(), started.pressed());
                    self.gate = Some(gate);
                    monitor = Some(started);
                    source_tx
                }
                Err(error) => {
                    tracing::warn!(%error, "PTT unavailable; transmission disabled");
                    return;
                }
            },
            None => self.mic_tx.clone(),
        };
        self.ptt = monitor;
        if stream_mode && mic_open.is_none() {
            mic_open = Some(Arc::new(AtomicBool::new(true)));
        }
        self.ptt_binding = self.ptt.as_ref().and_then(|_| binding.clone());

        // Playback (receiving voice).
        if self.playback.is_none() {
            match ts6_audio::start_playback(config.output_device.clone(), self.monitor.clone()) {
                Ok(playback) => {
                    for (client, volume) in &self.volumes {
                        playback.set_volume(*client, *volume);
                    }
                    if let Some(cue) = self.pending_cue.take() {
                        playback.play_cue(cue);
                    }
                    self.playback = Some(playback);
                }
                Err(error) => tracing::warn!(%error, "playback unavailable"),
            }
        }

        // Source: synthetic tone when asked, otherwise the real microphone.
        // An unavailable microphone is reported, never silently replaced by a tone.
        if config.tone {
            if let Some((stop, thread)) = spawn_tone(target) {
                self.tone_stop = stop;
                self.tone_thread = Some(thread);
                self.tone = true;
            }
        } else {
            match ts6_audio::start_capture(
                ts6_audio::CaptureConfig {
                    device: config.input_device.clone(),
                    bitrate: 64_000,
                    // Push-to-talk gates downstream; otherwise voice activation
                    // decides, unless continuous transmission was chosen.
                    // In stream mode with a key there is no gate downstream, so the
                    // capture's own gate closes the voice when nothing is heard.
                    voice_activation: if stream_mode && self.ptt.is_some() {
                        true
                    } else {
                        self.ptt.is_none() && config.voice_activation
                    },
                    threshold_db: if stream_mode && self.ptt.is_some() {
                        STREAM_KEY_THRESHOLD_DB
                    } else {
                        config.threshold_db
                    },
                    noise_suppression: config.noise_suppression,
                    monitor: self.monitor.clone(),
                    mic_open,
                },
                target,
            ) {
                Ok(capture) => self.capture = Some(capture),
                Err(error) => tracing::warn!(%error, "capture unavailable"),
            }
        }
    }

    fn teardown_input(&mut self) {
        let was_transmitting = self.transmitting();
        if let Some(gate) = self.gate.take() {
            gate.stop();
        }
        if let Some(ptt) = self.ptt.take() {
            ptt.stop();
        }
        if let Some(capture) = self.capture.take() {
            capture.stop();
        }
        if let Some(stop) = self.tone_stop.take() {
            stop.store(true, Ordering::Relaxed);
        }
        if let Some(thread) = self.tone_thread.take() {
            let _ = thread.join();
        }
        self.tone = false;
        self.ptt_binding = None;
        // Producers are stopped; stale frames must not survive reconfiguration.
        while self.mic_rx.try_recv().is_ok() {}
        // Stopping mid-sentence still ends the voice session for listeners.
        if was_transmitting {
            let _ = self.mic_tx.try_send(Vec::new());
        }
        self.applied = None;
    }

    /// Play a join or leave sound, when the speakers are open.
    pub fn play_cue(&mut self, cue: ts6_audio::cues::Cue) {
        match &self.playback {
            Some(playback) => playback.play_cue(cue),
            None => self.pending_cue = Some(cue),
        }
    }

    /// How loud one client plays for us (1.0 = unchanged).
    pub fn set_volume(&mut self, client: u16, volume: f32) {
        self.volumes.insert(client, volume);
        if let Some(playback) = &self.playback {
            playback.set_volume(client, volume);
        }
    }

    /// Whether a share is currently sending the computer's sound.
    pub fn stream_audio_on(&self) -> bool {
        #[cfg(windows)]
        {
            self.system_audio.is_some()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    /// Starts or stops sending the computer's sound (everything except Mirage)
    /// in our voice. Fails, with the reason, where the system cannot capture it.
    pub fn set_stream_audio(&mut self, on: bool) -> Result<(), String> {
        #[cfg(windows)]
        {
            if on == self.system_audio.is_some() {
                return Ok(());
            }
            self.system_audio = if on {
                Some(
                    ts6_audio::loopback::Loopback::start(std::process::id())
                        .map_err(|error| format!("{error:#}"))?,
                )
            } else {
                None
            };
            // The microphone path is gated differently in stream mode: rebuild it.
            if let Some(applied) = self.applied.take() {
                self.configure(&applied);
            }
            Ok(())
        }
        #[cfg(not(windows))]
        {
            if on {
                Err("stream sound needs Windows".to_owned())
            } else {
                Ok(())
            }
        }
    }

    /// Whether our own voice is going out right now.
    pub fn transmitting(&self) -> bool {
        if self.tone {
            return self
                .ptt
                .as_ref()
                .is_none_or(|m| m.pressed().load(Ordering::Relaxed));
        }
        let Some(capture) = &self.capture else {
            return false;
        };
        match &self.ptt {
            Some(monitor) => monitor.pressed().load(Ordering::Relaxed),
            None => capture.transmitting(),
        }
    }

    /// Reset the per-session counters when a new session starts, so the UI never
    /// shows counters accumulated across connections.
    pub fn reset_counters(&mut self) {
        self.rx_packets = 0;
        self.speaking.clear();
        self.last_publish = Instant::now();
    }
    pub fn stop(&mut self) {
        self.teardown_input();
        if let Some(playback) = self.playback.take() {
            playback.stop();
        }
        self.speaking.clear();
        self.pending_cue = None;
    }

    /// Snapshot of the counters for the UI, refreshed at most every 400 ms.
    pub fn stats_due(&mut self) -> bool {
        self.last_publish.elapsed() >= STATS_INTERVAL
    }

    pub fn mark_published(&mut self) {
        self.last_publish = Instant::now();
    }

    pub fn counters(&mut self) -> Counters {
        let now = Instant::now();
        self.speaking
            .retain(|_, at| now.duration_since(*at) < SPEAKING_HOLD);
        let mut speaking: Vec<u16> = self.speaking.keys().copied().collect();
        speaking.sort_unstable();
        Counters {
            rx_packets: self.rx_packets,
            ptt_down: self
                .ptt
                .as_ref()
                .map(|monitor| monitor.pressed().load(Ordering::Relaxed))
                .unwrap_or(false),
            speaking,
            transmitting: self.transmitting(),
            input_level_db: self.capture.as_ref().map(|c| c.level_db()),
            capture: self.capture.as_ref().map(|c| c.device_name.clone()),
            playback: self.playback.as_ref().map(|p| p.device_name.clone()),
            tone: self.tone,
            ptt_binding: self.ptt_binding.clone(),
            levels: self
                .playback
                .as_ref()
                .map(|p| p.peer_levels())
                .unwrap_or_default(),
        }
    }
}

impl Default for AudioEngine {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Counters {
    pub rx_packets: u64,
    pub ptt_down: bool,
    pub speaking: Vec<u16>,
    pub transmitting: bool,
    pub input_level_db: Option<f32>,
    pub capture: Option<String>,
    pub playback: Option<String>,
    pub tone: bool,
    pub ptt_binding: Option<String>,
    /// Voice level (RMS, 0..1) of each peer talking right now.
    pub levels: std::collections::HashMap<u16, f32>,
}

/// Spawn the synthetic tone generator; returns its stop flag and thread handle.
pub(crate) fn spawn_tone(
    sink: tokio::sync::mpsc::Sender<Vec<u8>>,
) -> Option<(Option<Arc<AtomicBool>>, std::thread::JoinHandle<()>)> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let thread = std::thread::Builder::new()
        .name("ts6-bridge-tone".into())
        .spawn(move || {
            let mut encoder = match ts6_audio::VoiceEncoder::new(64_000) {
                Ok(encoder) => encoder,
                Err(error) => {
                    tracing::error!(%error, "cannot create the tone encoder");
                    return;
                }
            };
            let mut tone = ts6_audio::ToneSource::new(440.0, 0.1);
            // An absolute cadence reduces cumulative scheduling drift. Historical
            // measurements showed unexplained excess packets; this scheduling
            // choice alone does not establish their cause or resolution.
            let frame_duration = Duration::from_millis(20);
            let mut next_frame_at = std::time::Instant::now();
            while !stop_thread.load(Ordering::Relaxed) {
                let frame = tone.next_frame();
                if let Ok(encoded) = encoder.encode(&frame) {
                    if sink.try_send(encoded.to_vec()).is_err() {
                        // Session is not draining; skip this frame.
                    }
                }
                next_frame_at += frame_duration;
                let now = std::time::Instant::now();
                if next_frame_at > now {
                    std::thread::sleep(next_frame_at - now);
                } else {
                    // Fell behind (or the system clock jumped): resynchronise.
                    next_frame_at = now;
                }
            }
        })
        .ok()?;
    Some((Some(stop), thread))
}

/// Refresh the device lists.
pub fn device_lists() -> crate::protocol::DeviceLists {
    crate::protocol::DeviceLists {
        inputs: ts6_audio::devices::inputs()
            .into_iter()
            .map(Into::into)
            .collect(),
        outputs: ts6_audio::devices::outputs()
            .into_iter()
            .map(Into::into)
            .collect(),
    }
}

#[cfg(test)]
mod safety_tests {
    use super::*;

    #[test]
    fn a_cue_before_playback_waits_for_it() {
        let mut engine = AudioEngine::new();
        engine.play_cue(ts6_audio::cues::Cue::Join);
        assert_eq!(engine.pending_cue, Some(ts6_audio::cues::Cue::Join));
        // Leaving the call drops a cue that never played.
        engine.stop();
        assert_eq!(engine.pending_cue, None);
    }

    #[test]
    fn invalid_ptt_never_starts_a_transmitting_source() {
        let mut engine = AudioEngine::new();
        engine.mic_tx.try_send(vec![1, 2, 3]).unwrap();
        engine.configure(&AudioConfig {
            enabled: true,
            tone: true,
            ptt: Some("not-a-key".into()),
            ..AudioConfig::default()
        });
        let transmitting = engine.tone || engine.capture.is_some();
        let queued = engine.mic_rx.try_recv().is_ok();
        engine.stop();
        assert!(!transmitting, "invalid PTT must fail closed");
        assert!(!queued, "queued audio leaked after disabling PTT");
    }
}

#[cfg(all(test, windows))]
mod stream_sound_tests {
    use super::*;
    use std::process::Command;
    use std::time::Duration;

    /// A 4 s, 440 Hz, 16-bit mono WAV.
    fn tone_wav() -> Vec<u8> {
        let rate = 44_100u32;
        let samples: Vec<i16> = (0..rate * 4)
            .map(|i| {
                ((i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 12_000.0) as i16
            })
            .collect();
        let data_len = (samples.len() * 2) as u32;
        let mut wav = b"RIFF".to_vec();
        wav.extend((36 + data_len).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16u32.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(rate.to_le_bytes());
        wav.extend((rate * 2).to_le_bytes());
        wav.extend(2u16.to_le_bytes());
        wav.extend(16u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend(data_len.to_le_bytes());
        for sample in samples {
            wav.extend(sample.to_le_bytes());
        }
        wav
    }

    /// The whole path on a real machine: sound from another process ->
    /// process loopback -> stream mode -> Opus voice packets, with no key held
    /// and no microphone needed. Needs an output device and Windows process
    /// loopback: `cargo test -p ts6-bridge stream_sound -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs an output device and Windows process loopback"]
    fn the_computers_sound_leaves_as_voice_without_a_key() {
        let path = std::env::temp_dir().join("mirage-stream-sound.wav");
        std::fs::write(&path, tone_wav()).unwrap();
        let mut engine = AudioEngine::new();
        engine.configure(&AudioConfig {
            enabled: true,
            // A key is bound: stream sound must pass although it is never pressed.
            ptt: Some("F9".into()),
            ..AudioConfig::default()
        });
        engine.set_stream_audio(true).expect("system audio starts");
        assert!(engine.stream_audio_on());

        // Started through WMI, so the player is not a child of this process
        // (a child would be inside the excluded process tree, like Mirage's own
        // call audio; an application being shared never is).
        let command = format!(
            "powershell -NoProfile -STA -Command \"(New-Object Media.SoundPlayer '{}').PlaySync()\"",
            path.display()
        );
        let mut starter = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{{CommandLine='{}'}} | Out-Null",
                    command.replace('\'', "''")
                ),
            ])
            .spawn()
            .unwrap();
        let mut decoder = ts6_audio::VoiceDecoder::new().unwrap();
        let (mut packets, mut peak) = (0u32, 0.0f32);
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(4) {
            while let Ok(packet) = engine.mic_rx_mut().try_recv() {
                if packet.is_empty() {
                    continue;
                }
                packets += 1;
                peak = decoder
                    .decode(Some(&packet))
                    .unwrap()
                    .iter()
                    .fold(peak, |m, s| m.max(s.abs()));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = starter.wait();
        let _ = std::fs::remove_file(&path);
        println!("{packets} voice packets, peak {peak:.3}");
        engine.set_stream_audio(false).unwrap();
        assert!(!engine.stream_audio_on());
        engine.stop();
        assert!(packets > 20, "only {packets} voice packets");
        assert!(
            peak > 0.05,
            "the tone did not arrive as voice (peak {peak})"
        );
    }
}
