//! Noise suppression with RNNoise (`nnnoiseless`, a pure-Rust port of the
//! recurrent-network denoiser used by many voice apps).
//!
//! RNNoise works on 10 ms frames at 48 kHz, which is exactly our sample rate, so
//! a 20 ms voice frame is two RNNoise frames and no resampling is needed. It
//! adds one 10 ms frame of algorithmic delay.
//!
//! Levels:
//! * `Off`: the signal is untouched.
//! * `Standard`: RNNoise removes steady noise (fans, hum, hiss, traffic).
//! * `Strong`: as `Standard`, and frames the network does not consider speech
//!   are muted, which also removes keyboard clicks, mouse clicks and breathing
//!   between words.

use nnnoiseless::DenoiseState;

/// RNNoise frame: 10 ms at 48 kHz.
const RNN_FRAME: usize = DenoiseState::FRAME_SIZE;
/// RNNoise expects 16-bit sample scale.
const SCALE: f32 = 32_768.0;
/// Below this speech probability `Strong` mutes the frame.
const STRONG_VOICE_PROBABILITY: f32 = 0.55;
/// Frames of speech probability kept after the last speech frame in `Strong`,
/// so word endings are not clipped (10 frames of 10 ms).
const STRONG_HOLD: u32 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NoiseSuppression {
    Off,
    #[default]
    Standard,
    Strong,
}

impl NoiseSuppression {
    pub fn from_level(level: u8) -> Self {
        match level {
            0 => Self::Off,
            2 => Self::Strong,
            _ => Self::Standard,
        }
    }

    pub fn level(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::Standard => 1,
            Self::Strong => 2,
        }
    }
}

pub struct Denoiser {
    state: Box<DenoiseState<'static>>,
    input: Vec<f32>,
    output: Vec<f32>,
    hold: u32,
}

impl Default for Denoiser {
    fn default() -> Self {
        Self::new()
    }
}

impl Denoiser {
    pub fn new() -> Self {
        Self {
            state: DenoiseState::new(),
            input: vec![0.0; RNN_FRAME],
            output: vec![0.0; RNN_FRAME],
            hold: 0,
        }
    }

    /// Processes `frame` (any multiple of 10 ms) in place and returns the
    /// highest speech probability seen in it (0..1).
    pub fn process(&mut self, frame: &mut [f32], mode: NoiseSuppression) -> f32 {
        if mode == NoiseSuppression::Off {
            return 1.0;
        }
        let mut speech = 0.0f32;
        for chunk in frame.chunks_mut(RNN_FRAME) {
            if chunk.len() != RNN_FRAME {
                // Partial tail: leave it as is (never happens with 20 ms frames).
                continue;
            }
            for (dst, src) in self.input.iter_mut().zip(chunk.iter()) {
                *dst = src * SCALE;
            }
            let probability = self.state.process_frame(&mut self.output, &self.input);
            speech = speech.max(probability);

            let mute = if mode == NoiseSuppression::Strong {
                if probability >= STRONG_VOICE_PROBABILITY {
                    self.hold = STRONG_HOLD;
                    false
                } else if self.hold > 0 {
                    self.hold -= 1;
                    false
                } else {
                    true
                }
            } else {
                false
            };

            for (dst, src) in chunk.iter_mut().zip(self.output.iter()) {
                *dst = if mute {
                    0.0
                } else {
                    (src / SCALE).clamp(-1.0, 1.0)
                };
            }
        }
        speech
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vad::level_db;

    /// Deterministic white-ish noise.
    fn noise(len: usize, amplitude: f32, seed: &mut u32) -> Vec<f32> {
        (0..len)
            .map(|_| {
                *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((*seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0) * amplitude
            })
            .collect()
    }

    #[test]
    fn steady_noise_is_strongly_attenuated() {
        let mut denoiser = Denoiser::new();
        let mut seed = 7;
        let mut before = 0.0;
        let mut after = 0.0;
        // Let the network adapt for one second, then measure one second.
        for index in 0..100 {
            let mut frame = noise(960, 0.05, &mut seed);
            let input_level = level_db(&frame);
            denoiser.process(&mut frame, NoiseSuppression::Standard);
            if index >= 50 {
                before += input_level;
                after += level_db(&frame);
            }
        }
        let reduction = (before - after) / 50.0;
        // Synthetic white noise is the hardest case for RNNoise (it is trained
        // on real noise such as fans and traffic); even so it must remove a
        // clearly audible amount.
        assert!(reduction > 6.0, "only {reduction:.1} dB of noise removed");
    }

    #[test]
    fn off_leaves_the_signal_untouched() {
        let mut denoiser = Denoiser::new();
        let mut seed = 3;
        let original = noise(960, 0.2, &mut seed);
        let mut frame = original.clone();
        denoiser.process(&mut frame, NoiseSuppression::Off);
        assert_eq!(frame, original);
    }

    #[test]
    fn strong_mutes_non_speech() {
        let mut denoiser = Denoiser::new();
        let mut seed = 11;
        let mut last = Vec::new();
        for _ in 0..60 {
            let mut frame = noise(960, 0.05, &mut seed);
            denoiser.process(&mut frame, NoiseSuppression::Strong);
            last = frame;
        }
        assert!(
            last.iter().all(|s| *s == 0.0),
            "noise passed the strong gate"
        );
    }
}
#[cfg(test)]
mod speed {
    #[test]
    fn denoise_is_faster_than_real_time() {
        let mut d = super::Denoiser::new();
        let mut frame = vec![0.01f32; 960];
        let start = std::time::Instant::now();
        for _ in 0..500 {
            d.process(&mut frame, super::NoiseSuppression::Standard);
        }
        let took = start.elapsed();
        println!("10 s of audio denoised in {took:?}");
        assert!(
            took < std::time::Duration::from_secs(3),
            "too slow: {took:?}"
        );
    }
}
