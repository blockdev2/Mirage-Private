//! End to end on this machine: the primary monitor is captured, encoded by a
//! hardware H.264 encoder, and sent through `ts6-stream` to a loopback viewer.
//!
//! Ignored by default: it needs an interactive desktop, a GPU encoder and some
//! motion on screen (Windows Graphics Capture only reports changes). It prints
//! counts only and keeps no picture.
//!
//! `cargo test -p ts6-capture --test screen_to_viewer -- --ignored --nocapture`

#![cfg(windows)]

use std::net::{IpAddr, Ipv4Addr};
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::Receiver;
use ts6_capture::nal::analyze;
use ts6_capture::{CaptureConfig, ScreenCapture};
use ts6_stream::{Publisher, Signal, Viewer};

const LOCALHOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

async fn drain(from: &mut Receiver<Signal>) -> Vec<String> {
    let mut wire = Vec::new();
    while let Ok(Some(signal)) = tokio::time::timeout(Duration::from_millis(10), from.recv()).await
    {
        wire.push(signal.to_json());
    }
    wire
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs an interactive desktop, a hardware H.264 encoder and screen motion"]
async fn the_primary_monitor_reaches_a_loopback_viewer() {
    let mut publisher = Publisher::new(LOCALHOST).await.expect("publisher");
    let mut viewer = Viewer::new(LOCALHOST).await.expect("viewer");
    let offer = publisher.offer().await.expect("offer");
    let answer = viewer.accept_offer(&offer).await.expect("answer");
    publisher
        .apply(Signal::Answer(answer))
        .await
        .expect("answer applies");

    // Connect first: the stream starts with a keyframe the viewer must not miss.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !publisher.is_connected() {
        assert!(Instant::now() < deadline, "peers did not connect");
        for json in drain(publisher.signals()).await {
            viewer.apply(Signal::parse(&json).unwrap()).await.unwrap();
        }
        for json in drain(viewer.signals()).await {
            publisher
                .apply(Signal::parse(&json).unwrap())
                .await
                .unwrap();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let capture = ScreenCapture::start(CaptureConfig::default()).expect("capture starts");
    let (mut units, mut keyframes, mut bytes) = (0u32, 0u32, 0usize);
    let mut first_startable = None;
    let mut profile_idc = None;
    let deadline = Instant::now() + Duration::from_secs(12);
    while Instant::now() < deadline && viewer.packets() < 30 {
        match capture.units().recv_timeout(Duration::from_millis(100)) {
            Ok(unit) => {
                let info = analyze(&unit.data);
                if first_startable.is_none() {
                    first_startable = Some(unit.keyframe && info.parameter_sets);
                    // SPS (NAL 7): the byte after the header is profile_idc.
                    profile_idc = unit
                        .data
                        .windows(5)
                        .find(|w| w[..3] == [0, 0, 1] && w[3] & 0x1f == 7)
                        .map(|w| w[4]);
                }
                units += 1;
                keyframes += u32::from(unit.keyframe);
                bytes += unit.data.len();
                publisher
                    .send_sample(unit.data, Duration::from_millis(33))
                    .await
                    .expect("send unit");
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        for json in drain(publisher.signals()).await {
            viewer.apply(Signal::parse(&json).unwrap()).await.unwrap();
        }
        for json in drain(viewer.signals()).await {
            publisher
                .apply(Signal::parse(&json).unwrap())
                .await
                .unwrap();
        }
    }
    let failure = capture.error();
    capture.stop().expect("capture stops");
    eprintln!(
        "units={units} keyframes={keyframes} bytes={bytes} profile_idc={profile_idc:?} viewer_packets={} failure={failure:?}",
        viewer.packets()
    );

    assert!(failure.is_none(), "capture failed: {failure:?}");
    assert!(units > 0, "no frame arrived: move something on screen");
    assert_eq!(
        first_startable,
        Some(true),
        "the first unit must be a keyframe with parameter sets"
    );
    assert!(
        viewer.packets() >= 30,
        "viewer saw {} packets",
        viewer.packets()
    );
    publisher.close().await;
    viewer.close().await;
}
