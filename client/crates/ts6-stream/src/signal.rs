//! The JSON that TeamSpeak 6 clients exchange inside `streamsignaling`.
//!
//! Shapes: `{"cmd":"answer","args":{"answer":"<sdp>"}}` and
//! `{"cmd":"iceCandidate","args":{"sdp":"<candidate>","mid":"0","mLine":0}}`.
//! The wire shape belongs to TeamSpeak, so it is built and read as plain JSON
//! values rather than derived from a Rust type.

use serde_json::{json, Value};
use std::fmt;

/// Same bound as the control-notice parser in `ts6-core`.
pub const MAX_SIGNAL_BYTES: usize = 16 * 1024;

/// One relayed signaling message. SDP carries addresses and ICE credentials,
/// so `Debug` never prints it.
#[derive(Clone, PartialEq, Eq)]
pub enum Signal {
    Answer(String),
    IceCandidate {
        sdp: String,
        mid: Option<String>,
        m_line: Option<u16>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalError {
    TooLarge,
    Malformed,
    UnknownCommand,
}

impl fmt::Display for SignalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TooLarge => "signaling payload exceeds the size bound",
            Self::Malformed => "signaling payload is malformed",
            Self::UnknownCommand => "signaling command is not supported",
        })
    }
}

impl std::error::Error for SignalError {}

impl fmt::Debug for Signal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Answer(_) => f.write_str("Signal::Answer(<redacted>)"),
            Self::IceCandidate { .. } => f.write_str("Signal::IceCandidate(<redacted>)"),
        }
    }
}

impl Signal {
    pub fn to_json(&self) -> String {
        match self {
            Self::Answer(sdp) => json!({"cmd": "answer", "args": {"answer": sdp}}),
            Self::IceCandidate { sdp, mid, m_line } => {
                json!({"cmd": "iceCandidate", "args": {"sdp": sdp, "mid": mid, "mLine": m_line}})
            }
        }
        .to_string()
    }

    pub fn parse(json: &str) -> Result<Self, SignalError> {
        if json.len() > MAX_SIGNAL_BYTES {
            return Err(SignalError::TooLarge);
        }
        let value: Value = serde_json::from_str(json).map_err(|_| SignalError::Malformed)?;
        let command = value
            .get("cmd")
            .and_then(Value::as_str)
            .ok_or(SignalError::Malformed)?;
        let args = value.get("args").ok_or(SignalError::Malformed)?;
        let text = |name: &str| {
            args.get(name)
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        match command {
            "answer" => Ok(Self::Answer(text("answer").ok_or(SignalError::Malformed)?)),
            "iceCandidate" => Ok(Self::IceCandidate {
                sdp: text("sdp").ok_or(SignalError::Malformed)?,
                mid: args.get("mid").and_then(Value::as_str).map(str::to_owned),
                m_line: match args.get("mLine") {
                    None | Some(Value::Null) => None,
                    Some(number) => Some(
                        number
                            .as_u64()
                            .and_then(|n| u16::try_from(n).ok())
                            .ok_or(SignalError::Malformed)?,
                    ),
                },
            }),
            _ => Err(SignalError::UnknownCommand),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_signals_survive_a_round_trip() {
        let answer = Signal::Answer("v=0\r\n".into());
        assert_eq!(Signal::parse(&answer.to_json()), Ok(answer));
        let candidate = Signal::IceCandidate {
            sdp: "candidate:1 1 udp 1 127.0.0.1 9 typ host".into(),
            mid: Some("0".into()),
            m_line: Some(0),
        };
        assert_eq!(Signal::parse(&candidate.to_json()), Ok(candidate));
    }

    #[test]
    fn reads_the_shapes_teamspeak_sends() {
        assert_eq!(
            Signal::parse(r#"{"cmd":"answer","args":{"answer":"v=0"}}"#),
            Ok(Signal::Answer("v=0".into()))
        );
        assert_eq!(
            Signal::parse(
                r#"{"cmd":"iceCandidate","args":{"sdp":"candidate:1","mid":null,"mLine":null}}"#
            ),
            Ok(Signal::IceCandidate {
                sdp: "candidate:1".into(),
                mid: None,
                m_line: None
            })
        );
    }

    #[test]
    fn rejects_what_it_cannot_trust() {
        for text in [
            "",
            "[]",
            r#"{"cmd":"answer"}"#,
            r#"{"cmd":"answer","args":{"answer":""}}"#,
            r#"{"cmd":"answer","args":{"answer":7}}"#,
            r#"{"cmd":"iceCandidate","args":{"sdp":"c","mLine":70000}}"#,
            r#"{"cmd":"iceCandidate","args":{"sdp":"c","mLine":-1}}"#,
        ] {
            assert_eq!(Signal::parse(text), Err(SignalError::Malformed), "{text}");
        }
        assert_eq!(
            Signal::parse(r#"{"cmd":"hangup","args":{}}"#),
            Err(SignalError::UnknownCommand)
        );
        let huge = format!(
            r#"{{"cmd":"answer","args":{{"answer":"{}"}}}}"#,
            "x".repeat(MAX_SIGNAL_BYTES)
        );
        assert_eq!(Signal::parse(&huge), Err(SignalError::TooLarge));
    }

    #[test]
    fn debug_never_prints_the_payload() {
        let text = format!("{:?}", Signal::Answer("secret-ice-ufrag".into()));
        assert!(!text.contains("secret"));
    }
}
