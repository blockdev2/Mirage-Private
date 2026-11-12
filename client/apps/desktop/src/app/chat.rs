//! Room chat history and delivery tracking.
//!
//! The server accepts a message with a reply to the command and, as in the
//! standard TeamSpeak protocol, does not echo it back to the sender. So a
//! message we send is pending until the bridge reports that the server accepted
//! it, and failed if the server refuses it or never answers. An echo, if one
//! ever comes, only confirms the same message and is never shown twice.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How long to wait for the server's answer before giving up on a message.
/// The bridge itself stops waiting after 15 seconds.
pub const SEND_TIMEOUT: Duration = Duration::from_secs(20);
/// An echo this long after sending still counts as the echo of that message.
const ECHO_WINDOW: Duration = Duration::from_secs(30);
/// Oldest messages are dropped beyond this many.
const MAX_ENTRIES: usize = 500;
/// TeamSpeak refuses text messages longer than this many bytes.
pub const MAX_MESSAGE_BYTES: usize = 1024;

/// Local wall-clock time of a message, as hour and minute.
pub type Clock = (u8, u8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Everyone in a channel.
    Channel,
    /// Everyone on the server.
    Server,
    /// Only the two of us.
    Private,
}

impl Scope {
    /// Parses the bridge's `scope` field.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "channel" => Some(Self::Channel),
            "server" => Some(Self::Server),
            "private" => Some(Self::Private),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// The server never answered.
    NoAnswer,
    /// The server refused it.
    Rejected,
    /// There was no connection to send it on.
    Offline,
}

impl Failure {
    /// Parses the bridge's `chat_failed` code.
    pub fn parse(code: &str) -> Self {
        match code {
            "not_connected" => Self::Offline,
            _ => Self::Rejected,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Handed to the bridge, waiting for the server's answer.
    Pending(Instant),
    /// The server accepted it (every message from someone else is delivered).
    Delivered,
    Failed(Failure),
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub scope: Scope,
    /// Channel the message belongs to; `None` for server-wide and private ones.
    pub channel: Option<u64>,
    pub from_id: Option<u16>,
    pub author: String,
    pub text: String,
    pub mine: bool,
    pub delivery: Delivery,
    pub time: Clock,
    /// When we sent it; `None` for messages from others.
    sent: Option<Instant>,
    /// Whether the server's echo of it has been seen.
    echoed: bool,
}

impl Entry {
    /// True when this entry continues `previous`: same author, same scope, and
    /// sent in the same minute.
    pub fn groups_with(&self, previous: &Entry) -> bool {
        self.mine == previous.mine
            && self.scope == previous.scope
            && self.from_id == previous.from_id
            && self.author == previous.author
            && self.time == previous.time
    }
}

/// Everything about a message that arrived from the server.
pub struct Incoming {
    pub scope: Scope,
    pub channel: Option<u64>,
    pub from_id: Option<u16>,
    pub author: String,
    pub text: String,
    pub mine: bool,
}

#[derive(Default)]
pub struct ChatLog {
    entries: VecDeque<Entry>,
}

impl ChatLog {
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// A message from the server. If it is the echo of one we sent, it only
    /// marks that one as echoed instead of adding a duplicate.
    pub fn receive(&mut self, message: Incoming, now: Instant, time: Clock) {
        if message.mine {
            let sent = self.entries.iter_mut().find(|entry| {
                entry.mine
                    && !entry.echoed
                    && entry.scope == message.scope
                    && entry.text == message.text
                    && entry
                        .sent
                        .is_some_and(|at| now.duration_since(at) <= ECHO_WINDOW)
            });
            if let Some(entry) = sent {
                entry.echoed = true;
                if !matches!(
                    entry.delivery,
                    Delivery::Failed(Failure::Rejected | Failure::Offline)
                ) {
                    entry.delivery = Delivery::Delivered;
                }
                return;
            }
        }
        self.push(Entry {
            scope: message.scope,
            channel: message.channel,
            from_id: message.from_id,
            author: message.author,
            text: message.text,
            mine: message.mine,
            delivery: Delivery::Delivered,
            time,
            sent: None,
            echoed: false,
        });
    }

    /// A channel message we have just handed to the bridge.
    pub fn sent(
        &mut self,
        channel: Option<u64>,
        from_id: Option<u16>,
        author: String,
        text: String,
        now: Instant,
        time: Clock,
    ) {
        self.push(Entry {
            scope: Scope::Channel,
            channel,
            from_id,
            author,
            text,
            mine: true,
            delivery: Delivery::Pending(now),
            time,
            sent: Some(now),
            echoed: false,
        });
    }

    /// The server accepted the oldest pending message.
    pub fn deliver_oldest_pending(&mut self) {
        if let Some(entry) = self.oldest_pending() {
            entry.delivery = Delivery::Delivered;
        }
    }

    /// The bridge reported that the oldest pending message did not leave.
    pub fn fail_oldest_pending(&mut self, failure: Failure) {
        if let Some(entry) = self.oldest_pending() {
            entry.delivery = Delivery::Failed(failure);
        }
    }

    fn oldest_pending(&mut self) -> Option<&mut Entry> {
        self.entries
            .iter_mut()
            .find(|entry| matches!(entry.delivery, Delivery::Pending(_)))
    }

    /// Marks messages whose answer is overdue. Returns whether anything changed.
    pub fn expire(&mut self, now: Instant) -> bool {
        let mut changed = false;
        for entry in &mut self.entries {
            if let Delivery::Pending(at) = entry.delivery {
                if now.duration_since(at) >= SEND_TIMEOUT {
                    entry.delivery = Delivery::Failed(Failure::NoAnswer);
                    changed = true;
                }
            }
        }
        changed
    }

    /// How long until the next pending message times out, if any.
    pub fn next_expiry(&self, now: Instant) -> Option<Duration> {
        self.entries
            .iter()
            .filter_map(|entry| match entry.delivery {
                Delivery::Pending(at) => Some(SEND_TIMEOUT.saturating_sub(now.duration_since(at))),
                _ => None,
            })
            .min()
    }

    /// The most recent messages of the conversation shown for `channel`:
    /// that channel's messages plus server-wide and private ones.
    pub fn visible(&self, channel: Option<u64>, limit: usize) -> Vec<Entry> {
        let mut shown: Vec<Entry> = self
            .entries
            .iter()
            .rev()
            .filter(|entry| entry.scope != Scope::Channel || entry.channel == channel)
            .take(limit)
            .cloned()
            .collect();
        shown.reverse();
        shown
    }

    fn push(&mut self, entry: Entry) {
        if self.entries.len() >= MAX_ENTRIES {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIME: Clock = (11, 17);

    fn incoming(author: &str, id: u16, text: &str, mine: bool) -> Incoming {
        Incoming {
            scope: Scope::Channel,
            channel: Some(1),
            from_id: Some(id),
            author: author.into(),
            text: text.into(),
            mine,
        }
    }

    fn log_with_pending(text: &str, at: Instant) -> ChatLog {
        let mut log = ChatLog::default();
        log.sent(Some(1), Some(7), "eu".into(), text.into(), at, TIME);
        log
    }

    #[test]
    fn the_server_accepting_the_command_delivers_the_message() {
        let mut log = log_with_pending("oi", Instant::now());
        log.deliver_oldest_pending();
        assert_eq!(log.visible(Some(1), 10)[0].delivery, Delivery::Delivered);
    }

    #[test]
    fn deliveries_follow_the_order_of_sending() {
        let now = Instant::now();
        let mut log = log_with_pending("primeira", now);
        log.sent(Some(1), Some(7), "eu".into(), "segunda".into(), now, TIME);
        log.deliver_oldest_pending();
        let shown = log.visible(Some(1), 10);
        assert_eq!(shown[0].delivery, Delivery::Delivered);
        assert!(matches!(shown[1].delivery, Delivery::Pending(_)));
    }

    #[test]
    fn an_echo_of_a_sent_message_is_never_shown_twice() {
        let now = Instant::now();
        let mut log = log_with_pending("oi", now);
        log.deliver_oldest_pending();
        log.receive(incoming("eu", 7, "oi", true), now, TIME);
        assert_eq!(log.visible(Some(1), 10).len(), 1);
    }

    #[test]
    fn an_echo_also_delivers_a_message_still_pending() {
        let now = Instant::now();
        let mut log = log_with_pending("oi", now);
        log.receive(incoming("eu", 7, "oi", true), now, TIME);
        let shown = log.visible(Some(1), 10);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].delivery, Delivery::Delivered);
    }

    #[test]
    fn a_late_echo_is_a_new_message_from_another_session_of_ours() {
        let start = Instant::now();
        let mut log = log_with_pending("oi", start);
        log.deliver_oldest_pending();
        log.receive(
            incoming("eu", 7, "oi", true),
            start + ECHO_WINDOW + Duration::from_secs(1),
            TIME,
        );
        assert_eq!(log.visible(Some(1), 10).len(), 2);
    }

    #[test]
    fn a_message_without_an_answer_fails_instead_of_staying_pending() {
        let start = Instant::now();
        let mut log = log_with_pending("oi", start);
        assert!(!log.expire(start + SEND_TIMEOUT - Duration::from_millis(1)));
        assert!(log.expire(start + SEND_TIMEOUT));
        assert_eq!(
            log.visible(Some(1), 10)[0].delivery,
            Delivery::Failed(Failure::NoAnswer)
        );
    }

    #[test]
    fn a_refused_send_fails_the_oldest_pending_message() {
        let now = Instant::now();
        let mut log = log_with_pending("primeira", now);
        log.sent(Some(1), Some(7), "eu".into(), "segunda".into(), now, TIME);
        log.fail_oldest_pending(Failure::Rejected);
        let shown = log.visible(Some(1), 10);
        assert_eq!(shown[0].delivery, Delivery::Failed(Failure::Rejected));
        assert!(matches!(shown[1].delivery, Delivery::Pending(_)));
    }

    #[test]
    fn messages_from_others_are_delivered_and_never_merged() {
        let mut log = ChatLog::default();
        let now = Instant::now();
        log.receive(incoming("ana", 3, "oi", false), now, TIME);
        log.receive(incoming("ana", 3, "oi", false), now, TIME);
        assert_eq!(log.visible(Some(1), 10).len(), 2);
    }

    #[test]
    fn channel_messages_stay_in_their_channel_but_server_ones_follow_you() {
        let mut log = ChatLog::default();
        let now = Instant::now();
        log.receive(incoming("ana", 3, "no canal 1", false), now, TIME);
        let mut server = incoming("ana", 3, "para todos", false);
        server.scope = Scope::Server;
        server.channel = None;
        log.receive(server, now, TIME);
        assert_eq!(log.visible(Some(1), 10).len(), 2);
        let in_two = log.visible(Some(2), 10);
        assert_eq!(in_two.len(), 1);
        assert_eq!(in_two[0].text, "para todos");
    }

    #[test]
    fn history_is_capped_and_keeps_the_newest() {
        let mut log = ChatLog::default();
        let now = Instant::now();
        for index in 0..(MAX_ENTRIES + 20) {
            let mut message = incoming("ana", 3, &index.to_string(), false);
            message.scope = Scope::Server;
            log.receive(message, now, TIME);
        }
        let shown = log.visible(None, MAX_ENTRIES + 20);
        assert_eq!(shown.len(), MAX_ENTRIES);
        assert_eq!(shown.last().unwrap().text, (MAX_ENTRIES + 19).to_string());
    }

    #[test]
    fn next_expiry_counts_down_only_for_pending_messages() {
        let start = Instant::now();
        let mut log = log_with_pending("oi", start);
        let left = log.next_expiry(start + Duration::from_secs(2)).unwrap();
        assert_eq!(left, SEND_TIMEOUT - Duration::from_secs(2));
        log.deliver_oldest_pending();
        assert!(log.next_expiry(start).is_none());
    }

    #[test]
    fn runs_of_one_author_group_within_a_minute() {
        let mut log = ChatLog::default();
        let now = Instant::now();
        log.receive(incoming("ana", 3, "a", false), now, (11, 17));
        log.receive(incoming("ana", 3, "b", false), now, (11, 17));
        log.receive(incoming("ana", 3, "c", false), now, (11, 18));
        log.receive(incoming("bia", 4, "d", false), now, (11, 18));
        let shown = log.visible(Some(1), 10);
        assert!(shown[1].groups_with(&shown[0]));
        assert!(!shown[2].groups_with(&shown[1]));
        assert!(!shown[3].groups_with(&shown[2]));
    }
}
