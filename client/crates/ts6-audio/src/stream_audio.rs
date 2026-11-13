//! Audio of a screen share being watched: Opus (stereo, 48 kHz) from the
//! stream's WebRTC audio track -> mono -> mixed into the playback callback next
//! to voice. One stream is watched at a time, so the sink is process-wide.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result};
use audiopus::coder::Decoder;
use audiopus::{Channels, SampleRate};

/// Playback starts once this much is queued (60 ms) and restarts after running dry.
const PREBUFFER: usize = 2_880;
/// Older audio is dropped past this (300 ms): the stream and the sound card
/// clocks differ, and a growing delay would put the sound behind the picture.
const MAX_QUEUED: usize = 14_400;

#[derive(Default)]
struct State {
    samples: VecDeque<f32>,
    playing: bool,
}

/// Shared between the stream viewer (producer) and the playback callback.
#[derive(Default)]
pub struct StreamAudio(Mutex<State>);

impl StreamAudio {
    /// Queue decoded mono 48 kHz samples.
    pub fn push(&self, mono: &[f32]) {
        let mut state = self.0.lock().unwrap_or_else(|p| p.into_inner());
        state.samples.extend(mono);
        let excess = state.samples.len().saturating_sub(MAX_QUEUED);
        state.samples.drain(..excess);
        if state.samples.len() >= PREBUFFER {
            state.playing = true;
        }
    }

    /// Forget everything queued (the stream ended or changed).
    pub fn clear(&self) {
        let mut state = self.0.lock().unwrap_or_else(|p| p.into_inner());
        state.samples.clear();
        state.playing = false;
    }

    /// Add queued audio to `mono`. Called from the audio callback, so it never
    /// waits on the producer.
    pub fn mix_into(&self, mono: &mut [f32]) {
        let Ok(mut state) = self.0.try_lock() else {
            return;
        };
        if !state.playing {
            return;
        }
        let n = mono.len().min(state.samples.len());
        for (out, sample) in mono.iter_mut().zip(state.samples.drain(..n)) {
            *out += sample;
        }
        if state.samples.is_empty() {
            state.playing = false;
        }
    }
}

/// The process-wide sink.
pub fn stream_audio() -> &'static StreamAudio {
    static SINK: OnceLock<StreamAudio> = OnceLock::new();
    SINK.get_or_init(StreamAudio::default)
}

/// Decodes the stream's stereo Opus to mono.
pub struct StreamDecoder {
    decoder: Decoder,
    pcm: Vec<f32>,
    mono: Vec<f32>,
}

impl StreamDecoder {
    pub fn new() -> Result<Self> {
        Ok(Self {
            decoder: Decoder::new(SampleRate::Hz48000, Channels::Stereo)
                .context("creating the stream opus decoder")?,
            // 120 ms of stereo, the longest Opus packet.
            pcm: vec![0.0; 5_760 * 2],
            mono: Vec::new(),
        })
    }

    /// Decode one RTP payload; `None` conceals one lost 20 ms packet.
    pub fn decode(&mut self, payload: Option<&[u8]>) -> Result<&[f32]> {
        let input = match payload {
            Some(bytes) => {
                Some(audiopus::packet::Packet::try_from(bytes).context("wrapping an opus packet")?)
            }
            None => None,
        };
        let request = if payload.is_none() {
            960 * 2
        } else {
            self.pcm.len()
        };
        let output = audiopus::MutSignals::try_from(&mut self.pcm[..request])
            .context("wrapping the pcm output buffer")?;
        let frames = self
            .decoder
            .decode_float(input, output, false)
            .context("decoding stream audio")?;
        self.mono.clear();
        self.mono.extend(
            self.pcm[..frames * 2]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| (pair[0] + pair[1]) * 0.5),
        );
        Ok(&self.mono)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plays_after_the_prebuffer_and_stops_when_dry() {
        let sink = StreamAudio::default();
        let mut out = [0.0; 4];
        sink.push(&[0.5; 100]);
        sink.mix_into(&mut out);
        assert_eq!(out, [0.0; 4], "still prebuffering");

        sink.push(&vec![0.25; PREBUFFER]);
        sink.mix_into(&mut out);
        assert_eq!(out, [0.5; 4], "the earlier samples play first");

        sink.clear();
        let mut after = [0.0; 4];
        sink.mix_into(&mut after);
        assert_eq!(after, [0.0; 4], "a cleared sink adds nothing");
    }

    #[test]
    fn queue_is_bounded() {
        let sink = StreamAudio::default();
        sink.push(&vec![0.1; MAX_QUEUED * 2]);
        assert_eq!(sink.0.lock().unwrap().samples.len(), MAX_QUEUED);
    }

    #[test]
    fn stereo_opus_comes_out_mono() {
        let encoder = audiopus::coder::Encoder::new(
            SampleRate::Hz48000,
            Channels::Stereo,
            audiopus::Application::Audio,
        )
        .unwrap();
        // 20 ms of a 440 Hz tone, identical on both channels.
        let pcm: Vec<f32> = (0..960)
            .flat_map(|i| {
                let v = (i as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.5;
                [v, v]
            })
            .collect();
        let mut packet = [0u8; 1275];
        let mut decoder = StreamDecoder::new().unwrap();
        let mut peak = 0.0f32;
        for _ in 0..5 {
            let len = encoder.encode_float(&pcm, &mut packet).unwrap();
            peak = peak.max(
                decoder
                    .decode(Some(&packet[..len]))
                    .unwrap()
                    .iter()
                    .fold(0.0, |m, s| m.max(s.abs())),
            );
        }
        assert!(peak > 0.3, "decoded tone peak {peak}");
        assert_eq!(decoder.decode(None).unwrap().len(), 960);
    }
}
