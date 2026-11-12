/// Wire types shared with `crates/ts6-bridge/src/protocol.rs`.
/// Keep the field names identical: the bridge serializes with serde defaults.

export type Device = {
  name: string;
  is_default: boolean;
  channels: number;
  sample_rate: number;
  sample_format: string;
};

export type DeviceLists = { inputs: Device[]; outputs: Device[] };

export type ChannelInfo = {
  id: number;
  parent: number;
  name: string;
  topic: string | null;
  codec: string;
  clients: number;
};

export type ClientInfo = {
  id: number;
  channel_id: number;
  name: string;
  uid: string | null;
  input_muted: boolean;
  output_muted: boolean;
  is_self: boolean;
  avatar_hash?: string | null;
};

export type ServerInfo = {
  name: string;
  version: string;
  platform: string;
  max_clients: number;
  welcome_message: string;
  icon_id?: number;
};

export type Snapshot = {
  own_client_id: number;
  own_uid: string | null;
  server: ServerInfo;
  channels: ChannelInfo[];
  clients: ClientInfo[];
};

export type StatusEvent = {
  session_id?: string | null;
  connected: boolean;
  address: string | null;
  nickname: string | null;
  error: string | null;
};

export type AudioEvent = {
  rx_packets: number;
  tx_frames: number;
  ptt_down: boolean;
  speaking: number[];
  capture: string | null;
  playback: string | null;
  tone: boolean;
  ptt_binding: string | null;
};

export type LogLine = { level: string; text: string; ts: string };

export type ChatMessage = { channel: string; sender: string; text: string; from_self: boolean; ts: string };
/** `code` is a stable reason; `not_confirmed` sends may still have reached the server. */
export type ChatResult = {
  request_id: string;
  error: string | null;
  code?: "not_connected" | "stale_session" | "stale_channel" | "invalid" | "not_confirmed" | null;
};
export type BridgeEvent =
  | ({ ev: "chat" } & ChatMessage)
  | ({ ev: "chat_result" } & ChatResult)
  | { ev: "hello"; channel_chat?: boolean; version: string; devices: DeviceLists }
  | ({ ev: "status" } & StatusEvent)
  | { ev: "state"; snapshot: Snapshot }
  | ({ ev: "log" } & LogLine)
  | ({ ev: "audio" } & AudioEvent)
  | { ev: "devices"; devices: DeviceLists }
  | { ev: "media"; avatars: Record<string, string>; server_icon: string | null };

export type Command =
  | { op: "chat"; session_id: string; channel: string; text: string; request_id: string }
  | { op: "connect"; address: string; nickname?: string; channel?: string; server_password?: string }
  | { op: "disconnect" }
  | { op: "join"; channel: string }
  | { op: "nick"; name: string }
  | { op: "mute"; input?: boolean; output?: boolean }
  | {
      op: "audio";
      enabled?: boolean;
      tone?: boolean;
      ptt?: string;
      input_device?: string;
      output_device?: string;
    }
  | { op: "devices" }
  | { op: "refresh" }
  | { op: "set_avatar"; path: string | null };
