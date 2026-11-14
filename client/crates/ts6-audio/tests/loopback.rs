//! Process loopback on a real machine: plays a tone from another process
//! (PowerShell) and checks it reaches the system audio queue, while this
//! process is excluded. Needs a Windows build with process loopback and an
//! active output device, so it is ignored by default:
//! `cargo test -p ts6-audio --test loopback -- --ignored --nocapture`

#![cfg(windows)]

use std::process::Command;
use std::time::{Duration, Instant};
use ts6_audio::loopback::Loopback;
use ts6_audio::system_audio::system_audio;

/// A 3 s, 440 Hz, 16-bit mono WAV.
fn tone_wav() -> Vec<u8> {
    let rate = 44_100u32;
    let samples: Vec<i16> = (0..rate * 3)
        .map(|i| ((i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 12_000.0) as i16)
        .collect();
    let data_len = (samples.len() * 2) as u32;
    let mut wav = Vec::new();
    wav.extend(b"RIFF");
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

/// Plays the tone from a new PowerShell and returns it.
fn play(path: &std::path::Path) -> std::process::Child {
    Command::new("powershell")
        .args([
            "-NoProfile",
            "-STA",
            "-Command",
            &format!(
                "(New-Object Media.SoundPlayer '{}').PlaySync()",
                path.display()
            ),
        ])
        .spawn()
        .expect("powershell starts")
}

/// Highest sample the queue yields over four seconds.
fn listen() -> f32 {
    let started = Instant::now();
    let mut peak = 0.0f32;
    let mut frame = [0.0f32; 960];
    while started.elapsed() < Duration::from_secs(4) {
        system_audio().take(&mut frame);
        peak = peak.max(frame.iter().fold(0.0, |m, s| m.max(s.abs())));
        std::thread::sleep(Duration::from_millis(20));
    }
    peak
}

/// The exclusion is per process tree: sound from an unrelated process is
/// captured, sound from the excluded process is not. (A child of the excluded
/// process counts as part of its tree, which is why the helper is a sibling.)
#[test]
#[ignore = "needs Windows process loopback and an output device"]
fn excluding_a_process_keeps_only_the_others() {
    let path = std::env::temp_dir().join("mirage-loopback-test.wav");
    std::fs::write(&path, tone_wav()).unwrap();

    // Excluded: the player itself.
    let mut excluded = play(&path);
    let capture = Loopback::start(excluded.id()).expect("process loopback starts");
    let muted = listen();
    let _ = excluded.kill();
    let _ = excluded.wait();
    drop(capture);
    println!("excluded player peak {muted:.3}");

    // Not excluded: a stand-in for Mirage, while the player plays elsewhere.
    let mut stand_in = Command::new("ping")
        .args(["-n", "8", "127.0.0.1"])
        .spawn()
        .unwrap();
    let capture = Loopback::start(stand_in.id()).expect("process loopback starts");
    let mut other = play(&path);
    let heard = listen();
    let _ = other.kill();
    let _ = other.wait();
    let _ = stand_in.kill();
    let _ = stand_in.wait();
    assert!(system_audio().is_on());
    drop(capture);
    assert!(!system_audio().is_on(), "stopping turns the queue off");
    let _ = std::fs::remove_file(&path);
    println!("other player peak {heard:.3}");
    assert!(
        muted < 0.02,
        "the excluded process was captured (peak {muted})"
    );
    assert!(
        heard > 0.05,
        "an unrelated process was not captured (peak {heard})"
    );
}

/// Control: ordinary device loopback hears the same tone, so a silent process
/// loopback is the process filter and not a silent machine.
#[test]
#[ignore = "needs an output device"]
fn control_device_loopback_hears_the_tone() {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let path = std::env::temp_dir().join("mirage-loopback-control.wav");
    std::fs::write(&path, tone_wav()).unwrap();
    let device = cpal::default_host()
        .default_output_device()
        .expect("an output device");
    let config = device.default_output_config().expect("output config");
    let peak = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let seen = std::sync::Arc::clone(&peak);
    let stream = device
        .build_input_stream(
            config.config(),
            move |data: &[f32], _| {
                let max = data.iter().fold(0.0f32, |m, s| m.max(s.abs()));
                seen.fetch_max(max.to_bits(), std::sync::atomic::Ordering::Relaxed);
            },
            |error| eprintln!("stream error: {error}"),
            None,
        )
        .expect("loopback stream");
    stream.play().unwrap();
    let mut player = Command::new("powershell")
        .args([
            "-NoProfile",
            "-STA",
            "-Command",
            &format!(
                "(New-Object Media.SoundPlayer '{}').PlaySync()",
                path.display()
            ),
        ])
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_secs(4));
    let _ = player.kill();
    let _ = player.wait();
    drop(stream);
    let _ = std::fs::remove_file(&path);
    let peak = f32::from_bits(peak.load(std::sync::atomic::Ordering::Relaxed));
    println!("control peak {peak:.3}");
    assert!(
        peak > 0.05,
        "device loopback did not hear it either (peak {peak})"
    );
}

/// Which mode hears what: including the player's own tree must hear it.
#[test]
#[ignore = "needs Windows process loopback and an output device"]
fn including_the_player_hears_the_tone() {
    let path = std::env::temp_dir().join("mirage-loopback-include.wav");
    std::fs::write(&path, tone_wav()).unwrap();
    let mut player = Command::new("powershell")
        .args([
            "-NoProfile",
            "-STA",
            "-Command",
            &format!(
                "(New-Object Media.SoundPlayer '{}').PlaySync()",
                path.display()
            ),
        ])
        .spawn()
        .unwrap();
    let capture = Loopback::start_only(player.id()).expect("include mode starts");
    let started = Instant::now();
    let mut peak = 0.0f32;
    let mut frame = [0.0f32; 960];
    while started.elapsed() < Duration::from_secs(4) {
        system_audio().take(&mut frame);
        peak = peak.max(frame.iter().fold(0.0, |m, s| m.max(s.abs())));
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = player.kill();
    let _ = player.wait();
    drop(capture);
    let _ = std::fs::remove_file(&path);
    println!("include peak {peak:.3}");
    assert!(
        peak > 0.05,
        "include mode did not hear the player (peak {peak})"
    );
}
