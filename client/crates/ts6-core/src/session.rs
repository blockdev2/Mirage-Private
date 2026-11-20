//! The session: connection lifecycle, event surfacing and the command layer.
//!
//! Every command — including the TS6 stream commands we add later — goes through
//! [`Session::send_command`], which sends a raw TS command and waits for the
//! server's `MessageResult` for *that* command (correlated by `return_code`, not
//! by arrival order).

use std::collections::VecDeque;
use std::time::Duration;

use anyhow::{anyhow, Result};
use futures::prelude::*;
use tokio::time::{timeout_at, Instant};
use tsclientlib::prelude::*;
use tsclientlib::{Connection, DisconnectOptions, InMessage, MessageHandle, StreamItem};
use tsproto_packets::packets::{Direction, Flags, InAudioBuf, OutCommand, OutPacket, PacketType};

use crate::config::ClientConfig;
use crate::state::Snapshot;

/// Where a text message goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatTarget {
    Server,
    Channel,
    Client(u16),
}

/// What the core surfaces upwards.
///
/// Not `Clone`: `InAudioBuf` owns a decoded buffer that is moved, never copied.
#[derive(Debug)]
pub enum Item {
    /// Client/channel/server bookkeeping changed: re-read the snapshot.
    StateChanged,
    /// A chat or poke message.
    TextMessage(TextMessage),
    /// A notify the vendored library does not model.
    ///
    /// This is how all TS6 stream events arrive (`notifystreamstarted`,
    /// `notifyjoinstreamrequest`, `notifystreamsignaling`, ...). `content` is the
    /// raw, still TS-escaped command line.
    UnknownCommand { name: String, content: String },
    /// Identity security-level grind progress.
    IdentityLevel(u8),
    /// The connection dropped and is being rebuilt.
    Reconnecting,
    /// One received voice packet (server to client).
    ///
    /// Decoding/mixing is `ts6-audio`'s job; the core only hands the packet over.
    Audio(InAudioBuf),
    /// A requested download is ready: read `size` bytes from `stream`.
    ///
    /// The stream is a separate TCP connection to the file-transfer port, so the
    /// caller reads it off the session loop; it never competes with polling.
    FileDownload {
        handle: u16,
        size: u64,
        stream: tokio::net::TcpStream,
    },
    /// A requested upload is ready: write the file to `stream`.
    FileUpload {
        handle: u16,
        stream: tokio::net::TcpStream,
    },
    /// The server refused or failed a file transfer.
    FileFailed { handle: u16, error: String },
}

#[derive(Debug, Clone)]
pub struct TextMessage {
    pub target: tsclientlib::MessageTarget,
    pub channel_id: Option<u64>,
    pub invoker_id: Option<u16>,
    pub invoker_name: Option<String>,
    pub message: String,
    pub from_self: bool,
}

/// A wake-up from [`Session::poll_with`].
#[derive(Debug)]
pub enum Wake<T = ()> {
    /// Something happened on the server connection.
    Item(Item),
    /// One batch of captured frames was transmitted; poll again to keep draining.
    MicSent,
    /// A command arrived from the caller (UI, bridge, …).
    Command(T),
}

/// Our own connection quality, measured by the protocol layer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OwnNetwork {
    pub ping_ms: f64,
    /// Packet loss of the last second, 0..1.
    pub loss: f32,
    /// Packets lost and packets counted since the connection began.
    pub lost_packets: u64,
    pub counted_packets: u64,
}

/// Who our voice goes to instead of the channel: the people and channels of a
/// whisper. Empty lists are not a target (that is "no whisper").
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WhisperTarget {
    pub clients: Vec<u16>,
    pub channels: Vec<u64>,
}

/// The packet header counts each list in one byte; this is far below it, and
/// far above what a person picks by hand.
const WHISPER_CLIENTS_MAX: usize = 64;
const WHISPER_CHANNELS_MAX: usize = 16;

impl WhisperTarget {
    /// Removes repeats and refuses an empty or oversized target.
    pub fn checked(mut self) -> Result<Self, String> {
        self.clients.sort_unstable();
        self.clients.dedup();
        self.channels.sort_unstable();
        self.channels.dedup();
        if self.clients.is_empty() && self.channels.is_empty() {
            return Err("a whisper needs at least one person or channel".into());
        }
        if self.clients.len() > WHISPER_CLIENTS_MAX || self.channels.len() > WHISPER_CHANNELS_MAX {
            return Err(format!(
                "a whisper takes at most {WHISPER_CLIENTS_MAX} people and {WHISPER_CHANNELS_MAX} channels"
            ));
        }
        Ok(self)
    }
}

pub struct Session {
    session_id: String,
    con: Connection,
    config: ClientConfig,
    /// Items consumed while waiting for a command result, replayed to the caller.
    pending: VecDeque<Item>,
    /// Voice frames handed to the transport so far (surfaced to the UI).
    audio_frames_sent: u64,
    /// While set, captured voice goes to these targets, not to the channel.
    whisper: Option<WhisperTarget>,
    /// Why the connection ended, once it has (kick, ban, timeout, shutdown).
    end_reason: Option<String>,
}

impl Session {
    /// Open the connection and return only once the server pushed its initial
    /// state (the library's `connect()` returns before the handshake completes).
    pub async fn connect(config: ClientConfig) -> Result<Self> {
        let identity = crate::identity::load_or_create(&config.identity_file)?;
        let mut builder = Connection::build(config.address.clone())
            .name(config.nickname.clone())
            .identity(identity)
            .log_commands(config.log_commands);

        if let Some(channel) = &config.channel {
            builder = builder.channel(channel.clone());
        }
        if let Some(pwd) = &config.channel_password {
            builder = builder.channel_password(pwd.clone());
        }
        if let Some(pwd) = &config.server_password {
            builder = builder.password(pwd.clone());
        }
        if let Some(ip) = config.local_ip {
            builder = builder.local_address(std::net::SocketAddr::new(ip, 0));
        }

        let con = builder.connect()?;
        static NEXT_SESSION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let session_id = format!(
            "{:x}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            NEXT_SESSION.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let mut session = Self {
            session_id,
            con,
            config,
            pending: VecDeque::new(),
            audio_frames_sent: 0,
            whisper: None,
            end_reason: None,
        };
        session.wait_synced().await?;
        tracing::info!(addr = %session.config.address, "connected and synced");
        Ok(session)
    }

    /// Wait for the initial bookkeeping, then keep draining until the server goes
    /// quiet (channel/client lists arrive as a burst right after `initserver`).
    async fn wait_synced(&mut self) -> Result<()> {
        let hard_deadline = Instant::now() + Duration::from_secs(20);

        // 1. The handshake is done when the first book event arrives.
        loop {
            let item = match timeout_at(hard_deadline, self.con.events().next()).await {
                Err(_) => return Err(anyhow!("timed out during the handshake")),
                Ok(None) => return Err(anyhow!("connection closed during the handshake")),
                Ok(Some(Err(err))) => return Err(anyhow!("handshake failed: {err}")),
                Ok(Some(Ok(item))) => item,
            };
            match item {
                StreamItem::BookEvents(_) => break,
                other => self.buffer(other),
            }
        }

        // 2. Drain the burst of channel/client book events until it goes quiet.
        //
        // The quiet window is not enough on its own: a peer already streaming voice
        // keeps the stream busy forever, so the hard deadline must be enforced here
        // too (measured: a receiver joining a channel with a tone never finished
        // syncing).
        loop {
            let now = Instant::now();
            if now >= hard_deadline {
                tracing::debug!("sync drain reached the hard deadline; continuing");
                return Ok(());
            }
            let quiet = std::cmp::min(now + Duration::from_millis(350), hard_deadline);

            // Hoisted into a `let` so the borrow of `self.con` ends before we call
            // `self.buffer(..)`.
            let next = timeout_at(quiet, self.con.events().next()).await;
            match next {
                Err(_) => return Ok(()),
                Ok(None) => return Ok(()),
                Ok(Some(Err(_))) => return Ok(()),
                Ok(Some(Ok(item))) => self.buffer(item),
            }
        }
    }

    /// Translate a library stream item into our own queue (used while waiting).
    fn buffer(&mut self, item: StreamItem) {
        // A channel with active voice traffic can produce items faster than the
        // caller drains them; keep the queue bounded so a busy server cannot grow
        // it without limit.
        if self.pending.len() >= MAX_PENDING_ITEMS {
            self.pending.pop_front();
        }
        match item {
            StreamItem::BookEvents(events) => self.book(events),
            StreamItem::UnknownCommand { name, content } => self
                .pending
                .push_back(Item::UnknownCommand { name, content }),
            StreamItem::MessageEvent(InMessage::TextMessage(msg)) => {
                for item in raw_text_items(
                    &InMessage::TextMessage(msg),
                    self.own_id(),
                    current_channel(&self.con),
                ) {
                    enqueue_pending(&mut self.pending, item);
                }
            }
            StreamItem::IdentityLevelIncreasing(level) => {
                self.pending.push_back(Item::IdentityLevel(level))
            }
            StreamItem::DisconnectedTemporarily(_) => self.pending.push_back(Item::Reconnecting),
            StreamItem::Audio(packet) => self.pending.push_back(Item::Audio(packet)),
            other => {
                if let Some(item) = file_item(other) {
                    self.pending.push_back(item);
                }
            }
        }
    }

    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    /// Server destination of this live session, for local frontend resynchronization.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn address(&self) -> &str {
        &self.config.address
    }

    pub fn snapshot(&self) -> Result<Snapshot> {
        self.con
            .get_state()
            .map(Snapshot::from_state)
            .map_err(Into::into)
    }

    /// Next event from the connection (pending replayed first).
    pub async fn next_item(&mut self) -> Option<Item> {
        if let Some(item) = self.pending.pop_front() {
            return Some(item);
        }
        loop {
            let raw = self.con.events().next().await?;
            match raw {
                Ok(StreamItem::BookEvents(events)) => {
                    self.book(events);
                    return self.pending.pop_front();
                }
                Ok(StreamItem::UnknownCommand { name, content }) => {
                    return Some(Item::UnknownCommand { name, content })
                }
                Ok(StreamItem::MessageEvent(InMessage::TextMessage(msg))) => {
                    let mut items = raw_text_items(
                        &InMessage::TextMessage(msg),
                        self.own_id(),
                        current_channel(&self.con),
                    )
                    .into_iter();
                    match items.next() {
                        Some(item) => {
                            for item in items {
                                enqueue_pending(&mut self.pending, item);
                            }
                            return Some(item);
                        }
                        None => continue,
                    }
                }
                // TextMessage is the only message we surface for now; other
                // in-messages are either handled by the library or not needed yet.
                Ok(StreamItem::MessageEvent(_)) => continue,
                Ok(StreamItem::IdentityLevelIncreasing(level)) => {
                    return Some(Item::IdentityLevel(level))
                }
                Ok(StreamItem::IdentityLevelIncreased) => continue,
                Ok(StreamItem::DisconnectedTemporarily(reason)) => {
                    tracing::warn!(?reason, "temporarily disconnected");
                    return Some(Item::Reconnecting);
                }
                Ok(StreamItem::Audio(packet)) => return Some(Item::Audio(packet)),
                Ok(StreamItem::MessageResult(..)) => continue,
                Ok(other) => match file_item(other) {
                    Some(item) => return Some(item),
                    None => continue,
                },
                Err(err) => {
                    tracing::error!(%err, "connection error");
                    self.end_reason = Some(err.to_string());
                    return None;
                }
            }
        }
    }

    /// Queues what the book events mean for the caller, and remembers why we
    /// were removed when one of them says so.
    fn book(&mut self, events: Vec<tsclientlib::events::Event>) {
        let own = self.con.get_state().ok().map(|state| state.own_client.0);
        if let Some(reason) = removal_reason(&events, own) {
            self.end_reason = Some(reason);
        }
        enqueue_book_items(
            &mut self.pending,
            book_items(events, own, current_channel(&self.con)),
        );
    }

    fn own_id(&self) -> Option<u16> {
        self.con.get_state().ok().map(|s| s.own_client.0)
    }

    /// Map a raw library event into our own event type.
    ///
    /// `None` means "internal, keep polling" (message results, identity progress,
    /// messages we do not surface yet).
    fn map_raw(&mut self, raw: StreamItem) -> Option<Item> {
        match raw {
            StreamItem::BookEvents(events) => {
                self.book(events);
                self.pending.pop_front()
            }
            StreamItem::UnknownCommand { name, content } => {
                Some(Item::UnknownCommand { name, content })
            }
            StreamItem::MessageEvent(InMessage::TextMessage(msg)) => {
                let mut items = raw_text_items(
                    &InMessage::TextMessage(msg),
                    self.own_id(),
                    current_channel(&self.con),
                )
                .into_iter();
                match items.next() {
                    Some(item) => {
                        for item in items {
                            enqueue_pending(&mut self.pending, item);
                        }
                        Some(item)
                    }
                    None => None,
                }
            }
            StreamItem::MessageEvent(_) => None,
            StreamItem::IdentityLevelIncreasing(level) => Some(Item::IdentityLevel(level)),
            StreamItem::IdentityLevelIncreased => None,
            StreamItem::DisconnectedTemporarily(reason) => {
                tracing::warn!(?reason, "temporarily disconnected");
                Some(Item::Reconnecting)
            }
            StreamItem::Audio(packet) => Some(Item::Audio(packet)),
            StreamItem::MessageResult(..) => None,
            other => file_item(other),
        }
    }

    /// One coherent poll that advances the voice link and the caller's commands.
    ///
    /// Why a hand-written poll instead of `tokio::select!`: the events stream must
    /// not be cancelled between polls (measured in `docs/verification.md` V-13), so
    /// this polls the network first, then the microphone, then the command queue.
    /// Commands are returned to the caller *after* the borrow of the connection
    /// ends, so the caller can run async session methods for them.
    pub fn poll_with<T>(
        &mut self,
        mut mic: Option<&mut tokio::sync::mpsc::Receiver<Vec<u8>>>,
        mut commands: Option<&mut tokio::sync::mpsc::Receiver<T>>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Wake<T>>> {
        use std::task::Poll;

        loop {
            if let Some(item) = self.pending.pop_front() {
                return Poll::Ready(Some(Wake::Item(item)));
            }

            // Network first, and polled on every wake-up: never cancelled.
            let polled = {
                let mut stream = self.con.events();
                std::pin::Pin::new(&mut stream).poll_next(cx)
            };
            match polled {
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Ready(Some(Err(err))) => {
                    tracing::error!(%err, "connection error");
                    self.end_reason = Some(err.to_string());
                    return Poll::Ready(None);
                }
                Poll::Ready(Some(Ok(raw))) => {
                    if let Some(item) = self.map_raw(raw) {
                        return Poll::Ready(Some(Wake::Item(item)));
                    }
                    continue; // internal event: keep going
                }
                Poll::Pending => {}
            }

            // Nothing from the network: transmit captured frames.
            //
            // Sending is done inline and the loop CONTINUES, deliberately not
            // returning a "frame sent" wake-up. Returning early on every captured
            // frame starved voice reception in the bridge (measured: rx dropped to
            // zero while a tone was being transmitted). Draining a small batch and
            // immediately re-polling the network keeps both directions alive.
            if let Some(mic) = mic.as_mut() {
                let mut sent_any = false;
                for _ in 0..8 {
                    match mic.poll_recv(cx) {
                        Poll::Ready(Some(payload)) => {
                            let packet = voice_packet(self.whisper.as_ref(), &payload);
                            match self.con.send_audio(packet) {
                                Ok(()) => {
                                    self.audio_frames_sent += 1;
                                    sent_any = true;
                                }
                                Err(err) => {
                                    tracing::error!(%err, "failed to send captured audio");
                                    self.end_reason = Some(err.to_string());
                                    return Poll::Ready(None);
                                }
                            }
                        }
                        Poll::Ready(None) => break, // microphone side closed
                        Poll::Pending => break,
                    }
                }
                if sent_any {
                    // Hand control back to the caller after a bounded batch.
                    //
                    // A continue here looks harmless but is not: with a steady
                    // microphone rate this poll can keep finding exactly one new
                    // frame per iteration and never return (measured: a single call
                    // ran for 27 s while the bridge received nothing). Returning a
                    // wake keeps the caller's loop responsive, and the caller polls
                    // again immediately, so nothing is lost.
                    return Poll::Ready(Some(Wake::MicSent));
                }
            }

            // Still idle: hand a queued command to the caller.
            if let Some(commands) = commands.as_mut() {
                match commands.poll_recv(cx) {
                    Poll::Ready(Some(command)) => {
                        return Poll::Ready(Some(Wake::Command(command)));
                    }
                    Poll::Ready(None) => return Poll::Ready(None),
                    Poll::Pending => {}
                }
            }

            return Poll::Pending;
        }
    }

    /// Convenience wrapper used by the CLI: network + microphone only.
    pub fn poll_with_mic(
        &mut self,
        mic: &mut tokio::sync::mpsc::Receiver<Vec<u8>>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Wake<()>>> {
        self.poll_with(Some(mic), None, cx)
    }

    /// Send any TS command with arguments and wait for the correlated result.
    ///
    /// This is the same path the typed helpers use, and the one the TS6 stream
    /// commands will use.
    pub async fn send_command(&mut self, name: &str, args: &[(&str, String)]) -> Result<()> {
        let handle = self.send_command_no_wait(name, args)?;
        self.wait_result(name, handle).await
    }

    /// Sends an administration operation and returns the rows its reply carried
    /// (empty for operations that only answer with a result).
    pub async fn admin(&mut self, op: &crate::admin::AdminOp) -> Result<Vec<crate::admin::Row>> {
        let request = op.request().map_err(|reason| anyhow!(reason))?;
        let handle = self.send_command_no_wait(request.name, &request.args)?;
        self.wait_result(request.name, handle).await?;
        // The reply rows arrive before the result and were queued by
        // `wait_result`; take them out and leave everything else in order.
        let mut rows = Vec::new();
        let mut kept = VecDeque::with_capacity(self.pending.len());
        for item in self.pending.drain(..) {
            match item {
                Item::UnknownCommand { name, content }
                    if request.replies.contains(&name.as_str()) =>
                {
                    rows.extend(crate::admin::parse_rows(&content));
                }
                other => kept.push_back(other),
            }
        }
        self.pending = kept;
        Ok(rows)
    }

    /// Fire-and-forget variant: returns the handle so the caller can await it later.
    pub fn send_command_no_wait(
        &mut self,
        name: &str,
        args: &[(&str, String)],
    ) -> Result<MessageHandle> {
        let mut cmd = OutCommand::new(Direction::C2S, Flags::empty(), PacketType::Command, name);
        for (key, value) in args {
            cmd.write_arg(key, value);
        }
        let handle = cmd.send_with_result(&mut self.con)?;
        tracing::debug!(command = name, ?handle, "sent command");
        Ok(handle)
    }

    /// Await the result of a command sent earlier, replaying other events.
    pub async fn wait_result(&mut self, name: &str, handle: MessageHandle) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let stream_item = match timeout_at(deadline, self.con.events().next()).await {
                Err(_) => return Err(anyhow!("timed out waiting for the result of `{name}`")),
                Ok(None) => return Err(anyhow!("connection closed while awaiting `{name}`")),
                Ok(Some(Err(err))) => return Err(anyhow!("connection error: {err}")),
                Ok(Some(Ok(item))) => item,
            };

            match stream_item {
                StreamItem::MessageResult(got, result) if got == handle => {
                    return result.map_err(|err| match &err.missing_permission {
                        Some(permission) => anyhow!(
                            "server rejected `{name}`: {err} (missing permission {permission:?})"
                        ),
                        None => anyhow!("server rejected `{name}`: {err}"),
                    });
                }
                StreamItem::BookEvents(events) => self.book(events),
                StreamItem::UnknownCommand { name, content } => self
                    .pending
                    .push_back(Item::UnknownCommand { name, content }),
                StreamItem::MessageEvent(InMessage::TextMessage(msg)) => {
                    for item in raw_text_items(
                        &InMessage::TextMessage(msg),
                        self.own_id(),
                        current_channel(&self.con),
                    ) {
                        enqueue_pending(&mut self.pending, item);
                    }
                }
                StreamItem::Audio(packet) => self.pending.push_back(Item::Audio(packet)),
                other => {
                    if let Some(item) = file_item(other) {
                        self.pending.push_back(item);
                    }
                }
            }
            // The deadline bounds waiting time, not queue length: apply the same
            // drop-oldest policy as every other replay path.
            trim_pending(&mut self.pending);
        }
    }

    /// Ask the server for a file in channel 0 (avatars, icons).
    ///
    /// Returns at once with the transfer handle; the outcome arrives later as
    /// [`Item::FileDownload`] or [`Item::FileFailed`] through the normal poll.
    pub fn request_download(&mut self, path: &str) -> Result<u16> {
        let handle = self
            .con
            .download_file(tsclientlib::ChannelId(0), path, None, None)?;
        Ok(handle.0)
    }

    /// Ask to upload `size` bytes to `path` in channel 0, replacing any file there.
    pub fn request_upload(&mut self, path: &str, size: u64) -> Result<u16> {
        let handle =
            self.con
                .upload_file(tsclientlib::ChannelId(0), path, None, size, true, false)?;
        Ok(handle.0)
    }

    /// Upload `size` bytes to `path` in a channel's file transfer (chat
    /// attachments). Never overwrites: attachment names are unique.
    pub fn request_channel_upload(
        &mut self,
        channel_id: u64,
        path: &str,
        size: u64,
    ) -> Result<u16> {
        let handle = self.con.upload_file(
            tsclientlib::ChannelId(channel_id),
            path,
            None,
            size,
            false,
            false,
        )?;
        Ok(handle.0)
    }

    /// Download `path` from a channel's file transfer (chat attachments).
    pub fn request_channel_download(&mut self, channel_id: u64, path: &str) -> Result<u16> {
        let handle =
            self.con
                .download_file(tsclientlib::ChannelId(channel_id), path, None, None)?;
        Ok(handle.0)
    }

    /// The connected server's unique identifier (derived from its public
    /// key, as TeamSpeak clients do), used in `ts3file://` links.
    pub fn server_unique_id(&self) -> Option<String> {
        let state = self.con.get_state().ok()?;
        Some(state.server.public_key.get_uid())
    }

    /// Ask the server for a client's connection statistics. Does not wait: the
    /// answer updates the bookkeeping and arrives as [`Item::StateChanged`];
    /// read it with [`Session::connection_info`].
    pub fn request_connection_info(&mut self, client_id: u16) -> Result<()> {
        self.send_command_no_wait("getconnectioninfo", &[("clid", client_id.to_string())])?;
        Ok(())
    }

    /// Our round-trip time and packet loss, measured locally by the protocol
    /// layer: the server does not report our own.
    pub fn own_network(&self) -> Option<OwnNetwork> {
        let stats = self.con.get_network_stats().ok()?;
        // Zero until the first ping reply arrives.
        (!stats.rtt.is_zero()).then(|| OwnNetwork {
            ping_ms: stats.rtt.as_secs_f64() * 1000.0,
            loss: stats.get_packetloss(),
            lost_packets: stats.total_lost,
            counted_packets: stats.total_counted,
        })
    }

    /// The latest connection statistics the server sent for a client.
    pub fn connection_info(&self, client_id: u16) -> Option<crate::state::ConnectionInfo> {
        let state = self.con.get_state().ok()?;
        crate::state::ConnectionInfo::from_state(state, client_id)
    }

    /// Publish (or clear, with an empty value) our avatar flag.
    pub async fn set_avatar_flag(&mut self, flag: &str) -> Result<()> {
        self.send_command("clientupdate", &[("client_flag_avatar", flag.to_string())])
            .await
    }

    /// Send a text message.
    pub async fn send_chat(&mut self, target: ChatTarget, text: &str) -> Result<()> {
        let (target_mode, target_id) = match target {
            ChatTarget::Server => ("3", "0".to_string()),
            ChatTarget::Channel => ("2", "0".to_string()),
            ChatTarget::Client(id) => ("1", id.to_string()),
        };
        self.send_command(
            "sendtextmessage",
            &[
                ("targetmode", target_mode.to_string()),
                ("target", target_id),
                ("msg", text.to_string()),
            ],
        )
        .await
    }

    /// Why the connection ended, when the library reported an error (a kick, a
    /// ban, a timeout or a server shutdown). `None` while it is still open.
    pub fn end_reason(&self) -> Option<&str> {
        self.end_reason.as_deref()
    }

    /// Move ourselves to another channel.
    pub async fn join_channel(&mut self, channel_id: u64) -> Result<()> {
        self.join_channel_with_password(channel_id, None).await
    }

    /// Move ourselves to a channel, sending its password when it has one.
    pub async fn join_channel_with_password(
        &mut self,
        channel_id: u64,
        password: Option<&str>,
    ) -> Result<()> {
        let own = self.own_id().ok_or_else(|| anyhow!("not connected"))?;
        self.send_command("clientmove", &clientmove_args(own, channel_id, password))
            .await
    }

    /// Join a channel by numeric id or by (case-insensitive) name.
    ///
    /// Returns the id that was actually joined, so callers can confirm it.
    pub async fn join_channel_by_name(&mut self, needle: &str) -> Result<u64> {
        self.join_channel_by_name_with_password(needle, None).await
    }

    /// Like [`Self::join_channel_by_name`], with the channel's password.
    pub async fn join_channel_by_name_with_password(
        &mut self,
        needle: &str,
        password: Option<&str>,
    ) -> Result<u64> {
        let snapshot = self.snapshot()?;
        let channel_id = snapshot
            .find_channel(needle)
            .ok_or_else(|| anyhow!("channel `{needle}` not found"))?
            .id;
        self.join_channel_with_password(channel_id, password)
            .await?;
        Ok(channel_id)
    }

    pub async fn set_nickname(&mut self, nickname: &str) -> Result<()> {
        self.send_command("clientupdate", &[("client_nickname", nickname.to_string())])
            .await
    }

    /// Marks us away, with `message` (possibly empty), or present again.
    pub async fn set_away(&mut self, message: Option<&str>) -> Result<()> {
        self.send_command("clientupdate", &away_args(message)).await
    }

    pub async fn set_muted(&mut self, input: bool, output: bool) -> Result<()> {
        self.send_command(
            "clientupdate",
            &[
                ("client_input_muted", bool_flag(input)),
                ("client_output_muted", bool_flag(output)),
            ],
        )
        .await
    }

    /// Sends our voice to these people and channels only (`None`: back to the
    /// channel). The server decides whether we may whisper.
    pub fn set_whisper(&mut self, target: Option<WhisperTarget>) -> Result<(), String> {
        self.whisper = target.map(WhisperTarget::checked).transpose()?;
        Ok(())
    }

    /// The current whisper target, if any.
    pub fn whisper(&self) -> Option<&WhisperTarget> {
        self.whisper.as_ref()
    }

    /// Voice frames handed to the transport so far (for the UI counters).
    pub fn audio_frames_sent(&self) -> u64 {
        self.audio_frames_sent
    }
    /// Send one encoded voice packet (see `ts6-audio` for the encoder).
    pub fn send_audio(&mut self, packet: OutPacket) -> Result<()> {
        self.con.send_audio(packet)?;
        Ok(())
    }

    /// Whether the server currently lets us transmit voice (not muted, talk power).
    pub fn can_send_audio(&self) -> bool {
        self.con.can_send_audio()
    }

    /// Whether we should play back incoming voice (output not muted).
    pub fn can_receive_audio(&self) -> bool {
        self.con.can_receive_audio()
    }

    /// Leave cleanly.
    pub async fn disconnect(mut self) -> Result<()> {
        self.con.disconnect(DisconnectOptions::new())?;

        // Drain until the server closes the stream, but NEVER wait forever: a peer
        // streaming audio into our channel keeps producing items, so an unbounded
        // drain hangs the process (measured: the CLI never exited while the UI was
        // transmitting a tone). Three seconds is enough for a clean shutdown.
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match timeout_at(deadline, self.con.events().next()).await {
                Ok(None) => break,
                Ok(Some(Err(_))) => break,
                Ok(Some(Ok(_))) => {}
                Err(_) => {
                    tracing::debug!("disconnect drain timed out; closing anyway");
                    break;
                }
            }
        }
        Ok(())
    }
}

/// File-transfer outcomes; every other unmodelled item is only logged.
fn file_item(item: StreamItem) -> Option<Item> {
    match item {
        StreamItem::FileDownload(handle, result) => Some(Item::FileDownload {
            handle: handle.0,
            size: result.size,
            stream: result.stream,
        }),
        StreamItem::FileUpload(handle, result) => Some(Item::FileUpload {
            handle: handle.0,
            stream: result.stream,
        }),
        StreamItem::FiletransferFailed(handle, error) => Some(Item::FileFailed {
            handle: handle.0,
            error: error.to_string(),
        }),
        other => {
            tracing::debug!(?other, "unhandled stream item");
            None
        }
    }
}

/// Arguments of `clientmove`; `cpw` is sent only for a non-empty password.
fn clientmove_args(
    own: u16,
    channel_id: u64,
    password: Option<&str>,
) -> Vec<(&'static str, String)> {
    let mut args = vec![("clid", own.to_string()), ("cid", channel_id.to_string())];
    if let Some(password) = password.filter(|p| !p.is_empty()) {
        args.push(("cpw", password.to_string()));
    }
    args
}

/// Arguments of the `clientupdate` that changes the away status.
fn away_args(message: Option<&str>) -> Vec<(&'static str, String)> {
    match message {
        Some(text) => vec![
            ("client_away", "1".to_string()),
            ("client_away_message", text.to_string()),
        ],
        None => vec![("client_away", "0".to_string())],
    }
}

fn bool_flag(value: bool) -> String {
    if value {
        "1".to_string()
    } else {
        "0".to_string()
    }
}

/// Keep replay bounded even when one library event contains a batch of messages.
/// The plain-English reason for a removal of our own client.
fn describe_removal(reason: Option<tsclientlib::Reason>, by: Option<&str>) -> Option<String> {
    use tsclientlib::Reason;
    let by = by.map(|name| format!(" by {name}")).unwrap_or_default();
    match reason? {
        Reason::KickChannel => Some(format!("kicked from the channel{by}")),
        Reason::KickServer => Some(format!("kicked from the server{by}")),
        Reason::KickServerBan => Some(format!("banned from the server{by}")),
        Reason::Serverstop | Reason::ClientdisconnectServerShutdown => {
            Some("the server shut down".to_string())
        }
        Reason::LostConnection => Some("the connection timed out".to_string()),
        _ => None,
    }
}

/// Why we were taken off the server, when a book event says so (kick, ban or
/// shutdown); `None` for ordinary changes.
fn removal_reason(events: &[tsclientlib::events::Event], own_id: Option<u16>) -> Option<String> {
    use tsclientlib::events::{Event, PropertyId};
    events.iter().find_map(|event| {
        let Event::PropertyRemoved {
            id: PropertyId::Client(client),
            invoker,
            extra,
            ..
        } = event
        else {
            return None;
        };
        if Some(client.0) != own_id {
            return None;
        }
        describe_removal(
            extra.reason,
            invoker.as_ref().map(|invoker| invoker.name.as_str()),
        )
    })
}

fn enqueue_book_items(pending: &mut VecDeque<Item>, items: Vec<Item>) {
    for item in items {
        enqueue_pending(pending, item);
    }
}

fn enqueue_pending(pending: &mut VecDeque<Item>, item: Item) {
    if pending.len() >= MAX_PENDING_ITEMS {
        pending.pop_front();
    }
    pending.push_back(item);
}

/// Upper bound on items held for replay. Overflow drops the oldest item so the
/// network keeps being polled; there is no blocking backpressure.
const MAX_PENDING_ITEMS: usize = 4096;

fn trim_pending(pending: &mut VecDeque<Item>) {
    while pending.len() > MAX_PENDING_ITEMS {
        pending.pop_front();
    }
}

fn current_channel(con: &Connection) -> Option<u64> {
    let state = con.get_state().ok()?;
    state
        .clients
        .get(&state.own_client)
        .map(|client| client.channel.0)
}

fn book_items(
    events: Vec<tsclientlib::events::Event>,
    own_id: Option<u16>,
    channel_id: Option<u64>,
) -> Vec<Item> {
    let mut items = Vec::new();
    for event in events {
        if let tsclientlib::events::Event::Message {
            invoker,
            message,
            target,
        } = event
        {
            let invoker_id = Some(invoker.id.0);
            items.push(Item::TextMessage(TextMessage {
                invoker_id,
                invoker_name: Some(invoker.name),
                channel_id: if matches!(target, tsclientlib::MessageTarget::Channel) {
                    channel_id
                } else {
                    None
                },
                target,
                message,
                from_self: invoker_id == own_id,
            }));
        }
    }
    items.push(Item::StateChanged);
    items
}

fn raw_text_items(message: &InMessage, own_id: Option<u16>, channel_id: Option<u64>) -> Vec<Item> {
    let InMessage::TextMessage(messages) = message else {
        return Vec::new();
    };
    messages
        .iter()
        .filter_map(|message| {
            let target = match message.target {
                tsclientlib::TextMessageTargetMode::Channel => tsclientlib::MessageTarget::Channel,
                tsclientlib::TextMessageTargetMode::Server => tsclientlib::MessageTarget::Server,
                tsclientlib::TextMessageTargetMode::Client => {
                    tsclientlib::MessageTarget::Client(message.target_client_id?)
                }
                tsclientlib::TextMessageTargetMode::Unknown => return None,
            };
            let invoker_id = Some(message.invoker_id.0);
            Some(Item::TextMessage(TextMessage {
                invoker_id,
                invoker_name: Some(message.invoker_name.clone()),
                message: message.message.clone(),
                channel_id,
                target,
                from_self: invoker_id == own_id,
            }))
        })
        .collect()
}

#[cfg(test)]
mod chat_tests {
    use super::*;

    #[test]
    fn a_removal_is_described_with_who_did_it() {
        use tsclientlib::Reason;
        assert_eq!(
            describe_removal(Some(Reason::KickServerBan), Some("Ana")).as_deref(),
            Some("banned from the server by Ana")
        );
        assert_eq!(
            describe_removal(Some(Reason::KickChannel), None).as_deref(),
            Some("kicked from the channel")
        );
        assert_eq!(describe_removal(Some(Reason::Moved), Some("Ana")), None);
        assert_eq!(describe_removal(None, None), None);
    }

    #[test]
    fn the_away_status_sends_its_message_only_when_going_away() {
        assert_eq!(away_args(None), vec![("client_away", "0".to_string())]);
        let away = away_args(Some("volto logo"));
        assert_eq!(away[0], ("client_away", "1".to_string()));
        assert_eq!(away[1], ("client_away_message", "volto logo".to_string()));
    }

    #[test]
    fn clientmove_sends_the_channel_password_only_when_given() {
        let plain = clientmove_args(4, 9, None);
        assert_eq!(plain.len(), 2);
        assert_eq!(clientmove_args(4, 9, Some("")), plain);
        let locked = clientmove_args(4, 9, Some("s3cret"));
        assert_eq!(locked[2], ("cpw", "s3cret".to_string()));
    }

    #[test]
    fn message_batches_keep_the_replay_queue_bounded() {
        let mut pending = VecDeque::new();
        pending.extend((0..MAX_PENDING_ITEMS).map(|_| Item::StateChanged));
        enqueue_book_items(
            &mut pending,
            vec![Item::TextMessage(TextMessage {
                invoker_id: Some(7),
                invoker_name: None,
                message: "newest fixture".into(),
                channel_id: Some(3),
                target: tsclientlib::MessageTarget::Channel,
                from_self: false,
            })],
        );
        assert_eq!(pending.len(), MAX_PENDING_ITEMS);
        assert!(
            matches!(pending.back(), Some(Item::TextMessage(msg)) if msg.message == "newest fixture")
        );
    }

    #[test]
    fn silent_reply_with_continuing_traffic_stays_bounded() {
        let mut pending = VecDeque::new();
        for _ in 0..(MAX_PENDING_ITEMS * 3) {
            pending.push_back(Item::StateChanged);
            trim_pending(&mut pending);
        }
        pending.push_back(Item::Reconnecting);
        trim_pending(&mut pending);
        assert_eq!(pending.len(), MAX_PENDING_ITEMS);
        assert!(matches!(pending.back(), Some(Item::Reconnecting)));
    }

    #[test]
    fn bookkeeping_messages_preserve_text_sender_and_self_echo() {
        let events = [7, 9]
            .into_iter()
            .map(|id| tsclientlib::events::Event::Message {
                target: tsclientlib::MessageTarget::Channel,
                invoker: tsclientlib::Invoker {
                    id: tsclientlib::ClientId(id),
                    name: "fixture peer".into(),
                    uid: None,
                },
                message: "Unicode: emoji \u{1f600} | slash / backslash \\ newline\n".into(),
            })
            .collect();
        let items = book_items(events, Some(7), Some(3));
        let messages: Vec<_> = items
            .iter()
            .filter_map(|item| match item {
                Item::TextMessage(msg) => Some(msg),
                _ => None,
            })
            .collect();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].channel_id, Some(3));
        assert_eq!(messages[0].invoker_id, Some(7));
        assert_eq!(messages[0].invoker_name.as_deref(), Some("fixture peer"));
        assert!(messages[0].from_self);
        assert!(!messages[1].from_self);
        assert_eq!(
            messages[0].message,
            "Unicode: emoji \u{1f600} | slash / backslash \\ newline\n"
        );
        assert!(items.iter().any(|item| matches!(item, Item::StateChanged)));
    }

    #[test]
    fn raw_text_message_batches_preserve_valid_records_and_skip_unknown_targets() {
        let mut header_bytes = [0_u8; tsproto_packets::S2C_HEADER_LEN];
        header_bytes[tsproto_packets::S2C_HEADER_LEN - 1] = PacketType::Command as u8;
        let header = tsproto_packets::packets::InHeader::new(Direction::S2C, &header_bytes);
        let message = InMessage::new(
            &header,
            b"notifytextmessage targetmode=2 msg=channel invokerid=7 invokername=Alice|targetmode=3 msg=server invokerid=8 invokername=Bob|targetmode=0 msg=unknown invokerid=9 invokername=Unknown|targetmode=1 msg=missing-target invokerid=10 invokername=Missing|targetmode=1 target=42 msg=private invokerid=11 invokername=Carol",
        )
        .expect("parse batched text notification");

        let items = raw_text_items(&message, Some(7), Some(3));
        let messages: Vec<_> = items
            .iter()
            .filter_map(|item| match item {
                Item::TextMessage(message) => Some(message),
                _ => None,
            })
            .collect();

        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].message, "channel");
        assert_eq!(messages[0].target, tsclientlib::MessageTarget::Channel);
        assert!(messages[0].from_self);
        assert_eq!(messages[1].message, "server");
        assert_eq!(messages[1].target, tsclientlib::MessageTarget::Server);
        assert_eq!(messages[2].message, "private");
        assert_eq!(
            messages[2].target,
            tsclientlib::MessageTarget::Client(tsclientlib::ClientId(42))
        );
    }
}

/// One encoded voice frame as a packet: for the channel, or a whisper.
fn voice_packet(whisper: Option<&WhisperTarget>, payload: &[u8]) -> OutPacket {
    use tsproto_packets::packets::{AudioData, CodecType, OutAudio};
    let codec = CodecType::OpusVoice;
    OutAudio::new(&match whisper {
        Some(target) => AudioData::C2SWhisper {
            id: 0,
            codec,
            channels: target.channels.clone(),
            clients: target.clients.clone(),
            data: payload,
        },
        None => AudioData::C2S {
            id: 0,
            codec,
            data: payload,
        },
    })
}

#[cfg(test)]
mod whisper_tests {
    use super::*;
    use tsproto_packets::packets::PacketType;

    #[test]
    fn a_whisper_target_is_deduplicated_and_bounded() {
        let target = WhisperTarget {
            clients: vec![7, 3, 7],
            channels: vec![],
        }
        .checked()
        .unwrap();
        assert_eq!(target.clients, vec![3, 7]);
        assert!(WhisperTarget::default().checked().is_err());
        let many = WhisperTarget {
            clients: (0..=WHISPER_CLIENTS_MAX as u16).collect(),
            channels: vec![],
        };
        assert!(many.checked().is_err());
    }

    #[test]
    fn voice_goes_as_a_whisper_packet_only_with_a_target() {
        let target = WhisperTarget {
            clients: vec![4],
            channels: vec![],
        };
        let whisper = voice_packet(Some(&target), &[1, 2, 3]);
        assert_eq!(whisper.header().packet_type(), PacketType::VoiceWhisper);
        let plain = voice_packet(None, &[1, 2, 3]);
        assert_eq!(plain.header().packet_type(), PacketType::Voice);
    }
}
