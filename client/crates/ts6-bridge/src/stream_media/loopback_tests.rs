//! A webrtc-rs publisher shaped like the official TS6 offer (H.264 video plus
//! an Opus audio m-line) streams a green fixture to the worker on loopback.

use super::decode::test_support::encode_fixture;
use super::frame::frames;
use super::*;
use rtc::interceptor::Registry;
use rtc::media::Sample;
use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::configuration::interceptor_registry::register_default_interceptors;
use rtc::peer_connection::configuration::media_engine::{
    MediaEngine, MIME_TYPE_H264, MIME_TYPE_OPUS,
};
use rtc::peer_connection::configuration::RTCConfigurationBuilder;
use rtc::peer_connection::event::RTCPeerConnectionIceEvent;
use rtc::peer_connection::sdp::RTCSessionDescription;
use rtc::peer_connection::transport::RTCIceCandidateInit;
use rtc::rtp_transceiver::rtp_sender::{
    RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters, RTCRtpEncodingParameters,
    RtpCodecKind,
};
use std::sync::Arc;
use std::time::{Duration, Instant};
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::media_stream::track_local::TrackLocal;
use webrtc::media_stream::Track;
use webrtc::peer_connection::{PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler};

/// The frame slot is process-wide: these tests take turns.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Publisher {
    candidates: tokio::sync::mpsc::UnboundedSender<String>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Publisher {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        if let Ok(init) = event.candidate.to_json() {
            let json = serde_json::json!({"cmd": "iceCandidate", "args": {
                "sdp": init.candidate, "mid": init.sdp_mid, "mLine": init.sdp_mline_index}});
            let _ = self.candidates.send(json.to_string());
        }
    }
}

fn codec(mime: &str, clock: u32, channels: u16, fmtp: &str, pt: u8) -> RTCRtpCodecParameters {
    RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: mime.to_owned(),
            clock_rate: clock,
            channels,
            sdp_fmtp_line: fmtp.to_owned(),
            rtcp_feedback: vec![],
        },
        payload_type: pt,
    }
}

fn track(
    kind: RtpCodecKind,
    codec: &RTCRtpCodecParameters,
    ssrc: u32,
) -> Arc<TrackLocalStaticSample> {
    Arc::new(
        TrackLocalStaticSample::new(
            Instant::now(),
            MediaStreamTrack::new(
                format!("stream-{kind}"),
                format!("track-{kind}"),
                format!("label-{kind}"),
                kind,
                vec![RTCRtpEncodingParameters {
                    rtp_coding_parameters: RTCRtpCodingParameters {
                        ssrc: Some(ssrc),
                        ..Default::default()
                    },
                    codec: codec.rtp_codec.clone(),
                    ..Default::default()
                }],
            ),
        )
        .unwrap(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watches_h264_from_a_webrtc_rs_publisher() {
    let _turn = SERIAL.lock().await;
    let units = encode_fixture(640, 360, [80, 180, 20], 60);

    let video = codec(
        MIME_TYPE_H264,
        90_000,
        0,
        "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f",
        102,
    );
    let audio = codec(MIME_TYPE_OPUS, 48_000, 2, "minptime=10;useinbandfec=1", 111);
    let mut engine = MediaEngine::default();
    engine
        .register_codec(video.clone(), RtpCodecKind::Video)
        .unwrap();
    engine
        .register_codec(audio.clone(), RtpCodecKind::Audio)
        .unwrap();
    let registry = register_default_interceptors(Registry::new(), &mut engine).unwrap();
    let (candidate_tx, mut candidate_rx) = tokio::sync::mpsc::unbounded_channel();
    let publisher = PeerConnectionBuilder::new()
        .with_configuration(RTCConfigurationBuilder::new().build())
        .with_media_engine(engine)
        .with_interceptor_registry(registry)
        .with_handler(Arc::new(Publisher {
            candidates: candidate_tx,
        }) as Arc<dyn PeerConnectionEventHandler>)
        .with_runtime(webrtc::runtime::default_runtime().unwrap())
        .with_udp_addrs(vec!["0.0.0.0:0".to_string()])
        .build()
        .await
        .unwrap();
    let video_track = track(RtpCodecKind::Video, &video, 0x4d49_0001);
    let video_sender = publisher
        .add_track(Arc::clone(&video_track) as Arc<dyn TrackLocal>)
        .await
        .unwrap();
    let audio_track = track(RtpCodecKind::Audio, &audio, 0x4d49_0002);
    publisher
        .add_track(Arc::clone(&audio_track) as Arc<dyn TrackLocal>)
        .await
        .unwrap();
    let offer = publisher.create_offer(None).await.unwrap();
    publisher.set_local_description(offer).await.unwrap();
    let offer = publisher.local_description().await.unwrap().sdp;

    let (event_tx, mut events) = tokio::sync::mpsc::channel(64);
    let worker = StreamWorker::spawn(event_tx).unwrap();
    assert!(worker
        .send(WorkerCommand::Offer {
            stream: "s".into(),
            sdp: offer,
        })
        .is_ok());

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut answered = false;
    let mut sending = None;
    let mut playing = false;
    while !(playing && frames().latest(0).is_some()) {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => panic!("no decoded frame in 20 s (answered={answered}, playing={playing})"),
            Some(json) = candidate_rx.recv() => {
                assert!(worker.send(WorkerCommand::RemoteIce { stream: "s".into(), json }).is_ok());
            }
            Some(event) = events.recv() => match event {
                WorkerEvent::Answer { sdp, .. } => {
                    // Accepted like a browser would (the packets are ignored):
                    // the official client discards answers that refuse it.
                    assert!(sdp.lines().any(|l| l.starts_with("m=audio 9 ")), "audio m-line must be accepted
        {sdp}");
                    publisher.set_remote_description(RTCSessionDescription::answer(sdp).unwrap()).await.unwrap();
                    answered = true;
                    let track = Arc::clone(&video_track);
                    let sender = Arc::clone(&video_sender);
                    let units = units.clone();
                    sending = Some(tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        let pt = sender.get_parameters().await.unwrap().rtp_parameters.codecs[0].payload_type;
                        let ssrc = track.ssrcs().await[0];
                        for unit in units {
                            let sample = Sample { data: unit.into(), duration: Duration::from_millis(50), ..Sample::new(Instant::now()) };
                            let _ = track.sample_writer(ssrc, pt).write_sample(&sample).await;
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                    }));
                }
                WorkerEvent::LocalIce { json, .. } => {
                    // libwebrtc drops candidates that arrive before the answer.
                    assert!(answered, "local candidate sent before the answer");
                    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
                    assert_eq!(value["cmd"], "iceCandidate");
                    let args = &value["args"];
                    publisher.add_ice_candidate(RTCIceCandidateInit {
                        candidate: args["sdp"].as_str().unwrap().to_owned(),
                        sdp_mid: args["mid"].as_str().map(str::to_owned),
                        sdp_mline_index: args["mLine"].as_u64().map(|i| i as u16),
                        ..Default::default()
                    }).await.unwrap();
                }
                WorkerEvent::Playing { .. } => playing = true,
                WorkerEvent::Failed { reason, .. } => panic!("worker failed: {reason:?}"),
            },
        }
    }
    let (sequence, frame) = frames().latest(0).unwrap();
    let at = ((frame.height / 2 * frame.width + frame.width / 2) * 4) as usize;
    let [r, g, b] = [frame.rgba[at], frame.rgba[at + 1], frame.rgba[at + 2]];
    assert!(
        r.abs_diff(20) <= 20 && g.abs_diff(180) <= 20 && b.abs_diff(80) <= 20,
        "{r},{g},{b}"
    );

    // Closing stops frames for good, even while the publisher still sends.
    assert!(worker.send(WorkerCommand::Close).is_ok());
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(frames().latest(0).is_none());
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(frames().latest(sequence).is_none());
    if let Some(task) = sending {
        task.abort();
    }
    let _ = publisher.close().await;
    drop(worker);
}

#[tokio::test]
async fn refuses_offer_without_h264() {
    let _turn = SERIAL.lock().await;
    let (event_tx, mut events) = tokio::sync::mpsc::channel(8);
    let worker = StreamWorker::spawn(event_tx).unwrap();
    let vp8 = "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\na=rtpmap:96 VP8/90000\r\n";
    assert!(worker
        .send(WorkerCommand::Offer {
            stream: "s".into(),
            sdp: vp8.into(),
        })
        .is_ok());
    let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("an answer in time")
        .expect("an event");
    assert!(matches!(
        event,
        WorkerEvent::Failed {
            reason: FailReason::Unsupported,
            ..
        }
    ));
}

/// The official TS6 offer as captured by the spike (2026-11-13), with its own
/// header-extension IDs; credentials, fingerprint and addresses are fakes.
const OFFICIAL_OFFER: &str = "v=0\r
o=- 1 2 IN IP4 127.0.0.1\r
s=-\r
t=0 0\r
a=group:BUNDLE 0 1\r
a=extmap-allow-mixed\r
a=msid-semantic: WMS\r
m=video 9 UDP/TLS/RTP/SAVPF 39 102 104 96 97 103\r
c=IN IP4 0.0.0.0\r
a=rtcp:9 IN IP4 0.0.0.0\r
a=ice-ufrag:abcd\r
a=ice-pwd:abcdefghijklmnopqrstuvwx\r
a=ice-options:trickle\r
a=fingerprint:sha-256 11:22:33:44:55:66:77:88:99:00:AA:BB:CC:DD:EE:FF:11:22:33:44:55:66:77:88:99:00:AA:BB:CC:DD:EE:FF\r
a=setup:actpass\r
a=mid:0\r
a=extmap:3 urn:3gpp:video-orientation\r
a=extmap:4 http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01\r
a=extmap:6 http://www.webrtc.org/experiments/rtp-hdrext/video-content-type\r
a=extmap:8 http://www.webrtc.org/experiments/rtp-hdrext/color-space\r
a=extmap:9 urn:ietf:params:rtp-hdrext:sdes:mid\r
a=sendonly\r
a=msid:- v\r
a=rtcp-mux\r
a=rtcp-rsize\r
a=rtpmap:39 AV1/90000\r
a=rtcp-fb:39 nack\r
a=rtcp-fb:39 nack pli\r
a=fmtp:39 level-idx=5;profile=0;tier=0\r
a=rtpmap:102 H264/90000\r
a=rtcp-fb:102 goog-remb\r
a=rtcp-fb:102 transport-cc\r
a=rtcp-fb:102 ccm fir\r
a=rtcp-fb:102 nack\r
a=rtcp-fb:102 nack pli\r
a=fmtp:102 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=640c1f\r
a=rtpmap:104 H264/90000\r
a=fmtp:104 level-asymmetry-allowed=1;packetization-mode=0;profile-level-id=640c1f\r
a=rtpmap:96 VP8/90000\r
a=rtpmap:97 rtx/90000\r
a=fmtp:97 apt=96\r
a=rtpmap:103 rtx/90000\r
a=fmtp:103 apt=102\r
a=ssrc-group:FID 1111 2222\r
a=ssrc:1111 cname:x\r
a=ssrc:2222 cname:x\r
m=audio 9 UDP/TLS/RTP/SAVPF 111\r
c=IN IP4 0.0.0.0\r
a=rtcp:9 IN IP4 0.0.0.0\r
a=ice-ufrag:abcd\r
a=ice-pwd:abcdefghijklmnopqrstuvwx\r
a=ice-options:trickle\r
a=fingerprint:sha-256 11:22:33:44:55:66:77:88:99:00:AA:BB:CC:DD:EE:FF:11:22:33:44:55:66:77:88:99:00:AA:BB:CC:DD:EE:FF\r
a=setup:actpass\r
a=mid:1\r
a=extmap:4 http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01\r
a=extmap:9 urn:ietf:params:rtp-hdrext:sdes:mid\r
a=sendonly\r
a=msid:- a\r
a=rtcp-mux\r
a=rtpmap:111 opus/48000/2\r
a=fmtp:111 minptime=10;useinbandfec=1\r
a=ssrc:3333 cname:x\r
";

/// libwebrtc rejects an answer whose header extensions were not offered or
/// carry another ID than the offer gave them.
#[tokio::test]
async fn answer_keeps_the_official_offers_extension_ids() {
    let _turn = SERIAL.lock().await;
    let (event_tx, mut events) = tokio::sync::mpsc::channel(64);
    let worker = StreamWorker::spawn(event_tx).unwrap();
    assert!(worker
        .send(WorkerCommand::Offer {
            stream: "s".into(),
            sdp: OFFICIAL_OFFER.into(),
        })
        .is_ok());
    let answer = loop {
        match tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("an answer in time")
            .expect("an event")
        {
            WorkerEvent::Answer { sdp, .. } => break sdp,
            WorkerEvent::Failed { reason, .. } => panic!("worker failed: {reason:?}"),
            _ => {}
        }
    };
    let offered: Vec<&str> = OFFICIAL_OFFER
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("a=extmap:"))
        .collect();
    for line in answer
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("a=extmap:"))
    {
        assert!(
            offered.contains(&line),
            "answer extension not offered as-is: {line}\n{answer}"
        );
    }
    assert!(answer.contains("a=group:BUNDLE 0 1"), "{answer}");
    assert!(answer.contains("a=rtpmap:111 opus/48000/2"), "{answer}");
    let video = answer.split("m=audio").next().unwrap();
    assert!(video.contains("a=rtpmap:102 H264/90000"), "{answer}");
    assert!(answer.contains("a=setup:active") || answer.contains("a=setup:passive"));
}

/// Diagnostic, not a regression test: answers the offer in the file named by
/// `MIRAGE_ORACLE_OFFER` exactly like the app's viewer does, writes the answer
/// to `MIRAGE_ORACLE_OUT` and the local candidates (one JSON per line, with the
/// milliseconds since the answer) after it. A real libwebrtc (a browser) can then
/// apply them, which shows whether the official client could.
#[tokio::test]
#[ignore = "diagnostic: needs MIRAGE_ORACLE_OFFER and MIRAGE_ORACLE_OUT"]
async fn oracle_answer_for_a_given_offer() {
    let _turn = SERIAL.lock().await;
    let offer = std::fs::read_to_string(std::env::var("MIRAGE_ORACLE_OFFER").unwrap()).unwrap();
    let (event_tx, mut events) = tokio::sync::mpsc::channel(64);
    let worker = StreamWorker::spawn(event_tx).unwrap();
    assert!(worker
        .send(WorkerCommand::Offer {
            stream: "s".into(),
            sdp: offer,
        })
        .is_ok());
    let mut out = String::new();
    let mut answered_at = None;
    let end = tokio::time::Instant::now() + Duration::from_secs(8);
    while let Ok(Some(event)) = tokio::time::timeout_at(end, events.recv()).await {
        match event {
            WorkerEvent::Answer { sdp, .. } => {
                answered_at = Some(Instant::now());
                out.push_str(&sdp);
                out.push_str(
                    "
=====
",
                );
            }
            WorkerEvent::LocalIce { json, .. } => {
                let ms = answered_at.map_or(0, |t| t.elapsed().as_millis());
                out.push_str(&format!(
                    "{ms} {json}
"
                ));
            }
            WorkerEvent::Failed { reason, .. } => out.push_str(&format!(
                "FAILED {reason:?}
"
            )),
            WorkerEvent::Playing { .. } => {}
        }
    }
    std::fs::write(std::env::var("MIRAGE_ORACLE_OUT").unwrap(), out).unwrap();
}

/// Diagnostic, not a regression test: the app's viewer as a child process of a
/// script. Reads JSON lines on stdin (`{"op":"offer","sdp":..}`,
/// `{"op":"ice","json":..}`, `{"op":"quit"}`) and prints events as JSON lines
/// (`answer`, `ice`, `playing`, `failed`). RUST_LOG=ice=debug traces go to stderr.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "diagnostic: driven by a script over stdin/stdout"]
async fn oracle_interactive_viewer() {
    use std::io::{BufRead, Write};
    let _turn = SERIAL.lock().await;
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .try_init();
    let (event_tx, mut events) = tokio::sync::mpsc::channel(256);
    let worker = StreamWorker::spawn(event_tx).unwrap();
    let (line_tx, mut lines) = tokio::sync::mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if line_tx.send(line).is_err() {
                break;
            }
        }
    });
    let emit = |value: serde_json::Value| {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "@@{value}");
        let _ = out.flush();
    };
    let end = tokio::time::Instant::now() + Duration::from_secs(120);
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(end) => break,
            Some(line) = lines.recv() => {
                let Ok(command) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
                match command["op"].as_str() {
                    Some("offer") => {
                        let sdp = command["sdp"].as_str().unwrap_or_default().to_owned();
                        let _ = worker.send(WorkerCommand::Offer { stream: "s".into(), sdp });
                    }
                    Some("ice") => {
                        let json = command["json"].as_str().unwrap_or_default().to_owned();
                        let _ = worker.send(WorkerCommand::RemoteIce { stream: "s".into(), json });
                    }
                    Some("quit") => break,
                    _ => {}
                }
            }
            Some(event) = events.recv() => match event {
                WorkerEvent::Answer { sdp, .. } => emit(serde_json::json!({"ev": "answer", "sdp": sdp})),
                WorkerEvent::LocalIce { json, .. } => emit(serde_json::json!({"ev": "ice", "json": json})),
                WorkerEvent::Playing { .. } => emit(serde_json::json!({"ev": "playing"})),
                WorkerEvent::Failed { reason, .. } => emit(serde_json::json!({"ev": "failed", "reason": format!("{reason:?}")})),
            },
        }
    }
}
