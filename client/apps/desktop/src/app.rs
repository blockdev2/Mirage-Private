//! The main window: sidebar, voice session, chat, status bar and connect screen.
//!
//! All user-facing text comes from [`crate::strings`]. Code comments are English.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use egui::{
    pos2, vec2, Align, Align2, Color32, CornerRadius, FontId, Frame, Layout, Margin, Rect,
    RichText, Sense, Stroke, UiBuilder,
};

use crate::bridge::{Bridge, Incoming, SocketState};
use crate::clock;
use crate::protocol::{
    AudioEvent, BridgeEvent, ChannelInfo, ClientInfo, Command, DeviceLists, LogLine, Snapshot,
    StatusEvent,
};
use crate::store::{AddressProblem, Store, TransmitMode};
use crate::strings::{Strings, PT};
use crate::theme::{self, icon, Tone};

mod chat;
#[cfg(debug_assertions)]
mod dev;
mod home;
mod peer;
mod settings;
mod share;
mod stream;
mod updates;
use chat::{ChatLog, Delivery, Entry, Failure, Incoming as ChatIncoming, Scope, MAX_MESSAGE_BYTES};
use home::{Dialog, PendingConnect, Section};

const MAX_LOG_LINES: usize = 300;
const MAX_TREE_DEPTH: usize = 16;
/// How long the connect button stays disabled while waiting for the bridge.
const CONNECT_WAIT: Duration = Duration::from_secs(15);

// Layout metrics, all multiples of 4 where it matters.
const SIDEBAR_WIDTH: f32 = 272.0;
const STATUS_BAR_HEIGHT: f32 = 40.0;
/// Widest the count, view switch and participant cards may grow together.
const CONTENT_MAX_WIDTH: f32 = 1040.0;
const CARD_MIN_WIDTH: f32 = 220.0;
const CARD_MIN_HEIGHT: f32 = 96.0;
/// Smallest chat-history area worth drawing; below it the history is skipped.
const HISTORY_MIN_HEIGHT: f32 = 28.0;
const CARD_GAP: f32 = 16.0;
const MAX_COLUMNS: usize = 4;
/// Height kept below the grid: controls, divider, chat heading, composer and a
/// usable slice of history.
const BELOW_GRID_MIN_HEIGHT: f32 = 300.0;
/// Space above and below the call controls.
const CONTROLS_GAP_ABOVE: f32 = 20.0;
const CONTROLS_GAP_BELOW: f32 = 8.0;
/// Space between the divider and the chat heading.
const SPLIT_GAP_BELOW: f32 = 2.0;
/// Height of the grab area between the call controls and the chat.
const SPLIT_HANDLE_HEIGHT: f32 = 24.0;
/// Smallest chat the divider can shrink to: heading, composer and a sliver of
/// history.
const CHAT_MIN_HEIGHT: f32 = 96.0;
/// Distance one arrow-key press moves the divider.
const SPLIT_KEY_STEP: f32 = 24.0;
const CONTROL_SIZE: f32 = 44.0;
/// Avatar beside a chat message, and the send button in the composer.
const CHAT_AVATAR: f32 = 40.0;
/// Space between a message's avatar and its text.
const CHAT_GAP: f32 = 12.0;
/// Message text: softer than the sender's name, brighter than the time.
const CHAT_TEXT: Color32 = Color32::from_rgb(205, 210, 219);
const CHAT_SEND_SIZE: f32 = 28.0;
/// Largest size a chat picture is drawn at.
const CHAT_PICTURE_MAX: egui::Vec2 = egui::vec2(360.0, 240.0);
/// Files per message, as the official client allows.
const MAX_STAGED: usize = 10;

struct ImageViewer {
    file: String,
    uri: String,
    zoom: f32,
    offset: egui::Vec2,
}

/// A chat attachment's local state.
#[derive(Clone)]
enum ChatFile {
    Loading,
    Ready {
        file: String,
        uri: String,
        picture: bool,
    },
    Unavailable,
}

/// The text of a message without the BBCode of its attachments, which are
/// drawn separately.
fn display_text(segments: &[ts6_bridge::attachments::Segment]) -> String {
    use ts6_bridge::attachments::Segment;
    let mut text = String::new();
    for segment in segments {
        if let Segment::Text(part) = segment {
            text.push_str(part);
        }
    }
    text.trim().to_string()
}
/// Most messages drawn at once; older ones stay in the log.
const CHAT_RENDER_LIMIT: usize = 200;
/// Width of the logo in the sidebar, in points. The bitmap is resampled to
/// exactly this many device pixels, so it is never stretched or blurred.
const LOGO_WIDTH: f32 = 208.0;
const LOGO: &[u8] = include_bytes!("../assets/logo.webp");
const CONTROL_GAP: f32 = 8.0;
/// Width of the "Convidar" button.
const HEADER_ACTIONS_WIDTH: f32 = 132.0;
/// How long "Link copiado" replaces "Convidar".
const INVITE_FEEDBACK: Duration = Duration::from_millis(1800);

pub struct App {
    /// Active text table. Switch this reference to change language.
    t: &'static Strings,
    bridge: Bridge,
    socket: SocketState,
    bridge_version: Option<String>,
    devices: DeviceLists,
    status: StatusEvent,
    snapshot: Option<Snapshot>,
    audio: Option<AudioEvent>,
    logs: Vec<LogLine>,
    session_started: Option<Instant>,

    // Local profiles, favorites and history (see `store`).
    store: Store,
    section: Section,
    dialog: Option<Dialog>,
    search: String,

    // Quick-connect form. The password is never persisted.
    address: String,
    address_problem: Option<AddressProblem>,
    selected_favorite: Option<String>,
    use_other_nickname: bool,
    other_nickname: String,
    password: String,
    save_favorite: bool,
    show_password: bool,
    /// Profile photo textures by profile id, with the photo revision they show.
    photo_cache: HashMap<String, (u64, Option<egui::TextureHandle>)>,
    /// Volume last sent to the engine per client id in this session.
    volumes_sent: HashMap<u16, f32>,
    /// A volume changed but is not written to disk yet.
    volumes_dirty: bool,
    /// Open connection info window, if any.
    info_view: Option<peer::InfoView>,
    /// Our own ping, one sample a second, for the status bar graph.
    ping_history: std::collections::VecDeque<f32>,
    ping_sampled: Option<std::time::Instant>,
    /// Cached server-side avatars by client uid, and the server icon (local paths).
    media_avatars: HashMap<String, String>,
    server_icon: Option<String>,
    /// Textures of those files by path; None marks a file that failed to load.
    media_textures: HashMap<String, Option<egui::TextureHandle>>,
    /// What to record once the connection in flight succeeds.
    pending: Option<PendingConnect>,

    /// Nickname of the connected session, editable in the settings window.
    nickname: String,

    // Session view state.
    /// When the invite link was copied; the button says so for a moment.
    invite_copied: Option<Instant>,
    /// Scripted test plan and the input it injects (debug builds only).
    #[cfg(debug_assertions)]
    dev_plan: Option<dev::Plan>,
    #[cfg(debug_assertions)]
    dev_events: Vec<egui::Event>,
    /// Channel the call timer belongs to; the timer restarts when it changes.
    call_channel: Option<u64>,
    /// Height the user gave the chat by dragging the divider. `None` keeps the
    /// automatic layout, where the chat takes whatever the participants leave.
    chat_height: Option<f32>,
    mic_muted: bool,
    output_muted: bool,
    chat_draft: String,
    /// The message being answered: author and its first line.
    chat_reply: Option<(String, String)>,
    /// A chat picture opened large: cached file, texture uri, zoom and pan.
    image_viewer: Option<ImageViewer>,
    /// Chat attachments by (channel, server path): fetched once, then shown.
    chat_files: HashMap<(String, String), ChatFile>,
    /// Attachment sends waiting for their `chat_result`, by request id.
    pending_attachments: HashMap<String, String>,
    /// Files picked or pasted, waiting in the composer for a caption.
    staged: Vec<std::path::PathBuf>,
    /// Preview textures of staged pictures (None when not a picture).
    staged_previews: HashMap<std::path::PathBuf, Option<egui::TextureHandle>>,
    /// The composer's height last frame, to size the history above it.
    composer_height: f32,
    /// Staged files already sent off, uploaded one at a time in order.
    upload_queue: std::collections::VecDeque<std::path::PathBuf>,
    /// Room chat history and the delivery state of what we sent.
    chat: ChatLog,
    /// Address of the server of the live session (also where its chat is kept).
    session_address: Option<String>,
    /// Whether the bridge advertised the correlated channel-chat contract.
    channel_chat: bool,
    /// Counter for chat request ids, unique within this application run.
    next_chat_request: u64,
    /// The logo resampled for the current display scale.
    logo: Option<LogoTexture>,
    /// Why the last attempt to send was refused before it left, if it was.
    chat_notice: Option<&'static str>,
    /// The screen stream being watched, if any.
    watching: Option<stream::Watching>,
    /// Whether the window is currently fullscreen because of a stream.
    window_fullscreen: bool,
    /// Our own screen share, as the bridge last reported it.
    sharing: Option<share::Sharing>,
    /// Monitors listed when the share picker was last opened.
    share_monitors: Vec<share::MonitorChoice>,
    /// When the last connect request was sent. While it is recent the button
    /// stays disabled: repeated clicks make the server flood-ban this address.
    connect_started: Option<Instant>,
    /// Expand/collapse overrides of the channel tree. A channel without an
    /// entry is open only while it is the one the user is in.
    open_channels: HashMap<u64, bool>,
    /// Whether the connected server shows its channels in the sidebar.
    server_open: bool,

    // Settings window and diagnostics.
    show_settings: bool,
    /// Server addresses are censored until the user reveals them.
    show_addresses: bool,
    /// The quick-connect address field has keyboard focus.
    address_focused: bool,
    /// Name and photo editing inside the settings window.
    profile_edit: settings::ProfileEdit,
    show_logs: bool,
    /// In-app updates; see `updater.rs`.
    updater: crate::updater::Updater,
    /// When the window opened, for the one automatic update check.
    opened_at: Instant,
    startup_update_check: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx);
        stream::install_waker(&cc.egui_ctx);
        let store = Store::open();
        // Start from the server used last, if any.
        let address = store
            .data
            .history
            .first()
            .map(|e| e.address.clone())
            .unwrap_or_default();
        let selected_favorite = store.favorite_for(&address).map(|f| f.id.clone());
        Self {
            store,
            section: Section::Home,
            dialog: None,
            search: String::new(),
            address_problem: None,
            selected_favorite,
            use_other_nickname: false,
            other_nickname: String::new(),
            save_favorite: false,
            show_password: false,
            photo_cache: HashMap::new(),
            volumes_sent: HashMap::new(),
            volumes_dirty: false,
            info_view: None,
            ping_history: Default::default(),
            ping_sampled: None,
            media_avatars: HashMap::new(),
            server_icon: None,
            media_textures: HashMap::new(),
            pending: None,
            nickname: String::new(),
            t: &PT,
            bridge: Bridge::spawn(cc.egui_ctx.clone()),
            socket: SocketState::Connecting,
            bridge_version: None,
            devices: DeviceLists::default(),
            status: StatusEvent::default(),
            snapshot: None,
            audio: None,
            logs: Vec::new(),
            session_started: None,
            address,
            password: String::new(),
            invite_copied: None,
            call_channel: None,
            #[cfg(debug_assertions)]
            dev_plan: dev::Plan::from_env(),
            #[cfg(debug_assertions)]
            dev_events: Vec::new(),
            chat_height: None,
            mic_muted: false,
            output_muted: false,
            chat_draft: String::new(),
            chat_reply: None,
            image_viewer: None,
            chat: ChatLog::default(),
            session_address: None,
            channel_chat: false,
            next_chat_request: 0,
            chat_files: HashMap::new(),
            pending_attachments: HashMap::new(),
            staged: Vec::new(),
            staged_previews: HashMap::new(),
            composer_height: 56.0,
            upload_queue: Default::default(),
            logo: None,
            chat_notice: None,
            watching: None,
            window_fullscreen: false,
            sharing: None,
            share_monitors: Vec::new(),
            connect_started: None,
            open_channels: HashMap::new(),
            server_open: true,
            show_settings: false,
            show_addresses: false,
            address_focused: false,
            profile_edit: settings::ProfileEdit::default(),
            show_logs: false,
            updater: {
                let ctx = cc.egui_ctx.clone();
                crate::updater::Updater::start(move || ctx.request_repaint())
            },
            opened_at: Instant::now(),
            startup_update_check: false,
        }
    }

    /// Applies everything the link thread has produced since the last frame.
    fn drain_events(&mut self) {
        while let Ok(incoming) = self.bridge.incoming.try_recv() {
            match incoming {
                Incoming::Socket(state) => {
                    self.socket = state;
                    if state != SocketState::Open {
                        self.bridge_version = None;
                        self.channel_chat = false;
                    }
                }
                Incoming::Event(event) => self.apply_event(event),
            }
        }
        let meter = self
            .bridge
            .audio
            .lock()
            .ok()
            .and_then(|mut slot| slot.take());
        // A meter read before a disconnect must not revive the call view.
        if let Some(meter) = meter.filter(|_| self.status.connected) {
            self.apply_event(BridgeEvent::Audio(Box::new(meter)));
        }
    }

    fn apply_event(&mut self, event: BridgeEvent) {
        match event {
            BridgeEvent::Hello {
                version,
                channel_chat,
                devices,
            } => {
                self.bridge_version = Some(version);
                self.channel_chat = channel_chat;
                self.devices = devices;
            }
            BridgeEvent::Status(status) => {
                if status.connected && !self.status.connected {
                    self.session_started = Some(Instant::now());
                }
                if !status.connected {
                    if self.status.connected {
                        // The session ended: go back to the hub.
                        self.section = Section::Home;
                    }
                    self.snapshot = None;
                    self.audio = None;
                    self.watching = None;
                    self.sharing = None;
                    self.session_started = None;
                    self.call_channel = None;
                    self.ping_history.clear();
                    self.ping_sampled = None;
                    self.volumes_sent.clear();
                    self.info_view = None;
                    self.media_avatars.clear();
                    self.server_icon = None;
                    self.media_textures.clear();
                    self.chat.clear();
                    self.session_address = None;
                    self.chat_notice = None;
                    // A failed attempt must not be recorded as a connection.
                    if status.error.is_some() || self.status.connected {
                        self.pending = None;
                    }
                }
                if status.connected {
                    if let Some(name) = &status.nickname {
                        self.nickname = name.clone();
                    }
                }
                self.status = status;
                // Any answer from the bridge ends the wait.
                self.connect_started = None;
            }
            BridgeEvent::State { snapshot } => {
                if self.snapshot.is_none() {
                    // First view of the server: it counts as connected.
                    self.section = Section::Voice;
                    let profile = self
                        .pending
                        .as_ref()
                        .and_then(|p| p.profile_id.clone())
                        .or_else(|| self.store.active_profile_id());
                    self.session_address = self.pending.as_ref().map(|p| p.address.clone());
                    self.finish_connection(&snapshot.server.name);
                    if let Some(address) = self.session_address.clone() {
                        for (channel, saved) in self.store.transcripts_for(&address) {
                            self.chat.restore(channel, saved);
                        }
                    }
                    // The voice identity is shared by every local profile, so the
                    // server avatar always follows the profile used to connect:
                    // its photo, or none.
                    self.publish_avatar(profile.as_deref());
                    // Voice works as soon as the call starts, with the saved settings.
                    self.apply_audio();
                }
                self.snapshot = Some(snapshot);
                self.check_watched();
                self.sync_volumes();
            }
            BridgeEvent::Log(line) => {
                self.logs.push(line);
                if self.logs.len() > MAX_LOG_LINES {
                    let excess = self.logs.len() - MAX_LOG_LINES;
                    self.logs.drain(..excess);
                }
            }
            BridgeEvent::Audio(audio) => {
                if let Some(ping) = audio.ping_ms.filter(|_| {
                    self.ping_sampled
                        .is_none_or(|at| at.elapsed() >= std::time::Duration::from_secs(1))
                }) {
                    if self.ping_history.len() >= peer::HISTORY {
                        self.ping_history.pop_front();
                    }
                    self.ping_history.push_back(ping as f32);
                    self.ping_sampled = Some(std::time::Instant::now());
                }
                self.audio = Some(*audio);
            }
            BridgeEvent::Devices { devices } => self.devices = devices,
            BridgeEvent::ConnectionInfo { client, info } => {
                if let (Some(view), Some(info)) = (self.info_view.as_mut(), info) {
                    if view.client == client {
                        view.push(info);
                    }
                }
            }
            BridgeEvent::Chat(message) => {
                // The bridge forwards channel messages only, tagged with the
                // channel they were received in.
                if let Ok(channel) = message.channel.parse::<u64>() {
                    let incoming = ChatIncoming {
                        scope: Scope::Channel,
                        channel: Some(channel),
                        from_id: None,
                        author: message.sender,
                        text: message.text,
                        mine: message.from_self,
                    };
                    self.chat
                        .receive(incoming, Instant::now(), clock::local_hour_minute());
                    self.keep_chat();
                }
            }
            BridgeEvent::ChatResult {
                request_id,
                error,
                code,
            } => {
                if self.pending_attachments.remove(&request_id).is_some() {
                    // The posted message arrives like any other one; only a
                    // failure needs saying.
                    self.chat_notice = error.map(|_| match code.as_deref() {
                        Some("upload_permission") => self.t.chat_attach_no_permission,
                        _ => self.t.chat_attach_failed,
                    });
                    return;
                }
                let failure = error.map(|_| Failure::parse(code.as_deref()));
                self.chat.resolve(&request_id, failure);
                self.keep_chat();
            }
            BridgeEvent::ChatFile {
                channel,
                path,
                file,
                kind,
            } => {
                let state = match file {
                    Some(file) => ChatFile::Ready {
                        uri: format!("file://{}", file.replace('\\', "/")),
                        file,
                        picture: matches!(kind.as_deref(), Some("image" | "gif")),
                    },
                    None => ChatFile::Unavailable,
                };
                self.chat_files.insert((channel, path), state);
            }
            BridgeEvent::Media {
                avatars,
                server_icon,
            } => {
                self.media_avatars = avatars;
                self.server_icon = server_icon;
            }
            BridgeEvent::StreamState(update) => self.on_stream_state(update),
            BridgeEvent::ShareState(update) => self.on_share_state(update),
        }
    }

    fn send(&self, command: Command) {
        // A closed channel means the link thread stopped; the UI keeps running.
        // Never block the UI: a full or closed queue drops the command. A
        // dropped chat send stays pending and then reports no answer.
        let _ = self.bridge.outgoing.try_send(command);
    }

    /// The bridge applies both flags on each call, so always send the pair.
    fn send_mute(&self) {
        self.send(Command::Mute {
            input: self.mic_muted,
            output: self.output_muted,
        });
    }

    /// True for a short window after a connect request, until the bridge
    /// answers or the wait times out.
    fn is_connecting(&self) -> bool {
        self.connect_started
            .is_some_and(|started| started.elapsed() < CONNECT_WAIT)
    }

    fn in_session(&self) -> bool {
        self.status.connected && self.snapshot.is_some()
    }

    /// The screen to draw. The voice screen needs a live session; without one
    /// the hub is shown instead.
    fn effective_section(&self) -> Section {
        match self.section {
            Section::Voice if !self.in_session() => Section::Home,
            other => other,
        }
    }

    fn own_channel(&self, snapshot: &Snapshot) -> Option<u64> {
        snapshot
            .clients
            .iter()
            .find(|c| c.is_self)
            .map(|c| c.channel_id)
    }

    /// Speaking state: for others, voice arriving from the server; for us, our
    /// own microphone actually transmitting (voice detected or key held).
    fn is_speaking(&self, client_id: u16) -> bool {
        let own = self.snapshot.as_ref().map(|s| s.own_client_id);
        self.audio.as_ref().is_some_and(|audio| {
            if own == Some(client_id) {
                audio.transmitting && !self.mic_muted
            } else {
                audio.speaking.contains(&client_id)
            }
        })
    }

    /// How loud someone is right now, 0..1: our own microphone level, or the
    /// decoded voice of a peer. Mapped from -55..-15 dBFS, where speech lives.
    fn voice_level(&self, client_id: u16) -> f32 {
        let Some(audio) = self.audio.as_ref() else {
            return 0.0;
        };
        let own = self.snapshot.as_ref().map(|s| s.own_client_id);
        let db = if own == Some(client_id) {
            audio.input_level_db.unwrap_or(-90.0)
        } else {
            audio
                .levels
                .get(&client_id.to_string())
                .map_or(-90.0, |rms| 20.0 * rms.max(1e-6).log10())
        };
        ((db + 55.0) / 40.0).clamp(0.0, 1.0)
    }

    /// The state mark at the right of a participant, vertically centered on
    /// `at` (its right edge): deafened, then muted, both in red; otherwise
    /// live bars that follow the voice while the person speaks.
    fn voice_badge(
        &self,
        painter: &egui::Painter,
        at: egui::Pos2,
        size: f32,
        client: &ClientInfo,
        speaking: bool,
    ) {
        let glyph = if client.output_muted {
            Some(icon::SPEAKER_SLASH)
        } else if client.input_muted {
            Some(icon::MICROPHONE_SLASH)
        } else {
            None
        };
        if let Some(glyph) = glyph {
            painter.text(
                at,
                Align2::RIGHT_CENTER,
                glyph,
                FontId::new(size, theme::icons()),
                theme::DANGER,
            );
            return;
        }
        let ctx = painter.ctx();
        let target = if speaking {
            self.voice_level(client.id).max(0.15)
        } else {
            0.0
        };
        // Eased so the bars glide between the ~12 updates a second.
        let level =
            ctx.animate_value_with_time(egui::Id::unique(("voice-level", client.id)), target, 0.09);
        if level <= 0.01 {
            return;
        }
        theme::voice_bars(painter, at, size, level, ctx.input(|i| i.time) as f32);
        ctx.request_repaint();
    }

    /// Sends the saved audio settings to the voice engine and persists them.
    fn apply_audio(&mut self) {
        let audio = self.store.data.audio.clone();
        self.send(Command::Audio {
            enabled: audio.enabled,
            // The app always uses the real microphone.
            tone: false,
            ptt: if audio.mode == TransmitMode::PushToTalk {
                audio.ptt.trim().to_string()
            } else {
                String::new()
            },
            input_device: audio.input_device.clone(),
            output_device: audio.output_device.clone(),
            voice_activation: audio.mode == TransmitMode::VoiceActivation,
            vad_threshold: audio.threshold_db,
            noise_suppression: audio.noise.level(),
            monitor: audio.monitor,
        });
        self.store.save();
    }
    fn elapsed_label(&self) -> String {
        let secs = self
            .session_started
            .map_or(0, |start| start.elapsed().as_secs());
        format!(
            "{:02}:{:02}:{:02}",
            secs / 3600,
            (secs % 3600) / 60,
            secs % 60
        )
    }
}

impl eframe::App for App {
    /// Closing the window during a call must end the session on the server;
    /// otherwise the identity lingers and reopening fails as a clone. Waits at
    /// most one second so a stuck bridge cannot hold the window open.
    fn on_exit(&mut self) {
        if !self.status.connected {
            return;
        }
        self.send(Command::Disconnect);
        let deadline = Instant::now() + Duration::from_secs(1);
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match self.bridge.incoming.recv_timeout(left) {
                Ok(Incoming::Event(BridgeEvent::Status(status))) if !status.connected => break,
                Ok(_) => {}
                Err(_) => break,
            }
        }
    }

    #[cfg(debug_assertions)]
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        raw_input.events.append(&mut self.dev_events);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain_events();
        let ctx = ui.ctx().clone();
        #[cfg(debug_assertions)]
        self.dev_step(&ctx);
        if self.is_connecting() {
            // Re-enables the button when the wait times out.
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        let now = Instant::now();
        let channel = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| self.own_channel(snapshot));
        if channel != self.call_channel {
            self.call_channel = channel;
            self.session_started = channel.map(|_| now);
        }
        self.chat.expire(now);
        if let Some(wait) = self.chat.next_expiry(now) {
            // Marks a message as unconfirmed the moment its wait runs out.
            ctx.request_repaint_after(wait + Duration::from_millis(50));
        }
        if self.show_settings {
            // Keeps the microphone level meter live.
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        if self.status.connected {
            // Keeps the session timer moving even when no event arrives.
            ctx.request_repaint_after(Duration::from_secs(1));
        }

        self.sync_stream_fullscreen(&ctx);
        if self.watching.as_ref().is_some_and(|w| w.fullscreen) {
            // The stream alone, edge to edge. Esc or the corner button returns.
            egui::CentralPanel::default()
                .frame(Frame::new().fill(Color32::BLACK))
                .show(ui, |ui| {
                    let stage = ui.max_rect();
                    self.stream_stage(ui, stage);
                });
            self.dialogs(&ctx);
            return;
        }

        egui::Panel::left("sidebar")
            .exact_size(SIDEBAR_WIDTH)
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(theme::SIDEBAR)
                    .inner_margin(Margin::symmetric(16, 20))
                    .stroke(Stroke::new(1.0, theme::BORDER_SOFT)),
            )
            .show(ui, |ui| self.sidebar(ui));

        egui::Panel::bottom("status_bar")
            .exact_size(STATUS_BAR_HEIGHT)
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(theme::BG)
                    .inner_margin(Margin::symmetric(24, 0))
                    .stroke(Stroke::new(1.0, theme::BORDER_SOFT)),
            )
            .show(ui, |ui| self.status_bar(ui));

        if self.show_logs {
            egui::Panel::bottom("diagnostics")
                .default_size(170.0)
                .resizable(true)
                .frame(
                    Frame::new()
                        .fill(theme::SIDEBAR)
                        .inner_margin(Margin::same(14))
                        .stroke(Stroke::new(1.0, theme::BORDER_SOFT)),
                )
                .show(ui, |ui| self.log_panel(ui));
        }

        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(theme::BG)
                    .inner_margin(Margin::symmetric(24, 20)),
            )
            .show(ui, |ui| match self.effective_section() {
                Section::Voice => self.session_view(ui),
                Section::Home => self.home_view(ui),
                Section::History => self.history_view(ui),
            });

        self.ensure_profile_dialog();
        self.dialogs(&ctx);
        self.update_housekeeping(&ctx);
        self.settings_window(&ctx);
        self.image_viewer_window(&ctx);
        // Uploads go one by one so they post in the order they were added.
        if self.pending_attachments.is_empty() {
            if let Some(file) = self.upload_queue.pop_front() {
                self.attach_file(file);
            }
        }
        self.connection_window(&ctx);
    }
}

// Sidebar -------------------------------------------------------------------

impl App {
    fn logo(&mut self, ui: &mut egui::Ui) {
        let ppp = ui.ctx().pixels_per_point();
        let key = (ppp * 100.0).round() as u32;
        if self.logo.as_ref().is_none_or(|logo| logo.key != key) {
            self.logo = LogoTexture::build(ui.ctx(), LOGO_WIDTH * ppp, key);
        }
        match &self.logo {
            Some(logo) => {
                ui.add(
                    egui::Image::new(&logo.texture)
                        .fit_to_exact_size(vec2(LOGO_WIDTH, LOGO_WIDTH / logo.aspect)),
                );
            }
            None => {
                ui.label(RichText::new(self.t.brand_name).size(24.0).strong());
            }
        }
    }

    /// The connected server: a header row that opens the call view, with its
    /// channels folded under it. It stays in the list on every screen, so the
    /// call is never out of reach.
    fn connected_server(&mut self, ui: &mut egui::Ui) {
        let Some(snapshot) = self.snapshot.clone() else {
            return;
        };
        let t = self.t;
        let active = self.effective_section() == Section::Voice;
        let icon = self
            .server_icon
            .clone()
            .and_then(|path| self.media_texture(ui.ctx(), &path));
        let name = snapshot.server.name.as_str();
        let channel = self.own_channel(&snapshot).and_then(|id| {
            snapshot
                .channels
                .iter()
                .find(|c| c.id == id)
                .map(|c| c.name.as_str())
        });

        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::click());
        response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, active, name));
        let painter = ui.painter();
        if response.has_focus() {
            theme::focus_ring(painter, rect);
        }
        if active {
            painter.rect_filled(rect, CornerRadius::same(6), theme::ACCENT_SOFT);
            let bar = Rect::from_min_size(rect.left_top() + vec2(0.0, 16.0), vec2(3.0, 20.0));
            painter.rect_filled(bar, CornerRadius::same(2), theme::ACCENT);
        } else if response.hovered() {
            painter.rect_filled(rect, CornerRadius::same(6), theme::SURFACE);
        }
        draw_avatar(
            ui,
            rect.left_center() + vec2(30.0, 0.0),
            34.0,
            name,
            false,
            icon.as_ref(),
        );
        let painter = ui.painter();
        let text_left = rect.left() + 56.0;
        let text_room = rect.width() - 56.0 - 34.0;
        let title = theme::fit_text(
            painter,
            name,
            FontId::new(14.0, theme::semibold()),
            theme::TEXT,
            text_room,
        );
        painter.galley(pos2(text_left, rect.center().y - 17.0), title, theme::TEXT);
        // A live dot and the channel the call is in.
        painter.circle_filled(
            pos2(text_left + 4.0, rect.center().y + 10.0),
            3.5,
            theme::SUCCESS,
        );
        let line = theme::fit_text(
            painter,
            channel.unwrap_or(t.voice_connected),
            FontId::new(12.0, theme::medium()),
            theme::TEXT_2,
            text_room - 14.0,
        );
        painter.galley(
            pos2(
                text_left + 14.0,
                rect.center().y + 10.0 - line.size().y / 2.0,
            ),
            line,
            theme::TEXT_2,
        );
        painter.text(
            rect.right_center() - vec2(18.0, 0.0),
            Align2::CENTER_CENTER,
            if self.server_open {
                icon::CARET_DOWN
            } else {
                icon::CARET_RIGHT
            },
            FontId::new(12.0, theme::icons()),
            theme::TEXT_3,
        );

        if response.clicked() {
            let on_caret = response
                .interact_pointer_pos()
                .is_some_and(|pos| pos.x > rect.right() - 36.0);
            if active || on_caret {
                self.server_open = !self.server_open;
            } else {
                self.section = Section::Voice;
                self.server_open = true;
            }
        }
        if self.server_open {
            ui.add_space(2.0);
            self.channel_tree(ui, &snapshot);
        }
    }

    /// Sidebar navigation row. Returns true when it was clicked this frame.
    fn nav_item(&self, ui: &mut egui::Ui, glyph: &str, label: &str, active: bool) -> bool {
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
        response
            .widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, active, label));
        let painter = ui.painter();
        if response.has_focus() {
            theme::focus_ring(painter, rect);
        }
        if active {
            painter.rect_filled(rect, CornerRadius::same(6), theme::ACCENT_SOFT);
            let bar = Rect::from_min_size(rect.left_top() + vec2(0.0, 10.0), vec2(3.0, 20.0));
            painter.rect_filled(bar, CornerRadius::same(2), theme::ACCENT);
        } else if response.hovered() {
            painter.rect_filled(rect, CornerRadius::same(6), theme::SURFACE);
        }
        let color = if active { theme::TEXT } else { theme::TEXT_2 };
        painter.text(
            rect.left_center() + vec2(18.0, 0.0),
            Align2::LEFT_CENTER,
            glyph,
            FontId::new(17.0, theme::icons()),
            color,
        );
        painter.text(
            rect.left_center() + vec2(46.0, 0.0),
            Align2::LEFT_CENTER,
            label,
            FontId::new(14.0, theme::medium()),
            color,
        );
        response.clicked()
    }

    fn channel_tree(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        let own = self.own_channel(snapshot);
        let ids: std::collections::HashSet<u64> = snapshot.channels.iter().map(|c| c.id).collect();
        // Channels whose parent is missing from the list (usually 0) are roots.
        for channel in snapshot
            .channels
            .iter()
            .filter(|c| c.id != c.parent && !ids.contains(&c.parent))
        {
            self.channel_node(ui, snapshot, channel, 0, own);
        }
    }

    fn channel_node(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &Snapshot,
        channel: &ChannelInfo,
        depth: usize,
        own: Option<u64>,
    ) {
        let selected = own == Some(channel.id);
        let members: Vec<&ClientInfo> = snapshot
            .clients
            .iter()
            .filter(|c| c.channel_id == channel.id)
            .collect();
        let children: Vec<&ChannelInfo> = snapshot
            .channels
            .iter()
            .filter(|c| c.parent == channel.id && c.id != channel.id)
            .collect();
        let expandable = !members.is_empty() || !children.is_empty();
        let open = self
            .open_channels
            .get(&channel.id)
            .copied()
            .unwrap_or(selected);

        let indent = depth as f32 * 16.0;
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::click());
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::Role::Button, true, channel.name.as_str())
        });
        let painter = ui.painter();
        if selected {
            painter.rect_filled(rect, CornerRadius::same(6), theme::SURFACE_RAISED);
        } else if response.hovered() {
            painter.rect_filled(rect, CornerRadius::same(6), theme::SURFACE);
        }
        if response.has_focus() {
            painter.rect_stroke(
                rect,
                CornerRadius::same(6),
                Stroke::new(2.0, theme::ACCENT),
                egui::StrokeKind::Inside,
            );
        }
        let ink = if selected { theme::TEXT } else { theme::TEXT_2 };
        if expandable {
            painter.text(
                rect.left_center() + vec2(16.0 + indent, 0.0),
                Align2::CENTER_CENTER,
                if open {
                    icon::CARET_DOWN
                } else {
                    icon::CARET_RIGHT
                },
                FontId::new(12.0, theme::icons()),
                theme::TEXT_3,
            );
        }
        painter.text(
            rect.left_center() + vec2(38.0 + indent, 0.0),
            Align2::CENTER_CENTER,
            icon::SPEAKER,
            FontId::new(16.0, theme::icons()),
            ink,
        );
        let name_room =
            rect.width() - (54.0 + indent) - if members.is_empty() { 12.0 } else { 36.0 };
        let name = theme::fit_text(
            painter,
            &channel.name,
            FontId::new(
                14.0,
                if selected {
                    theme::semibold()
                } else {
                    theme::medium()
                },
            ),
            ink,
            name_room,
        );
        painter.galley(
            pos2(
                rect.left() + 54.0 + indent,
                rect.center().y - name.size().y / 2.0,
            ),
            name,
            ink,
        );
        if !members.is_empty() {
            painter.text(
                rect.right_center() - vec2(12.0, 0.0),
                Align2::RIGHT_CENTER,
                members.len().to_string(),
                FontId::new(12.0, theme::medium()),
                theme::TEXT_3,
            );
        }

        if response.clicked() {
            let on_caret = response
                .interact_pointer_pos()
                .is_some_and(|pos| pos.x < rect.left() + 30.0 + indent);
            if (on_caret || selected) && expandable {
                self.open_channels.insert(channel.id, !open);
            } else if !selected {
                self.open_channels.insert(channel.id, true);
                self.send(Command::Join {
                    channel: channel.id.to_string(),
                });
            }
            if !(on_caret && expandable) {
                self.section = Section::Voice;
            }
        }

        if !open {
            return;
        }
        for client in &members {
            self.member_row(ui, client, depth + 1);
        }
        if depth >= MAX_TREE_DEPTH {
            return;
        }
        for child in children {
            self.channel_node(ui, snapshot, child, depth + 1, own);
        }
    }

    fn member_row(&mut self, ui: &mut egui::Ui, client: &ClientInfo, depth: usize) {
        let row = self.member_row_inner(ui, client, depth);
        self.peer_menu(&row, client);
    }

    fn member_row_inner(
        &mut self,
        ui: &mut egui::Ui,
        client: &ClientInfo,
        depth: usize,
    ) -> egui::Response {
        let photo = self.client_photo(ui.ctx(), client);
        let speaking = self.is_speaking(client.id);
        let indent = depth as f32 * 16.0;
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
        let painter = ui.painter();
        let name_color = if speaking { theme::TEXT } else { theme::TEXT_2 };
        draw_avatar(
            ui,
            rect.left_center() + vec2(30.0 + indent, 0.0),
            22.0,
            &client.name,
            speaking,
            photo.as_ref(),
        );
        let name = theme::fit_text(
            painter,
            &client.name,
            FontId::new(13.0, theme::medium()),
            name_color,
            rect.width() - (50.0 + indent) - 32.0,
        );
        painter.galley(
            pos2(
                rect.left() + 50.0 + indent,
                rect.center().y - name.size().y / 2.0,
            ),
            name,
            name_color,
        );
        self.voice_badge(
            painter,
            rect.right_center() - vec2(12.0, 0.0),
            14.0,
            client,
            speaking,
        );
        response
    }

    fn user_card(&mut self, ui: &mut egui::Ui) {
        let t = self.t;
        // In a call the server's nickname; otherwise the active local profile.
        let profile_name = self
            .store
            .active_profile_id()
            .and_then(|id| self.store.profile(&id).map(|p| p.nickname.clone()));
        let name = if self.in_session() && !self.nickname.is_empty() {
            self.nickname.clone()
        } else {
            profile_name
                .clone()
                .unwrap_or_else(|| t.no_profile.to_string())
        };
        let (status, color) = if self.in_session() {
            (t.available, theme::SUCCESS)
        } else if profile_name.is_some() {
            (t.local_profile, theme::TEXT_3)
        } else {
            (t.disconnected, theme::TEXT_3)
        };
        // Added first, so in the bottom-up layout the row sits at the bottom
        // and the hairline above it.
        let photo = self
            .store
            .active_profile_id()
            .and_then(|id| self.profile_photo(ui.ctx(), &id));
        ui.horizontal(|ui| {
            theme::avatar_or_photo(ui, &name, 36.0, photo.as_ref());
            ui.vertical(|ui| {
                // Leave room for the settings button on the right, so a long
                // name is cut with an ellipsis instead of pushing it out.
                ui.set_max_width((ui.available_width() - 32.0 - 12.0).max(40.0));
                ui.add(
                    egui::Label::new(RichText::new(&name).font(FontId::new(14.0, theme::medium())))
                        .truncate(),
                );
                // The default row height is a button's; this line is plain text.
                ui.scope(|ui| {
                    ui.spacing_mut().interact_size.y = 16.0;
                    ui.horizontal(|ui| {
                        let (dot, _) = ui.allocate_exact_size(vec2(8.0, 14.0), Sense::hover());
                        ui.painter().circle_filled(dot.center(), 3.5, color);
                        ui.label(RichText::new(status).small().color(theme::TEXT_2));
                    });
                });
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let gear = theme::glyph_button(ui, icon::GEAR, t.settings_title, 32.0, 45.0);
                if self.update_waiting() {
                    // A quiet dot: a new version is waiting in the settings.
                    ui.painter().circle_filled(
                        gear.rect.right_top() + vec2(-7.0, 7.0),
                        4.0,
                        theme::SUCCESS,
                    );
                }
                if gear.clicked() {
                    let page = if self.update_waiting() {
                        settings::Page::Updates
                    } else {
                        settings::Page::Profile
                    };
                    self.open_settings(page);
                }
            });
        });
        ui.add_space(10.0);
        ui.separator();
    }
}

// Session view -------------------------------------------------------------

impl App {
    fn session_view(&mut self, ui: &mut egui::Ui) {
        let t = self.t;
        let Some(snapshot) = self.snapshot.clone() else {
            return;
        };
        let channel_id = self.own_channel(&snapshot);
        let channel = channel_id.and_then(|id| snapshot.channels.iter().find(|c| c.id == id));

        ui.label(
            RichText::new(t.breadcrumb(&snapshot.server.name))
                .small()
                .color(theme::TEXT_3),
        );
        ui.add_space(8.0);
        self.channel_header(ui, channel);
        ui.add_space(16.0);

        let participants: Vec<&ClientInfo> = snapshot
            .clients
            .iter()
            .filter(|c| Some(c.channel_id) == channel_id)
            .collect();

        // The cards sit in one centered column.
        let width = ui.available_width().min(CONTENT_MAX_WIDTH);
        let left = ui.max_rect().left() + ((ui.available_width() - width) / 2.0).max(0.0);
        let area = Rect::from_min_size(
            pos2(left, ui.cursor().top()),
            vec2(width, ui.available_height()),
        );
        let below_fixed = CONTROLS_GAP_ABOVE
            + CONTROL_SIZE
            + CONTROLS_GAP_BELOW
            + SPLIT_HANDLE_HEIGHT
            + SPLIT_GAP_BELOW
            + 4.0 * ui.spacing().item_spacing.y;
        let mut split_room = 0.0;
        let chat_min = self.chat_min_height();
        ui.scope_builder(UiBuilder::new().max_rect(area), |ui| {
            // Everything from here down to the window edge: participants,
            // controls, divider and chat. The divider splits it.
            split_room = ui.available_height();
            let slot = self.chat_height.map(|height| {
                let max = max_chat_height(split_room, below_fixed);
                let height = height.clamp(chat_min.min(max), max);
                (split_room - below_fixed - height).max(CARD_MIN_HEIGHT)
            });
            let reserve = BELOW_GRID_MIN_HEIGHT.max(below_fixed + chat_min);
            self.participants_area(ui, &participants, slot, reserve);
        });

        ui.add_space(CONTROLS_GAP_ABOVE);
        self.call_controls(ui);
        ui.add_space(CONTROLS_GAP_BELOW);
        self.split_handle(ui, split_room, below_fixed);
        ui.add_space(SPLIT_GAP_BELOW);

        let channel_name = channel.map_or(t.no_channel, |c| c.name.as_str());
        self.chat_section(ui, channel_id, channel_name);
    }

    /// Icon, title and description on the left; invite and options on the right.
    fn channel_header(&mut self, ui: &mut egui::Ui, channel: Option<&ChannelInfo>) {
        let t = self.t;
        let name = channel.map_or(t.no_channel, |c| c.name.as_str());
        let topic = channel
            .and_then(|c| c.topic.clone())
            // The server fills an empty topic with an English "<name> has no topic".
            .filter(|topic| {
                let topic = topic.trim();
                !topic.is_empty() && !topic.ends_with("has no topic")
            });

        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_max_width((ui.available_width() - HEADER_ACTIONS_WIDTH - 16.0).max(120.0));
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(icon::SPEAKER)
                            .font(FontId::new(24.0, theme::icons()))
                            .color(theme::TEXT_2),
                    );
                    ui.add(
                        egui::Label::new(
                            RichText::new(name).font(FontId::new(24.0, theme::semibold())),
                        )
                        .truncate(),
                    );
                });
                ui.add(
                    egui::Label::new(
                        RichText::new(t.channel_subtitle(topic.as_deref())).color(theme::TEXT_2),
                    )
                    .truncate(),
                );
            });

            ui.add_space((ui.available_width() - HEADER_ACTIONS_WIDTH).max(0.0));
            let copied = self
                .invite_copied
                .is_some_and(|at| at.elapsed() < INVITE_FEEDBACK);
            if copied {
                // Back to "Convidar" once the confirmation has been read.
                ui.ctx().request_repaint_after(INVITE_FEEDBACK);
            }
            let (label, glyph) = if copied {
                (t.invite_copied, icon::CHECK)
            } else {
                (t.invite, icon::USER_PLUS)
            };
            let invite = theme::button(
                ui,
                label,
                Some(glyph),
                Tone::Secondary,
                HEADER_ACTIONS_WIDTH,
            )
            .on_hover_text(t.invite_hint);
            if invite.clicked() {
                let address = self.status.address.clone().unwrap_or_default();
                ui.ctx().copy_text(invite_link(&address, name));
                self.invite_copied = Some(Instant::now());
            }
        });
    }

    /// Grid of 16:9 cards (each row centered), or the list. The grid takes the
    /// height left after the controls and a usable chat are reserved, so the
    /// cards shrink on short windows instead of pushing the chat away.
    ///
    /// With a `slot` (the user has set the chat height) the area fills exactly
    /// that height and centers its content, so the divider moves in both
    /// directions. Without one, the grid keeps its natural size.
    fn participants_area(
        &mut self,
        ui: &mut egui::Ui,
        participants: &[&ClientInfo],
        slot: Option<f32>,
        reserve: f32,
    ) {
        let room = slot
            .unwrap_or_else(|| ui.available_height() - reserve)
            .max(CARD_MIN_HEIGHT);
        if self.watching.is_some() {
            let room = slot.unwrap_or(room);
            self.watching_area(ui, participants, room);
            return;
        }
        let Some(slot) = slot else {
            self.participants_content(ui, participants, room, None);
            return;
        };
        let width = ui.available_width();
        ui.allocate_ui_with_layout(vec2(width, slot), Layout::top_down(Align::Min), |ui| {
            ui.set_min_height(slot);
            self.participants_content(ui, participants, room, Some(slot));
        });
    }

    fn participants_content(
        &mut self,
        ui: &mut egui::Ui,
        participants: &[&ClientInfo],
        room: f32,
        slot: Option<f32>,
    ) {
        // Space that centers `visible` inside the slot, when there is one.
        let center = |ui: &mut egui::Ui, visible: f32| {
            if let Some(slot) = slot {
                ui.add_space(((slot - visible) / 2.0 - ui.spacing().item_spacing.y).max(0.0));
            }
        };
        let available = ui.available_width();
        let count = participants.len().max(1);
        let fit = |columns: usize| {
            let rows = count.div_ceil(columns);
            let column_width = (available - CARD_GAP * (columns - 1) as f32) / columns as f32;
            let row_room = (room - CARD_GAP * (rows - 1) as f32) / rows as f32;
            // 16:9 from the column width, limited by the height left; when height
            // is the limit the width follows it, so cards never distort.
            let height = (column_width * 9.0 / 16.0).min(row_room);
            (rows, column_width, height)
        };
        // The column count that gives the biggest cards for this room, so the
        // grid wraps into more rows when the chat leaves height to spare.
        let most = (((available + CARD_GAP) / (CARD_MIN_WIDTH + CARD_GAP)) as usize)
            .clamp(1, count.min(MAX_COLUMNS));
        let columns = (1..=most)
            .max_by(|a, b| fit(*a).2.total_cmp(&fit(*b).2).then(a.cmp(b)))
            .unwrap_or(1);
        let (rows, column_width, card_height) = fit(columns);
        // Never smaller than a readable card; past that the grid scrolls.
        let card_height = card_height.max(CARD_MIN_HEIGHT);
        let card_width = (card_height * 16.0 / 9.0).min(column_width);
        let content = rows as f32 * card_height + (rows - 1) as f32 * CARD_GAP;
        let visible = content.min(room.max(card_height));
        center(ui, visible);
        egui::ScrollArea::vertical()
            .id_salt("participants")
            .max_height(visible)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = vec2(CARD_GAP, CARD_GAP);
                let inner = ui.available_width();
                for row in participants.chunks(columns) {
                    let row_width =
                        row.len() as f32 * card_width + (row.len() - 1) as f32 * CARD_GAP;
                    ui.horizontal(|ui| {
                        ui.add_space(((inner - row_width) / 2.0).max(0.0));
                        for client in row {
                            self.participant_card(ui, client, card_width, card_height);
                        }
                    });
                }
            });
    }

    /// Avatar centered, name (and "Você" for the local user) bottom-left,
    /// microphone state bottom-right. Painted at fixed offsets so every card
    /// has identical geometry. Muted is an icon, not only a color.
    fn participant_card(
        &mut self,
        ui: &mut egui::Ui,
        client: &ClientInfo,
        width: f32,
        height: f32,
    ) {
        let t = self.t;
        let photo = self.client_photo(ui.ctx(), client);
        let speaking = self.is_speaking(client.id);
        let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
        self.peer_menu(&response, client);
        let painter = ui.painter();
        let (border, border_width) = if speaking {
            (theme::SUCCESS, 1.5)
        } else {
            (theme::BORDER_SOFT, 1.0)
        };
        painter.rect_filled(rect, CornerRadius::same(8), theme::SURFACE);
        let avatar = (height * 0.40).clamp(36.0, 80.0);
        painter.rect_stroke(
            rect,
            CornerRadius::same(8),
            Stroke::new(border_width, border),
            egui::StrokeKind::Inside,
        );
        draw_avatar(
            ui,
            pos2(rect.center().x, rect.center().y - 10.0),
            avatar,
            &client.name,
            false,
            photo.as_ref(),
        );

        let baseline = rect.bottom() - 14.0;
        let you = client.is_self.then(|| {
            painter.layout_no_wrap(t.you.to_string(), FontId::proportional(12.0), theme::TEXT_3)
        });
        // Name, "Você" and the microphone icon share the bottom row. The name
        // gives way (with an ellipsis) so they can never overlap.
        let reserved = 16.0 + 16.0 + 28.0 + you.as_ref().map_or(0.0, |g| g.size().x + 8.0);
        let name = theme::fit_text(
            painter,
            &client.name,
            FontId::new(14.0, theme::medium()),
            theme::TEXT,
            (width - reserved).max(24.0),
        );
        let name_width = name.size().x;
        painter.galley(
            pos2(rect.left() + 16.0, baseline - name.size().y),
            name,
            theme::TEXT,
        );
        if let Some(you) = you {
            painter.galley(
                pos2(
                    rect.left() + 16.0 + name_width + 8.0,
                    baseline - you.size().y,
                ),
                you,
                theme::TEXT_3,
            );
        }

        let corner = pos2(rect.right() - 16.0, baseline);
        self.voice_badge(painter, corner - vec2(0.0, 9.0), 18.0, client, speaking);

        // Someone sharing their screen: badge, and a click watches it.
        if client.is_streaming {
            stream::live_badge(painter, rect.left_top() + vec2(10.0, 10.0), t.live_badge);
            let watched = self
                .watching
                .as_ref()
                .is_some_and(|w| w.client == client.id);
            if !client.is_self && !watched {
                let response = response.on_hover_text(t.watch_stream);
                if response.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    let pill = Rect::from_center_size(
                        pos2(rect.center().x, rect.bottom() - 40.0),
                        vec2(96.0, 28.0),
                    );
                    painter.rect_filled(
                        pill,
                        CornerRadius::same(14),
                        Color32::from_black_alpha(190),
                    );
                    painter.text(
                        pill.center(),
                        Align2::CENTER_CENTER,
                        t.watch_stream,
                        FontId::new(13.0, theme::semibold()),
                        Color32::WHITE,
                    );
                }
                if response.clicked() {
                    self.watch_stream(client.id);
                }
            }
        }
    }

    /// The divider between participants and chat. Drag the grip up to give the
    /// chat more room, down to give the participants more. Arrow keys do the
    /// same when it has focus, and a double click restores the automatic layout.
    fn split_handle(&mut self, ui: &mut egui::Ui, room: f32, below_fixed: f32) {
        let t = self.t;
        let (rect, response) = ui.allocate_exact_size(
            vec2(ui.available_width(), SPLIT_HANDLE_HEIGHT),
            Sense::click_and_drag(),
        );
        response
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Splitter, true, t.resize_chat));

        // Height of the chat as drawn now, so the first drag starts from it
        // instead of jumping.
        let drawn = (ui.max_rect().bottom() - rect.bottom() - SPLIT_GAP_BELOW).max(CHAT_MIN_HEIGHT);
        let current = self.chat_height.unwrap_or(drawn);
        let mut target = None;
        if response.dragged() {
            target = Some(current - response.drag_delta().y);
        }
        if response.has_focus() {
            let step = ui.input(|input| {
                f32::from(input.key_pressed(egui::Key::ArrowUp))
                    - f32::from(input.key_pressed(egui::Key::ArrowDown))
            });
            if step != 0.0 {
                target = Some(current + step * SPLIT_KEY_STEP);
            }
        }
        if let Some(height) = target {
            let max = max_chat_height(room, below_fixed);
            self.chat_height = Some(height.clamp(self.chat_min_height().min(max), max));
            ui.ctx().request_repaint();
        }
        if response.double_clicked() {
            self.chat_height = None;
            ui.ctx().request_repaint();
        }

        let active = response.dragged();
        let hot = active || response.hovered() || response.has_focus();
        if hot {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        }
        let painter = ui.painter();
        let y = rect.center().y;
        let (line, line_width) = if active {
            (theme::ACCENT_HOVER, 2.0)
        } else if hot {
            (theme::TEXT_3, 1.0)
        } else {
            (theme::BORDER_SOFT, 1.0)
        };
        painter.line_segment(
            [pos2(rect.left(), y), pos2(rect.right(), y)],
            Stroke::new(line_width, line),
        );

        // The grip: a small pill with two rows of three dots.
        let pill = Rect::from_center_size(rect.center(), vec2(44.0, 18.0));
        let (fill, border) = if active {
            (theme::SURFACE_HOVER, theme::ACCENT_HOVER)
        } else if hot {
            (theme::SURFACE_HOVER, theme::TEXT_3)
        } else {
            (theme::SURFACE_RAISED, theme::BORDER)
        };
        painter.rect_filled(pill, CornerRadius::same(9), fill);
        let border_width = if response.has_focus() { 1.5 } else { 1.0 };
        painter.rect_stroke(
            pill,
            CornerRadius::same(9),
            Stroke::new(border_width, border),
            egui::StrokeKind::Inside,
        );
        let dot = if hot { theme::TEXT } else { theme::TEXT_3 };
        for row in [-3.0, 3.0] {
            for column in [-6.0, 0.0, 6.0] {
                painter.circle_filled(pill.center() + vec2(column, row), 1.4, dot);
            }
        }

        if !active {
            response.on_hover_text(t.resize_chat_hint);
        }
    }

    /// Microphone, headphones, screen share, options and hang up, centered
    /// under the participants.
    fn call_controls(&mut self, ui: &mut egui::Ui) {
        let t = self.t;
        let total = 5.0 * CONTROL_SIZE + 4.0 * CONTROL_GAP;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = CONTROL_GAP;
            ui.add_space(((ui.available_width() - total) / 2.0).max(0.0));

            let (mic_glyph, mic_label) = if self.mic_muted {
                (icon::MICROPHONE_SLASH, t.mic_unmute)
            } else {
                (icon::MICROPHONE, t.mic_mute)
            };
            if theme::icon_button(ui, mic_glyph, mic_label, self.mic_muted, CONTROL_SIZE).clicked()
            {
                self.mic_muted = !self.mic_muted;
                self.send_mute();
            }

            let output_label = if self.output_muted {
                t.output_unmute
            } else {
                t.output_mute
            };
            if theme::icon_button(
                ui,
                icon::HEADPHONES,
                output_label,
                self.output_muted,
                CONTROL_SIZE,
            )
            .clicked()
            {
                self.output_muted = !self.output_muted;
                self.send_mute();
            }

            self.share_button(ui, CONTROL_SIZE);

            if theme::icon_button(ui, icon::DOTS_THREE, t.more_options, false, CONTROL_SIZE)
                .clicked()
            {
                self.show_settings = true;
            }

            if theme::icon_button_danger(ui, icon::PHONE_DISCONNECT, t.leave_call, CONTROL_SIZE)
                .clicked()
            {
                self.send(Command::Disconnect);
            }
        });
    }

    /// Chat under the participants: heading, scrolling history, composer.
    /// What we send shows as pending until the server echoes it back.
    /// Height the chat needs to show its heading, a line of history and the
    /// composer as it is now (taller with a reply or staged files).
    fn chat_min_height(&self) -> f32 {
        let notice = if self.chat_notice.is_some() {
            22.0
        } else {
            0.0
        };
        CHAT_MIN_HEIGHT.max(40.0 + HISTORY_MIN_HEIGHT + 8.0 + notice + self.composer_height)
    }

    fn chat_section(&mut self, ui: &mut egui::Ui, channel_id: Option<u64>, channel_name: &str) {
        let t = self.t;
        ui.label(RichText::new(t.chat_heading).font(FontId::new(16.0, theme::semibold())));
        ui.add_space(8.0);

        // Top-down: the history gets the room the composer (measured last
        // frame) leaves, so the composer stays at the bottom and grows upward
        // into the history when files are staged, never the other way around.
        let notice = if self.chat_notice.is_some() {
            22.0
        } else {
            0.0
        };
        let history = ui.available_height() - self.composer_height - notice - 8.0;
        if history >= HISTORY_MIN_HEIGHT {
            ui.allocate_ui_with_layout(
                vec2(ui.available_width(), history),
                Layout::top_down(Align::Min),
                |ui| {
                    ui.set_min_height(history);
                    self.chat_history(ui, channel_id);
                },
            );
        } else {
            ui.add_space(history.max(0.0));
        }
        ui.add_space(8.0);
        if let Some(notice) = self.chat_notice {
            ui.label(RichText::new(notice).small().color(theme::DANGER));
        }
        let composer = ui
            .scope(|ui| self.chat_composer(ui, channel_name))
            .response
            .rect;
        if (composer.height() - self.composer_height).abs() > 0.5 {
            self.composer_height = composer.height();
            ui.ctx().request_repaint();
        }
    }

    fn chat_composer(&mut self, ui: &mut egui::Ui, channel_name: &str) {
        let t = self.t;
        let placeholder = t.chat_placeholder_in(channel_name);
        let mut submit = false;
        theme::input_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            if let Some((author, _)) = self.chat_reply.clone() {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(t.chat_replying_to(&author))
                            .small()
                            .color(theme::TEXT_2),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(RichText::new("✕").small().color(theme::TEXT_3))
                                    .frame(false),
                            )
                            .on_hover_text(t.chat_cancel_reply)
                            .clicked()
                        {
                            self.chat_reply = None;
                        }
                    });
                });
            }
            if !self.staged.is_empty() {
                self.attachment_tray(ui);
                ui.add_space(8.0);
            }
            // A fixed-height row with centered items, so the placeholder text
            // and the send button share one baseline.
            ui.allocate_ui_with_layout(
                vec2(ui.available_width(), 32.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    let can_attach = self.channel_chat && self.staged.len() < MAX_STAGED;
                    let attach = ui
                        .add_enabled_ui(can_attach, |ui| {
                            ui.add_sized(
                                vec2(CHAT_SEND_SIZE, CHAT_SEND_SIZE),
                                egui::Button::new(
                                    RichText::new(icon::PAPERCLIP)
                                        .font(FontId::new(19.0, theme::icons()))
                                        .color(theme::TEXT_2),
                                )
                                .frame(false),
                            )
                            .on_hover_text(t.chat_attach)
                        })
                        .inner
                        .clicked();
                    if attach {
                        if let Some(files) = rfd::FileDialog::new().pick_files() {
                            let room = MAX_STAGED - self.staged.len();
                            self.staged.extend(files.into_iter().take(room));
                        }
                    }
                    let button = CHAT_SEND_SIZE + ui.spacing().item_spacing.x;
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut self.chat_draft)
                            .hint_text(placeholder)
                            .frame(Frame::NONE)
                            .char_limit(MAX_MESSAGE_BYTES)
                            .desired_width(ui.available_width() - button)
                            .align(Align2::LEFT_CENTER)
                            .min_size(vec2(0.0, 32.0)),
                    );
                    if edit.changed() {
                        self.chat_notice = None;
                    }
                    let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let can_send = self.channel_chat
                        && (!self.chat_draft.trim().is_empty() || !self.staged.is_empty());
                    // Files dragged from Explorer onto the window join the tray.
                    let dropped: Vec<_> = ui.input(|i| {
                        i.raw
                            .dropped_files
                            .iter()
                            .map(|f| f.path().to_path_buf())
                            .filter(|path| path.is_file())
                            .collect()
                    });
                    if can_attach && !dropped.is_empty() {
                        let room = MAX_STAGED - self.staged.len();
                        self.staged.extend(dropped.into_iter().take(room));
                    }
                    // Ctrl+V with a picture on the clipboard attaches it.
                    let pasted = ui.input(|i| {
                        i.events.iter().find_map(|e| match e {
                            egui::Event::PasteImage(image) => Some(image.clone()),
                            _ => None,
                        })
                    });
                    if let (Some(image), true) = (pasted, can_attach) {
                        if let Some(file) = pasted_picture(&image) {
                            self.staged.push(file);
                        }
                    }
                    let clicked = ui
                        .add_enabled_ui(can_send, |ui| {
                            theme::icon_button(ui, icon::SEND, t.chat_send, false, CHAT_SEND_SIZE)
                        })
                        .inner
                        .clicked();
                    if (enter || clicked) && can_send {
                        submit = true;
                    }
                    if enter {
                        // Enter leaves the field; keep typing without a click.
                        edit.request_focus();
                    }
                },
            );
        });
        if submit {
            // The caption goes first, then the files in the order added.
            if !self.chat_draft.trim().is_empty() {
                self.submit_chat();
            }
            self.upload_queue.extend(self.staged.drain(..));
            self.staged_previews.clear();
        }
    }

    /// The staged files above the composer, as in Discord: a card per file
    /// with a preview, its name, and view/remove on hover.
    fn attachment_tray(&mut self, ui: &mut egui::Ui) {
        let t = self.t;
        let mut remove = None;
        egui::ScrollArea::horizontal()
            .id_salt("attachment-tray")
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    for (index, path) in self.staged.iter().enumerate() {
                        let (card, response) =
                            ui.allocate_exact_size(vec2(168.0, 168.0), Sense::hover());
                        let painter = ui.painter();
                        painter.rect(
                            card,
                            CornerRadius::same(8),
                            theme::SURFACE_RAISED,
                            Stroke::new(1.0, theme::BORDER),
                            egui::StrokeKind::Inside,
                        );
                        let preview = Rect::from_min_max(
                            card.left_top() + vec2(10.0, 10.0),
                            card.right_bottom() - vec2(10.0, 34.0),
                        );
                        let uri =
                            format!("file://{}", path.display().to_string().replace('\\', "/"));
                        let texture = self
                            .staged_previews
                            .entry(path.clone())
                            .or_insert_with(|| staged_preview(ui.ctx(), path))
                            .clone();
                        if let Some(texture) = texture {
                            // Contain: the whole picture, centered, no distortion.
                            let size = texture.size_vec2();
                            let scale = (preview.width() / size.x).min(preview.height() / size.y);
                            let shown = Rect::from_center_size(preview.center(), size * scale);
                            ui.painter().image(
                                texture.id(),
                                shown,
                                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                                Color32::WHITE,
                            );
                        } else {
                            ui.painter().text(
                                preview.center(),
                                Align2::CENTER_CENTER,
                                icon::PAPERCLIP,
                                FontId::new(36.0, theme::icons()),
                                theme::TEXT_3,
                            );
                        }
                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        let galley = ui.painter().layout_no_wrap(
                            name,
                            FontId::new(12.5, theme::medium()),
                            theme::TEXT_2,
                        );
                        let clip = ui.painter().with_clip_rect(card.shrink(10.0));
                        clip.galley(
                            pos2(card.left() + 10.0, card.bottom() - 24.0),
                            galley,
                            theme::TEXT_2,
                        );
                        // View and remove float over the corner on hover.
                        if ui.rect_contains_pointer(card) || response.hovered() {
                            let bar = Rect::from_min_size(
                                pos2(card.right() - 74.0, card.top() - 12.0),
                                vec2(68.0, 32.0),
                            );
                            ui.painter().rect(
                                bar,
                                CornerRadius::same(6),
                                theme::SIDEBAR,
                                Stroke::new(1.0, theme::BORDER),
                                egui::StrokeKind::Inside,
                            );
                            let mut tools = ui.new_child(
                                egui::UiBuilder::new()
                                    .max_rect(bar.shrink2(vec2(4.0, 1.0)))
                                    .layout(Layout::left_to_right(Align::Center)),
                            );
                            tools.spacing_mut().item_spacing.x = 0.0;
                            if theme::glyph_button(&mut tools, icon::EYE, t.image_open, 30.0, 0.0)
                                .clicked()
                                && is_picture(path)
                            {
                                self.image_viewer = Some(ImageViewer {
                                    file: path.display().to_string(),
                                    uri: uri.clone(),
                                    zoom: 1.0,
                                    offset: egui::Vec2::ZERO,
                                });
                            }
                            if theme::glyph_button(
                                &mut tools,
                                icon::TRASH,
                                t.attachment_remove,
                                30.0,
                                0.0,
                            )
                            .clicked()
                            {
                                remove = Some(index);
                            }
                        }
                    }
                });
            });
        if let Some(index) = remove {
            let path = self.staged.remove(index);
            self.staged_previews.remove(&path);
        }
    }

    /// Sends a local file to the current channel. The bridge uploads it to the
    /// channel's file transfer and posts a message that references it; the
    /// picture then appears when that message arrives.
    fn attach_file(&mut self, file: std::path::PathBuf) {
        let (Some(snapshot), Some(session_id)) = (&self.snapshot, self.status.session_id.clone())
        else {
            self.chat_notice = Some(self.t.chat_offline);
            return;
        };
        let Some(channel) = self.own_channel(snapshot) else {
            return;
        };
        self.next_chat_request += 1;
        let request_id = format!("native-file-{}", self.next_chat_request);
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.pending_attachments.insert(request_id.clone(), name);
        self.chat_notice = Some(self.t.chat_attach_sending);
        self.send(Command::ChatAttach {
            session_id,
            channel: channel.to_string(),
            path: file.display().to_string(),
            request_id,
        });
    }

    /// Requests a picture once and reports what to show for it now.
    fn chat_file(&mut self, channel: u64, path: String) -> ChatFile {
        let key = (channel.to_string(), path);
        if let Some(state) = self.chat_files.get(&key) {
            return state.clone();
        }
        self.chat_files.insert(key.clone(), ChatFile::Loading);
        self.send(Command::ChatFetch {
            channel: key.0,
            path: key.1,
        });
        ChatFile::Loading
    }

    /// Writes the delivered conversations to disk when they changed.
    fn keep_chat(&mut self) {
        let Some(address) = self.session_address.clone() else {
            return;
        };
        if self.store.save_transcripts(&address, self.chat.saved()) {
            self.store.save();
        }
    }

    /// Hands the draft to the bridge, correlated with this session and channel,
    /// and shows it as pending until its `chat_result` (or an echo) arrives.
    fn submit_chat(&mut self) {
        let mut text = self.chat_draft.trim().to_string();
        if text.is_empty() {
            return;
        }
        // shortcut: TS6 replies are Matrix relations; this sends a plain
        // "> author: line" quote, which every client shows as text. Upgrade
        // with the Matrix chat client.
        if let Some((author, line)) = &self.chat_reply {
            text = format!("> {author}: {line}\n{text}");
        }
        if text.len() > MAX_MESSAGE_BYTES {
            self.chat_notice = Some(self.t.chat_too_long);
            return;
        }
        let (Some(snapshot), Some(session_id)) = (&self.snapshot, self.status.session_id.clone())
        else {
            self.chat_notice = Some(self.t.chat_offline);
            return;
        };
        if !self.channel_chat {
            self.chat_notice = Some(self.t.chat_offline);
            return;
        }
        let Some(channel) = self.own_channel(snapshot) else {
            return;
        };
        let me = snapshot.clients.iter().find(|c| c.is_self);
        let author = me.map_or_else(|| self.nickname.clone(), |c| c.name.clone());
        let from_id = Some(snapshot.own_client_id);
        self.next_chat_request += 1;
        let request_id = format!("native-{}", self.next_chat_request);
        self.chat.sent(
            request_id.clone(),
            Some(channel),
            from_id,
            author,
            text.clone(),
            Instant::now(),
            clock::local_hour_minute(),
        );
        self.send(Command::Chat {
            session_id,
            channel: channel.to_string(),
            text,
            request_id,
        });
        self.chat_draft.clear();
        self.chat_reply = None;
        self.chat_notice = None;
    }

    fn chat_history(&mut self, ui: &mut egui::Ui, channel_id: Option<u64>) {
        let t = self.t;
        let entries = self.chat.visible(channel_id, CHAT_RENDER_LIMIT);
        let snapshot = self.snapshot.clone();
        egui::ScrollArea::vertical()
            .id_salt("chat_history")
            .stick_to_bottom(true)
            .min_scrolled_height(0.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if entries.is_empty() {
                    ui.vertical_centered(|ui| {
                        // With little room, show the title alone.
                        let compact = ui.available_height() < 56.0;
                        ui.add_space(if compact {
                            0.0
                        } else {
                            (ui.available_height() * 0.3).max(8.0)
                        });
                        ui.label(
                            RichText::new(t.chat_empty_title)
                                .font(FontId::new(14.0, theme::medium()))
                                .color(theme::TEXT_2),
                        );
                        if !compact {
                            ui.label(
                                RichText::new(t.chat_empty_body)
                                    .small()
                                    .color(theme::TEXT_3),
                            );
                        }
                    });
                    return;
                }
                // Top-down here: the scroll area inherits the bottom-up layout
                // of the chat section, which stacked messages out of order.
                ui.with_layout(Layout::top_down(Align::Min), |ui| {
                    ui.spacing_mut().item_spacing.y = 3.0;
                    for (index, entry) in entries.iter().enumerate() {
                        let previous = index.checked_sub(1).map(|i| &entries[i]);
                        let boundary = previous.is_some_and(|p| p.restored && !entry.restored);
                        let grouped = !boundary && previous.is_some_and(|p| entry.groups_with(p));
                        if boundary {
                            // Where the kept conversation ends and this session begins.
                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                let line = Stroke::new(1.0, theme::BORDER);
                                let label = ui.label(
                                    RichText::new(t.chat_new_messages)
                                        .small()
                                        .color(theme::TEXT_3),
                                );
                                let y = label.rect.center().y;
                                let right = ui.max_rect().right();
                                ui.painter()
                                    .hline(label.rect.right() + 8.0..=right, y, line);
                            });
                            ui.add_space(8.0);
                        } else if !grouped && index > 0 {
                            ui.add_space(12.0);
                        }
                        self.chat_row(ui, index, entry, grouped, snapshot.as_ref());
                    }
                });
            });
    }

    /// Pictures inline (animated GIFs play), files as a save button, web
    /// pictures and links as links. Web pictures are never loaded by Mirage:
    /// fetching them would reveal the user's address to whoever posted them.
    fn chat_attachments(
        &mut self,
        ui: &mut egui::Ui,
        segments: &[ts6_bridge::attachments::Segment],
    ) {
        use ts6_bridge::attachments::Segment;
        let t = self.t;
        for segment in segments {
            match segment {
                Segment::Text(_) => {}
                Segment::Picture(file) => match self.chat_file(file.channel_id, file.path()) {
                    ChatFile::Ready {
                        uri, picture: true, ..
                    } => {
                        let picture = ui
                            .add(
                                egui::Image::new(uri.clone())
                                    .max_height(CHAT_PICTURE_MAX.y)
                                    .max_width(CHAT_PICTURE_MAX.x.min(ui.available_width()))
                                    .corner_radius(6)
                                    .sense(Sense::click()),
                            )
                            .on_hover_cursor(egui::CursorIcon::ZoomIn);
                        let cached = match self.chat_file(file.channel_id, file.path()) {
                            ChatFile::Ready { file, .. } => file,
                            _ => String::new(),
                        };
                        if picture.clicked() {
                            self.image_viewer = Some(ImageViewer {
                                file: cached.clone(),
                                uri: uri.clone(),
                                zoom: 1.0,
                                offset: egui::Vec2::ZERO,
                            });
                        }
                        egui::Popup::context_menu(&picture).show(|ui| {
                            ui.set_width(190.0);
                            if peer::menu_item(ui, icon::MAGNIFYING_GLASS, t.image_open).clicked() {
                                self.image_viewer = Some(ImageViewer {
                                    file: cached.clone(),
                                    uri: uri.clone(),
                                    zoom: 1.0,
                                    offset: egui::Vec2::ZERO,
                                });
                                ui.close();
                            }
                            if peer::menu_item(ui, icon::COPY, t.image_copy).clicked() {
                                copy_picture(ui.ctx(), &cached);
                                ui.close();
                            }
                            if peer::menu_item(ui, icon::DOWNLOAD, t.image_save).clicked() {
                                save_picture(&cached);
                                ui.close();
                            }
                        });
                    }
                    ChatFile::Loading => {
                        ui.label(
                            RichText::new(t.chat_attachment_loading)
                                .small()
                                .color(theme::TEXT_3),
                        );
                    }
                    _ => {
                        ui.label(
                            RichText::new(t.chat_attachment_unavailable)
                                .small()
                                .color(theme::TEXT_3),
                        );
                    }
                },
                Segment::File(file) => {
                    let size = file
                        .size
                        .map(|bytes| format!(" ({} MB)", decimal(bytes as f64 / 1_000_000.0, 1)))
                        .unwrap_or_default();
                    let label = format!("📎 {}{size}", file.name);
                    if ui
                        .button(label)
                        .on_hover_text(t.chat_attachment_save)
                        .clicked()
                    {
                        let target = rfd::FileDialog::new()
                            .set_file_name(file.name.as_str())
                            .save_file();
                        if let Some(target) = target {
                            match self.chat_file(file.channel_id, file.path()) {
                                ChatFile::Ready { file: cached, .. } => {
                                    if std::fs::copy(&cached, &target).is_err() {
                                        self.chat_notice = Some(t.chat_attachment_unavailable);
                                    }
                                }
                                _ => self.chat_notice = Some(t.chat_attachment_loading),
                            }
                        }
                    }
                }
                Segment::WebPicture(url) => {
                    ui.hyperlink_to(t.chat_web_picture, url);
                }
                Segment::Link { url, label } => {
                    let text = if label.trim().is_empty() { url } else { label };
                    ui.hyperlink_to(text, url);
                }
            }
        }
    }

    /// One message in the style of a chat client: a round avatar, the sender's
    /// nickname with the time, then the text. Messages of one sender within the
    /// same minute share a single header. Delivery state shows only when it is
    /// not "delivered".
    fn chat_row(
        &mut self,
        ui: &mut egui::Ui,
        index: usize,
        entry: &Entry,
        grouped: bool,
        snapshot: Option<&Snapshot>,
    ) {
        let t = self.t;
        let segments = ts6_bridge::attachments::parse(&entry.text);
        let display =
            official_attachment(&entry.text, t).unwrap_or_else(|| display_text(&segments));
        let (quote, body) = split_quote(&display);
        // Reserved under the row, filled once we know it is hovered.
        let highlight = ui.painter().add(egui::Shape::Noop);
        let menu_id = ui.scope_id().with(("chat-row", index));
        let row_area = ui
            .ctx()
            .data(|d| d.get_temp::<Rect>(menu_id))
            .unwrap_or(Rect::NOTHING);
        let row_hit = ui.interact(row_area, menu_id, Sense::click());
        let row = ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = CHAT_GAP;
            if grouped {
                // Allocated (not add_space) so the gap after it applies too.
                ui.allocate_exact_size(vec2(CHAT_AVATAR, 0.0), Sense::hover());
            } else {
                // Received messages carry only the sender's name, so fall
                // back to it when there is no client id.
                let client = snapshot
                    .and_then(|s| {
                        s.clients.iter().find(|c| match entry.from_id {
                            Some(id) => c.id == id,
                            None => (entry.mine && c.is_self) || c.name == entry.author,
                        })
                    })
                    .cloned();
                let photo = client.and_then(|c| self.client_photo(ui.ctx(), &c));
                theme::avatar_or_photo(ui, &entry.author, CHAT_AVATAR, photo.as_ref());
            }
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                if !grouped {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        ui.label(
                            RichText::new(&entry.author).font(FontId::new(15.0, theme::semibold())),
                        );
                        let (hour, minute) = entry.time;
                        ui.label(
                            RichText::new(format!("{hour:02}:{minute:02}"))
                                .font(FontId::new(12.0, theme::medium()))
                                .color(theme::TEXT_3),
                        );
                        let tag = match entry.scope {
                            Scope::Channel => None,
                            Scope::Server => Some(t.chat_scope_server),
                            Scope::Private => Some(t.chat_scope_private),
                        };
                        if let Some(tag) = tag {
                            ui.label(RichText::new(tag).small().color(theme::TEXT_3));
                        }
                    });
                }
                let ink = match entry.delivery {
                    Delivery::Delivered => CHAT_TEXT,
                    _ => theme::TEXT_3,
                };
                if let Some(quote) = quote {
                    let line = ui.add(
                        egui::Label::new(RichText::new(quote).small().color(theme::TEXT_3))
                            .truncate(),
                    );
                    ui.painter().vline(
                        line.rect.left() - 7.0,
                        line.rect.y_range(),
                        Stroke::new(2.0, theme::BORDER),
                    );
                }
                ui.add(
                    egui::Label::new(
                        RichText::new(body)
                            .font(FontId::proportional(14.5))
                            .color(ink),
                    )
                    .wrap()
                    // Not selectable, so a right-click reaches the row menu.
                    .selectable(false),
                );
                self.chat_attachments(ui, &segments);
                match entry.delivery {
                    Delivery::Delivered => {}
                    Delivery::Pending(_) => {
                        ui.label(RichText::new(t.chat_sending).small().color(theme::TEXT_3));
                    }
                    Delivery::Failed(failure) => {
                        let reason = match failure {
                            Failure::NoAnswer => t.chat_no_answer,
                            Failure::Rejected => t.chat_rejected,
                            Failure::Offline => t.chat_offline,
                        };
                        ui.label(RichText::new(reason).small().color(theme::DANGER));
                    }
                }
            });
        });
        // The whole line is one target: the chat column plus a small margin
        // on each side, and half the gap above and below.
        let column = ui.max_rect();
        let area = Rect::from_x_y_ranges(
            column.left() - 8.0..=column.right() + 8.0,
            row.response.rect.top() - 3.0..=row.response.rect.bottom() + 3.0,
        );
        // Registered under the row's widgets would steal the pictures' clicks,
        // so the hit area is taken from last frame and added first (below).
        ui.ctx().data_mut(|d| d.insert_temp(menu_id, area));
        let response = row_hit;
        let reply = |app: &mut Self| {
            let line: String = body.lines().next().unwrap_or("").chars().take(80).collect();
            app.chat_reply = Some((entry.author.clone(), line));
        };
        let active = ui.rect_contains_pointer(area) || response.context_menu_opened();
        if active {
            ui.painter().set(
                highlight,
                egui::Shape::rect_filled(area, CornerRadius::same(6), theme::SURFACE),
            );
            // Quick reply: a bare glyph at the row's right, centered on it.
            let spot = Rect::from_center_size(
                pos2(area.right() - 20.0, area.center().y),
                vec2(24.0, 24.0),
            );
            // A detached child, not `ui.put`: that would move the layout
            // cursor to the button and drag the rows below it upwards.
            let mut overlay = ui.new_child(egui::UiBuilder::new().max_rect(spot));
            let button = theme::glyph_button(&mut overlay, icon::REPLY, t.chat_reply, 24.0, 0.0);
            if button.clicked() {
                reply(self);
            }
        }
        egui::Popup::context_menu(&response).show(|ui| {
            ui.set_width(180.0);
            ui.spacing_mut().item_spacing.y = 2.0;
            if peer::menu_item(ui, icon::REPLY, t.chat_reply).clicked() {
                reply(self);
                ui.close();
            }
            if peer::menu_item(ui, icon::COPY, t.chat_copy_text).clicked() {
                ui.ctx().copy_text(body.to_string());
                ui.close();
            }
        });
    }
}

/// Saves a pasted picture as `imagem.png` in a folder of its own, so the
/// name others see is a plain one.
/// shortcut: the temporary file is left for the OS to clean, since the bridge
/// reads it later; delete it on the upload result if it matters.
fn pasted_picture(picture: &egui::ColorImage) -> Option<std::path::PathBuf> {
    let [width, height] = picture.size;
    let image = image::RgbaImage::from_raw(
        width as u32,
        height as u32,
        picture
            .pixels
            .iter()
            .flat_map(|c| c.to_srgba_unmultiplied())
            .collect(),
    )?;
    let dir = std::env::temp_dir().join(format!(
        "mirage-paste-{}-{}",
        crate::store::now_secs(),
        crate::store::new_id()
    ));
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("imagem.png");
    image.save(&path).ok()?;
    Some(path)
}

/// A small texture of a staged picture, decoded once; None for other files.
fn staged_preview(ctx: &egui::Context, path: &std::path::Path) -> Option<egui::TextureHandle> {
    if !is_picture(path) {
        return None;
    }
    let image = image::open(path).ok()?.thumbnail(320, 320).to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    Some(ctx.load_texture(
        format!("staged-{}", path.display()),
        egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw()),
        egui::TextureOptions::LINEAR,
    ))
}

/// Whether a staged file can be previewed as a picture, by extension.
fn is_picture(path: &std::path::Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"
        )
    })
}

/// Copies a cached chat picture to the clipboard as an image.
fn copy_picture(ctx: &egui::Context, file: &str) {
    if let Ok(image) = image::open(file) {
        let rgba = image.to_rgba8();
        let size = [rgba.width() as usize, rgba.height() as usize];
        ctx.copy_image(egui::ColorImage::from_rgba_unmultiplied(
            size,
            rgba.as_raw(),
        ));
    }
}

/// Asks where to save a cached chat picture and copies it there.
fn save_picture(file: &str) {
    let name = std::path::Path::new(file)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "imagem".into());
    if let Some(target) = rfd::FileDialog::new().set_file_name(name).save_file() {
        let _ = std::fs::copy(file, target);
    }
}

/// The official TS6 client sends attachments as a JSON message pointing at
/// the myTeamSpeak service, which needs an account Mirage does not have yet.
/// Shows its name and size instead of the raw JSON.
fn official_attachment(text: &str, t: &Strings) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
    if value.get("msg_type")?.as_str()? != "ts.file.myts" {
        return None;
    }
    let name = value
        .get("file_name")
        .and_then(|v| v.as_str())
        .unwrap_or("arquivo");
    let size = value
        .get("file_size")
        .and_then(|v| v.as_u64())
        .map(|bytes| format!(" ({} MB)", decimal(bytes as f64 / 1_000_000.0, 1)))
        .unwrap_or_default();
    Some(format!("📎 {name}{size}\n{}", t.chat_official_attachment))
}

/// Splits a leading "> quote" line off a message, as sent by replies.
fn split_quote(text: &str) -> (Option<&str>, &str) {
    match text
        .strip_prefix("> ")
        .and_then(|rest| rest.split_once('\n'))
    {
        Some((quote, body)) => (Some(quote), body),
        None => (None, text),
    }
}

// Picture viewer ---------------------------------------------------------------

impl App {
    /// A chat picture opened large over a dimmed app: wheel to zoom, drag to
    /// pan, double-click to reset; copy, save and close on top.
    fn image_viewer_window(&mut self, ctx: &egui::Context) {
        let t = self.t;
        let Some(view) = self.image_viewer.as_mut() else {
            return;
        };
        let mut close = false;
        let screen = ctx.content_rect();
        let modal = egui::Modal::new(egui::Id::unique("image-viewer"))
            .frame(Frame::NONE)
            .backdrop_color(Color32::from_black_alpha(210))
            .show(ctx, |ui| {
                let stage = screen.shrink2(vec2(48.0, 64.0));
                let (_, response) = ui.allocate_exact_size(screen.size(), Sense::click_and_drag());
                if response.dragged() {
                    view.offset += response.drag_delta();
                }
                if response.double_clicked() {
                    view.zoom = 1.0;
                    view.offset = egui::Vec2::ZERO;
                }
                let scroll = ui.input_mut(|i| std::mem::take(&mut i.smooth_scroll_delta).y);
                if scroll != 0.0 {
                    view.zoom = (view.zoom * (1.0 + scroll * 0.002)).clamp(0.5, 8.0);
                }
                let fitted = egui::Image::new(view.uri.clone())
                    .load_for_size(ui.ctx(), stage.size())
                    .ok()
                    .and_then(|poll| poll.size())
                    .map(|size| {
                        let fit = (stage.width() / size.x)
                            .min(stage.height() / size.y)
                            .min(1.0);
                        size * fit
                    })
                    .unwrap_or(stage.size() * 0.5);
                let rect = Rect::from_center_size(stage.center() + view.offset, fitted * view.zoom);
                egui::Image::new(view.uri.clone()).paint_at(ui, rect);

                // Toolbar, top right.
                let mut bar = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(Rect::from_min_size(
                            pos2(screen.right() - 168.0, screen.top() + 16.0),
                            vec2(152.0, 36.0),
                        ))
                        .layout(Layout::right_to_left(Align::Center)),
                );
                bar.spacing_mut().item_spacing.x = 8.0;
                if theme::glyph_button(&mut bar, icon::X, t.close, 32.0, 0.0).clicked() {
                    close = true;
                }
                if theme::glyph_button(&mut bar, icon::DOWNLOAD, t.image_save, 32.0, 0.0).clicked()
                {
                    save_picture(&view.file);
                }
                if theme::glyph_button(&mut bar, icon::COPY, t.image_copy, 32.0, 0.0).clicked() {
                    copy_picture(bar.ctx(), &view.file);
                }
            });
        if close || modal.should_close() {
            self.image_viewer = None;
        }
    }
}

// Status bar ---------------------------------------------------------------

impl App {
    /// The connection on the left (one button during a call, one line
    /// otherwise) and the push-to-talk key on the right.
    fn status_bar(&self, ui: &mut egui::Ui) {
        let t = self.t;
        ui.horizontal_centered(|ui| {
            if self.in_session() {
                self.connection_button(ui);
            } else {
                self.offline_status(ui);
            }

            let binding = self
                .audio
                .as_ref()
                .and_then(|a| a.ptt_binding.clone())
                .filter(|b| !b.is_empty());
            if let Some(key) = binding {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    theme::chip(ui, &key, theme::TEXT, theme::SURFACE_HOVER);
                    ui.label(RichText::new(t.push_to_talk).small().color(theme::TEXT_3));
                });
            }
        });
    }

    /// Signal bars, drawn as three rising pillars.
    fn signal_bars(painter: &egui::Painter, rect: Rect, ink: Color32) {
        for step in 0..3 {
            let height = 5.0 + 4.5 * step as f32;
            let x = rect.left() + step as f32 * 6.0;
            painter.rect_filled(
                Rect::from_min_max(
                    pos2(x, rect.bottom() - height),
                    pos2(x + 4.0, rect.bottom()),
                ),
                CornerRadius::same(1),
                ink,
            );
        }
    }

    fn offline_status(&self, ui: &mut egui::Ui) {
        let t = self.t;
        let (bars, _) = ui.allocate_exact_size(vec2(16.0, 14.0), Sense::hover());
        Self::signal_bars(ui.painter(), bars, theme::TEXT_3);
        ui.label(
            RichText::new(t.disconnected)
                .font(FontId::new(13.0, theme::semibold()))
                .color(theme::TEXT_3),
        );
        let (text, color) = if self.socket != SocketState::Open {
            (t.bridge_offline_short, theme::WARNING)
        } else if self.is_connecting() {
            (t.connecting, theme::TEXT_2)
        } else {
            (t.ready_to_connect, theme::TEXT_2)
        };
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(text).small().color(color));
        });
    }

    /// "Voz conectada", the time in this call and the ping as one button; a
    /// click opens the details. Everything about the call's link lives here.
    fn connection_button(&self, ui: &mut egui::Ui) {
        const PAD: f32 = 10.0;
        const GAP: f32 = 14.0;
        let t = self.t;
        let ping = self
            .audio
            .as_ref()
            .and_then(|audio| audio.ping_ms)
            .map(|ms| ms.round() as u32);
        let mono = FontId::new(12.0, egui::FontFamily::Monospace);
        let painter = ui.painter().clone();
        let state = painter.layout_no_wrap(
            t.voice_connected.to_string(),
            FontId::new(13.0, theme::semibold()),
            theme::SUCCESS,
        );
        let time = painter.layout_no_wrap(self.elapsed_label(), mono.clone(), theme::TEXT_2);
        let ping = ping
            .map(|ms| painter.layout_no_wrap(t.ping_label(ms), mono, peer::ping_color(ms as f32)));
        let mut width = PAD + 16.0 + 8.0 + state.size().x + GAP + time.size().x + PAD;
        if let Some(ping) = &ping {
            width += GAP + ping.size().x;
        }

        let (rect, response) = ui.allocate_exact_size(vec2(width, 28.0), Sense::click());
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::Role::Button, true, t.connection_title)
        });
        let response = response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(t.ping_hint);
        let popup = egui::Popup::from_toggle_button_response(&response)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
        if response.hovered() || popup.is_open() || response.has_focus() {
            painter.rect_filled(rect, CornerRadius::same(6), theme::SURFACE);
        }
        if response.has_focus() {
            theme::focus_ring(&painter, rect);
        }
        let bars = Rect::from_min_size(
            pos2(rect.left() + PAD, rect.center().y - 7.0),
            vec2(16.0, 14.0),
        );
        let quality = self
            .audio
            .as_ref()
            .and_then(|audio| audio.ping_ms)
            .map_or(theme::SUCCESS, |ms| peer::ping_color(ms as f32));
        Self::signal_bars(&painter, bars, quality);
        let mut x = bars.right() + 8.0;
        for galley in [Some(state), Some(time), ping].into_iter().flatten() {
            let y = rect.center().y - galley.size().y / 2.0;
            let advance = galley.size().x + GAP;
            painter.galley(pos2(x, y), galley, theme::TEXT);
            x += advance;
        }
        popup.show(|ui| self.connection_details(ui));
    }

    /// Ping, its graph and the packets lost since the call began.
    fn connection_details(&self, ui: &mut egui::Ui) {
        const WIDTH: f32 = 296.0;
        let t = self.t;
        let server = self
            .snapshot
            .as_ref()
            .map_or("", |snapshot| snapshot.server.name.as_str());
        let values: Vec<f32> = self.ping_history.iter().copied().collect();
        let ping = self
            .audio
            .as_ref()
            .and_then(|audio| audio.ping_ms)
            .unwrap_or(0.0);
        let average = if values.is_empty() {
            ping as f32
        } else {
            values.iter().sum::<f32>() / values.len() as f32
        };
        let (lost, counted) = self.audio.as_ref().map_or((0, 0), |audio| {
            (
                audio.lost_packets.unwrap_or(0),
                audio.counted_packets.unwrap_or(0),
            )
        });

        Frame::new().inner_margin(Margin::same(14)).show(ui, |ui| {
            ui.set_width(WIDTH);
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.add(
                egui::Label::new(
                    RichText::new(t.connection_title).font(FontId::new(15.0, theme::semibold())),
                )
                .selectable(false),
            );
            ui.add(
                egui::Label::new(RichText::new(server).small().color(theme::TEXT_3))
                    .truncate()
                    .selectable(false),
            );
            ui.add_space(6.0);
            let (area, _) = ui.allocate_exact_size(vec2(WIDTH, 88.0), Sense::hover());
            ui.painter()
                .rect_filled(area, CornerRadius::same(6), theme::SURFACE_RAISED);
            peer::sparkline(ui.painter(), area.shrink(8.0), &values, peer::ping_color);
            if let Some(max) = values.iter().copied().reduce(f32::max) {
                ui.painter().text(
                    area.right_top() + vec2(-8.0, 6.0),
                    Align2::RIGHT_TOP,
                    format!("{max:.0} ms"),
                    FontId::new(10.5, egui::FontFamily::Monospace),
                    theme::TEXT_3,
                );
            }
            ui.add_space(6.0);
            // Label at the left edge, value at the right edge, one line each.
            let row = |ui: &mut egui::Ui, label: &str, value: String| {
                ui.allocate_ui_with_layout(
                    vec2(WIDTH, 22.0),
                    Layout::right_to_left(Align::Center),
                    |ui| {
                        ui.label(
                            RichText::new(value)
                                .font(FontId::new(13.0, theme::semibold()))
                                .color(theme::TEXT),
                        );
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            ui.label(RichText::new(label).color(theme::TEXT_2));
                        });
                    },
                )
                .response
            };
            row(ui, t.ping_average, format!("{average:.0} ms"));
            row(ui, t.ping_last, format!("{ping:.0} ms"));
            row(ui, t.ping_lost, loss_label(lost, counted));
            row(
                ui,
                t.ping_voice_delay,
                format!("~{:.0} ms", voice_delay_ms(ping)),
            )
            .on_hover_text(t.voice_delay_hint);
        });
    }
}

/// Rough mouth-to-ear delay in a call, from our ping to the server.
/// shortcut: assumes the other person has the same ping and a fixed local
/// pipeline (20 ms capture frame, about 60 ms of jitter and device buffers);
/// upgrade by measuring the live jitter buffer depth if precision matters.
fn voice_delay_ms(ping_ms: f64) -> f64 {
    ping_ms + 80.0
}

/// "12 (0.4%)": packets lost and their share of the packets counted.
fn loss_label(lost: u64, counted: u64) -> String {
    let share = if counted == 0 {
        0.0
    } else {
        lost as f64 * 100.0 / counted as f64
    };
    format!("{lost} ({}%)", decimal(share, 1))
}

/// A number the way the interface language writes it: comma as the decimal mark.
fn decimal(value: f64, places: usize) -> String {
    format!("{value:.places$}").replace('.', ",")
}

/// The `ts3server://` link every TeamSpeak client opens, pointing at a channel.
/// Carries neither a nickname nor a password.
fn invite_link(address: &str, channel: &str) -> String {
    let (host, port) = match address.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => (host, Some(port)),
        _ => (address, None),
    };
    let mut link = format!("ts3server://{}", host.trim_matches(['[', ']']));
    let mut sep = '?';
    if let Some(port) = port {
        link.push_str(&format!("{sep}port={port}"));
        sep = '&';
    }
    if !channel.is_empty() {
        link.push_str(&format!(
            "{sep}channel={}",
            ts6_bridge::attachments::encode(channel)
        ));
    }
    link
}

// Settings and diagnostics -----------------------------------------------------

impl App {
    fn log_panel(&mut self, ui: &mut egui::Ui) {
        let t = self.t;
        ui.horizontal(|ui| {
            theme::section_label(ui, &t.diagnostics.to_uppercase());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if theme::button(ui, t.clear, None, Tone::Ghost, 72.0).clicked() {
                    self.logs.clear();
                }
            });
        });
        ui.add_space(4.0);
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for line in &self.logs {
                    let color = match line.level.as_str() {
                        "error" => theme::DANGER,
                        "warn" => theme::WARNING,
                        _ => theme::TEXT_2,
                    };
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(&line.ts)
                                .font(FontId::new(12.0, egui::FontFamily::Monospace))
                                .color(theme::TEXT_3),
                        );
                        ui.label(
                            RichText::new(&line.text)
                                .font(FontId::new(12.0, egui::FontFamily::Monospace))
                                .color(color),
                        );
                    });
                }
            });
    }
}

// Helpers ----------------------------------------------------------------------

/// Initial avatar, or the photo when there is one, centered at center.
fn draw_avatar(
    ui: &egui::Ui,
    center: egui::Pos2,
    size: f32,
    name: &str,
    ring: bool,
    photo: Option<&egui::TextureHandle>,
) {
    match photo {
        Some(texture) => theme::photo_at(
            ui,
            Rect::from_center_size(center, vec2(size, size)),
            texture,
            ring.then_some(theme::SUCCESS),
        ),
        None => theme::avatar_tiny(ui.painter(), center, size, name, ring),
    }
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Tallest the chat may be while the participants keep a minimum-size card.
fn max_chat_height(room: f32, below_fixed: f32) -> f32 {
    (room - below_fixed - CARD_MIN_HEIGHT).max(CHAT_MIN_HEIGHT)
}

/// The logo cropped to its visible part and resampled to the exact pixel size
/// it is shown at, for one display scale.
struct LogoTexture {
    key: u32,
    texture: egui::TextureHandle,
    /// Width over height of the cropped logo.
    aspect: f32,
}

impl LogoTexture {
    fn build(ctx: &egui::Context, pixel_width: f32, key: u32) -> Option<Self> {
        let source = image::load_from_memory_with_format(LOGO, image::ImageFormat::WebP)
            .ok()?
            .to_rgba8();
        let (x0, y0, x1, y1) = visible_bounds(&source)?;
        let (width, height) = (x1 - x0, y1 - y0);
        let aspect = width as f32 / height as f32;
        let target_w = pixel_width.round().max(1.0) as u32;
        let target_h = (pixel_width / aspect).round().max(1.0) as u32;

        // Resample only the coverage channel with a high-quality filter and give
        // every pixel the same ink color. Filtering the color of transparent
        // pixels as well would leave dark halos around the edges.
        let coverage = image::GrayImage::from_fn(width, height, |x, y| {
            image::Luma([source.get_pixel(x0 + x, y0 + y)[3]])
        });
        let coverage = image::imageops::resize(
            &coverage,
            target_w,
            target_h,
            image::imageops::FilterType::Lanczos3,
        );
        let ink = theme::TEXT;
        let pixels = coverage
            .pixels()
            .map(|p| Color32::from_rgba_unmultiplied(ink.r(), ink.g(), ink.b(), p[0]))
            .collect();
        let image = egui::ColorImage::new([target_w as usize, target_h as usize], pixels);
        let texture = ctx.load_texture("mirage-logo", image, egui::TextureOptions::LINEAR);
        Some(Self {
            key,
            texture,
            aspect,
        })
    }
}

/// Bounds `(x0, y0, x1, y1)` of the visible pixels, end exclusive, so the
/// transparent margins around the logo can be cut off.
fn visible_bounds(image: &image::RgbaImage) -> Option<(u32, u32, u32, u32)> {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for (x, y, pixel) in image.enumerate_pixels() {
        if pixel[3] > 16 {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x + 1);
            y1 = y1.max(y + 1);
        }
    }
    (x1 > x0 && y1 > y0).then_some((x0, y0, x1, y1))
}

#[cfg(test)]
mod tests {
    use super::{visible_bounds, LOGO};

    #[test]
    fn logo_has_a_visible_crop() {
        let image = image::load_from_memory_with_format(LOGO, image::ImageFormat::WebP)
            .expect("logo decodes")
            .to_rgba8();
        let (x0, y0, x1, y1) = visible_bounds(&image).expect("logo should contain visible pixels");
        assert!(x1 > x0 && y1 > y0);
        let aspect = (x1 - x0) as f32 / (y1 - y0) as f32;
        assert!(
            aspect > 1.0,
            "the wordmark is wider than tall, got {aspect}"
        );
    }
}

#[cfg(test)]
mod reply_tests {
    use super::{invite_link, loss_label, official_attachment, split_quote};

    #[test]
    fn invite_links_point_at_the_channel_and_never_carry_secrets() {
        assert_eq!(
            invite_link("voice.example.com:9987", "Sala 1"),
            "ts3server://voice.example.com?port=9987&channel=Sala%201"
        );
        assert_eq!(
            invite_link("voice.example.com", ""),
            "ts3server://voice.example.com"
        );
        assert_eq!(
            invite_link("[::1]:9987", "A"),
            "ts3server://::1?port=9987&channel=A"
        );
    }

    #[test]
    fn packet_loss_reads_as_count_and_share() {
        assert_eq!(loss_label(0, 0), "0 (0,0%)");
        assert_eq!(loss_label(12, 3000), "12 (0,4%)");
    }

    #[test]
    fn official_client_attachments_show_their_name() {
        let t = &crate::strings::PT;
        let json = r#"{"msg_type":"ts.file.myts","file_id":"x","file_name":"cat.gif","file_size":693210,"v2":true}"#;
        let shown = official_attachment(json, t).unwrap();
        assert!(shown.starts_with("📎 cat.gif (0,7 MB)"), "{shown}");
        assert_eq!(official_attachment("plain text", t), None);
        assert_eq!(official_attachment(r#"{"msg_type":"other"}"#, t), None);
    }

    #[test]
    fn quotes_split_from_the_body() {
        assert_eq!(split_quote("> ana: oi\nolá"), (Some("ana: oi"), "olá"));
        assert_eq!(split_quote("> sem corpo"), (None, "> sem corpo"));
        assert_eq!(split_quote("texto"), (None, "texto"));
    }
}
