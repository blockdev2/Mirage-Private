//! Bounded stream control and redacted signaling transport for diagnostics.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlKind {
    Started,
    Info,
    Stopped,
    JoinRequested,
    JoinResponded,
    Signaling,
    /// A viewer attached to our stream (`notifystreamclientjoined`).
    ClientJoined,
    /// A viewer detached from our stream (`notifystreamclientleft`).
    ClientLeft,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlNotice {
    pub kind: ControlKind,
    pub stream_id: String,
    pub client_id: u16,
    pub accepted: Option<bool>,
    /// `is_remove=1` on a join request: the viewer is leaving, not asking.
    pub removed: bool,
}

impl ControlNotice {
    /// Parse one control record, rejecting ambiguous batches or duplicate keys.
    /// The 16 KiB bound is this parser's policy, not a universal TS server limit.
    pub fn parse(content: &str) -> Option<Self> {
        use tsproto_packets::commands::{CommandItem, CommandParser};

        if content.len() > 16 * 1024 {
            return None;
        }
        let (name, arguments) = CommandParser::new(content.as_bytes());
        let kind = match name {
            b"notifystreamstarted" => ControlKind::Started,
            b"notifystreaminfo" => ControlKind::Info,
            b"notifystreamstopped" => ControlKind::Stopped,
            b"notifyjoinstreamrequest" => ControlKind::JoinRequested,
            b"notifyrespondjoinstreamrequest" => ControlKind::JoinResponded,
            b"notifystreamsignaling" => ControlKind::Signaling,
            b"notifystreamclientjoined" => ControlKind::ClientJoined,
            b"notifystreamclientleft" => ControlKind::ClientLeft,
            _ => return None,
        };
        let (mut stream_id, mut client_id, mut accepted) = (None, None, None);
        let mut removed = false;
        for argument in arguments {
            let CommandItem::Argument(argument) = argument else {
                return None;
            };
            match argument.name() {
                b"id" | b"streamid" => {
                    if stream_id.is_some() {
                        return None;
                    }
                    let id = argument.value().get_str().ok()?.into_owned();
                    if id.is_empty()
                        || id.len() > 128
                        || id.chars().any(char::is_whitespace)
                        || id.chars().any(char::is_control)
                    {
                        return None;
                    }
                    stream_id = Some(id);
                }
                b"clid" => {
                    if client_id.is_some() {
                        return None;
                    }
                    let id: u16 = argument.value().get_str().ok()?.parse().ok()?;
                    if id == 0 {
                        return None;
                    }
                    client_id = Some(id);
                }
                b"decision" => {
                    if accepted.is_some() {
                        return None;
                    }
                    accepted = Some(match argument.value().get_str().ok()?.as_ref() {
                        "0" => false,
                        "1" => true,
                        _ => return None,
                    });
                }
                b"is_remove" => removed = argument.value().get_str().ok()?.as_ref() == "1",
                _ => {}
            }
        }
        if kind == ControlKind::JoinResponded && accepted.is_none() {
            return None;
        }
        Some(Self {
            kind,
            stream_id: stream_id?,
            client_id: client_id?,
            accepted,
            removed,
        })
    }
}

/// Sensitive transport data, deliberately redacted from ordinary diagnostics.
pub struct StreamEvent {
    pub control: ControlNotice,
    pub payload: Option<String>,
}

impl std::fmt::Debug for StreamEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamEvent")
            .field("control", &self.control)
            .field("payload", &self.payload.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl StreamEvent {
    pub fn parse(content: &str) -> Option<Self> {
        use tsproto_packets::commands::{CommandItem, CommandParser};
        let control = ControlNotice::parse(content)?;
        let key: &[u8] = match control.kind {
            ControlKind::JoinResponded => b"offer",
            ControlKind::Signaling => b"json",
            _ => {
                return Some(Self {
                    control,
                    payload: None,
                })
            }
        };
        let mut payload = None;
        for item in CommandParser::new(content.as_bytes()).1 {
            let CommandItem::Argument(argument) = item else {
                return None;
            };
            if argument.name() == key {
                if payload.is_some() {
                    return None;
                }
                payload = Some(argument.value().get_str().ok()?.into_owned());
            }
        }
        if (control.kind == ControlKind::Signaling || control.accepted == Some(true))
            && payload.as_ref().is_none_or(|text| text.is_empty())
        {
            return None;
        }
        Some(Self { control, payload })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signaling_decodes_payload_but_debug_redacts_it() {
        let event = StreamEvent::parse("notifystreamsignaling id=42 clid=9 json={\"cmd\":\"answer\",\"args\":{\"answer\":\"private-fixture\"}}").unwrap();
        assert_eq!(event.control.kind, ControlKind::Signaling);
        assert!(event.payload.as_ref().unwrap().contains("private-fixture"));
        assert!(!format!("{event:?}").contains("private-fixture"));
        let offer = StreamEvent::parse(
            "notifyrespondjoinstreamrequest id=42 clid=9 decision=1 offer=v=0\\r\\n",
        )
        .unwrap();
        assert_eq!(offer.payload.as_deref(), Some("v=0\r\n"));
    }

    #[test]
    fn signaling_rejects_duplicate_or_missing_payloads() {
        for text in [
            "notifystreamsignaling id=42 clid=9",
            "notifystreamsignaling id=42 clid=9 json=a json=b",
            "notifyrespondjoinstreamrequest id=42 clid=9 decision=1",
            "notifystreamsignaling id=42 clid=9 json=a|json=b",
        ] {
            assert!(StreamEvent::parse(text).is_none());
        }
    }

    #[test]
    fn projects_control_fields_without_retaining_offer() {
        let notice = ControlNotice::parse(
            "notifyrespondjoinstreamrequest id=stream-1 clid=42 decision=0 offer=private-fixture",
        )
        .unwrap();
        assert_eq!(notice.kind, ControlKind::JoinResponded);
        assert_eq!(notice.stream_id, "stream-1");
        assert_eq!(notice.client_id, 42);
        assert_eq!(notice.accepted, Some(false));
        assert!(!format!("{notice:?}").contains("private-fixture"));
    }

    #[test]
    fn unescapes_opaque_id_and_recognizes_lifecycle() {
        for (name, kind) in [
            ("notifystreamstarted", ControlKind::Started),
            ("notifystreamstopped", ControlKind::Stopped),
            ("notifyjoinstreamrequest", ControlKind::JoinRequested),
        ] {
            let notice = ControlNotice::parse(&format!("{name} id=stream\\p1 clid=7")).unwrap();
            assert_eq!(notice.kind, kind);
            assert_eq!(notice.stream_id, "stream|1");
            assert_eq!(notice.accepted, None);
        }
    }

    #[test]
    fn accepts_info_alias_and_positive_admission() {
        let info =
            ControlNotice::parse("notifystreaminfo streamid=42 clid=9 name=fixture").unwrap();
        assert_eq!(info.kind, ControlKind::Info);
        assert_eq!(info.stream_id, "42");
        assert_eq!(info.client_id, 9);
        let admitted = ControlNotice::parse(
            "notifyrespondjoinstreamrequest id=42 clid=9 decision=1 offer=not-retained",
        )
        .unwrap();
        assert_eq!(admitted.accepted, Some(true));
    }

    #[test]
    fn recognizes_viewer_attach_detach_and_removal() {
        for (name, kind) in [
            ("notifystreamclientjoined", ControlKind::ClientJoined),
            ("notifystreamclientleft", ControlKind::ClientLeft),
        ] {
            let notice = ControlNotice::parse(&format!("{name} id=s clid=7")).unwrap();
            assert_eq!(notice.kind, kind);
            assert!(!notice.removed);
        }
        let leaving =
            ControlNotice::parse("notifyjoinstreamrequest id=s clid=7 is_remove=1").unwrap();
        assert!(leaving.removed);
        let asking =
            ControlNotice::parse("notifyjoinstreamrequest id=s clid=7 is_remove=0").unwrap();
        assert!(!asking.removed);
    }

    #[test]
    fn rejects_ambiguous_invalid_or_unbounded_notices() {
        for text in [
            "notifystreamstarted id=one clid=1 clid=2",
            "notifystreamstarted id=one streamid=two clid=1",
            "notifystreamstarted id=one clid=1|id=two clid=2",
            "notifystreamstarted id=one",
            "notifystreamstarted id=one clid=65536",
            "notifystreamstarted id=one clid=0",
            "notifyrespondjoinstreamrequest id=one clid=1 decision=2",
            "notifyrespondjoinstreamrequest id=one clid=1",
            "notifyrespondjoinstreamrequest id=one clid=1 decision=0 decision=1",
            "notifychatlogintoken id=one clid=1",
            "notifystreamstarted id=bad\\nvalue clid=1",
        ] {
            assert!(ControlNotice::parse(text).is_none());
        }
        assert!(ControlNotice::parse(&format!(
            "notifystreamstarted id={} clid=1",
            "x".repeat(129)
        ))
        .is_none());
        assert!(ControlNotice::parse(&format!(
            "notifystreamstarted id=one clid=1 name={}",
            "x".repeat(16384)
        ))
        .is_none());
    }
}
