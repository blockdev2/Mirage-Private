//! Encodes the primary monitor with every quality preset for a few seconds and
//! prints units per second and the real bitrate. Needs a desktop session and a
//! hardware H.264 encoder; a still screen produces few units (capture only
//! delivers frames when the picture changes), so move something while it runs:
//! `cargo test -p ts6-capture --test quality_presets -- --ignored --nocapture`
//! `PRESET=standard`, `REPEAT=n` (rounds) and `SECS=n` narrow it. Starting and
//! stopping many captures in a row also guards the COM apartment fix in
//! `capture.rs` (the fourth start used to crash).

#![cfg(windows)]

use std::time::{Duration, Instant};
use ts6_capture::{CaptureConfig, Quality, ScreenCapture};

#[test]
#[ignore = "needs a desktop session and a hardware encoder"]
fn every_preset_encodes_on_this_machine() {
    let only = std::env::var("PRESET").ok();
    let repeat: usize = std::env::var("REPEAT")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(1);
    for quality in Quality::ALL
        .into_iter()
        .cycle()
        .take(Quality::ALL.len() * repeat)
    {
        if only.as_deref().is_some_and(|name| name != quality.name()) {
            continue;
        }
        let config = quality.apply(CaptureConfig::default());
        let capture = ScreenCapture::start(config).expect("capture starts");
        let (mut units, mut bytes, mut keys) = (0u32, 0usize, 0u32);
        let started = Instant::now();
        while started.elapsed()
            < Duration::from_secs(
                std::env::var("SECS")
                    .ok()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(4),
            )
        {
            if let Ok(unit) = capture.units().recv_timeout(Duration::from_millis(200)) {
                units += 1;
                bytes += unit.data.len();
                keys += u32::from(unit.keyframe);
            }
        }
        let seconds = started.elapsed().as_secs_f64();
        println!(
            "{quality:?}: {}x{} @{} -> {:.1} units/s, {:.0} kbit/s, {keys} keyframes",
            config.max_width,
            config.max_height,
            config.fps,
            f64::from(units) / seconds,
            bytes as f64 * 8.0 / 1000.0 / seconds,
        );
        assert!(capture.error().is_none(), "{:?}", capture.error());
        assert!(units > 0, "{quality:?} produced nothing");
        capture.stop().expect("clean stop");
    }
}
