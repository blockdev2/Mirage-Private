//! Receiving side: jitter buffers per peer (from the vendored library) mixed into
//! one playback buffer.

use anyhow::Result;
use tsclientlib::audio::AudioHandler;

pub use tsproto_packets::packets::AudioData;
use tsproto_packets::packets::InAudioBuf;

/// A peer is identified by its client id on the server.
pub type PeerId = u16;

/// Mixes all incoming voice streams.
pub struct Mixer {
    handler: AudioHandler<PeerId>,
    /// Smoothed RMS (0..1) of each talking peer's decoded voice.
    levels: std::collections::HashMap<PeerId, f32>,
}

impl Mixer {
    pub fn new() -> Self {
        Self {
            handler: AudioHandler::new(),
            levels: Default::default(),
        }
    }

    /// Feed one received voice packet. The handler keeps a per-peer queue,
    /// reorders it and decodes with Opus (loss concealment included).
    pub fn push(&mut self, from: PeerId, packet: InAudioBuf) -> Result<()> {
        self.handler.handle_packet(from, packet)?;
        Ok(())
    }

    /// Applies per-peer playback volume (1.0 = unchanged, 2.0 = +100%).
    /// Peers without an entry play at 1.0.
    pub fn apply_volumes(&mut self, volumes: &std::collections::HashMap<PeerId, f32>) {
        for (peer, queue) in self.handler.get_mut_queues() {
            queue.volume = volumes.get(peer).copied().unwrap_or(1.0);
        }
    }

    /// Peers that currently have queued audio.
    pub fn active_peers(&self) -> Vec<PeerId> {
        let mut peers: Vec<PeerId> = self.handler.get_queues().keys().copied().collect();
        peers.sort_unstable();
        peers
    }

    /// Render `frames` interleaved stereo frames (the shape `cpal` wants).
    pub fn render_stereo(&mut self, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0; frames * 2];
        let levels = &mut self.levels;
        self.handler
            .fill_buffer_with_proc(&mut out, |peer, samples| {
                let rms = (samples.iter().map(|s| s * s).sum::<f32>()
                    / samples.len().max(1) as f32)
                    .sqrt();
                let level = levels.entry(*peer).or_insert(0.0);
                // Rise at once, fall gently, like a meter. Chunks are ~5 ms and
                // the UI reads every ~80 ms, so this must hold between reads.
                *level = rms.max(*level * 0.98);
            });
        let queues = self.handler.get_queues();
        self.levels.retain(|peer, _| queues.contains_key(peer));
        out
    }

    /// Current voice level (RMS, 0..1) of each peer that is talking.
    pub fn levels(&self) -> std::collections::HashMap<PeerId, f32> {
        self.levels.clone()
    }

    /// Render `frames` mono frames, for metering and for tests.
    pub fn render_mono(&mut self, frames: usize) -> Vec<f32> {
        let stereo = self.render_stereo(frames);
        stereo
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| (pair[0] + pair[1]) * 0.5)
            .collect()
    }

    pub fn clear(&mut self) {
        self.handler.reset();
    }
}

impl Default for Mixer {
    fn default() -> Self {
        Self::new()
    }
}

/// Extract who sent a voice packet, if it is a server-to-client voice packet.
pub fn sender_of(packet: &InAudioBuf) -> Option<PeerId> {
    match packet.data().data() {
        AudioData::S2C { from, .. } => Some(*from),
        AudioData::S2CWhisper { from, .. } => Some(*from),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_mixer_renders_silence_of_the_requested_length() {
        let mut mixer = Mixer::new();
        assert!(mixer.active_peers().is_empty());

        let mono = mixer.render_mono(480);
        assert_eq!(mono.len(), 480);
        assert!(mono.iter().all(|s| *s == 0.0), "idle mixer must be silent");

        let stereo = mixer.render_stereo(240);
        assert_eq!(stereo.len(), 480);
    }
}
