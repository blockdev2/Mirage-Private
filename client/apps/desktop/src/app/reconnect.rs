//! Reconnecting by itself after the connection to a server is lost.
//!
//! The rules live here so they can be tested without a window: which losses
//! are worth another try (not a kick or a ban), how long to wait between tries
//! and when to give up. The app drives it from its frame loop.

use std::time::{Duration, Instant};

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
