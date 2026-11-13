//! Stream watching signaling, as a pure state machine: TS6 stream notices,
//! the UI's watch requests and the stream worker's reports go in; commands
//! for the server, the worker and the UI come out. One stream at a time.

use crate::stream_media::{offer::MAX_OFFER_BYTES, FailReason, WorkerCommand, WorkerEvent};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use ts6_core::streams::{ControlKind, StreamEvent};

/// How long to wait for the stream id or the publisher's offer.
pub const OFFER_DEADLINE: Duration = Duration::from_secs(15);
/// Remote candidates relayed per watch.
const MAX_REMOTE_CANDIDATES: usize = 64;
/// Largest ICE JSON relayed to the worker.
const MAX_ICE_JSON: usize = 2 * 1024;

pub enum Action {
    /// A server command with its arguments.
    Send(&'static str, Vec<(&'static str, String)>),
    Worker(WorkerCommand),
    /// UI state: `connecting | playing | ended | failed`, with a reason when failed.
    Publish {
        client: u16,
        state: &'static str,
        reason: Option<&'static str>,
    },
}

impl std::fmt::Debug for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Send(name, _) => write!(f, "Send({name})"),
            Self::Worker(WorkerCommand::Offer { .. }) => write!(f, "Worker(Offer)"),
            Self::Worker(WorkerCommand::RemoteIce { .. }) => write!(f, "Worker(RemoteIce)"),
            Self::Worker(WorkerCommand::Close) => write!(f, "Worker(Close)"),
            Self::Publish { state, reason, .. } => write!(f, "Publish({state}, {reason:?})"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    AwaitingInfo { since: Instant },
    AwaitingOffer { since: Instant },
    Negotiating,
    Playing,
}

#[derive(Debug, Clone)]
struct Watch {
    client: u16,
    /// Known once the stream id is.
    stream: Option<String>,
    phase: Phase,
    candidates: usize,
}

#[derive(Default)]
pub struct StreamRegistry {
    /// Stream id by publisher.
    known: HashMap<u16, String>,
    watch: Option<Watch>,
}

fn join(stream: &str, client: u16, remove: bool) -> Action {
    Action::Send(
        "joinstreamrequest",
        vec![
            ("id", stream.to_owned()),
            ("clid", client.to_string()),
            ("msg", String::new()),
            ("is_remove", if remove { "1" } else { "0" }.to_owned()),
        ],
    )
}

fn reason_code(reason: FailReason) -> &'static str {
    match reason {
        FailReason::Unsupported => "unsupported",
        FailReason::Network => "network",
        FailReason::NoDecoder => "no_decoder",
        // No picture 20 s after the answer: the media path is blocked.
        FailReason::Timeout => "network",
    }
}

impl StreamRegistry {
    pub fn on_event(&mut self, event: &StreamEvent, now: Instant) -> Vec<Action> {
        let notice = &event.control;
        let (client, stream) = (notice.client_id, notice.stream_id.as_str());
        match notice.kind {
            ControlKind::Started | ControlKind::Info => {
                self.known.insert(client, stream.to_owned());
                match &mut self.watch {
                    Some(watch)
                        if watch.client == client
                            && matches!(watch.phase, Phase::AwaitingInfo { .. }) =>
                    {
                        watch.stream = Some(stream.to_owned());
                        watch.phase = Phase::AwaitingOffer { since: now };
                        vec![join(stream, client, false)]
                    }
                    _ => Vec::new(),
                }
            }
            ControlKind::Stopped => {
                if self.known.get(&client).is_some_and(|known| known == stream) {
                    self.known.remove(&client);
                }
                if self.watching(client, stream) {
                    self.watch = None;
                    vec![
                        Action::Worker(WorkerCommand::Close),
                        Action::Publish {
                            client,
                            state: "ended",
                            reason: None,
                        },
                    ]
                } else {
                    Vec::new()
                }
            }
            ControlKind::JoinResponded => {
                let Some(watch) = self.watch.as_mut().filter(|w| {
                    w.client == client
                        && w.stream.as_deref() == Some(stream)
                        && matches!(w.phase, Phase::AwaitingOffer { .. })
                }) else {
                    return Vec::new();
                };
                match (notice.accepted, &event.payload) {
                    (Some(true), Some(offer)) if offer.len() <= MAX_OFFER_BYTES => {
                        watch.phase = Phase::Negotiating;
                        vec![Action::Worker(WorkerCommand::Offer {
                            stream: stream.to_owned(),
                            sdp: offer.clone(),
                        })]
                    }
                    (Some(true), _) => self.fail("unsupported"),
                    _ => {
                        self.watch = None;
                        vec![Action::Publish {
                            client,
                            state: "failed",
                            reason: Some("refused"),
                        }]
                    }
                }
            }
            ControlKind::Signaling => {
                let Some(json) = &event.payload else {
                    return Vec::new();
                };
                let Some(watch) = self.watch.as_mut().filter(|w| {
                    w.client == client
                        && w.stream.as_deref() == Some(stream)
                        && matches!(w.phase, Phase::Negotiating | Phase::Playing)
                }) else {
                    return Vec::new();
                };
                let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
                    return Vec::new();
                };
                let command = value.get("cmd").and_then(|c| c.as_str()).unwrap_or("");
                // The publisher re-offers (new ICE credentials) when the first
                // attempt does not connect: answer it with a fresh peer.
                if matches!(command, "offer" | "reconnectOffer") {
                    let args = value.get("args");
                    let sdp = args.and_then(|a| a.as_str()).or_else(|| {
                        ["offer", "sdp"]
                            .iter()
                            .find_map(|key| args?.get(key)?.as_str())
                    });
                    return match sdp.filter(|sdp| sdp.len() <= MAX_OFFER_BYTES) {
                        Some(sdp) => {
                            watch.phase = Phase::Negotiating;
                            watch.candidates = 0;
                            vec![Action::Worker(WorkerCommand::Offer {
                                stream: stream.to_owned(),
                                sdp: sdp.to_owned(),
                            })]
                        }
                        None => Vec::new(),
                    };
                }
                if command != "iceCandidate"
                    || json.len() > MAX_ICE_JSON
                    || watch.candidates >= MAX_REMOTE_CANDIDATES
                {
                    return Vec::new();
                }
                watch.candidates += 1;
                vec![Action::Worker(WorkerCommand::RemoteIce {
                    stream: stream.to_owned(),
                    json: json.clone(),
                })]
            }
            ControlKind::JoinRequested => Vec::new(),
        }
    }

    pub fn watch(&mut self, client: u16, now: Instant) -> Vec<Action> {
        let mut actions = self.stop();
        let stream = self.known.get(&client).cloned();
        match &stream {
            Some(stream) => actions.push(join(stream, client, false)),
            None => actions.push(Action::Send(
                "requeststreaminfo",
                vec![("clid", client.to_string())],
            )),
        }
        let phase = match stream {
            Some(_) => Phase::AwaitingOffer { since: now },
            None => Phase::AwaitingInfo { since: now },
        };
        self.watch = Some(Watch {
            client,
            stream,
            phase,
            candidates: 0,
        });
        actions.push(Action::Publish {
            client,
            state: "connecting",
            reason: None,
        });
        actions
    }

    /// Leaves the watched stream, if any.
    pub fn stop(&mut self) -> Vec<Action> {
        let Some(watch) = self.watch.take() else {
            return Vec::new();
        };
        let mut actions = vec![Action::Worker(WorkerCommand::Close)];
        if let Some(stream) = &watch.stream {
            actions.push(join(stream, watch.client, true));
        }
        actions
    }

    pub fn on_worker(&mut self, event: WorkerEvent) -> Vec<Action> {
        let stream = match &event {
            WorkerEvent::Answer { stream, .. }
            | WorkerEvent::LocalIce { stream, .. }
            | WorkerEvent::Playing { stream }
            | WorkerEvent::Failed { stream, .. } => stream.clone(),
        };
        let Some(watch) = self
            .watch
            .as_mut()
            .filter(|w| w.stream.as_deref() == Some(stream.as_str()))
        else {
            return Vec::new();
        };
        let client = watch.client;
        let signal = |json: String| {
            Action::Send(
                "streamsignaling",
                vec![
                    ("id", stream.clone()),
                    ("clid", client.to_string()),
                    ("json", json),
                ],
            )
        };
        match event {
            WorkerEvent::Answer { sdp, .. } => vec![signal(
                serde_json::json!({"cmd": "answer", "args": {"answer": sdp}}).to_string(),
            )],
            WorkerEvent::LocalIce { json, .. } => vec![signal(json)],
            WorkerEvent::Playing { .. } => {
                watch.phase = Phase::Playing;
                vec![Action::Publish {
                    client,
                    state: "playing",
                    reason: None,
                }]
            }
            WorkerEvent::Failed { reason, .. } => self.fail(reason_code(reason)),
        }
    }

    pub fn tick(&mut self, now: Instant) -> Vec<Action> {
        let expired = self.watch.as_ref().is_some_and(|w| match w.phase {
            Phase::AwaitingInfo { since } | Phase::AwaitingOffer { since } => {
                now.duration_since(since) >= OFFER_DEADLINE
            }
            _ => false,
        });
        if expired {
            self.fail("timeout")
        } else {
            Vec::new()
        }
    }

    /// Forgets everything (disconnect or channel change); stale events for
    /// old streams then do nothing.
    pub fn reset(&mut self) {
        self.known.clear();
        self.watch = None;
    }

    fn watching(&self, client: u16, stream: &str) -> bool {
        self.watch
            .as_ref()
            .is_some_and(|w| w.client == client && w.stream.as_deref() == Some(stream))
    }

    /// Ends the watch with a failure shown to the user.
    fn fail(&mut self, reason: &'static str) -> Vec<Action> {
        let client = self.watch.as_ref().map(|w| w.client);
        let mut actions = self.stop();
        if let Some(client) = client {
            actions.push(Action::Publish {
                client,
                state: "failed",
                reason: Some(reason),
            });
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn published(actions: &[Action]) -> Vec<(&'static str, Option<&'static str>)> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Publish { state, reason, .. } => Some((*state, *reason)),
                _ => None,
            })
            .collect()
    }

    fn has_close(actions: &[Action]) -> bool {
        actions
            .iter()
            .any(|a| matches!(a, Action::Worker(WorkerCommand::Close)))
    }

    /// Started by 302 and watched, with the offer relayed to the worker.
    fn negotiating(now: Instant) -> StreamRegistry {
        let mut r = StreamRegistry::default();
        r.on_event(&event("notifystreamstarted id=s clid=302"), now);
        r.watch(302, now);
        r.on_event(
            &event("notifyrespondjoinstreamrequest id=s clid=302 decision=1 offer=v=0"),
            now,
        );
        r
    }

    #[test]
    fn watch_sends_join_and_relays_offer() {
        let now = Instant::now();
        let mut r = StreamRegistry::default();
        assert!(r
            .on_event(&event("notifystreamstarted id=s clid=302"), now)
            .is_empty());
        let actions = r.watch(302, now);
        let join = sends(&actions, "joinstreamrequest");
        assert_eq!(join.len(), 1);
        assert_eq!(arg(join[0], "id"), "s");
        assert_eq!(arg(join[0], "clid"), "302");
        assert_eq!(arg(join[0], "msg"), "");
        assert_eq!(arg(join[0], "is_remove"), "0");
        assert_eq!(published(&actions), vec![("connecting", None)]);
        let actions = r.on_event(
            &event("notifyrespondjoinstreamrequest id=s clid=302 decision=1 offer=v=0"),
            now,
        );
        assert!(matches!(
            actions.as_slice(),
            [Action::Worker(WorkerCommand::Offer { stream, sdp })] if stream == "s" && sdp == "v=0"
        ));
    }

    #[test]
    fn answer_and_ice_become_streamsignaling() {
        let now = Instant::now();
        let mut r = negotiating(now);
        let actions = r.on_worker(WorkerEvent::Answer {
            stream: "s".into(),
            sdp: "v=0 answer".into(),
        });
        let signal = sends(&actions, "streamsignaling");
        assert_eq!(arg(signal[0], "id"), "s");
        assert_eq!(arg(signal[0], "clid"), "302");
        let json: serde_json::Value = serde_json::from_str(arg(signal[0], "json")).unwrap();
        assert_eq!(json["cmd"], "answer");
        assert_eq!(json["args"]["answer"], "v=0 answer");

        let ice = r#"notifystreamsignaling id=s clid=302 json={"cmd":"iceCandidate","args":{"sdp":"c","mid":"0","mLine":0}}"#;
        for _ in 0..MAX_REMOTE_CANDIDATES {
            let actions = r.on_event(&event(ice), now);
            assert!(matches!(
                actions.as_slice(),
                [Action::Worker(WorkerCommand::RemoteIce { .. })]
            ));
        }
        assert!(
            r.on_event(&event(ice), now).is_empty(),
            "65th candidate dropped"
        );
    }

    #[test]
    fn a_new_offer_restarts_the_viewer() {
        let now = Instant::now();
        let mut r = negotiating(now);
        // Spellings the official client may use (per the Go reference).
        for (json, sdp) in [
            (r#"{"cmd":"reconnectOffer","args":{"offer":"v=1"}}"#, "v=1"),
            (r#"{"cmd":"offer","args":{"sdp":"v=2"}}"#, "v=2"),
            (r#"{"cmd":"offer","args":"v=3"}"#, "v=3"),
        ] {
            let text = format!("notifystreamsignaling id=s clid=302 json={json}");
            let actions = r.on_event(&event(&text), now);
            assert!(
                matches!(actions.as_slice(), [Action::Worker(WorkerCommand::Offer { stream, sdp: got })] if stream == "s" && got == sdp),
                "{json}: {actions:?}"
            );
        }
        // Candidates of the new session are counted afresh.
        let ice =
            r#"notifystreamsignaling id=s clid=302 json={"cmd":"iceCandidate","args":{"sdp":"c"}}"#;
        for _ in 0..MAX_REMOTE_CANDIDATES {
            assert!(!r.on_event(&event(ice), now).is_empty());
        }
    }

    #[test]
    fn refusal_publishes_failed_refused() {
        let now = Instant::now();
        let mut r = StreamRegistry::default();
        r.on_event(&event("notifystreamstarted id=s clid=302"), now);
        r.watch(302, now);
        let actions = r.on_event(
            &event("notifyrespondjoinstreamrequest id=s clid=302 decision=0"),
            now,
        );
        assert_eq!(published(&actions), vec![("failed", Some("refused"))]);
    }

    #[test]
    fn offer_deadline_15s() {
        let now = Instant::now();
        let mut r = StreamRegistry::default();
        r.on_event(&event("notifystreamstarted id=s clid=302"), now);
        r.watch(302, now);
        assert!(r.tick(now + Duration::from_secs(14)).is_empty());
        let actions = r.tick(now + OFFER_DEADLINE);
        assert_eq!(published(&actions), vec![("failed", Some("timeout"))]);
        assert!(has_close(&actions));
    }

    #[test]
    fn stop_while_negotiating_ends_cleanly() {
        let now = Instant::now();
        let mut r = negotiating(now);
        let actions = r.on_event(&event("notifystreamstopped id=s clid=302"), now);
        assert!(has_close(&actions));
        assert_eq!(published(&actions), vec![("ended", None)]);
        let late = r.on_event(
            &event("notifyrespondjoinstreamrequest id=s clid=302 decision=1 offer=v=0"),
            now,
        );
        assert!(late.is_empty());
        assert!(r
            .on_worker(WorkerEvent::Answer {
                stream: "s".into(),
                sdp: "x".into()
            })
            .is_empty());
    }

    #[test]
    fn second_watch_replaces_first() {
        let now = Instant::now();
        let mut r = negotiating(now);
        r.on_event(&event("notifystreamstarted id=t clid=303"), now);
        let actions = r.watch(303, now);
        let joins = sends(&actions, "joinstreamrequest");
        assert_eq!(joins.len(), 2);
        assert_eq!(
            (arg(joins[0], "clid"), arg(joins[0], "is_remove")),
            ("302", "1")
        );
        assert_eq!(
            (arg(joins[1], "clid"), arg(joins[1], "is_remove")),
            ("303", "0")
        );
        let close = actions
            .iter()
            .position(|a| matches!(a, Action::Worker(WorkerCommand::Close)))
            .unwrap();
        let join = actions
            .iter()
            .position(|a| matches!(a, Action::Send("joinstreamrequest", args) if arg(args, "clid") == "303"))
            .unwrap();
        assert!(close < join);
    }

    #[test]
    fn late_join_asks_for_info_first() {
        let now = Instant::now();
        let mut r = StreamRegistry::default();
        let actions = r.watch(302, now);
        let info = sends(&actions, "requeststreaminfo");
        assert_eq!(arg(info[0], "clid"), "302");
        assert!(sends(&actions, "joinstreamrequest").is_empty());
        let actions = r.on_event(&event("notifystreaminfo streamid=s clid=302"), now);
        let join = sends(&actions, "joinstreamrequest");
        assert_eq!(arg(join[0], "id"), "s");
    }

    #[test]
    fn reset_drops_everything() {
        let now = Instant::now();
        let mut r = negotiating(now);
        r.reset();
        let ice =
            r#"notifystreamsignaling id=s clid=302 json={"cmd":"iceCandidate","args":{"sdp":"c"}}"#;
        assert!(r.on_event(&event(ice), now).is_empty());
        assert!(r
            .on_worker(WorkerEvent::Playing { stream: "s".into() })
            .is_empty());
        assert!(r.tick(now + Duration::from_secs(60)).is_empty());
    }

    #[test]
    fn worker_reports_reach_the_ui() {
        let now = Instant::now();
        let mut r = negotiating(now);
        let actions = r.on_worker(WorkerEvent::Playing { stream: "s".into() });
        assert_eq!(published(&actions), vec![("playing", None)]);
        let actions = r.on_worker(WorkerEvent::Failed {
            stream: "s".into(),
            reason: FailReason::Network,
        });
        assert_eq!(published(&actions), vec![("failed", Some("network"))]);
        let leave = sends(&actions, "joinstreamrequest");
        assert_eq!(arg(leave[0], "is_remove"), "1");
    }

    #[test]
    fn oversized_offer_fails_unsupported() {
        let now = Instant::now();
        let mut r = StreamRegistry::default();
        r.on_event(&event("notifystreamstarted id=s clid=302"), now);
        r.watch(302, now);
        let huge = format!(
            "notifyrespondjoinstreamrequest id=s clid=302 decision=1 offer={}",
            "x".repeat(MAX_OFFER_BYTES + 1)
        );
        // The core parser bounds the whole notice first; either way no offer
        // reaches the worker.
        if let Some(parsed) = StreamEvent::parse(&huge) {
            let actions = r.on_event(&parsed, now);
            assert!(!actions
                .iter()
                .any(|a| matches!(a, Action::Worker(WorkerCommand::Offer { .. }))));
        }
    }
}
