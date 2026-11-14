//! System audio of a screen share: everything the computer plays except Mirage
//! itself, captured through WASAPI process loopback and mixed into the voice
//! the sharer transmits. The official client sends stream sound the same way
//! (inside the Opus voice channel, never in the WebRTC audio section).
//!
//! Mirage's own process tree is excluded: otherwise the voices of the call,
//! played by Mirage, would be captured and sent back into the channel.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

/// Older audio is dropped past this (200 ms): a stalled voice path must not
/// turn into a growing delay.
const MAX_QUEUED: usize = 9_600;

/// Mono 48 kHz samples captured from the system, waiting for the microphone
/// pipeline. One process-wide queue: only one share exists at a time.
#[derive(Default)]
pub struct SystemAudio {
    samples: Mutex<VecDeque<f32>>,
    on: AtomicBool,
}

impl SystemAudio {
    /// Whether a capture is feeding the queue (the voice path checks this per frame).
    pub fn is_on(&self) -> bool {
        self.on.load(Ordering::Relaxed)
    }

    pub fn set_on(&self, on: bool) {
        self.on.store(on, Ordering::Relaxed);
        if !on {
            self.clear();
        }
    }

    pub fn push(&self, mono: &[f32]) {
        let mut samples = self.samples.lock().unwrap_or_else(|p| p.into_inner());
        samples.extend(mono);
        let excess = samples.len().saturating_sub(MAX_QUEUED);
        samples.drain(..excess);
    }

    pub fn clear(&self) {
        self.samples
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
    }

    /// Fills `out` with queued audio; what is missing stays silent.
    /// Returns how many samples were real.
    pub fn take(&self, out: &mut [f32]) -> usize {
        out.fill(0.0);
        let Ok(mut samples) = self.samples.try_lock() else {
            return 0;
        };
        let n = out.len().min(samples.len());
        for (slot, sample) in out.iter_mut().zip(samples.drain(..n)) {
            *slot = sample;
        }
        n
    }
}

/// The process-wide queue.
pub fn system_audio() -> &'static SystemAudio {
    static QUEUE: OnceLock<SystemAudio> = OnceLock::new();
    QUEUE.get_or_init(SystemAudio::default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_pads_with_silence_and_the_queue_is_bounded() {
        let queue = SystemAudio::default();
        queue.push(&[0.5; 3]);
        let mut out = [1.0; 5];
        assert_eq!(queue.take(&mut out), 3);
        assert_eq!(out, [0.5, 0.5, 0.5, 0.0, 0.0]);
        queue.push(&vec![0.1; MAX_QUEUED * 2]);
        assert_eq!(queue.samples.lock().unwrap().len(), MAX_QUEUED);
        queue.set_on(false);
        assert_eq!(queue.take(&mut out), 0);
    }
}
