//! Snapshot of the server state we keep locally.
//!
//! `tsclientlib` already maintains the authoritative book; this module projects it
//! into owned DTOs so the UI layer never borrows the connection.

use serde::Serialize;
use tsclientlib::data;

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub own_client_id: u16,
    pub own_uid: Option<String>,
    pub server: ServerInfo,
    pub channels: Vec<ChannelInfo>,
    pub clients: Vec<ClientInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    pub platform: String,
    pub max_clients: u16,
    pub welcome_message: String,
    /// Custom server icon id (0 or a built-in id when there is none to fetch).
    pub icon_id: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelInfo {
    pub id: u64,
    pub parent: u64,
    pub name: String,
    pub topic: Option<String>,
    pub codec: String,
    pub clients: usize,
    /// Joining needs a password (`join_channel_with_password`).
    pub has_password: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClientInfo {
    pub id: u16,
    pub channel_id: u64,
    pub name: String,
    pub uid: Option<String>,
    pub input_muted: bool,
    pub output_muted: bool,
    pub is_self: bool,
    /// The client's avatar flag (hex MD5 of the image), `None` without avatar.
    pub avatar_hash: Option<String>,
    /// HTTPS address of the myTeamSpeak account avatar that official TS6
    /// clients announce instead of a file-transfer avatar.
    pub myteamspeak_avatar: Option<String>,
    /// Whether the client is sharing a TS6 stream (screen) right now.
    pub is_streaming: bool,
}

/// Connection statistics of one client, as the server reports them after a
/// `getconnectioninfo` request (the "connection info" window of TeamSpeak).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ConnectionInfo {
    pub client_id: u16,
    pub ping_ms: Option<f64>,
    pub ping_deviation_ms: Option<f64>,
    /// Packet loss from the server to that client, 0..1.
    pub loss_incoming: Option<f32>,
    /// Packet loss from that client to the server, 0..1.
    pub loss_outgoing: f32,
    pub idle_secs: u64,
    pub connected_secs: Option<u64>,
    pub bytes_received: u64,
    pub bytes_sent: u64,
    /// Only present when we have the permission to see it.
    pub address: Option<String>,
}

impl ConnectionInfo {
    pub fn from_state(state: &data::Connection, client_id: u16) -> Option<Self> {
        let client = state.clients.get(&tsclientlib::ClientId(client_id))?;
        let data = client.connection_data.as_ref()?;
        let sum = |values: [Option<u64>; 3]| values.iter().flatten().sum::<u64>();
        Some(Self {
            client_id,
            ping_ms: data.ping.map(|d| d.as_seconds_f64() * 1000.0),
            ping_deviation_ms: data.ping_deviation.map(|d| d.as_seconds_f64() * 1000.0),
            loss_incoming: data.server_to_client_packetloss_total,
            loss_outgoing: data.client_to_server_packetloss_total,
            idle_secs: data.idle_time.whole_seconds().max(0) as u64,
            connected_secs: data.connected_time.map(|d| d.whole_seconds().max(0) as u64),
            bytes_received: sum([
                data.bytes_received_speech,
                data.bytes_received_keepalive,
                data.bytes_received_control,
            ]),
            bytes_sent: sum([
                data.bytes_sent_speech,
                data.bytes_sent_keepalive,
                data.bytes_sent_control,
            ]),
            address: data.client_address.map(|a| a.ip().to_string()),
        })
    }
}

impl Snapshot {
    pub fn from_state(state: &data::Connection) -> Self {
        let own_client_id = state.own_client.0;

        let clients: Vec<ClientInfo> = state
            .clients
            .values()
            .map(|c| ClientInfo {
                id: c.id.0,
                channel_id: c.channel.0,
                name: c.name.clone(),
                uid: c.uid.as_ref().map(|u| format!("{}", &**u)),
                input_muted: c.input_muted,
                output_muted: c.output_muted,
                is_self: c.id.0 == own_client_id,
                avatar_hash: Some(c.avatar_hash.trim().to_string()).filter(|h| !h.is_empty()),
                myteamspeak_avatar: c
                    .my_team_speak_avatar
                    .as_deref()
                    .and_then(crate::media::myteamspeak_avatar_url),
                is_streaming: c.is_streaming.unwrap_or(false),
            })
            .collect();

        let mut channels: Vec<ChannelInfo> = state
            .channels
            .values()
            .map(|ch| ChannelInfo {
                id: ch.id.0,
                parent: ch.parent.0,
                name: ch.name.clone(),
                topic: ch.topic.clone(),
                codec: format!("{:?}", ch.codec),
                clients: clients.iter().filter(|c| c.channel_id == ch.id.0).count(),
                has_password: ch.has_password.unwrap_or(false),
            })
            .collect();
        channels.sort_by_key(|c| c.id);
        let mut clients = clients;
        clients.sort_by_key(|c| c.id);

        Snapshot {
            own_client_id,
            own_uid: clients
                .iter()
                .find(|c| c.is_self)
                .and_then(|c| c.uid.clone()),
            server: ServerInfo {
                name: state.server.name.clone(),
                version: state.server.version.clone(),
                platform: state.server.platform.clone(),
                max_clients: state.server.max_clients,
                welcome_message: state.server.welcome_message.clone(),
                icon_id: state.server.icon.0,
            },
            channels,
            clients,
        }
    }

    /// Find a channel by numeric id or by (case-insensitive) name.
    pub fn find_channel(&self, needle: &str) -> Option<&ChannelInfo> {
        if let Ok(id) = needle.parse::<u64>() {
            if let Some(ch) = self.channels.iter().find(|c| c.id == id) {
                return Some(ch);
            }
        }
        self.channels
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(needle))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(id: u64, name: &str) -> ChannelInfo {
        ChannelInfo {
            id,
            parent: 0,
            name: name.to_string(),
            topic: None,
            codec: "OpusVoice".to_string(),
            clients: 0,
            has_password: false,
        }
    }

    fn snapshot(channels: Vec<ChannelInfo>) -> Snapshot {
        Snapshot {
            own_client_id: 1,
            own_uid: None,
            server: ServerInfo {
                name: "server".into(),
                version: "6.0.0-beta13.1".into(),
                platform: "Windows".into(),
                max_clients: 32,
                welcome_message: String::new(),
                icon_id: 0,
            },
            channels,
            clients: Vec::new(),
        }
    }

    #[test]
    fn finds_channel_by_id_and_by_name_case_insensitively() {
        let s = snapshot(vec![
            channel(1, "Default Channel"),
            channel(3, "Permanent Channel"),
        ]);

        assert_eq!(
            s.find_channel("3").map(|c| c.name.as_str()),
            Some("Permanent Channel")
        );
        assert_eq!(s.find_channel("permanent channel").map(|c| c.id), Some(3));
        assert_eq!(s.find_channel("DEFAULT CHANNEL").map(|c| c.id), Some(1));
        assert!(s.find_channel("does-not-exist").is_none());
    }

    #[test]
    fn clients_carry_the_streaming_flag_on_the_wire() {
        let client = ClientInfo {
            id: 302,
            channel_id: 1,
            name: "novalloy".into(),
            uid: None,
            input_muted: false,
            output_muted: false,
            is_self: false,
            avatar_hash: None,
            myteamspeak_avatar: None,
            is_streaming: true,
        };
        let json = serde_json::to_value(&client).unwrap();
        assert_eq!(json["is_streaming"], serde_json::json!(true));
    }

    #[test]
    fn numeric_id_wins_over_a_channel_named_like_a_number() {
        let s = snapshot(vec![channel(1, "Default Channel"), channel(7, "7")]);

        // "7" parses as an id, so the channel with id 7 is returned even though
        // another channel is literally named "7".
        assert_eq!(s.find_channel("7").map(|c| c.id), Some(7));
    }
}
