//! Reconnecting by itself after the connection to a server is lost.
//!
//! The rules live here so they can be tested without a window: which losses
//! are worth another try (not a kick or a ban), how long to wait between tries
//! and when to give up. The app drives it from its frame loop.

use std::time::{Duration, Instant};

use crate::protocol::ChannelInfo;

/// Attempts before giving up (about three minutes in all).
pub const MAX_ATTEMPTS: u32 = 8;

/// Wait before attempt number `attempt` (0 is the first): 3, 6, 12, 24, then
/// 30 seconds. A server keeps the previous session of an identity for about
/// 30 seconds, so the early tries may be refused; the later ones are not.
pub fn delay(attempt: u32) -> Duration {
    Duration::from_secs((3u64 << attempt.min(4)).min(30))
}

/// Whether a lost connection is worth reconnecting: anything but being
/// removed on purpose. A server shutdown counts (it may come back).
pub fn is_recoverable(error: &str) -> bool {
    error.starts_with("connection closed") && !error.contains("kicked") && !error.contains("banned")
}

/// The channel to rejoin after a reconnection: the one we were in, when it
/// still exists, needs no password and is not where we already are.
pub fn rejoin_target(channels: &[ChannelInfo], wanted: u64, current: Option<u64>) -> Option<u64> {
    let channel = channels.iter().find(|c| c.id == wanted)?;
    (!channel.has_password && current != Some(wanted)).then_some(wanted)
}

/// Whether a failed attempt is worth repeating. A ban, a wrong password or a
/// too-low identity level will not pass by waiting, and trying again only
/// teaches the server to ban the address.
pub fn is_worth_retrying(error: &str) -> bool {
    let error = error.to_lowercase();
    !["banned", "password", "securitylevel", "security level"]
        .iter()
        .any(|word| error.contains(word))
}

/// A reconnection in progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconnect {
    pub address: String,
    /// Attempts that have failed so far.
    pub attempt: u32,
    /// When to try next; `None` while an attempt is running.
    pub at: Option<Instant>,
}

impl Reconnect {
    pub fn new(address: String, now: Instant) -> Self {
        Self {
            address,
            attempt: 0,
            at: Some(now + delay(0)),
        }
    }

    pub fn due(&self, now: Instant) -> bool {
        self.at.is_some_and(|at| now >= at)
    }

    /// An attempt has started: wait for its answer.
    pub fn started(&mut self) {
        self.at = None;
    }

    /// The attempt failed: the next one, or `None` once the tries are used up.
    pub fn failed(mut self, now: Instant) -> Option<Self> {
        self.attempt += 1;
        if self.attempt >= MAX_ATTEMPTS {
            return None;
        }
        self.at = Some(now + delay(self.attempt));
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wait_grows_and_stops_at_thirty_seconds() {
        let waits: Vec<u64> = (0..MAX_ATTEMPTS).map(|n| delay(n).as_secs()).collect();
        assert_eq!(waits, [3, 6, 12, 24, 30, 30, 30, 30]);
    }

    #[test]
    fn being_removed_on_purpose_is_not_recovered() {
        assert!(is_recoverable("connection closed"));
        assert!(is_recoverable(
            "connection closed: the connection timed out"
        ));
        assert!(is_recoverable("connection closed: the server shut down"));
        assert!(!is_recoverable(
            "connection closed: kicked from the server by Ana"
        ));
        assert!(!is_recoverable("connection closed: banned from the server"));
        // A failed first connection is not a lost one.
        assert!(!is_recoverable("handshake failed"));
    }

    fn channel(id: u64, has_password: bool) -> ChannelInfo {
        ChannelInfo {
            id,
            parent: 0,
            name: format!("c{id}"),
            topic: None,
            codec: String::new(),
            clients: 0,
            has_password,
        }
    }

    #[test]
    fn the_old_channel_is_rejoined_only_when_it_is_open_and_elsewhere() {
        let channels = [channel(1, false), channel(2, false), channel(3, true)];
        assert_eq!(rejoin_target(&channels, 2, Some(1)), Some(2));
        // Already there, locked, or gone: stay put.
        assert_eq!(rejoin_target(&channels, 1, Some(1)), None);
        assert_eq!(rejoin_target(&channels, 3, Some(1)), None);
        assert_eq!(rejoin_target(&channels, 9, Some(1)), None);
    }

    #[test]
    fn permanent_refusals_are_not_retried() {
        assert!(is_worth_retrying(
            "handshake failed: Os { kind: ConnectionReset }"
        ));
        assert!(is_worth_retrying("handshake failed: timed out"));
        assert!(!is_worth_retrying("handshake failed: ConnectFailedBanned"));
        assert!(!is_worth_retrying("server rejected: ServerPasswordWrong"));
        assert!(!is_worth_retrying("identity security level too low"));
    }

    #[test]
    fn it_gives_up_after_the_last_attempt() {
        let now = Instant::now();
        let mut state = Some(Reconnect::new("127.0.0.1:9987".into(), now));
        let mut failures = 0;
        while let Some(current) = state {
            assert!(current.due(now + Duration::from_secs(31)));
            failures += 1;
            state = current.failed(now);
        }
        assert_eq!(failures, MAX_ATTEMPTS);
    }

    #[test]
    fn a_running_attempt_is_never_due() {
        let now = Instant::now();
        let mut state = Reconnect::new("a".into(), now);
        assert!(!state.due(now));
        assert!(state.due(now + Duration::from_secs(3)));
        state.started();
        assert!(!state.due(now + Duration::from_secs(3600)));
    }
}
