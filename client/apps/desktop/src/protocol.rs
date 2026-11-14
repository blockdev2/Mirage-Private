#![allow(dead_code)] // Wire mirror: not every field is read yet.
//! Wire types for the `ts6-bridge` JSON protocol.
//!
//! These mirror `client/crates/ts6-bridge/src/protocol.rs` and
//! `client/apps/ui/src/types.ts`. The bridge serializes `ts6-core` types that
//! only implement `Serialize`, so the desktop client keeps its own owned copies.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Device {
    pub name: String,
    pub is_default: bool,
    pub channels: u16,
    pub sample_rate: u32,
    pub sample_format: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DeviceLists {
    pub inputs: Vec<Device>,
    pub outputs: Vec<Device>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChannelInfo {
    pub id: u64,
    pub parent: u64,
    pub name: String,
    pub topic: Option<String>,
    pub codec: String,
    pub clients: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClientInfo {
    pub id: u16,
    pub channel_id: u64,
    pub name: String,
    pub uid: Option<String>,
    pub input_muted: bool,
    pub output_muted: bool,
    pub is_self: bool,
    /// Avatar flag; older bridges do not send it.
    #[serde(default)]
    pub avatar_hash: Option<String>,
    /// Sharing a TS6 stream; older bridges do not send it.
    #[serde(default)]
    pub is_streaming: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    pub platform: String,
    pub max_clients: u16,
    pub welcome_message: String,
    #[serde(default)]
    pub icon_id: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Snapshot {
    pub own_client_id: u16,
    pub own_uid: Option<String>,
    pub server: ServerInfo,
    pub channels: Vec<ChannelInfo>,
    pub clients: Vec<ClientInfo>,
}

/// Connection statistics of one client (TeamSpeak's connection info).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ConnectionInfo {
    pub client_id: u16,
    pub ping_ms: Option<f64>,
    pub ping_deviation_ms: Option<f64>,
    pub loss_incoming: Option<f32>,
    pub loss_outgoing: f32,
    pub idle_secs: u64,
    pub connected_secs: Option<u64>,
    pub bytes_received: u64,
    pub bytes_sent: u64,
    pub address: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct StatusEvent {
    #[serde(default)]
    pub session_id: Option<String>,
    pub connected: bool,
    pub address: Option<String>,
    pub nickname: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AudioEvent {
    pub rx_packets: u64,
    pub tx_frames: u64,
    pub ptt_down: bool,
    pub speaking: Vec<u16>,
    /// Whether our own voice is going out. Older bridges do not send it.
    #[serde(default)]
    pub transmitting: bool,
    /// Microphone level in dBFS.
    #[serde(default)]
    pub input_level_db: Option<f32>,
    pub capture: Option<String>,
    pub playback: Option<String>,
    pub tone: bool,
    pub ptt_binding: Option<String>,
    /// Our round-trip time to the server. Older bridges do not send it.
    #[serde(default)]
    pub ping_ms: Option<f64>,
    /// Our packet loss towards the server, 0..1.
    #[serde(default)]
    pub loss_out: Option<f32>,
    /// Packets lost and counted since the connection began.
    #[serde(default)]
    pub lost_packets: Option<u64>,
    #[serde(default)]
    pub counted_packets: Option<u64>,
    /// Voice level (RMS, 0..1) of each peer talking. Older bridges omit it.
    #[serde(default)]
    /// Keyed by client id as text: JSON map keys are strings, and this
    /// internally tagged event cannot turn them back into numbers.
    pub levels: std::collections::HashMap<String, f32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LogLine {
    pub level: String,
    pub text: String,
    pub ts: String,
}

/// Events pushed by the bridge. The `ev` field is the discriminant.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "ev", rename_all = "snake_case")]
pub enum BridgeEvent {
    Chat(ChatMessage),
    /// Answer to one `chat` command; `code` is absent on older bridges.
    ChatResult {
        request_id: String,
        error: Option<String>,
        #[serde(default)]
        code: Option<String>,
    },
    Hello {
        version: String,
        #[serde(default)]
        channel_chat: bool,
        devices: DeviceLists,
    },
    Status(StatusEvent),
    State {
        snapshot: Snapshot,
    },
    Log(LogLine),
    /// Boxed: it is by far the largest event and the queue holds many.
    Audio(Box<AudioEvent>),
    Devices {
        devices: DeviceLists,
    },
    ConnectionInfo {
        client: u16,
        info: Option<ConnectionInfo>,
    },
    /// Answer to `chat_fetch`: a cached attachment, or `None` on failure.
    ChatFile {
        channel: String,
        path: String,
        file: Option<String>,
        #[serde(default)]
        kind: Option<String>,
    },
    /// Local paths of cached avatars (by client uid) and of the server icon.
    Media {
        #[serde(default)]
        avatars: std::collections::HashMap<String, String>,
        #[serde(default)]
        server_icon: Option<String>,
    },
    /// Progress of the stream (screen share) being watched.
    StreamState(StreamState),
    /// Progress of our own screen share.
    ShareState(ShareState),
}

/// `state`: starting, live, stopped or failed; `reason` (when failed):
/// unsupported, capture, timeout, busy or offline; `viewers` are connected.
#[derive(Debug, Clone, Deserialize)]
pub struct ShareState {
    pub state: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub viewers: usize,
}

/// `state`: connecting, playing, ended or failed; `reason` (when failed):
/// refused, timeout, unsupported, network or no_decoder.
#[derive(Debug, Clone, Deserialize)]
pub struct StreamState {
    pub client: u16,
    pub state: String,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Commands sent to the bridge. The `op` field is the discriminant.
///
/// No `Debug` derive on purpose: `Connect` carries a server password.
#[derive(Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Command {
    Chat {
        session_id: String,
        channel: String,
        text: String,
        request_id: String,
    },
    /// Send a local file to the current channel (photo, GIF or any file).
    ChatAttach {
        session_id: String,
        channel: String,
        path: String,
        request_id: String,
    },
    /// Download an attachment referenced by a message into the bridge cache.
    ChatFetch {
        channel: String,
        path: String,
    },
    Connect {
        address: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        nickname: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        server_password: Option<String>,
    },
    Disconnect,
    Join {
        channel: String,
    },
    Nick {
        name: String,
    },
    /// The bridge treats an omitted field as `false`, so callers should send both.
    Mute {
        input: bool,
        output: bool,
    },
    Audio {
        enabled: bool,
        tone: bool,
        ptt: String,
        input_device: String,
        output_device: String,
        voice_activation: bool,
        vad_threshold: f32,
        noise_suppression: u8,
        monitor: bool,
    },
    Devices,
    Refresh,
    /// Playback volume of one client: 1.0 unchanged, up to 4.0.
    Volume {
        client: u16,
        volume: f32,
    },
    /// Watch a client's connection statistics; None stops.
    ConnectionInfo {
        client: Option<u16>,
    },
    /// Publish a local image file as our avatar; None removes it.
    SetAvatar {
        path: Option<String>,
    },
    /// Watch the screen stream this client is sharing.
    WatchStream {
        client: u16,
    },
    StopWatching,
    /// Share a monitor (zero-based index) or one window (its handle) as our
    /// own screen stream.
    ShareStart {
        monitor: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        window: Option<i64>,
        /// `light`, `standard`, `high` or `max`; absent is the bridge default.
        #[serde(skip_serializing_if = "Option::is_none")]
        quality: Option<&'static str>,
        /// Send the computer's sound in our voice.
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        audio: bool,
    },
    ShareStop,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_state_events_parse() {
        for (state, reason) in [
            ("connecting", None),
            ("playing", None),
            ("ended", None),
            ("failed", Some("refused")),
            ("failed", Some("timeout")),
            ("failed", Some("unsupported")),
            ("failed", Some("network")),
            ("failed", Some("no_decoder")),
        ] {
            let json = serde_json::json!({"ev": "stream_state", "client": 302, "state": state, "reason": reason});
            let event: BridgeEvent = serde_json::from_value(json).unwrap();
            let BridgeEvent::StreamState(parsed) = event else {
                panic!("not a stream state");
            };
            assert_eq!(parsed.client, 302);
            assert_eq!(parsed.state, state);
            assert_eq!(parsed.reason.as_deref(), reason);
        }
    }

    #[test]
    fn share_state_events_parse() {
        for (state, reason, viewers) in [
            ("starting", None, 0),
            ("live", None, 2),
            ("stopped", None, 0),
            ("failed", Some("unsupported"), 0),
            ("failed", Some("capture"), 0),
            ("failed", Some("timeout"), 0),
        ] {
            let json = serde_json::json!({"ev": "share_state", "state": state, "reason": reason, "viewers": viewers});
            let event: BridgeEvent = serde_json::from_value(json).unwrap();
            let BridgeEvent::ShareState(parsed) = event else {
                panic!("not a share state");
            };
            assert_eq!(parsed.state, state);
            assert_eq!(parsed.reason.as_deref(), reason);
            assert_eq!(parsed.viewers, viewers);
        }
    }

    #[test]
    fn share_commands_serialize_like_the_bridge_expects() {
        assert_eq!(
            serde_json::to_value(Command::ShareStart {
                monitor: Some(1),
                window: None,
                quality: None,
                audio: false
            })
            .unwrap(),
            serde_json::json!({"op": "share_start", "monitor": 1})
        );
        assert_eq!(
            serde_json::to_value(Command::ShareStart {
                monitor: None,
                window: Some(65678),
                quality: Some("high"),
                audio: true
            })
            .unwrap(),
            serde_json::json!({"op": "share_start", "monitor": null, "window": 65678, "quality": "high", "audio": true})
        );
        assert_eq!(
            serde_json::to_value(Command::ShareStop).unwrap(),
            serde_json::json!({"op": "share_stop"})
        );
    }

    #[test]
    fn watch_commands_serialize() {
        assert_eq!(
            serde_json::to_value(Command::WatchStream { client: 302 }).unwrap(),
            serde_json::json!({"op": "watch_stream", "client": 302})
        );
        assert_eq!(
            serde_json::to_value(Command::StopWatching).unwrap(),
            serde_json::json!({"op": "stop_watching"})
        );
    }

    #[test]
    fn channel_chat_events_preserve_scope_and_request_results() {
        let event: BridgeEvent = serde_json::from_str(r#"{"ev":"chat","channel":"3","sender":"fixture","text":"hello","from_self":false,"ts":"12:00:00"}"#).unwrap();
        assert!(
            matches!(event, BridgeEvent::Chat(message) if message.channel == "3" && message.text == "hello")
        );
        let result: BridgeEvent = serde_json::from_str(
            r#"{"ev":"chat_result","request_id":"fixture-1","error":"stale channel"}"#,
        )
        .unwrap();
        assert!(
            matches!(result, BridgeEvent::ChatResult { request_id, error: Some(_), .. } if request_id == "fixture-1")
        );
    }

    #[test]
    fn parses_state_event() {
        let json = r#"{"ev":"state","snapshot":{"own_client_id":2,"own_uid":null,
            "server":{"name":"S","version":"6.0.0","platform":"Linux","max_clients":32,"welcome_message":""},
            "channels":[{"id":1,"parent":0,"name":"Default","topic":null,"codec":"OpusVoice","clients":1}],
            "clients":[{"id":2,"channel_id":1,"name":"me","uid":null,"input_muted":false,"output_muted":false,"is_self":true}]}}"#;
        match serde_json::from_str::<BridgeEvent>(json).unwrap() {
            BridgeEvent::State { snapshot } => {
                assert_eq!(snapshot.channels[0].name, "Default");
                assert!(snapshot.clients[0].is_self);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn parses_status_and_audio_events() {
        let status: BridgeEvent = serde_json::from_str(
            r#"{"ev":"status","connected":false,"address":null,"nickname":null,"error":"boom"}"#,
        )
        .unwrap();
        assert!(matches!(status, BridgeEvent::Status(s) if s.error.as_deref() == Some("boom")));

        let audio: BridgeEvent = serde_json::from_str(
            r#"{"ev":"audio","rx_packets":1,"tx_frames":2,"ptt_down":true,"speaking":[3],"capture":null,"playback":null,"tone":false,"ptt_binding":"F13"}"#,
        )
        .unwrap();
        assert!(matches!(audio, BridgeEvent::Audio(a) if a.speaking == vec![3]));
    }

    #[test]
    fn attachment_commands_and_events_round_trip() {
        let json = serde_json::to_string(&Command::ChatAttach {
            session_id: "s1".into(),
            channel: "3".into(),
            path: "C:/pics/cat.gif".into(),
            request_id: "r9".into(),
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"op":"chat_attach","session_id":"s1","channel":"3","path":"C:/pics/cat.gif","request_id":"r9"}"#
        );
        let fetch = serde_json::to_string(&Command::ChatFetch {
            channel: "3".into(),
            path: "/mirage-a-cat.gif".into(),
        })
        .unwrap();
        assert_eq!(
            fetch,
            r#"{"op":"chat_fetch","channel":"3","path":"/mirage-a-cat.gif"}"#
        );
        let event: BridgeEvent = serde_json::from_str(
            r#"{"ev":"chat_file","channel":"3","path":"/x.gif","file":"C:/cache/chat_ab","kind":"gif"}"#,
        )
        .unwrap();
        assert!(matches!(event, BridgeEvent::ChatFile { file: Some(_), .. }));
        let chat: BridgeEvent = serde_json::from_str(
            r#"{"ev":"chat","channel":"3","sender":"a","text":"t","from_self":false,"ts":"x","attachments":[{"kind":"link","url":"https://x.test","label":"x"}]}"#,
        )
        .unwrap();
        assert!(matches!(chat, BridgeEvent::Chat(m) if m.attachments.len() == 1));
    }

    #[test]
    fn audio_events_with_peer_levels_parse() {
        // Integer map keys arrive as JSON strings; an internally tagged enum
        // used to reject them, freezing the speaking marks while others talked.
        let audio: BridgeEvent = serde_json::from_str(
            r#"{"ev":"audio","rx_packets":1,"tx_frames":2,"ptt_down":false,"speaking":[7],"capture":null,"playback":null,"tone":false,"ptt_binding":null,"levels":{"7":0.25}}"#,
        )
        .unwrap();
        assert!(matches!(audio, BridgeEvent::Audio(a) if a.levels.get("7") == Some(&0.25)));
    }

    #[test]
    fn serializes_the_correlated_chat_command() {
        let json = serde_json::to_string(&Command::Chat {
            session_id: "s1".into(),
            channel: "3".into(),
            text: "oi".into(),
            request_id: "r1".into(),
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"op":"chat","session_id":"s1","channel":"3","text":"oi","request_id":"r1"}"#
        );
        let result: BridgeEvent = serde_json::from_str(
            r#"{"ev":"chat_result","request_id":"r1","error":null,"code":null}"#,
        )
        .unwrap();
        assert!(matches!(
            result,
            BridgeEvent::ChatResult {
                error: None,
                code: None,
                ..
            }
        ));
    }

    #[test]
    fn serializes_commands_with_op_tag() {
        let json = serde_json::to_string(&Command::Mute {
            input: true,
            output: false,
        })
        .unwrap();
        assert_eq!(json, r#"{"op":"mute","input":true,"output":false}"#);

        let json = serde_json::to_string(&Command::Connect {
            address: "127.0.0.1:9987".into(),
            nickname: None,
            server_password: None,
        })
        .unwrap();
        assert_eq!(json, r#"{"op":"connect","address":"127.0.0.1:9987"}"#);
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChatMessage {
    pub channel: String,
    pub sender: String,
    pub text: String,
    pub from_self: bool,
    pub ts: String,
    /// Attachments the bridge recognised in `text`; absent on older bridges.
    #[serde(default)]
    #[allow(dead_code)] // rendering re-parses the text with the same rules
    pub attachments: Vec<serde_json::Value>,
}
