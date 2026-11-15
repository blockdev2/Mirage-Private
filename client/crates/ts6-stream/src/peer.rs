//! Publisher and viewer peers over webrtc-rs.
//!
//! Both sides are signaling-agnostic and bind one UDP socket on the address
//! they are given (loopback in tests; the machine's address in real use).
//! Nothing here logs SDP, ICE candidates or addresses.

use crate::signal::Signal;
use anyhow::{anyhow, bail, Context, Result};
use rtc::interceptor::Registry;
use rtc::media::Sample;
use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::configuration::interceptor_registry::register_default_interceptors;
use rtc::peer_connection::configuration::media_engine::{
    MediaEngine, MIME_TYPE_H264, MIME_TYPE_OPUS,
};
use rtc::peer_connection::configuration::{
    RTCConfiguration, RTCConfigurationBuilder, RTCIceServer,
};
use rtc::peer_connection::sdp::RTCSessionDescription;
use rtc::peer_connection::transport::RTCIceCandidateInit;
use rtc::rtp_transceiver::rtp_sender::{
    RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters, RTCRtpEncodingParameters,
    RtpCodecKind,
};
use rtc::rtp_transceiver::{PayloadType, RTCRtpTransceiverDirection, RTCRtpTransceiverInit};
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{channel, Receiver, Sender};
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::media_stream::track_local::TrackLocal;
use webrtc::media_stream::track_remote::{TrackRemote, TrackRemoteEvent};
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCPeerConnectionIceEvent,
    RTCPeerConnectionState,
};
use webrtc::runtime::default_runtime;

/// Pending signals the caller has not collected yet. A peer that outruns its
/// caller drops candidates instead of growing without bound.
const SIGNAL_QUEUE: usize = 64;
/// Candidates held while the remote description is still missing.
const PENDING_CANDIDATES: usize = 64;
const H264_PAYLOAD_TYPE: PayloadType = 102;
const H264_FMTP: &str = "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=640c1f";

fn opus() -> RTCRtpCodecParameters {
    RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: MIME_TYPE_OPUS.to_owned(),
            clock_rate: 48_000,
            channels: 2,
            sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
            rtcp_feedback: vec![],
        },
        payload_type: 111,
    }
}

fn h264() -> RTCRtpCodecParameters {
    RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: MIME_TYPE_H264.to_owned(),
            clock_rate: 90_000,
            channels: 0,
            sdp_fmtp_line: H264_FMTP.to_owned(),
            rtcp_feedback: vec![],
        },
        payload_type: H264_PAYLOAD_TYPE,
    }
}

/// webrtc-rs reports an empty `mid`; the reference clients send the media
/// section's own mid (`"0"` for the single video section).
pub fn candidate_mid(mid: Option<String>, m_line: Option<u16>) -> Option<String> {
    mid.filter(|m| !m.is_empty())
        .or_else(|| Some(m_line.unwrap_or(0).to_string()))
}

/// State shared with the connection's event handler.
#[derive(Clone)]
struct Shared {
    signals: Sender<Signal>,
    connected: Arc<AtomicBool>,
    packets: Arc<AtomicU64>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Shared {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        let Ok(init) = event.candidate.to_json() else {
            return;
        };
        // The kind only (host, srflx, relay): whether the router's public
        // address was found tells a blocked STUN apart from a blocked peer.
        let kind = init
            .candidate
            .split(" typ ")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .unwrap_or("?");
        tracing::info!(kind, "local ICE candidate");
        let _ = self.signals.try_send(Signal::IceCandidate {
            sdp: init.candidate,
            mid: candidate_mid(init.sdp_mid, init.sdp_mline_index),
            m_line: init.sdp_mline_index,
        });
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        tracing::info!(?state, "stream peer connection state");
        self.connected.store(
            state == RTCPeerConnectionState::Connected,
            Ordering::Release,
        );
    }

    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        let packets = Arc::clone(&self.packets);
        let Some(runtime) = default_runtime() else {
            return;
        };
        runtime.spawn(Box::pin(async move {
            while let Some(event) = track.poll().await {
                if matches!(event, TrackRemoteEvent::OnRtpPacket(_)) {
                    packets.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }
}

/// Candidates that arrive before the remote description are held, then applied
/// in order once it is set.
#[derive(Default)]
struct Remote {
    state: Mutex<(bool, Vec<RTCIceCandidateInit>)>,
}

impl Remote {
    /// `Some` when the candidate may be applied now.
    fn offer(&self, candidate: RTCIceCandidateInit) -> Option<RTCIceCandidateInit> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.0 {
            return Some(candidate);
        }
        if state.1.len() < PENDING_CANDIDATES {
            state.1.push(candidate);
        }
        None
    }

    /// Marks the description as set and returns what was waiting.
    fn open(&self) -> Vec<RTCIceCandidateInit> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.0 = true;
        std::mem::take(&mut state.1)
    }
}

fn candidate_init(sdp: String, mid: Option<String>, m_line: Option<u16>) -> RTCIceCandidateInit {
    RTCIceCandidateInit {
        candidate: sdp,
        sdp_mid: mid,
        sdp_mline_index: m_line,
        username_fragment: None,
        url: None,
    }
}

async fn apply_candidate(
    connection: &Arc<dyn PeerConnection>,
    remote: &Remote,
    candidate: RTCIceCandidateInit,
) -> Result<()> {
    if let Some(ready) = remote.offer(candidate) {
        connection
            .add_ice_candidate(ready)
            .await
            .context("ICE candidate rejected")?;
    }
    Ok(())
}

async fn open_remote(connection: &Arc<dyn PeerConnection>, remote: &Remote) -> Result<()> {
    for candidate in remote.open() {
        connection
            .add_ice_candidate(candidate)
            .await
            .context("queued ICE candidate rejected")?;
    }
    Ok(())
}

/// The STUN servers the official TeamSpeak client uses. STUN only reveals the
/// address the router shows to the internet; it never carries media, and the
/// official client configures no TURN relay either.
pub const STUN_SERVERS: [&str; 3] = [
    "stun:turn.teamspeak.com:3478",
    "stun:turn2.teamspeak.com:3478",
    "stun:stun.l.google.com:19302",
];

/// The peer configuration for peers bound to `bind`. A peer bound to loopback
/// talks only to this machine, so it gets no STUN servers (and no network
/// traffic or delay while gathering candidates).
pub fn configuration(bind: IpAddr) -> RTCConfiguration {
    let mut builder = RTCConfigurationBuilder::new();
    if !bind.is_loopback() {
        builder = builder.with_ice_servers(vec![RTCIceServer {
            urls: STUN_SERVERS.map(String::from).to_vec(),
            ..Default::default()
        }]);
    }
    builder.build()
}

async fn connect(
    bind: IpAddr,
    media: MediaEngine,
    shared: Shared,
) -> Result<Arc<dyn PeerConnection>> {
    let mut media = media;
    let registry = register_default_interceptors(Registry::new(), &mut media)
        .map_err(|_| anyhow!("cannot register interceptors"))?;
    let runtime = default_runtime().ok_or_else(|| anyhow!("no async runtime available"))?;
    let connection = PeerConnectionBuilder::new()
        .with_configuration(configuration(bind))
        .with_media_engine(media)
        .with_interceptor_registry(registry)
        .with_handler(Arc::new(shared))
        .with_runtime(runtime)
        .with_udp_addrs(vec![format!("{bind}:0")])
        .build()
        .await
        .context("cannot create the peer connection")?;
    Ok(Arc::new(connection))
}

/// Sends one video stream to one viewer. TS6 streams are peer to peer, so a
/// stream with several viewers has one publisher per viewer.
pub struct Publisher {
    connection: Arc<dyn PeerConnection>,
    track: Arc<TrackLocalStaticSample>,
    ssrc: u32,
    /// Set once the viewer's answer is applied; nothing is sent before.
    payload_type: Mutex<Option<PayloadType>>,
    remote: Remote,
    signals: Receiver<Signal>,
    connected: Arc<AtomicBool>,
}

impl Publisher {
    pub async fn new(bind: IpAddr) -> Result<Self> {
        let mut media = MediaEngine::default();
        media
            .register_codec(h264(), RtpCodecKind::Video)
            .map_err(|_| anyhow!("cannot register H.264"))?;
        media
            .register_codec(opus(), RtpCodecKind::Audio)
            .map_err(|_| anyhow!("cannot register Opus"))?;
        let (signal_tx, signals) = channel(SIGNAL_QUEUE);
        let connected = Arc::new(AtomicBool::new(false));
        let shared = Shared {
            signals: signal_tx,
            connected: Arc::clone(&connected),
            packets: Arc::new(AtomicU64::new(0)),
        };
        let connection = connect(bind, media, shared).await?;
        let ssrc = rand_ssrc();
        let track = Arc::new(
            TrackLocalStaticSample::new(
                Instant::now(),
                MediaStreamTrack::new(
                    "mirage-stream".to_owned(),
                    "mirage-video".to_owned(),
                    "mirage-video".to_owned(),
                    RtpCodecKind::Video,
                    vec![RTCRtpEncodingParameters {
                        rtp_coding_parameters: RTCRtpCodingParameters {
                            ssrc: Some(ssrc),
                            ..Default::default()
                        },
                        codec: h264().rtp_codec,
                        ..Default::default()
                    }],
                ),
            )
            .map_err(|_| anyhow!("cannot create the video track"))?,
        );
        connection
            .add_track(Arc::clone(&track) as Arc<dyn TrackLocal>)
            .await
            .context("cannot add the video track")?;
        // The reference publisher offers an inactive audio section next to the
        // video one (stream audio is not sent), and the official viewer expects both.
        // The reference publisher's video section is send-only, not send-receive.
        for transceiver in connection.get_transceivers().await {
            transceiver
                .set_direction(RTCRtpTransceiverDirection::Sendonly)
                .await
                .context("cannot make the video section send-only")?;
        }
        let audio = connection
            .add_transceiver_from_kind(
                RtpCodecKind::Audio,
                Some(RTCRtpTransceiverInit {
                    direction: RTCRtpTransceiverDirection::Recvonly,
                    streams: vec![],
                    send_encodings: vec![],
                }),
            )
            .await
            .context("cannot add the audio section")?;
        // Only receive-only sections can be added; make it inactive afterwards.
        audio
            .set_direction(RTCRtpTransceiverDirection::Inactive)
            .await
            .context("cannot make the audio section inactive")?;
        Ok(Self {
            connection,
            track,
            ssrc,
            payload_type: Mutex::new(None),
            remote: Remote::default(),
            signals,
            connected,
        })
    }

    /// The SDP offer for `respondjoinstreamrequest`, in the next SDP layout of
    /// the experiment (see `sdp`).
    pub async fn offer(&self) -> Result<String> {
        let (level, version) = crate::sdp::next_level();
        self.offer_with(level, version).await
    }

    /// The offer written at a given SDP level (see `sdp`).
    pub async fn offer_with(&self, level: u8, version: u32) -> Result<String> {
        let offer = self
            .connection
            .create_offer(None)
            .await
            .context("cannot create the offer")?;
        let text = offer.sdp.clone();
        self.connection
            .set_local_description(offer)
            .await
            .context("cannot set the local description")?;
        Ok(crate::sdp::shape(&text, level, version))
    }

    /// Applies a signal relayed from the viewer.
    pub async fn apply(&self, signal: Signal) -> Result<()> {
        match signal {
            Signal::Answer(sdp) => {
                let answer =
                    RTCSessionDescription::answer(sdp).map_err(|_| anyhow!("malformed answer"))?;
                self.connection
                    .set_remote_description(answer)
                    .await
                    .context("answer rejected")?;
                let senders = self.connection.get_senders().await;
                let sender = senders.first().ok_or_else(|| anyhow!("no video sender"))?;
                let payload_type = sender
                    .get_parameters()
                    .await
                    .context("cannot read the negotiated codec")?
                    .rtp_parameters
                    .codecs
                    .first()
                    .map(|codec| codec.payload_type)
                    .ok_or_else(|| anyhow!("the viewer accepted no codec"))?;
                *self.payload_type.lock().unwrap_or_else(|e| e.into_inner()) = Some(payload_type);
                open_remote(&self.connection, &self.remote).await
            }
            Signal::IceCandidate { sdp, mid, m_line } => {
                apply_candidate(
                    &self.connection,
                    &self.remote,
                    candidate_init(sdp, mid, m_line),
                )
                .await
            }
        }
    }

    /// Local ICE candidates to relay to the viewer.
    pub fn signals(&mut self) -> &mut Receiver<Signal> {
        &mut self.signals
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    /// Sends one encoded access unit (Annex-B H.264).
    pub async fn send_sample(&self, annex_b: Vec<u8>, duration: Duration) -> Result<()> {
        let Some(payload_type) = *self.payload_type.lock().unwrap_or_else(|e| e.into_inner())
        else {
            bail!("the viewer has not answered yet");
        };
        self.track
            .sample_writer(self.ssrc, payload_type)
            .write_sample(&Sample {
                data: annex_b.into(),
                duration,
                ..Sample::new(Instant::now())
            })
            .await
            .map_err(|_| anyhow!("cannot send the sample"))
    }

    pub async fn close(&self) {
        let _ = self.connection.close().await;
    }
}

fn rand_ssrc() -> u32 {
    // Not a secret: the SSRC only has to differ between streams.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |d| d.subsec_nanos());
    nanos | 1
}

/// Receives one video stream. Counts RTP packets for now; decoding arrives
/// with the viewer UI.
pub struct Viewer {
    connection: Arc<dyn PeerConnection>,
    remote: Remote,
    signals: Receiver<Signal>,
    packets: Arc<AtomicU64>,
}

impl Viewer {
    pub async fn new(bind: IpAddr) -> Result<Self> {
        let mut media = MediaEngine::default();
        media
            .register_codec(h264(), RtpCodecKind::Video)
            .map_err(|_| anyhow!("cannot register H.264"))?;
        let (signal_tx, signals) = channel(SIGNAL_QUEUE);
        let packets = Arc::new(AtomicU64::new(0));
        let shared = Shared {
            signals: signal_tx,
            connected: Arc::new(AtomicBool::new(false)),
            packets: Arc::clone(&packets),
        };
        Ok(Self {
            connection: connect(bind, media, shared).await?,
            remote: Remote::default(),
            signals,
            packets,
        })
    }

    /// Takes the publisher's offer and returns the answer to relay back.
    pub async fn accept_offer(&self, sdp: &str) -> Result<String> {
        let offer =
            RTCSessionDescription::offer(sdp.to_owned()).map_err(|_| anyhow!("malformed offer"))?;
        self.connection
            .set_remote_description(offer)
            .await
            .context("offer rejected")?;
        open_remote(&self.connection, &self.remote).await?;
        let answer = self
            .connection
            .create_answer(None)
            .await
            .context("cannot create the answer")?;
        let text = answer.sdp.clone();
        self.connection
            .set_local_description(answer)
            .await
            .context("cannot set the local description")?;
        Ok(text)
    }

    pub async fn apply(&self, signal: Signal) -> Result<()> {
        match signal {
            Signal::Answer(_) => bail!("a viewer does not take an answer"),
            Signal::IceCandidate { sdp, mid, m_line } => {
                apply_candidate(
                    &self.connection,
                    &self.remote,
                    candidate_init(sdp, mid, m_line),
                )
                .await
            }
        }
    }

    /// Local ICE candidates to relay to the publisher.
    pub fn signals(&mut self) -> &mut Receiver<Signal> {
        &mut self.signals
    }

    /// RTP packets received so far.
    pub fn packets(&self) -> u64 {
        self.packets.load(Ordering::Relaxed)
    }

    pub async fn close(&self) {
        let _ = self.connection.close().await;
    }
}

#[cfg(test)]
mod configuration_tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn loopback_peers_use_no_stun_servers() {
        for bind in [
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
        ] {
            assert!(configuration(bind).ice_servers().is_empty());
        }
    }

    #[test]
    fn network_peers_use_the_three_official_stun_servers() {
        let config = configuration(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        let servers = config.ice_servers();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].urls, STUN_SERVERS.map(String::from).to_vec());
        // A TURN URL would fail here for lack of credentials; STUN needs none.
        assert!(servers[0].urls().is_ok());
        assert!(servers[0].urls.iter().all(|url| url.starts_with("stun:")));
    }
}
