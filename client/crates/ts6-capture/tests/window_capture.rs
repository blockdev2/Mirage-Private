//! Captures one real window through Windows Graphics Capture and the hardware
//! encoder. Needs a desktop session, a visible window and a hardware H.264
//! encoder, so it is ignored by default:
//! `cargo test -p ts6-capture --test window_capture -- --ignored --nocapture`

#![cfg(windows)]

use std::time::Duration;
use ts6_capture::{list_windows, CaptureConfig, CaptureSource, ScreenCapture};

#[test]
#[ignore = "needs a desktop with a visible window and a hardware encoder"]
fn the_largest_window_produces_encoded_units() {
    let windows = list_windows();
    println!("{} candidate windows", windows.len());
    let target = windows
        .iter()
        .max_by_key(|w| u64::from(w.width) * u64::from(w.height))
        .expect("no shareable window on this desktop");
    println!(
        "capturing {:?} ({}) {}x{}",
        target.title, target.process, target.width, target.height
    );

    let capture = ScreenCapture::start(CaptureConfig {
        source: CaptureSource::Window(target.id),
        ..CaptureConfig::default()
    })
    .expect("window capture starts");
    let first = capture
        .units()
        .recv_timeout(Duration::from_secs(10))
        .expect("an encoded unit within 10 s");
    assert!(first.keyframe, "the first unit is a keyframe");
    assert!(!first.data.is_empty());
    println!("first unit: {} bytes", first.data.len());
    assert!(capture.error().is_none(), "{:?}", capture.error());
    capture.stop().expect("clean stop");
}

#[test]
fn a_dead_window_handle_is_refused() {
    // Handle 1 is never a visible top-level window.
    let result = ScreenCapture::start(CaptureConfig {
        source: CaptureSource::Window(1),
        ..CaptureConfig::default()
    });
    assert!(result.is_err());
}
