//! What to capture and how to encode it, plus the pure size arithmetic.

/// Capture and encoding settings. `Default` is a 720p, 30 FPS, 3 Mbit/s
/// stream, a sensible ceiling for several peer-to-peer viewers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureConfig {
    /// Zero-based monitor index; `None` is the primary monitor.
    pub monitor: Option<usize>,
    pub max_width: u32,
    pub max_height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            monitor: None,
            max_width: 1280,
            max_height: 720,
            fps: 30,
            bitrate_kbps: 3000,
        }
    }
}

impl CaptureConfig {
    /// Rejects values the encoder or the frame budget cannot honor.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(2..=7680).contains(&self.max_width) || !(2..=4320).contains(&self.max_height) {
            return Err("maximum size must be between 2x2 and 7680x4320");
        }
        if !(1..=120).contains(&self.fps) {
            return Err("frame rate must be between 1 and 120");
        }
        if !(100..=100_000).contains(&self.bitrate_kbps) {
            return Err("bitrate must be between 100 and 100000 kbit/s");
        }
        Ok(())
    }
}

/// One encoded access unit in Annex-B form, ready for an RTP packetizer.
#[derive(Clone, PartialEq, Eq)]
pub struct EncodedUnit {
    pub data: Vec<u8>,
    /// An IDR picture: a viewer can start decoding here.
    pub keyframe: bool,
    /// Capture time in 100 ns units since the capture started.
    pub time_100ns: i64,
}

impl std::fmt::Debug for EncodedUnit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncodedUnit")
            .field("bytes", &self.data.len())
            .field("keyframe", &self.keyframe)
            .field("time_100ns", &self.time_100ns)
            .finish()
    }
}

/// The encoded size for a source: fits inside `max`, keeps the aspect ratio,
/// never upscales, and is even in both directions (NV12 needs that).
pub fn fit_size(source: (u32, u32), max: (u32, u32)) -> (u32, u32) {
    let (width, height) = (source.0.max(2), source.1.max(2));
    let (mut w, mut h) = (u64::from(width), u64::from(height));
    let (max_w, max_h) = (u64::from(max.0.max(2)), u64::from(max.1.max(2)));
    if w > max_w || h > max_h {
        // Scale by the tighter side, rounding down.
        if w * max_h > h * max_w {
            h = (h * max_w / w).max(2);
            w = max_w;
        } else {
            w = (w * max_h / h).max(2);
            h = max_h;
        }
    }
    (((w & !1).max(2)) as u32, ((h & !1).max(2)) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_small_source_is_not_upscaled() {
        assert_eq!(fit_size((640, 360), (1280, 720)), (640, 360));
    }

    #[test]
    fn a_large_source_fits_and_keeps_its_aspect_ratio() {
        assert_eq!(fit_size((3840, 2160), (1280, 720)), (1280, 720));
        assert_eq!(fit_size((2560, 1080), (1280, 720)), (1280, 540));
        assert_eq!(fit_size((1080, 1920), (1280, 720)), (404, 720));
    }

    #[test]
    fn odd_sizes_become_even() {
        assert_eq!(fit_size((641, 361), (1280, 720)), (640, 360));
        assert_eq!(fit_size((1, 1), (1280, 720)), (2, 2));
    }

    #[test]
    fn the_default_is_valid_and_bad_values_are_rejected() {
        assert!(CaptureConfig::default().validate().is_ok());
        for bad in [
            CaptureConfig {
                fps: 0,
                ..Default::default()
            },
            CaptureConfig {
                fps: 500,
                ..Default::default()
            },
            CaptureConfig {
                bitrate_kbps: 1,
                ..Default::default()
            },
            CaptureConfig {
                max_width: 1,
                ..Default::default()
            },
        ] {
            assert!(bad.validate().is_err());
        }
    }

    #[test]
    fn debug_shows_sizes_not_picture_bytes() {
        let unit = EncodedUnit {
            data: vec![7; 3],
            keyframe: true,
            time_100ns: 0,
        };
        assert!(!format!("{unit:?}").contains("[7"));
    }
}
