//! Screen sharing signaling, as a pure state machine: the TS6 stream notices,
//! the UI's start/stop requests and the share worker's reports go in; server
//! commands, worker commands and UI state come out. One share at a time.
//!
//! Flow (docs/protocol-ts6-astrum.md section 2.4): capture starts, then
//! `setupstream`; the server answers with `notifystreamstarted` for our own
//! client id, which carries the stream id. Each `notifyjoinstreamrequest`
//! gets a publisher peer whose offer goes out in `respondjoinstreamrequest`;
//! the viewer's answer and candidates come back in `notifystreamsignaling`.

use crate::share_media::{
    CaptureSource, Quality, ShareCommand, ShareEvent, ShareFailure, MAX_REMOTE_CANDIDATES,
    MAX_VIEWERS,
};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use ts6_core::streams::{ControlKind, StreamEvent};

/// How long capture start, the server's confirmation and a cancel may take.
pub const START_DEADLINE: Duration = Duration::from_secs(15);
const CANCEL_DEADLINE: Duration = Duration::from_secs(30);
/// Largest ICE JSON relayed to the worker (an answer may be up to 16 KiB,
/// which the core parser already bounds).
const MAX_ICE_JSON: usize = 2 * 1024;
/// The name other clients show for the stream.
const STREAM_NAME: &str = "Mirage";

pub enum Action {
    /// A server command with its arguments.
    Send(&'static str, Vec<(&'static str, String)>),
    Worker(ShareCommand),
    /// Stop the worker (and with it the capture and every peer).
    CloseWorker,
    /// UI state: `starting | live | stopped | failed`, with a reason when
    /// failed and the number of connected viewers.
    Publish {
        state: &'static str,
        reason: Option<&'static str>,
        viewers: usize,
    },
}

impl std::fmt::Debug for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Send(name, _) => write!(f, "Send({name})"),
            Self::Worker(ShareCommand::Start { .. }) => write!(f, "Worker(Start)"),
            Self::Worker(ShareCommand::AddViewer { viewer }) => write!(f, "Worker(Add {viewer})"),
            Self::Worker(ShareCommand::Signal { viewer, .. }) => {
                write!(f, "Worker(Signal {viewer})")
            }
            Self::Worker(ShareCommand::RemoveViewer { viewer }) => {
                write!(f, "Worker(Remove {viewer})")
            }
            Self::CloseWorker => write!(f, "CloseWorker"),
            Self::Publish {
                state,
                reason,
                viewers,
            } => write!(f, "Publish({state}, {reason:?}, {viewers})"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
enum Phase {
    #[default]
    Idle,
    /// The worker is starting the capture.
    Capturing {
        since: Instant,
    },
    /// `setupstream` is sent; waiting for our own `notifystreamstarted`.
    Announcing {
        since: Instant,
    },
    Live {
        stream: String,
    },
    /// Stopped while announcing: the stream id is still unknown, so a late
    /// `notifystreamstarted` is answered with `stopstream`.
    Cancelling {
        since: Instant,
    },
}

#[derive(Default)]
pub struct ShareRegistry {
    own: u16,
    phase: Phase,
    /// Remote candidates applied so far, by viewer.
    viewers: HashMap<u16, usize>,
    connected: HashSet<u16>,
}

fn reason_code(reason: ShareFailure) -> &'static str {
    match reason {
        ShareFailure::Unsupported => "unsupported",
        ShareFailure::Capture => "capture",
    }
}

fn respond(stream: &str, viewer: u16, offer: String, accept: bool) -> Action {
    Action::Send(
        "respondjoinstreamrequest",
        vec![
            ("id", stream.to_owned()),
            ("clid", viewer.to_string()),
            ("msg", String::new()),
            ("offer", offer),
            ("decision", if accept { "1" } else { "0" }.to_owned()),
        ],
    )
}

impl ShareRegistry {
    #[cfg(test)]
    pub fn is_idle(&self) -> bool {
        self.phase == Phase::Idle
    }

    #[cfg(test)]
    pub fn viewers(&self) -> usize {
        self.connected.len()
    }

    /// Starts sharing; `own` is our client id (it identifies our own
    /// `notifystreamstarted`).
    pub fn start(
        &mut self,
        own: u16,
        source: CaptureSource,
        quality: Quality,
        now: Instant,
    ) -> Vec<Action> {
        match self.phase {
            Phase::Idle => {}
            // Already starting or sharing: nothing to do.
            Phase::Capturing { .. } | Phase::Announcing { .. } | Phase::Live { .. } => {
                return Vec::new()
            }
            Phase::Cancelling { .. } => {
                return vec![Action::Publish {
                    state: "failed",
                    reason: Some("busy"),
                    viewers: 0,
                }]
            }
        }
        self.own = own;
        self.phase = Phase::Capturing { since: now };
        vec![
            Action::Worker(ShareCommand::Start { source, quality }),
            Action::Publish {
                state: "starting",
                reason: None,
                viewers: 0,
            },
        ]
    }

    /// Ends the share on the user's request.
    pub fn stop(&mut self, now: Instant) -> Vec<Action> {
        self.end("stopped", None, now)
    }

    pub fn on_worker(&mut self, event: ShareEvent, now: Instant) -> Vec<Action> {
        match event {
            ShareEvent::CaptureReady => {
                if !matches!(self.phase, Phase::Capturing { .. }) {
                    return Vec::new();
                }
                self.phase = Phase::Announcing { since: now };
                vec![Action::Send(
                    "setupstream",
                    vec![
                        ("name", STREAM_NAME.to_owned()),
                        ("type", "2".to_owned()),
                        (
                            "bitrate",
                            ts6_capture::CaptureConfig::default()
                                .bitrate_kbps
                                .to_string(),
                        ),
                        ("accessibility", "1".to_owned()),
                        ("mode", "1".to_owned()),
                        ("viewer_limit", "0".to_owned()),
                        ("audio", "0".to_owned()),
                    ],
                )]
            }
            ShareEvent::Offer { viewer, sdp } => {
                let Phase::Live { stream } = &self.phase else {
                    return Vec::new();
                };
                if !self.viewers.contains_key(&viewer) {
                    return Vec::new();
                }
                // TS6 expects the SDP CRLF-terminated (docs/protocol-ts6-astrum.md 2.2).
                let sdp = if sdp.ends_with("\r\n") {
                    sdp
                } else {
                    format!("{}\r\n", sdp.trim_end())
                };
                vec![respond(stream, viewer, sdp, true)]
            }
            ShareEvent::LocalIce { viewer, json } => {
                let Phase::Live { stream } = &self.phase else {
                    return Vec::new();
                };
                if !self.viewers.contains_key(&viewer) {
                    return Vec::new();
                }
                vec![Action::Send(
                    "streamsignaling",
                    vec![
                        ("id", stream.clone()),
                        ("clid", viewer.to_string()),
                        ("json", json),
                    ],
                )]
            }
            ShareEvent::ViewerConnected { viewer } => {
                if self.viewers.contains_key(&viewer) {
                    self.connected.insert(viewer);
                    return vec![self.live_state()];
                }
                Vec::new()
            }
            ShareEvent::ViewerFailed { viewer } => {
                if self.viewers.remove(&viewer).is_some() {
                    self.connected.remove(&viewer);
                    return vec![self.live_state()];
                }
                Vec::new()
            }
            ShareEvent::Failed(reason) => self.end("failed", Some(reason_code(reason)), now),
        }
    }

    pub fn on_event(&mut self, event: &StreamEvent, _now: Instant) -> Vec<Action> {
        let notice = &event.control;
        let (client, stream) = (notice.client_id, notice.stream_id.as_str());
        match notice.kind {
            ControlKind::Started | ControlKind::Info if client == self.own => match self.phase {
                Phase::Announcing { .. } => {
                    self.phase = Phase::Live {
                        stream: stream.to_owned(),
                    };
                    vec![self.live_state()]
                }
                Phase::Cancelling { .. } => {
                    self.phase = Phase::Idle;
                    vec![Action::Send("stopstream", stop_args(stream))]
                }
                _ => Vec::new(),
            },
            ControlKind::Stopped if self.is_ours(client, stream) => {
                // The server ended it (it needs no `stopstream`).
                let had = self.phase != Phase::Idle;
                self.clear();
                if had {
                    vec![
                        Action::CloseWorker,
                        Action::Publish {
                            state: "stopped",
                            reason: None,
                            viewers: 0,
                        },
                    ]
                } else {
                    Vec::new()
                }
            }
            ControlKind::JoinRequested if self.is_ours_stream(stream) => {
                if notice.removed {
                    return self.remove_viewer(client);
                }
                let Phase::Live { stream } = &self.phase else {
                    return Vec::new();
                };
                let known = self.viewers.contains_key(&client);
                if !known && self.viewers.len() >= MAX_VIEWERS {
                    return vec![respond(stream, client, String::new(), false)];
                }
                let mut actions = Vec::new();
                if known {
                    // A new request replaces the old peer.
                    self.connected.remove(&client);
                    actions.push(Action::Worker(ShareCommand::RemoveViewer {
                        viewer: client,
                    }));
                }
                self.viewers.insert(client, 0);
                actions.push(Action::Worker(ShareCommand::AddViewer { viewer: client }));
                if known {
                    actions.push(self.live_state());
                }
                actions
            }
            ControlKind::ClientLeft if self.is_ours_stream(stream) => self.remove_viewer(client),
            ControlKind::Signaling if self.is_ours_stream(stream) => {
                let Some(json) = &event.payload else {
                    return Vec::new();
                };
                let Some(count) = self.viewers.get_mut(&client) else {
                    return Vec::new();
                };
                let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
                    return Vec::new();
                };
                match value.get("cmd").and_then(|c| c.as_str()) {
                    Some("answer") => {}
                    Some("iceCandidate")
                        if json.len() <= MAX_ICE_JSON && *count < MAX_REMOTE_CANDIDATES =>
                    {
                        *count += 1;
                    }
                    _ => return Vec::new(),
                }
                vec![Action::Worker(ShareCommand::Signal {
                    viewer: client,
                    json: json.clone(),
                })]
            }
            _ => Vec::new(),
        }
    }

    pub fn tick(&mut self, now: Instant) -> Vec<Action> {
        match self.phase {
            Phase::Capturing { since } if now.duration_since(since) >= START_DEADLINE => {
                self.end("failed", Some("capture"), now)
            }
            Phase::Announcing { since } if now.duration_since(since) >= START_DEADLINE => {
                self.end("failed", Some("timeout"), now)
            }
            Phase::Cancelling { since } if now.duration_since(since) >= CANCEL_DEADLINE => {
                self.phase = Phase::Idle;
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Forgets everything (disconnect); stale events then do nothing.
    pub fn reset(&mut self) {
        self.clear();
    }

    fn clear(&mut self) {
        self.phase = Phase::Idle;
        self.viewers.clear();
        self.connected.clear();
    }

    fn live_state(&self) -> Action {
        Action::Publish {
            state: "live",
            reason: None,
            viewers: self.connected.len(),
        }
    }

    fn is_ours_stream(&self, stream: &str) -> bool {
        matches!(&self.phase, Phase::Live { stream: ours } if ours == stream)
    }

    fn is_ours(&self, client: u16, stream: &str) -> bool {
        client == self.own && self.is_ours_stream(stream)
    }

    fn remove_viewer(&mut self, viewer: u16) -> Vec<Action> {
        if self.viewers.remove(&viewer).is_none() {
            return Vec::new();
        }
        self.connected.remove(&viewer);
        vec![
            Action::Worker(ShareCommand::RemoveViewer { viewer }),
            self.live_state(),
        ]
    }

    /// Ends the share: announces `stopstream` when the id is known, and
    /// remembers to do so later when it is not yet.
    fn end(
        &mut self,
        state: &'static str,
        reason: Option<&'static str>,
        now: Instant,
    ) -> Vec<Action> {
        let mut actions = Vec::new();
        match std::mem::take(&mut self.phase) {
            Phase::Idle | Phase::Cancelling { .. } => {
                return Vec::new();
            }
            Phase::Capturing { .. } => {}
            Phase::Announcing { .. } => self.phase = Phase::Cancelling { since: now },
            Phase::Live { stream } => actions.push(Action::Send("stopstream", stop_args(&stream))),
        }
        self.viewers.clear();
        self.connected.clear();
        actions.push(Action::CloseWorker);
        actions.push(Action::Publish {
            state,
            reason,
            viewers: 0,
        });
        actions
    }
}

fn stop_args(stream: &str) -> Vec<(&'static str, String)> {
    vec![("id", stream.to_owned()), ("reason", "1".to_owned())]
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWN: u16 = 5;

    fn event(text: &str) -> StreamEvent {
        StreamEvent::parse(text).expect(text)
    }

    fn sends<'a>(actions: &'a [Action], name: &str) -> Vec<&'a Vec<(&'static str, String)>> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Send(n, args) if *n == name => Some(args),
                _ => None,
            })
            .collect()
    }

    fn arg<'a>(args: &'a [(&'static str, String)], key: &str) -> &'a str {
        args.iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.as_str())
            .unwrap()
    }

    fn published(actions: &[Action]) -> Vec<(&'static str, Option<&'static str>, usize)> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Publish {
                    state,
                    reason,
                    viewers,
                } => Some((*state, *reason, *viewers)),
                _ => None,
            })
            .collect()
    }

    fn closes(actions: &[Action]) -> bool {
        actions.iter().any(|a| matches!(a, Action::CloseWorker))
    }

    /// Capture ready and the server's confirmation: sharing as stream "s".
    fn live(now: Instant) -> ShareRegistry {
        let mut r = ShareRegistry::default();
        r.start(OWN, CaptureSource::Monitor(None), Quality::default(), now);
        r.on_worker(ShareEvent::CaptureReady, now);
        r.on_event(&event("notifystreamstarted id=s clid=5"), now);
        r
    }

    fn join(r: &mut ShareRegistry, viewer: u16, now: Instant) -> Vec<Action> {
        r.on_event(
            &event(&format!("notifyjoinstreamrequest id=s clid={viewer}")),
            now,
        )
    }

    #[test]
    fn capture_comes_first_then_setupstream_then_live() {
        let now = Instant::now();
        let mut r = ShareRegistry::default();
        let actions = r.start(
            OWN,
            CaptureSource::Monitor(Some(1)),
            Quality::default(),
            now,
        );
        assert!(matches!(
            actions.as_slice(),
            [
                Action::Worker(ShareCommand::Start {
                    source: CaptureSource::Monitor(Some(1)),
                    quality: Quality::Standard
                }),
                Action::Publish {
                    state: "starting",
                    ..
                }
            ]
        ));
        let actions = r.on_worker(ShareEvent::CaptureReady, now);
        let setup = sends(&actions, "setupstream");
        assert_eq!(setup.len(), 1);
        assert_eq!(arg(setup[0], "mode"), "1");
        assert_eq!(arg(setup[0], "audio"), "0");
        // Someone else's stream does not confirm ours.
        assert!(r
            .on_event(&event("notifystreamstarted id=x clid=9"), now)
            .is_empty());
        let actions = r.on_event(&event("notifystreamstarted id=s clid=5"), now);
        assert_eq!(published(&actions), vec![("live", None, 0)]);
        assert!(
            r.start(OWN, CaptureSource::Monitor(None), Quality::default(), now)
                .is_empty(),
            "already sharing"
        );
    }

    #[test]
    fn a_join_request_gets_a_peer_and_its_offer_goes_out_crlf_terminated() {
        let now = Instant::now();
        let mut r = live(now);
        let actions = join(&mut r, 302, now);
        assert!(matches!(
            actions.as_slice(),
            [Action::Worker(ShareCommand::AddViewer { viewer: 302 })]
        ));
        let actions = r.on_worker(
            ShareEvent::Offer {
                viewer: 302,
                sdp: "v=0".into(),
            },
            now,
        );
        let response = sends(&actions, "respondjoinstreamrequest");
        assert_eq!(arg(response[0], "id"), "s");
        assert_eq!(arg(response[0], "clid"), "302");
        assert_eq!(arg(response[0], "decision"), "1");
        assert_eq!(arg(response[0], "offer"), "v=0\r\n");
        // An offer for a viewer we never accepted goes nowhere.
        assert!(r
            .on_worker(
                ShareEvent::Offer {
                    viewer: 9,
                    sdp: "v=0".into()
                },
                now
            )
            .is_empty());
    }

    #[test]
    fn candidates_and_answers_pass_both_ways_within_bounds() {
        let now = Instant::now();
        let mut r = live(now);
        join(&mut r, 302, now);
        let actions = r.on_worker(
            ShareEvent::LocalIce {
                viewer: 302,
                json: "{\"cmd\":\"iceCandidate\"}".into(),
            },
            now,
        );
        assert_eq!(arg(sends(&actions, "streamsignaling")[0], "clid"), "302");

        let answer =
            r#"notifystreamsignaling id=s clid=302 json={"cmd":"answer","args":{"answer":"v=0"}}"#;
        assert!(matches!(
            r.on_event(&event(answer), now).as_slice(),
            [Action::Worker(ShareCommand::Signal { viewer: 302, .. })]
        ));
        let ice = r#"notifystreamsignaling id=s clid=302 json={"cmd":"iceCandidate","args":{"sdp":"c","mid":"0","mLine":0}}"#;
        for _ in 0..MAX_REMOTE_CANDIDATES {
            assert_eq!(r.on_event(&event(ice), now).len(), 1);
        }
        assert!(r.on_event(&event(ice), now).is_empty(), "65th dropped");
        // Unknown commands and unknown viewers are ignored.
        let odd = r#"notifystreamsignaling id=s clid=302 json={"cmd":"nonsense","args":{}}"#;
        assert!(r.on_event(&event(odd), now).is_empty());
        let stranger = ice.replace("clid=302", "clid=77");
        assert!(r.on_event(&event(&stranger), now).is_empty());
    }

    #[test]
    fn viewers_beyond_the_limit_are_refused_with_decision_zero() {
        let now = Instant::now();
        let mut r = live(now);
        for viewer in 10..10 + MAX_VIEWERS as u16 {
            assert!(!join(&mut r, viewer, now).is_empty());
        }
        let actions = join(&mut r, 99, now);
        let refusal = sends(&actions, "respondjoinstreamrequest");
        assert_eq!(arg(refusal[0], "clid"), "99");
        assert_eq!(arg(refusal[0], "decision"), "0");
        assert_eq!(arg(refusal[0], "offer"), "");
        assert!(!actions
            .iter()
            .any(|a| matches!(a, Action::Worker(ShareCommand::AddViewer { .. }))));
    }

    #[test]
    fn a_second_request_from_the_same_viewer_replaces_its_peer() {
        let now = Instant::now();
        let mut r = live(now);
        join(&mut r, 302, now);
        r.on_worker(ShareEvent::ViewerConnected { viewer: 302 }, now);
        let actions = join(&mut r, 302, now);
        assert!(matches!(
            actions.as_slice(),
            [
                Action::Worker(ShareCommand::RemoveViewer { viewer: 302 }),
                Action::Worker(ShareCommand::AddViewer { viewer: 302 }),
                Action::Publish {
                    state: "live",
                    viewers: 0,
                    ..
                }
            ]
        ));
    }

    #[test]
    fn viewer_counts_follow_connections_and_departures() {
        let now = Instant::now();
        let mut r = live(now);
        join(&mut r, 302, now);
        join(&mut r, 303, now);
        let a = r.on_worker(ShareEvent::ViewerConnected { viewer: 302 }, now);
        assert_eq!(published(&a), vec![("live", None, 1)]);
        r.on_worker(ShareEvent::ViewerConnected { viewer: 303 }, now);
        assert_eq!(r.viewers(), 2);
        // Leaving by is_remove, by notifystreamclientleft, or by failing.
        let a = r.on_event(
            &event("notifyjoinstreamrequest id=s clid=302 is_remove=1"),
            now,
        );
        assert!(matches!(
            a.as_slice(),
            [
                Action::Worker(ShareCommand::RemoveViewer { viewer: 302 }),
                Action::Publish { viewers: 1, .. }
            ]
        ));
        let a = r.on_event(&event("notifystreamclientleft id=s clid=303"), now);
        assert_eq!(published(&a), vec![("live", None, 0)]);
        join(&mut r, 304, now);
        let a = r.on_worker(ShareEvent::ViewerFailed { viewer: 304 }, now);
        assert_eq!(published(&a), vec![("live", None, 0)]);
        assert!(r
            .on_event(&event("notifystreamclientleft id=s clid=304"), now)
            .is_empty());
    }

    #[test]
    fn stopping_while_live_sends_stopstream_and_closes() {
        let now = Instant::now();
        let mut r = live(now);
        join(&mut r, 302, now);
        let actions = r.stop(now);
        let stop = sends(&actions, "stopstream");
        assert_eq!((arg(stop[0], "id"), arg(stop[0], "reason")), ("s", "1"));
        assert!(closes(&actions));
        assert_eq!(published(&actions), vec![("stopped", None, 0)]);
        assert!(r.is_idle());
        assert!(join(&mut r, 303, now).is_empty(), "stale events do nothing");
    }

    #[test]
    fn stopping_before_the_capture_is_ready_announces_nothing() {
        let now = Instant::now();
        let mut r = ShareRegistry::default();
        r.start(OWN, CaptureSource::Monitor(None), Quality::default(), now);
        let actions = r.stop(now);
        assert!(sends(&actions, "stopstream").is_empty());
        assert!(closes(&actions));
        assert!(r.is_idle());
        // A late capture report is ignored.
        assert!(r.on_worker(ShareEvent::CaptureReady, now).is_empty());
    }

    #[test]
    fn stopping_while_announcing_still_removes_the_late_announcement() {
        let now = Instant::now();
        let mut r = ShareRegistry::default();
        r.start(OWN, CaptureSource::Monitor(None), Quality::default(), now);
        r.on_worker(ShareEvent::CaptureReady, now);
        let actions = r.stop(now);
        assert!(sends(&actions, "stopstream").is_empty());
        assert!(closes(&actions));
        assert!(!r.is_idle());
        assert_eq!(
            published(&r.start(OWN, CaptureSource::Monitor(None), Quality::default(), now)),
            vec![("failed", Some("busy"), 0)]
        );
        let late = r.on_event(&event("notifystreamstarted id=s clid=5"), now);
        assert_eq!(arg(sends(&late, "stopstream")[0], "id"), "s");
        assert!(r.is_idle());
    }

    #[test]
    fn deadlines_end_a_share_that_never_starts() {
        let now = Instant::now();
        let mut r = ShareRegistry::default();
        r.start(OWN, CaptureSource::Monitor(None), Quality::default(), now);
        assert!(r.tick(now + Duration::from_secs(14)).is_empty());
        let a = r.tick(now + START_DEADLINE);
        assert_eq!(published(&a), vec![("failed", Some("capture"), 0)]);

        let mut r = ShareRegistry::default();
        r.start(OWN, CaptureSource::Monitor(None), Quality::default(), now);
        r.on_worker(ShareEvent::CaptureReady, now);
        let a = r.tick(now + START_DEADLINE);
        assert_eq!(published(&a), vec![("failed", Some("timeout"), 0)]);
        // The id is unknown, so a cancel is pending until its own deadline.
        assert!(!r.is_idle());
        r.tick(now + START_DEADLINE + CANCEL_DEADLINE);
        assert!(r.is_idle());
    }

    #[test]
    fn worker_failures_reach_the_ui_with_a_reason() {
        let now = Instant::now();
        let mut r = ShareRegistry::default();
        r.start(OWN, CaptureSource::Monitor(None), Quality::default(), now);
        let a = r.on_worker(ShareEvent::Failed(ShareFailure::Unsupported), now);
        assert_eq!(published(&a), vec![("failed", Some("unsupported"), 0)]);
        assert!(closes(&a));
        assert!(r.is_idle());

        // Mid-share capture loss also announces the stop.
        let mut r = live(now);
        let a = r.on_worker(ShareEvent::Failed(ShareFailure::Capture), now);
        assert_eq!(sends(&a, "stopstream").len(), 1);
        assert_eq!(published(&a), vec![("failed", Some("capture"), 0)]);
    }

    #[test]
    fn the_server_ending_our_stream_closes_without_stopstream() {
        let now = Instant::now();
        let mut r = live(now);
        let a = r.on_event(&event("notifystreamstopped id=s clid=5"), now);
        assert!(sends(&a, "stopstream").is_empty());
        assert!(closes(&a));
        assert_eq!(published(&a), vec![("stopped", None, 0)]);
        assert!(r.is_idle());
        // Someone else's stream stopping changes nothing.
        let mut r = live(now);
        assert!(r
            .on_event(&event("notifystreamstopped id=x clid=9"), now)
            .is_empty());
    }

    #[test]
    fn reset_forgets_the_share() {
        let now = Instant::now();
        let mut r = live(now);
        join(&mut r, 302, now);
        r.reset();
        assert!(r.is_idle());
        assert!(join(&mut r, 302, now).is_empty());
        assert!(r.tick(now + Duration::from_secs(120)).is_empty());
    }
}
