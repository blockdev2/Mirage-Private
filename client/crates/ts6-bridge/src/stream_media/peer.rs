//! The worker's WebRTC side: one webrtc-rs peer per watched stream, H.264
//! only, decoding on a thread of its own so network work never waits on it.

use super::decode::H264Decoder;
use super::frame::frames;
use super::offer::check_offer;
use super::{FailReason, WorkerCommand, WorkerEvent};
use rtc::interceptor::Registry;
use rtc::media::io::sample_builder::SampleBuilder;
use rtc::peer_connection::configuration::interceptor_registry::register_default_interceptors;
use rtc::peer_connection::configuration::media_engine::{
    MediaEngine, MIME_TYPE_H264, MIME_TYPE_OPUS,
};
use rtc::peer_connection::event::RTCPeerConnectionIceEvent;
use rtc::peer_connection::sdp::RTCSessionDescription;
use rtc::peer_connection::transport::RTCIceCandidateInit;
use rtc::rtcp::payload_feedbacks::full_intra_request::{FirEntry, FullIntraRequest};
use rtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;
use rtc::rtp::codec::h264::H264Packet;
use rtc::rtp_transceiver::rtp_sender::{
    RTCPFeedback, RTCRtpCodec, RTCRtpCodecParameters, RtpCodecKind,
};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{Receiver, Sender, UnboundedSender};
use webrtc::media_stream::track_remote::{TrackRemote, TrackRemoteEvent};
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCPeerConnectionState,
};

/// Remote candidates accepted per stream.
pub(super) const MAX_REMOTE_CANDIDATES: usize = 64;
/// Largest ICE JSON accepted from a publisher.
const MAX_ICE_JSON: usize = 2 * 1024;
/// First picture deadline after the answer is sent.
const FIRST_FRAME: Duration = Duration::from_secs(20);
/// Access units waiting for the decoder; more means the decoder is behind.
const DECODE_QUEUE: usize = 30;

/// What the peer's callbacks and the decoder report back to the loop.
enum Internal {
    Playing(u64),
    Failed(u64, FailReason),
}

struct Active {
    stream: String,
    generation: u64,
    peer: Arc<dyn PeerConnection>,
    remote_candidates: usize,
    first_frame_by: Option<tokio::time::Instant>,
}

pub(super) async fn run(mut commands: Receiver<WorkerCommand>, events: Sender<WorkerEvent>) {
    let (internal_tx, mut internal) = tokio::sync::mpsc::unbounded_channel();
    let mut active: Option<Active> = None;
    loop {
        let deadline = active.as_ref().and_then(|a| a.first_frame_by);
        tokio::select! {
            command = commands.recv() => match command {
                None => break,
                Some(WorkerCommand::Offer { stream, sdp }) => {
                    close(&mut active).await;
                    match start(&stream, &sdp, &events, &internal_tx).await {
                        Ok(started) => active = Some(started),
                        Err(reason) => fail(&events, stream, reason),
                    }
                }
                Some(WorkerCommand::RemoteIce { stream, json }) => {
                    if let Some(current) = active.as_mut().filter(|a| a.stream == stream) {
                        if current.remote_candidates < MAX_REMOTE_CANDIDATES {
                            current.remote_candidates += 1;
                            if let Some(init) = parse_candidate(&json) {
                                let _ = current.peer.add_ice_candidate(init).await;
                            }
                        }
                    }
                }
                Some(WorkerCommand::Close) => close(&mut active).await,
            },
            Some(notice) = internal.recv() => {
                let Some(current) = active.as_mut() else { continue };
                match notice {
                    Internal::Playing(generation) if generation == current.generation => {
                        current.first_frame_by = None;
                        let _ = events.try_send(WorkerEvent::Playing { stream: current.stream.clone() });
                    }
                    Internal::Failed(generation, reason) if generation == current.generation => {
                        let stream = current.stream.clone();
                        close(&mut active).await;
                        fail(&events, stream, reason);
                    }
                    _ => {}
                }
            }
            _ = sleep_until(deadline), if deadline.is_some() => {
                if let Some(current) = active.as_ref() {
                    let stream = current.stream.clone();
                    close(&mut active).await;
                    fail(&events, stream, FailReason::Timeout);
                }
            }
        }
    }
    close(&mut active).await;
}

async fn sleep_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

fn fail(events: &Sender<WorkerEvent>, stream: String, reason: FailReason) {
    tracing::debug!(?reason, "stream viewer failed");
    let _ = events.try_send(WorkerEvent::Failed { stream, reason });
}

async fn close(active: &mut Option<Active>) {
    if let Some(current) = active.take() {
        frames().close();
        ts6_audio::stream_audio::stream_audio().clear();
        let _ = current.peer.close().await;
    }
}

/// `{"cmd":"iceCandidate","args":{"sdp","mid","mLine"}}` as relayed by TS6.
fn parse_candidate(json: &str) -> Option<RTCIceCandidateInit> {
    if json.len() > MAX_ICE_JSON {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    if value.get("cmd")?.as_str()? != "iceCandidate" {
        return None;
    }
    let args = value.get("args")?;
    Some(RTCIceCandidateInit {
        candidate: args.get("sdp")?.as_str()?.to_owned(),
        sdp_mid: args.get("mid").and_then(|m| m.as_str()).map(str::to_owned),
        sdp_mline_index: args
            .get("mLine")
            .and_then(|i| i.as_u64())
            .and_then(|i| u16::try_from(i).ok()),
        ..Default::default()
    })
}

fn local_candidate_json(init: &RTCIceCandidateInit) -> String {
    let mid = ts6_stream::candidate_mid(init.sdp_mid.clone(), init.sdp_mline_index);
    serde_json::json!({"cmd": "iceCandidate", "args": {
        "sdp": init.candidate, "mid": mid, "mLine": init.sdp_mline_index}})
    .to_string()
}

fn h264(profile: &str, payload_type: u8) -> RTCRtpCodecParameters {
    let feedback = |typ: &str, parameter: &str| RTCPFeedback {
        typ: typ.into(),
        parameter: parameter.into(),
    };
    RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: MIME_TYPE_H264.to_owned(),
            clock_rate: 90_000,
            channels: 0,
            sdp_fmtp_line: format!(
                "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id={profile}"
            ),
            rtcp_feedback: vec![
                feedback("goog-remb", ""),
                feedback("transport-cc", ""),
                feedback("ccm", "fir"),
                feedback("nack", ""),
                feedback("nack", "pli"),
            ],
        },
        payload_type,
    }
}

async fn start(
    stream: &str,
    sdp: &str,
    events: &Sender<WorkerEvent>,
    internal: &UnboundedSender<Internal>,
) -> Result<Active, FailReason> {
    check_offer(sdp).map_err(|_| FailReason::Unsupported)?;
    let runtime = webrtc::runtime::default_runtime().ok_or(FailReason::Network)?;
    let mut engine = MediaEngine::default();
    // High (what the official client offers) and constrained baseline.
    for (profile, payload_type) in [("640c1f", 102), ("42e01f", 125)] {
        engine
            .register_codec(h264(profile, payload_type), RtpCodecKind::Video)
            .map_err(|_| FailReason::Unsupported)?;
    }
    // Stream audio is accepted like a browser would (the official client
    // discards an answer that refuses its audio section) and played mixed in
    // with voice. shortcut: no separate volume or mute for it yet.
    let opus = RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: MIME_TYPE_OPUS.to_owned(),
            clock_rate: 48_000,
            channels: 2,
            sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
            rtcp_feedback: vec![],
        },
        payload_type: 111,
    };
    engine
        .register_codec(opus, RtpCodecKind::Audio)
        .map_err(|_| FailReason::Unsupported)?;
    let registry = register_default_interceptors(Registry::new(), &mut engine)
        .map_err(|_| FailReason::Network)?;

    let generation = frames().open();
    let keyframe = Arc::new(AtomicBool::new(false));
    let decode = spawn_decoder(generation, internal.clone(), Arc::clone(&keyframe))
        .map_err(|_| FailReason::NoDecoder)?;
    let handler = Arc::new(Viewer {
        stream: stream.to_owned(),
        generation,
        events: events.clone(),
        internal: internal.clone(),
        decode,
        keyframe,
        runtime: Arc::clone(&runtime),
    });
    let peer = PeerConnectionBuilder::new()
        .with_configuration(ts6_stream::configuration(IpAddr::V4(Ipv4Addr::UNSPECIFIED)))
        .with_media_engine(engine)
        .with_interceptor_registry(registry)
        .with_handler(handler as Arc<dyn PeerConnectionEventHandler>)
        .with_runtime(runtime)
        .with_udp_addrs(vec!["0.0.0.0:0".to_string()])
        .build()
        .await
        .map_err(|_| FailReason::Network)?;
    let peer: Arc<dyn PeerConnection> = Arc::new(peer);
    let negotiated = async {
        peer.set_remote_description(RTCSessionDescription::offer(sdp.to_owned())?)
            .await?;
        let answer = peer.create_answer(None).await?;
        peer.set_local_description(answer).await?;
        webrtc::error::Result::Ok(())
    }
    .await;
    let answer = match (negotiated, peer.local_description().await) {
        (Ok(()), Some(answer)) => {
            let (level, version) = ts6_stream::next_level();
            ts6_stream::shape(&answer.sdp, level, version)
        }
        _ => {
            frames().close();
            let _ = peer.close().await;
            return Err(FailReason::Unsupported);
        }
    };
    let _ = events.try_send(WorkerEvent::Answer {
        stream: stream.to_owned(),
        sdp: answer,
    });
    Ok(Active {
        stream: stream.to_owned(),
        generation,
        peer,
        remote_candidates: 0,
        first_frame_by: Some(tokio::time::Instant::now() + FIRST_FRAME),
    })
}

/// Decodes on its own thread (Media Foundation is per-thread). The thread
/// ends when the last sender of its queue is dropped.
fn spawn_decoder(
    generation: u64,
    internal: UnboundedSender<Internal>,
    keyframe: Arc<AtomicBool>,
) -> std::io::Result<SyncSender<Vec<u8>>> {
    let (units, queue) = sync_channel::<Vec<u8>>(DECODE_QUEUE);
    std::thread::Builder::new()
        .name("stream-decoder".into())
        .spawn(move || {
            #[cfg(windows)]
            {
                let mut decoder = match H264Decoder::new() {
                    Ok(decoder) => decoder,
                    Err(_) => {
                        let _ = internal.send(Internal::Failed(generation, FailReason::NoDecoder));
                        return;
                    }
                };
                let mut first = true;
                let (mut shown, mut busy, mut since) = (0u32, Duration::ZERO, Instant::now());
                while let Ok(unit) = queue.recv() {
                    let started = Instant::now();
                    let result = decoder.decode(&unit);
                    busy += started.elapsed();
                    if since.elapsed() >= Duration::from_secs(5) {
                        tracing::info!(
                            frames = shown,
                            busy_ms = busy.as_millis() as u64,
                            "stream decoder, last 5 s"
                        );
                        (shown, busy, since) = (0, Duration::ZERO, Instant::now());
                    }
                    match result {
                        Ok(Some(frame)) => {
                            shown += 1;
                            frames().publish(generation, frame);
                            if std::mem::take(&mut first) {
                                let _ = internal.send(Internal::Playing(generation));
                            }
                        }
                        Ok(None) => {}
                        Err(_) => keyframe.store(true, Ordering::Relaxed),
                    }
                }
            }
            #[cfg(not(windows))]
            {
                let _ = (queue, keyframe);
                let _ = internal.send(Internal::Failed(generation, FailReason::NoDecoder));
            }
        })?;
    Ok(units)
}

/// Reassembles RTP packets into access units. A 1080p keyframe spans well over
/// a hundred packets, so the window must be far larger than the 64 the first
/// version used: that size dropped every keyframe, and with no keyframe the
/// picture froze until the sender happened to produce another one.
fn new_sample_builder() -> SampleBuilder<H264Packet> {
    SampleBuilder::new(BUILDER_WINDOW, H264Packet::default(), 90_000)
        .with_max_time_delay(Duration::from_millis(500))
}

const BUILDER_WINDOW: u16 = 4096;

/// Whether an Annex B access unit holds an IDR slice (NAL type 5).
fn is_keyframe(unit: &[u8]) -> bool {
    unit.windows(4)
        .any(|w| w[..3] == [0, 0, 1] && w[3] & 0x1f == 5)
}

struct Viewer {
    stream: String,
    generation: u64,
    events: Sender<WorkerEvent>,
    internal: UnboundedSender<Internal>,
    decode: SyncSender<Vec<u8>>,
    /// Set when frames were dropped or failed: skip to the next keyframe.
    keyframe: Arc<AtomicBool>,
    runtime: Arc<dyn webrtc::runtime::Runtime>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Viewer {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        if let Ok(init) = event.candidate.to_json() {
            let _ = self.events.try_send(WorkerEvent::LocalIce {
                stream: self.stream.clone(),
                json: local_candidate_json(&init),
            });
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        tracing::info!(?state, "stream viewer connection state");
        if state == RTCPeerConnectionState::Failed {
            let _ = self
                .internal
                .send(Internal::Failed(self.generation, FailReason::Network));
        }
    }

    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        match track.kind().await {
            RtpCodecKind::Audio => {
                self.runtime.spawn(Box::pin(play_audio(track)));
                return;
            }
            RtpCodecKind::Video => {}
            _ => return,
        }
        let Some(media_ssrc) = track.ssrcs().await.first().copied() else {
            return;
        };
        let decode = self.decode.clone();
        let keyframe = Arc::clone(&self.keyframe);
        self.runtime.spawn(Box::pin(async move {
            let mut builder = new_sample_builder();
            let mut waiting_key = true;
            let mut last_pli: Option<Instant> = None;
            let mut fir_sequence = 0u8;
            // Counters for the 5 s log line: where a freeze happens.
            let (mut packets, mut samples, mut dropped, mut skipped, mut full, mut keys, mut asked) =
                (0u32, 0u32, 0u32, 0u32, 0u32, 0u32, 0u32);
            let mut since = Instant::now();
            while let Some(event) = track.poll().await {
                let packet = match event {
                    TrackRemoteEvent::OnRtpPacket(packet) => packet,
                    TrackRemoteEvent::OnEnded => break,
                    _ => continue,
                };
                let now = Instant::now();
                packets += 1;
                if since.elapsed() >= Duration::from_secs(5) {
                    tracing::info!(
                        packets,
                        samples,
                        dropped,
                        skipped,
                        full,
                        keys,
                        asked,
                        waiting_key,
                        "stream track, last 5 s"
                    );
                    (packets, samples, dropped, skipped, full, keys, asked) = (0, 0, 0, 0, 0, 0, 0);
                    since = now;
                }
                builder.push(now, packet);
                while let Some(sample) = builder.pop(now) {
                    samples += 1;
                    dropped += u32::from(sample.prev_dropped_packets);
                    if keyframe.swap(false, Ordering::Relaxed) {
                        waiting_key = true;
                    }
                    // Lost packets that retransmission did not recover leave
                    // later frames referencing missing data: wait for a keyframe
                    // instead of showing a frozen or smeared picture.
                    if sample.prev_dropped_packets > 0 {
                        waiting_key = true;
                    }
                    if waiting_key && !is_keyframe(&sample.data) {
                        skipped += 1;
                        continue;
                    }
                    keys += u32::from(is_keyframe(&sample.data));
                    waiting_key = false;
                    match decode.try_send(sample.data.to_vec()) {
                        Ok(()) => {}
                        Err(TrySendError::Full(_)) => {
                            full += 1;
                            waiting_key = true;
                        }
                        Err(TrySendError::Disconnected(_)) => return,
                    }
                }
                if waiting_key && last_pli.is_none_or(|at| at.elapsed() >= Duration::from_secs(1)) {
                    last_pli = Some(now);
                    let pli = PictureLossIndication {
                        sender_ssrc: 0,
                        media_ssrc,
                    };
                    fir_sequence = fir_sequence.wrapping_add(1);
                    let fir = FullIntraRequest {
                        sender_ssrc: 0,
                        media_ssrc,
                        fir: vec![FirEntry {
                            ssrc: media_ssrc,
                            sequence_number: fir_sequence,
                        }],
                    };
                    asked += 1;
                    // Both are negotiated (`nack pli`, `ccm fir`); either may be honored.
                    let _ = track.write_rtcp(vec![Box::new(pli), Box::new(fir)]).await;
                }
            }
        }));
    }
}

/// Decodes the stream's Opus track and hands it to playback.
async fn play_audio(track: Arc<dyn TrackRemote>) {
    use ts6_audio::stream_audio::{stream_audio, StreamDecoder};
    let Ok(mut decoder) = StreamDecoder::new() else {
        return;
    };
    let sink = stream_audio();
    let mut last: Option<u16> = None;
    let (mut packets, mut concealed, mut since) = (0u32, 0u32, Instant::now());
    while let Some(event) = track.poll().await {
        let packet = match event {
            TrackRemoteEvent::OnRtpPacket(packet) => packet,
            TrackRemoteEvent::OnEnded => break,
            _ => continue,
        };
        let seq = packet.header.sequence_number;
        // Skip late or duplicate packets; conceal a short gap (up to 3 packets).
        if let Some(previous) = last {
            let step = seq.wrapping_sub(previous);
            if step == 0 || step > 0x8000 {
                continue;
            }
            for _ in 1..step.min(4) {
                if let Ok(pcm) = decoder.decode(None) {
                    sink.push(pcm);
                    concealed += 1;
                }
            }
        }
        last = Some(seq);
        packets += 1;
        if let Ok(pcm) = decoder.decode(Some(&packet.payload)) {
            sink.push(pcm);
        }
        if since.elapsed() >= Duration::from_secs(5) {
            tracing::info!(packets, concealed, "stream audio, last 5 s");
            (packets, concealed, since) = (0, 0, Instant::now());
        }
    }
    sink.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_round_trip_through_the_ts6_json() {
        let init = RTCIceCandidateInit {
            candidate: "candidate:1 1 udp 1 192.0.2.1 5000 typ host".into(),
            sdp_mid: Some("0".into()),
            sdp_mline_index: Some(0),
            ..Default::default()
        };
        let parsed = parse_candidate(&local_candidate_json(&init)).unwrap();
        assert_eq!(parsed.candidate, init.candidate);
        assert_eq!(parsed.sdp_mid.as_deref(), Some("0"));
        assert_eq!(parsed.sdp_mline_index, Some(0));
        assert!(parse_candidate(r#"{"cmd":"answer","args":{}}"#).is_none());
        assert!(parse_candidate(&"x".repeat(MAX_ICE_JSON + 1)).is_none());
    }

    #[test]
    fn a_keyframe_of_hundreds_of_packets_is_rebuilt_whole() {
        use rtc::rtp::codec::h264::H264Payloader;
        use rtc::rtp::packetizer::Payloader;
        // About 250 KB of IDR slice: roughly 210 packets at a 1200-byte MTU.
        let mut unit = vec![0, 0, 0, 1, 0x65];
        unit.extend((0..250_000u32).map(|i| (i % 251) as u8 | 1));
        let payloads = H264Payloader::default()
            .payload(1200, &unit.into())
            .unwrap();
        assert!(payloads.len() > 64, "{} packets", payloads.len());
        let mut builder = new_sample_builder();
        let now = Instant::now();
        let last = payloads.len() - 1;
        for (i, payload) in payloads.into_iter().enumerate() {
            let mut packet = rtc::rtp::packet::Packet::default();
            packet.header.sequence_number = 100 + i as u16;
            packet.header.timestamp = 9000;
            packet.header.marker = i == last;
            packet.payload = payload;
            builder.push(now, packet);
        }
        // The next frame's first packet releases the previous one.
        let mut next = rtc::rtp::packet::Packet::default();
        next.header.sequence_number = 100 + last as u16 + 1;
        next.header.timestamp = 12000;
        next.header.marker = true;
        next.payload = vec![0x41, 9].into();
        builder.push(now, next);
        let sample = builder.pop(now).expect("the keyframe is complete");
        assert_eq!(sample.prev_dropped_packets, 0);
        assert!(is_keyframe(&sample.data));
        assert_eq!(sample.data.len(), 250_000 + 5);
    }

    #[test]
    fn finds_idr_slices() {
        assert!(is_keyframe(&[0, 0, 0, 1, 0x67, 1, 0, 0, 1, 0x65, 9]));
        assert!(!is_keyframe(&[0, 0, 0, 1, 0x41, 9]));
    }
}
