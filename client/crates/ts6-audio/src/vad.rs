//! Voice activation: decides, frame by frame, whether the microphone is
//! transmitting, the way TeamSpeak's "voice activation detection" does.
//!
//! * A frame louder than the threshold (dBFS) opens the gate.
//! * The gate stays open for a hold time after the last loud frame, so pauses
//!   between words do not chop speech.
//! * When it opens, the frame captured just before is sent first (pre-roll), so
//!   the first syllable is not cut.
//! * When it closes, an empty packet is sent: that is how TeamSpeak marks the
//!   end of a voice session, and what makes other clients drop the "speaking"
//!   indicator at once.

/// Default threshold, close to TeamSpeak's default voice activation level.
pub const DEFAULT_THRESHOLD_DB: f32 = -42.0;
/// 300 ms of hold at 20 ms frames.
pub const HOLD_FRAMES: u32 = 15;
/// Floor reported for silence (digital zero has no finite level).
pub const SILENCE_DB: f32 = -96.0;

/// RMS level of a frame in dBFS (0 dB = full scale).
pub fn level_db(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return SILENCE_DB;
    }
    let energy: f64 = samples.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
    let rms = (energy / samples.len() as f64).sqrt();
    if rms <= 0.0 {
        return SILENCE_DB;
    }
    (20.0 * rms.log10()).max(f64::from(SILENCE_DB)) as f32
}

/// What to do with the frame that was just encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Gate closed: send nothing.
    Silent,
    /// Gate just opened: send the previous frame, then this one.
    Start,
    /// Gate open: send this frame.
    Send,
    /// Gate just closed: send the end-of-voice marker (an empty packet).
    End,
}

#[derive(Debug, Clone)]
pub struct VoiceActivation {
    threshold_db: f32,
    hold: u32,
    open: bool,
}

impl VoiceActivation {
    pub fn new(threshold_db: f32) -> Self {
        Self {
            threshold_db,
            hold: 0,
            open: false,
        }
    }

    pub fn set_threshold_db(&mut self, threshold_db: f32) {
        self.threshold_db = threshold_db;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Feed the level of the next frame.
    pub fn next(&mut self, level_db: f32) -> Decision {
        if level_db >= self.threshold_db {
            self.hold = HOLD_FRAMES;
            if self.open {
                Decision::Send
            } else {
                self.open = true;
                Decision::Start
            }
        } else if self.open && self.hold > 0 {
            self.hold -= 1;
            Decision::Send
        } else if self.open {
            self.open = false;
            Decision::End
        } else {
            Decision::Silent
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_are_reported_in_dbfs() {
        assert_eq!(level_db(&[0.0; 960]), SILENCE_DB);
        let full = level_db(&[1.0; 960]);
        assert!(full.abs() < 0.01, "{full}");
        let tenth = level_db(&[0.1; 960]);
        assert!((tenth + 20.0).abs() < 0.01, "{tenth}");
    }

    #[test]
    fn speech_opens_with_preroll_holds_through_pauses_and_ends_with_a_marker() {
        let mut vad = VoiceActivation::new(-40.0);
        assert_eq!(vad.next(-60.0), Decision::Silent);
        assert_eq!(vad.next(-20.0), Decision::Start);
        assert_eq!(vad.next(-20.0), Decision::Send);
        // A short pause inside a sentence keeps transmitting.
        for _ in 0..HOLD_FRAMES {
            assert_eq!(vad.next(-70.0), Decision::Send);
        }
        // Then the gate closes exactly once, with the end marker.
        assert_eq!(vad.next(-70.0), Decision::End);
        assert!(!vad.is_open());
        assert_eq!(vad.next(-70.0), Decision::Silent);
    }

    #[test]
    fn background_noise_below_the_threshold_never_transmits() {
        let mut vad = VoiceActivation::new(DEFAULT_THRESHOLD_DB);
        for _ in 0..500 {
            assert_eq!(vad.next(-55.0), Decision::Silent);
        }
    }
}
