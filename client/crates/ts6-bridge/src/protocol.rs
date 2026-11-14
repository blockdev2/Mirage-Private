//! Wire protocol between the bridge and the desktop UI.
//!
//! Both directions are JSON objects with a discriminant: `op` for commands the UI
//! sends, `ev` for events the bridge pushes. Keeping it boring makes the UI simple
//! and the protocol easy to extend (chat, streams) without breaking old clients.

use serde::{Deserialize, Serialize};

/// One attachment or link recognised in a chat message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Attachment {
    /// A picture (photo or GIF) on this server's channel file transfer;
    /// fetch it with `chat_fetch`.
    Picture {
        channel: String,
        path: String,
        name: String,
    },
    /// A file on this server's channel file transfer.
    File {
        channel: String,
        path: String,
        name: String,
        size: Option<u64>,
    },
    /// A picture on the web. Not fetched by the bridge: loading it reveals the
    /// user's address to whoever chose the URL, so the UI must ask first.
    WebPicture { url: String },
    /// A plain web link.
    Link { url: String, label: String },
}

impl Attachment {
    /// Everything recognised in a message's BBCode.
    pub fn from_text(text: &str) -> Vec<Self> {
        use ts6_core::attachments::Segment;
        ts6_core::attachments::parse(text)
            .into_iter()
            .filter_map(|segment| match segment {
                Segment::Text(_) => None,
                Segment::Picture(file) => Some(Self::Picture {
                    channel: file.channel_id.to_string(),
                    path: file.path(),
                    name: file.name,
                }),
                Segment::File(file) => Some(Self::File {
                    channel: file.channel_id.to_string(),
                    path: file.path(),
                    name: file.name,
                    size: file.size,
                }),
                Segment::WebPicture(url) => Some(Self::WebPicture { url }),
                Segment::Link { url, label } => Some(Self::Link { url, label }),
            })
            .collect()
    }
}

/// Commands from the UI.
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Command {
    Connect {
        address: String,
        #[serde(default)]
        nickname: Option<String>,
        #[serde(default)]
        channel: Option<String>,
        #[serde(default)]
        server_password: Option<String>,
    },
    Chat {
        #[serde(default)]
        session_id: String,
        channel: String,
        text: String,
        request_id: String,
    },
    /// Send a local file (photo, GIF or any file) to the current channel.
    /// The native frontend picks the file; the bridge reads it, uploads it to
    /// the channel's file transfer and posts a message that references it.
    /// Answered with `chat_result` for the same `request_id`.
    ChatAttach {
        #[serde(default)]
        session_id: String,
        channel: String,
        path: String,
        request_id: String,
    },
    /// Download an attachment referenced by a received message into the local
    /// cache. Answered with `chat_file`.
    ChatFetch {
        channel: String,
        path: String,
    },
    /// A server administration operation (channels, moderation, groups,
    /// permissions); see `ts6_core::admin::AdminOp`. Answered with
    /// `admin_result` for the same `request_id`.
    Admin {
        #[serde(default)]
        session_id: String,
        request_id: String,
        #[serde(flatten)]
        what: ts6_core::admin::AdminOp,
    },
    Disconnect,
    Join {
        channel: String,
        /// The channel's password, when it has one.
        #[serde(default)]
        password: Option<String>,
    },
    Nick {
        name: String,
    },
    Mute {
        #[serde(default)]
        input: Option<bool>,
        #[serde(default)]
        output: Option<bool>,
    },
    /// (Re)configure the local audio pipeline.
    Audio {
        #[serde(default)]
        enabled: Option<bool>,
        /// Transmit a synthetic tone instead of the microphone (no-mic machines).
        #[serde(default)]
        tone: Option<bool>,
        /// Push-to-talk key binding, e.g. "F13"; empty string disables it.
        #[serde(default)]
        ptt: Option<String>,
        #[serde(default)]
        input_device: Option<String>,
        #[serde(default)]
        output_device: Option<String>,
        /// Without push-to-talk: transmit only while voice is detected.
        #[serde(default)]
        voice_activation: Option<bool>,
        /// Voice activation threshold in dBFS.
        #[serde(default)]
        vad_threshold: Option<f32>,
        /// Noise suppression: 0 off, 1 standard, 2 strong.
        #[serde(default)]
        noise_suppression: Option<u8>,
        /// Hear our own processed microphone locally.
        #[serde(default)]
        monitor: Option<bool>,
    },
    /// Ask for the device lists again (after plugging a headset, for example).
    Devices,
    /// Ask for the current snapshot again.
    Refresh,
    /// Playback volume of one client: 1.0 unchanged, up to 4.0 (400%).
    Volume {
        client: u16,
        volume: f32,
    },
    /// Watch a client's connection statistics (refreshed every second);
    /// `None` stops watching.
    ConnectionInfo {
        #[serde(default)]
        client: Option<u16>,
    },
    /// Publish a local image file as our avatar; `None` removes the avatar.
    SetAvatar {
        #[serde(default)]
        path: Option<String>,
    },
    /// Internal: a file download finished (sent by the bridge's own tasks).
    #[serde(skip)]
    MediaDone {
        target: crate::media::Target,
        path: Option<String>,
    },
    /// Internal: the avatar upload finished; `flag` is set on success.
    #[serde(skip)]
    AvatarUploaded {
        flag: Option<String>,
    },
    /// Internal: a chat attachment upload finished; `message` (the text that
    /// references it) is set on success.
    #[serde(skip)]
    AttachmentSent {
        request_id: String,
        channel: u64,
        message: Option<String>,
    },
    /// Internal: a chat attachment download finished.
    #[serde(skip)]
    ChatFileDone {
        channel: u64,
        path: String,
        file: Option<String>,
    },
    /// Internal: periodic wake-up so speaking indicators expire on time even
    /// when nobody is talking and no other event arrives.
    #[serde(skip)]
    Tick,
    /// Watch the TS6 stream (screen share) of this client.
    WatchStream {
        client: u16,
    },
    /// Leave the watched stream.
    StopWatching,
    /// Internal: a report from the stream viewer worker.
    #[serde(skip)]
    StreamWorker(crate::stream_media::WorkerEvent),
    /// Share a window (`window` is its handle) or a monitor (`None`: the
    /// primary one) as a TS6 screen stream.
    ShareStart {
        #[serde(default)]
        monitor: Option<usize>,
        #[serde(default)]
        window: Option<i64>,
        /// `light`, `standard` (the default), `high` or `max`.
        #[serde(default)]
        quality: Option<String>,
        /// Send the computer's sound (all but Mirage) in our voice.
        #[serde(default)]
        audio: bool,
    },
    /// Stop sharing.
    ShareStop,
    /// Internal: a report from the screen share worker.
    #[serde(skip)]
    ShareWorker(crate::share_media::ShareEvent),
}

// Log only the operation, never credential-bearing fields or user content.
impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Connect { .. } => "Connect",
            Self::Chat { .. } => "Chat",
            Self::Admin { .. } => "Admin",
            Self::Disconnect => "Disconnect",
            Self::Join { .. } => "Join",
            Self::Nick { .. } => "Nick",
            Self::Mute { .. } => "Mute",
            Self::Audio { .. } => "Audio",
            Self::Devices => "Devices",
            Self::Refresh => "Refresh",
            Self::SetAvatar { .. } => "SetAvatar",
            Self::MediaDone { .. } => "MediaDone",
            Self::AvatarUploaded { .. } => "AvatarUploaded",
            Self::ChatAttach { .. } => "ChatAttach",
            Self::ChatFetch { .. } => "ChatFetch",
            Self::AttachmentSent { .. } => "AttachmentSent",
            Self::ChatFileDone { .. } => "ChatFileDone",
            Self::Tick => "Tick",
            Self::WatchStream { .. } => "WatchStream",
            Self::StopWatching => "StopWatching",
            Self::StreamWorker(_) => "StreamWorker",
            Self::ShareStart { .. } => "ShareStart",
            Self::ShareStop => "ShareStop",
            Self::ShareWorker(_) => "ShareWorker",
            Self::Volume { .. } => "Volume",
            Self::ConnectionInfo { .. } => "ConnectionInfo",
        })
    }
}

/// Device description sent to the UI.
#[derive(Debug, Serialize, Clone)]
pub struct Device {
    pub name: String,
    pub is_default: bool,
    pub channels: u16,
    pub sample_rate: u32,
    pub sample_format: String,
}

impl From<ts6_audio::DeviceInfo> for Device {
    fn from(info: ts6_audio::DeviceInfo) -> Self {
        Self {
            name: info.name,
            is_default: info.is_default,
            channels: info.channels,
            sample_rate: info.sample_rate,
            sample_format: info.sample_format,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct DeviceLists {
    pub inputs: Vec<Device>,
    pub outputs: Vec<Device>,
}

/// Events pushed to the UI.
#[derive(Debug, Serialize)]
#[serde(tag = "ev", rename_all = "snake_case")]
pub enum Event<'a> {
    Chat {
        channel: String,
        sender: String,
        text: String,
        from_self: bool,
        /// Pictures, files and links recognised in `text` (BBCode). Text-only
        /// frontends can ignore this; `text` is unchanged.
        attachments: Vec<Attachment>,
        ts: String,
    },
    /// Answer to `chat_fetch`: the cached file, or `null` when the download
    /// failed or the server refused it. `kind` comes from the file content.
    ChatFile {
        channel: String,
        path: String,
        file: Option<String>,
        kind: Option<&'a str>,
    },
    /// Answer to one `chat` command. `error` is human-readable English;
    /// `code` is the stable machine-readable reason (`not_connected`,
    /// `stale_session`, `stale_channel`, `invalid` or `not_confirmed`). A
    /// `not_confirmed` send may still have reached the server.
    ChatResult {
        request_id: String,
        error: Option<&'a str>,
        code: Option<&'a str>,
    },
    /// Answer to one `admin` command. `rows` holds the reply of list
    /// operations; `error` is the server's refusal or a local rejection and
    /// `code` is `not_connected`, `stale_session`, `invalid` or `refused`.
    AdminResult {
        request_id: String,
        ok: bool,
        error: Option<String>,
        code: Option<&'a str>,
        rows: Vec<ts6_core::admin::Row>,
    },
    Hello {
        version: &'a str,
        channel_chat: bool,
        devices: DeviceLists,
    },
    Status {
        session_id: Option<&'a str>,
        connected: bool,
        address: Option<&'a str>,
        nickname: Option<&'a str>,
        error: Option<&'a str>,
    },
    State {
        snapshot: &'a ts6_core::Snapshot,
    },
    /// Watching a stream: `connecting | playing | ended | failed`; `reason`
    /// is `refused | timeout | unsupported | network | no_decoder` on failure.
    StreamState {
        client: u16,
        state: &'a str,
        reason: Option<&'a str>,
    },
    /// Our own screen share: `starting | live | stopped | failed`. `reason` is
    /// `unsupported | capture | timeout | busy | offline` on failure; `viewers`
    /// counts the connected viewers.
    ShareState {
        state: &'a str,
        reason: Option<&'a str>,
        viewers: usize,
    },
    Log {
        level: &'a str,
        text: String,
        ts: String,
    },
    Audio {
        rx_packets: u64,
        tx_frames: u64,
        ptt_down: bool,
        speaking: Vec<u16>,
        /// Whether our own voice is going out (voice detected or key held).
        transmitting: bool,
        /// Microphone level in dBFS, for the voice activation meter.
        input_level_db: Option<f32>,
        capture: Option<String>,
        playback: Option<String>,
        tone: bool,
        ptt_binding: Option<String>,
        /// Our round-trip time to the server, when the server has reported it.
        ping_ms: Option<f64>,
        /// Our packet loss towards the server, 0..1.
        loss_out: Option<f32>,
        /// Packets lost and packets counted since the connection began.
        lost_packets: Option<u64>,
        counted_packets: Option<u64>,
        /// Voice level (RMS, 0..1) of each peer talking right now.
        levels: std::collections::HashMap<u16, f32>,
    },
    Devices {
        devices: DeviceLists,
    },
    /// Latest connection statistics of the watched client.
    ConnectionInfo {
        client: u16,
        info: Option<&'a ts6_core::ConnectionInfo>,
    },
    /// Cached avatar files by client uid and the server icon file, as local
    /// paths (images are too large for a WebSocket message).
    Media {
        avatars: std::collections::HashMap<String, String>,
        server_icon: Option<String>,
    },
}

/// Send an event to every connected UI.
pub fn publish(sender: &tokio::sync::broadcast::Sender<String>, event: &Event<'_>) {
    if let Ok(json) = serde_json::to_string(event) {
        let _ = sender.send(json);
    }
}

/// Current wall-clock time as `HH:MM:SS`, for the event console.
pub fn timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let secs_of_day = now % 86_400;
    // The UI shows local time; the bridge only needs a stable, readable stamp.
    let offset = local_utc_offset_seconds();
    let local = (secs_of_day as i64 + offset).rem_euclid(86_400) as u64;
    format!(
        "{:02}:{:02}:{:02}",
        local / 3600,
        (local % 3600) / 60,
        local % 60
    )
}

#[cfg(windows)]
fn local_utc_offset_seconds() -> i64 {
    // Reading the Windows time-zone requires a syscall; the UI mostly needs a
    // monotonic-looking timestamp, so fall back to UTC when it is not available.
    // (Kept as a seam for a later `time`-crate based implementation.)
    0
}

#[cfg(not(windows))]
fn local_utc_offset_seconds() -> i64 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_chat_command_is_supported_without_logging_content() {
        let command: Command = serde_json::from_str(
            r#"{"op":"chat","channel":"1","text":"private fixture text","request_id":"fixture-1"}"#,
        )
        .expect("channel chat command");
        assert!(!format!("{command:?}").contains("private fixture text"));
    }

    #[test]
    fn stream_watch_commands_and_state_events_round_trip() {
        let watch: Command = serde_json::from_str(r#"{"op":"watch_stream","client":302}"#).unwrap();
        assert!(matches!(watch, Command::WatchStream { client: 302 }));
        let stop: Command = serde_json::from_str(r#"{"op":"stop_watching"}"#).unwrap();
        assert!(matches!(stop, Command::StopWatching));
        let event = serde_json::to_value(Event::StreamState {
            client: 302,
            state: "failed",
            reason: Some("refused"),
        })
        .unwrap();
        assert_eq!(
            event,
            serde_json::json!({"ev":"stream_state","client":302,"state":"failed","reason":"refused"})
        );
    }

    #[test]
    fn share_commands_and_state_events_round_trip() {
        let start: Command = serde_json::from_str(r#"{"op":"share_start","monitor":1}"#).unwrap();
        assert!(matches!(
            start,
            Command::ShareStart {
                monitor: Some(1),
                window: None,
                ..
            }
        ));
        let primary: Command = serde_json::from_str(r#"{"op":"share_start"}"#).unwrap();
        assert!(matches!(
            primary,
            Command::ShareStart {
                monitor: None,
                window: None,
                ..
            }
        ));
        let high: Command =
            serde_json::from_str(r#"{"op":"share_start","quality":"high"}"#).unwrap();
        assert!(matches!(
            high,
            Command::ShareStart { quality: Some(ref q), .. } if q == "high"
        ));
        let window: Command =
            serde_json::from_str(r#"{"op":"share_start","window":65678}"#).unwrap();
        assert!(matches!(
            window,
            Command::ShareStart {
                window: Some(65678),
                ..
            }
        ));
        let stop: Command = serde_json::from_str(r#"{"op":"share_stop"}"#).unwrap();
        assert!(matches!(stop, Command::ShareStop));
        let event = serde_json::to_value(Event::ShareState {
            state: "live",
            reason: None,
            viewers: 2,
        })
        .unwrap();
        assert_eq!(
            event,
            serde_json::json!({"ev":"share_state","state":"live","reason":null,"viewers":2})
        );
    }

    #[test]
    fn command_diagnostics_never_include_passwords() {
        let command: Command = serde_json::from_str(
            r#"{"op":"connect","address":"example.org","server_password":"fixture-sensitive-value"}"#,
        ).unwrap();
        assert!(!format!("{command:?}").contains("fixture-sensitive-value"));
    }

    #[test]
    fn admin_commands_carry_their_action_next_to_the_op() {
        let command: Command = serde_json::from_str(
            r#"{"op":"admin","request_id":"r1","action":"channel_delete","id":7,"force":true}"#,
        )
        .unwrap();
        assert!(matches!(
            &command,
            Command::Admin { request_id, what: ts6_core::admin::AdminOp::ChannelDelete { id: 7, force: true }, .. }
                if request_id == "r1"
        ));
        assert_eq!(format!("{command:?}"), "Admin");
        // No request id, or an action the backend does not have: refused at parse time.
        assert!(
            serde_json::from_str::<Command>(r#"{"op":"admin","action":"server_groups"}"#).is_err()
        );
        assert!(serde_json::from_str::<Command>(
            r#"{"op":"admin","request_id":"r","action":"nope"}"#
        )
        .is_err());
    }

    #[test]
    fn commands_parse_from_ui_json() {
        let connect: Command =
            serde_json::from_str(r#"{"op":"connect","address":"127.0.0.1:9987"}"#).unwrap();
        assert!(matches!(connect, Command::Connect { .. }));

        let join: Command = serde_json::from_str(r#"{"op":"join","channel":"3"}"#).unwrap();
        assert!(matches!(join, Command::Join { channel, password: None } if channel == "3"));
        let locked: Command =
            serde_json::from_str(r#"{"op":"join","channel":"Cofre","password":"abc"}"#).unwrap();
        assert!(matches!(locked, Command::Join { password: Some(p), .. } if p == "abc"));

        let audio: Command =
            serde_json::from_str(r#"{"op":"audio","tone":true,"ptt":"F13"}"#).unwrap();
        match audio {
            Command::Audio { tone, ptt, .. } => {
                assert_eq!(tone, Some(true));
                assert_eq!(ptt.as_deref(), Some("F13"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn events_serialize_with_a_type_tag() {
        let json = serde_json::to_string(&Event::Status {
            session_id: None,
            connected: true,
            address: Some("127.0.0.1:9987"),
            nickname: Some("tester"),
            error: None,
        })
        .unwrap();
        assert!(json.contains(r#""ev":"status""#), "{json}");
        assert!(json.contains(r#""connected":true"#));
    }

    #[test]
    fn audio_event_serializes_speaking_clients() {
        let json = serde_json::to_string(&Event::Audio {
            rx_packets: 10,
            tx_frames: 0,
            ptt_down: false,
            speaking: vec![3, 5],
            transmitting: false,
            input_level_db: None,
            capture: None,
            playback: Some("Speakers".into()),
            tone: false,
            ptt_binding: Some("F13".into()),
            ping_ms: None,
            loss_out: None,
            lost_packets: None,
            counted_packets: None,
            levels: Default::default(),
        })
        .unwrap();
        assert!(json.contains(r#""speaking":[3,5]"#), "{json}");
        assert!(json.contains(r#""ev":"audio""#));
    }
}
