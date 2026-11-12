//! Short notification sounds, synthesized so no third-party audio ships.
//!
//! Join rises, leave falls: the convention people already read from Discord
//! and TeamSpeak. The timbre is our own, a soft glassy mallet (a sine with a
//! quickly fading fourth harmonic), so it does not sound like either.

use crate::codec::SAMPLE_RATE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cue {
    /// Someone joined our channel (or we joined one).
    Join,
    /// Someone left our channel.
    Leave,
}

/// Mono 48 kHz samples of the cue.
pub fn render(cue: Cue) -> Vec<f32> {
    // (frequency Hz, start s, length s, gain)
    let notes: &[(f32, f32, f32, f32)] = match cue {
        // A5 then E6: a bright fifth upwards.
        Cue::Join => &[(880.0, 0.0, 0.16, 0.22), (1318.5, 0.085, 0.26, 0.22)],
        // E5 then B4: a lower, gentler fourth downwards.
        Cue::Leave => &[(659.3, 0.0, 0.16, 0.18), (493.9, 0.095, 0.30, 0.18)],
    };
    let end = notes.iter().map(|n| n.1 + n.2).fold(0.0, f32::max);
    let rate = SAMPLE_RATE as f32;
    let mut out = vec![0.0f32; (end * rate) as usize];
    for &(freq, start, length, gain) in notes {
        let first = (start * rate) as usize;
        for i in 0..(length * rate) as usize {
            let t = i as f32 / rate;
            // 4 ms attack to avoid a click, then an exponential ring-out.
            let attack = (t / 0.004).min(1.0);
            let body = (-t * 9.0 / length).exp();
            let tone = (std::f32::consts::TAU * freq * t).sin()
                + 0.25 * (-t * 40.0).exp() * (std::f32::consts::TAU * freq * 4.0 * t).sin();
            if let Some(sample) = out.get_mut(first + i) {
                *sample += gain * attack * body * tone;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pitch_trend(samples: &[f32]) -> f32 {
        // Zero crossings per half: more crossings means higher pitch.
        let crossings = |s: &[f32]| s.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        let half = samples.len() / 2;
        crossings(&samples[half..]) as f32 - crossings(&samples[..half]) as f32
    }

    #[test]
    fn join_rises_and_leave_falls_without_clipping() {
        let join = render(Cue::Join);
        let leave = render(Cue::Leave);
        assert!(pitch_trend(&join) > 0.0);
        assert!(pitch_trend(&leave) < 0.0);
        for samples in [&join, &leave] {
            assert!(samples.iter().all(|s| s.abs() < 1.0));
            assert!(samples.len() < SAMPLE_RATE as usize / 2, "cues stay short");
        }
    }
}
