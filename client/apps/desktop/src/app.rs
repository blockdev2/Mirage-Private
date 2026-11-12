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
use crate::protocol::{
    AudioEvent, BridgeEvent, ChannelInfo, ClientInfo, Command, Device, DeviceLists, LogLine,
    Snapshot, StatusEvent,
};
use crate::store::{
    AddressProblem, NoiseLevel, Store, TransmitMode, NICKNAME_MAX_CHARS, VAD_MAX_DB, VAD_MIN_DB,
};
use crate::strings::{Strings, PT};
use crate::theme::{self, icon, Tone};

mod home;
mod peer;
use home::{Dialog, PendingConnect, Section};

const LOGO: &[u8] = include_bytes!("../assets/logo.webp");
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
const CONTROL_SIZE: f32 = 44.0;
const CONTROL_GAP: f32 = 8.0;
/// "Convidar" (120) + gap (8) + options button (40).
const HEADER_ACTIONS_WIDTH: f32 = 168.0;

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
    logo_crop: Option<(Rect, f32)>,

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
    grid_view: bool,
    mic_muted: bool,
    output_muted: bool,
    chat_draft: String,
    chat_messages: Vec<crate::protocol::ChatMessage>,
    chat_pending: Option<(String, Instant)>,
    chat_error: Option<String>,
    /// Capability advertised by the bridge; absent on older versions.
    chat_available: bool,
    /// When the last connect request was sent. While it is recent the button
    /// stays disabled: repeated clicks make the server flood-ban this address.
    connect_started: Option<Instant>,
    /// Expand/collapse overrides of the channel tree. A channel without an
    /// entry is open only while it is the one the user is in.
    open_channels: HashMap<u64, bool>,

    // Settings window and diagnostics.
    show_settings: bool,
    show_logs: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx);
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
            logo_crop: logo_crop(),
            address,
            password: String::new(),
            grid_view: true,
            mic_muted: false,
            output_muted: false,
            chat_draft: String::new(),
            chat_messages: Vec::new(),
            chat_pending: None,
            chat_error: None,
            chat_available: false,
            connect_started: None,
            open_channels: HashMap::new(),
            show_settings: false,
            show_logs: false,
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
                        self.chat_available = false;
                        self.chat_messages.clear();
                        self.chat_pending = None;
                    }
                }
                Incoming::Event(event) => self.apply_event(event),
            }
        }
    }

    fn apply_event(&mut self, event: BridgeEvent) {
        match event {
            BridgeEvent::Hello {
                version,
                devices,
                channel_chat,
            } => {
                self.chat_available = channel_chat;
                self.bridge_version = Some(version);
                self.devices = devices;
            }
            BridgeEvent::Status(status) => {
                if status.connected && status.session_id != self.status.session_id {
                    self.chat_messages.clear();
                    self.chat_draft.clear();
                    self.chat_pending = None;
                    self.chat_error = None;
                }
                if status.connected && !self.status.connected {
                    self.session_started = Some(Instant::now());
                }
                if !status.connected {
                    if self.status.connected {
                        // The session ended: go back to the hub.
                        self.section = Section::Home;
                    }
                    self.chat_messages.clear();
                    self.chat_pending = None;
                    self.snapshot = None;
                    self.audio = None;
                    self.session_started = None;
                    self.volumes_sent.clear();
                    self.info_view = None;
                    self.media_avatars.clear();
                    self.server_icon = None;
                    self.media_textures.clear();
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
                let channel = snapshot
                    .clients
                    .iter()
                    .find(|c| c.is_self)
                    .map(|c| c.channel_id.to_string());
                if channel != self.chat_channel() {
                    self.chat_messages.clear();
                    self.chat_draft.clear();
                    self.chat_pending = None;
                    self.chat_error = None;
                }
                if self.snapshot.is_none() {
                    // First view of the server: it counts as connected.
                    self.section = Section::Voice;
                    let profile = self
                        .pending
                        .as_ref()
                        .and_then(|p| p.profile_id.clone())
                        .or_else(|| self.store.active_profile_id());
                    self.finish_connection(&snapshot.server.name);
                    // The voice identity is shared by every local profile, so the
                    // server avatar always follows the profile used to connect:
                    // its photo, or none.
                    self.publish_avatar(profile.as_deref());
                    // Voice works as soon as the call starts, with the saved settings.
                    self.apply_audio();
                }
                self.snapshot = Some(snapshot);
                self.sync_volumes();
            }
            BridgeEvent::Chat(message) => {
                if self.chat_channel().as_deref() == Some(message.channel.as_str()) {
                    self.chat_messages.push(message);
                    if self.chat_messages.len() > 200 {
                        self.chat_messages.remove(0);
                    }
                }
            }
            BridgeEvent::ChatResult { request_id, error } => {
                if self
                    .chat_pending
                    .as_ref()
                    .is_some_and(|(id, _)| id == &request_id)
                {
                    self.chat_pending = None;
                    if error.is_none() {
                        self.chat_draft.clear();
                    }
                    self.chat_error = error;
                }
            }
            BridgeEvent::Log(line) => {
                self.logs.push(line);
                if self.logs.len() > MAX_LOG_LINES {
                    let excess = self.logs.len() - MAX_LOG_LINES;
                    self.logs.drain(..excess);
                }
            }
            BridgeEvent::Audio(audio) => self.audio = Some(audio),
            BridgeEvent::Devices { devices } => self.devices = devices,
            BridgeEvent::ConnectionInfo { client, info } => {
                if let (Some(view), Some(info)) = (self.info_view.as_mut(), info) {
                    if view.client == client {
                        view.push(info);
                    }
                }
            }
            BridgeEvent::Media {
                avatars,
                server_icon,
            } => {
                self.media_avatars = avatars;
                self.server_icon = server_icon;
            }
        }
    }

    fn send(&self, command: Command) {
        // A closed channel means the link thread stopped; the UI keeps running.
        let _ = self.bridge.outgoing.send(command);
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

    /// Status text and its color, shared by the list rows.
    fn status_of(&self, client: &ClientInfo, speaking: bool) -> (&'static str, Color32) {
        let t = self.t;
        if client.input_muted {
            (t.muted_state, theme::DANGER)
        } else if speaking {
            (t.speaking, theme::SUCCESS)
        } else if client.is_self {
            (t.you, theme::TEXT_2)
        } else {
            (t.listening, theme::TEXT_3)
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain_events();
        let ctx = ui.ctx().clone();
        if self.is_connecting() {
            // Re-enables the button when the wait times out.
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        if self.show_settings {
            // Keeps the microphone level meter live.
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        if self.status.connected {
            // Keeps the session timer moving even when no event arrives.
            ctx.request_repaint_after(Duration::from_secs(1));
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
            .show(ui, |ui| match self.effective_section() {
                Section::Voice => self.sidebar(ui),
                _ => self.home_sidebar(ui),
            });

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
        self.settings_window(&ctx);
        self.connection_window(&ctx);
    }
}

// Sidebar -------------------------------------------------------------------

impl App {
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        let t = self.t;
        self.logo(ui);
        ui.add_space(20.0);

        self.server_header(ui);
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(8.0);

        if self.nav_item(ui, icon::HOUSE, t.nav_home, false) {
            self.section = Section::Home;
        }
        self.nav_item(ui, icon::HEADPHONES, t.nav_voice, true);

        ui.add_space(20.0);
        theme::section_label(ui, &t.channels_heading.to_uppercase());
        ui.add_space(8.0);

        let remaining = (ui.available_height() - 76.0).max(80.0);
        egui::ScrollArea::vertical()
            .max_height(remaining)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if let Some(snapshot) = self.snapshot.clone() {
                    self.channel_tree(ui, &snapshot);
                } else {
                    ui.label(
                        RichText::new(t.no_channels_hint)
                            .small()
                            .color(theme::TEXT_3),
                    );
                }
            });

        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            self.user_card(ui);
        });
    }

    fn logo(&self, ui: &mut egui::Ui) {
        match self.logo_crop {
            Some((uv, aspect)) => {
                let width = 160.0;
                let image = egui::Image::from_bytes("bytes://mirage-logo.webp", LOGO)
                    .uv(uv)
                    .fit_to_exact_size(vec2(width, width / aspect));
                ui.add(image);
            }
            None => {
                ui.label(RichText::new(self.t.logo_fallback).size(24.0).strong());
            }
        }
    }

    /// Server avatar, name and description. A flat header, not a boxed card.
    fn server_header(&mut self, ui: &mut egui::Ui) {
        let t = self.t;
        let icon = self
            .server_icon
            .clone()
            .and_then(|path| self.media_texture(ui.ctx(), &path));
        let (name, subtitle) = match &self.snapshot {
            Some(s) => (s.server.name.as_str(), t.private_server),
            None => (t.no_server, t.offline),
        };
        ui.horizontal(|ui| {
            theme::avatar_or_photo(ui, name, 36.0, icon.as_ref());
            ui.vertical(|ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(name).font(FontId::new(14.0, theme::semibold())),
                    )
                    .truncate(),
                );
                ui.label(RichText::new(subtitle).small().color(theme::TEXT_3));
            });
        });
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
        if client.input_muted {
            painter.text(
                rect.right_center() - vec2(12.0, 0.0),
                Align2::RIGHT_CENTER,
                icon::MICROPHONE_SLASH,
                FontId::new(14.0, theme::icons()),
                theme::TEXT_3,
            );
        } else if speaking {
            painter.text(
                rect.right_center() - vec2(12.0, 0.0),
                Align2::RIGHT_CENTER,
                icon::WAVEFORM,
                FontId::new(14.0, theme::icons()),
                theme::SUCCESS,
            );
        }
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
                if theme::icon_button(ui, icon::GEAR, t.settings_title, false, 32.0).clicked() {
                    self.show_settings = true;
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

        // Count, view switch and cards share one centered column, so the
        // switch stays next to the grid instead of at the window edge.
        let width = ui.available_width().min(CONTENT_MAX_WIDTH);
        let left = ui.max_rect().left() + ((ui.available_width() - width) / 2.0).max(0.0);
        let area = Rect::from_min_size(
            pos2(left, ui.cursor().top()),
            vec2(width, ui.available_height()),
        );
        ui.scope_builder(UiBuilder::new().max_rect(area), |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(t.participants_count(participants.len()))
                        .font(FontId::new(14.0, theme::medium())),
                );
                ui.add_space((ui.available_width() - theme::SEGMENTED_WIDTH).max(0.0));
                let selected = usize::from(self.grid_view);
                let options = [(icon::LIST, t.view_list), (icon::SQUARES_FOUR, t.view_grid)];
                if let Some(index) = theme::segmented(ui, options, selected) {
                    self.grid_view = index == 1;
                }
            });
            ui.add_space(12.0);
            self.participants_area(ui, &participants);
        });

        ui.add_space(20.0);
        self.call_controls(ui);
        ui.add_space(16.0);
        ui.separator();
        ui.add_space(6.0);

        let channel_name = channel.map_or(t.no_channel, |c| c.name.as_str());
        self.chat_section(ui, channel_name);
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
            })
            .unwrap_or_else(|| t.default_topic.to_string());

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
                        RichText::new(t.channel_subtitle(&topic)).color(theme::TEXT_2),
                    )
                    .truncate(),
                );
            });

            ui.add_space((ui.available_width() - HEADER_ACTIONS_WIDTH).max(0.0));
            ui.add_enabled_ui(false, |ui| {
                theme::button(ui, t.invite, Some(icon::USER_PLUS), Tone::Secondary, 120.0)
            })
            .inner
            .on_disabled_hover_text(t.invite_disabled);

            let more = theme::icon_button(ui, icon::DOTS_THREE, t.more_options, false, 40.0);
            egui::Popup::menu(&more).show(|ui| {
                if ui.button(t.settings_title).clicked() {
                    self.show_settings = true;
                }
                ui.checkbox(&mut self.show_logs, t.show_diagnostics);
            });
        });
    }

    /// Grid of 16:9 cards (each row centered), or the list. The grid takes the
    /// height left after the controls and a usable chat are reserved, so the
    /// cards shrink on short windows instead of pushing the chat away.
    fn participants_area(&mut self, ui: &mut egui::Ui, participants: &[&ClientInfo]) {
        let room = (ui.available_height() - BELOW_GRID_MIN_HEIGHT).max(CARD_MIN_HEIGHT);
        if self.grid_view {
            let available = ui.available_width();
            let count = participants.len().max(1);
            let columns = (((available + CARD_GAP) / (CARD_MIN_WIDTH + CARD_GAP)) as usize)
                .clamp(1, count.min(MAX_COLUMNS));
            let rows = count.div_ceil(columns);
            let column_width = (available - CARD_GAP * (columns - 1) as f32) / columns as f32;
            let row_room =
                ((room - CARD_GAP * (rows - 1) as f32) / rows as f32).max(CARD_MIN_HEIGHT);
            // 16:9 from the column width, limited by the height left; when height
            // is the limit the width follows it, so cards never distort.
            let card_height = (column_width * 9.0 / 16.0).min(row_room);
            let card_width = (card_height * 16.0 / 9.0).min(column_width);
            let content = rows as f32 * card_height + (rows - 1) as f32 * CARD_GAP;
            egui::ScrollArea::vertical()
                .id_salt("participants")
                .max_height(content.min(room.max(card_height)))
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
        } else {
            egui::ScrollArea::vertical()
                .id_salt("participants")
                .max_height((participants.len() as f32 * 53.0).min(room.max(106.0)))
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for client in participants {
                        self.participant_row(ui, client);
                    }
                });
        }
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
        painter.rect_stroke(
            rect,
            CornerRadius::same(8),
            Stroke::new(border_width, border),
            egui::StrokeKind::Inside,
        );
        let avatar = (height * 0.40).clamp(36.0, 80.0);
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
        if client.input_muted {
            painter.text(
                corner,
                Align2::RIGHT_BOTTOM,
                icon::MICROPHONE_SLASH,
                FontId::new(18.0, theme::icons()),
                theme::TEXT,
            );
        } else if speaking {
            painter.text(
                corner,
                Align2::RIGHT_BOTTOM,
                icon::WAVEFORM,
                FontId::new(18.0, theme::icons()),
                theme::SUCCESS,
            );
        }
    }

    /// A table row: hairline between rows, status as plain colored text.
    fn participant_row(&mut self, ui: &mut egui::Ui, client: &ClientInfo) {
        let photo = self.client_photo(ui.ctx(), client);
        let speaking = self.is_speaking(client.id);
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::click());
        self.peer_menu(&response, client);
        let painter = ui.painter();
        if speaking {
            painter.rect_stroke(
                rect.shrink(1.0),
                CornerRadius::same(6),
                Stroke::new(1.5, theme::SUCCESS),
                egui::StrokeKind::Inside,
            );
        }
        painter.line_segment(
            [rect.left_bottom(), rect.right_bottom()],
            Stroke::new(1.0, theme::BORDER_SOFT),
        );
        draw_avatar(
            ui,
            rect.left_center() + vec2(24.0, 0.0),
            32.0,
            &client.name,
            false,
            photo.as_ref(),
        );
        let name = theme::fit_text(
            painter,
            &client.name,
            FontId::new(14.0, theme::medium()),
            theme::TEXT,
            rect.width() - 50.0 - 120.0,
        );
        painter.galley(
            pos2(rect.left() + 50.0, rect.center().y - name.size().y / 2.0),
            name,
            theme::TEXT,
        );
        let (label, color) = self.status_of(client, speaking);
        painter.text(
            rect.right_center() - vec2(16.0, 0.0),
            Align2::RIGHT_CENTER,
            label,
            FontId::new(12.5, theme::medium()),
            color,
        );
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

            ui.add_enabled_ui(false, |ui| {
                theme::icon_button(ui, icon::SHARE_SCREEN, t.share_screen, false, CONTROL_SIZE)
            })
            .inner
            .on_disabled_hover_text(t.share_disabled);

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

    fn chat_channel(&self) -> Option<String> {
        self.snapshot
            .as_ref()?
            .clients
            .iter()
            .find(|client| client.is_self)
            .map(|client| client.channel_id.to_string())
    }

    /// Channel history contains server events only; command acknowledgments never add bubbles.
    fn chat_section(&mut self, ui: &mut egui::Ui, channel_name: &str) {
        let t = self.t;
        if self
            .chat_pending
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(20))
        {
            self.chat_pending = None;
            self.chat_error = Some(t.chat_uncertain.into());
        }
        let available =
            self.chat_available && self.status.connected && self.socket == SocketState::Open;
        ui.label(RichText::new(t.chat_heading).font(FontId::new(16.0, theme::semibold())));
        ui.add_space(8.0);
        let mut submit = false;
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            if self.chat_draft.len() > 1024 {
                ui.label(t.chat_limit);
            }
            if let Some(error) = &self.chat_error {
                ui.label(error);
            }
            let frame = if available {
                theme::input_frame()
            } else {
                theme::input_frame_disabled()
            };
            frame.show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icon::SEND).font(FontId::new(16.0, theme::icons())));
                    let edit = ui
                        .add_enabled(
                            available && self.chat_pending.is_none(),
                            egui::TextEdit::singleline(&mut self.chat_draft)
                                .hint_text(if available {
                                    t.chat_placeholder_in(channel_name)
                                } else {
                                    t.chat_unavailable_title.into()
                                })
                                .desired_width((ui.available_width() - 110.0).max(0.0)),
                        )
                        .on_disabled_hover_text(t.chat_disabled);
                    let valid = available
                        && self.chat_pending.is_none()
                        && !self.chat_draft.trim().is_empty()
                        && self.chat_draft.len() <= 1024;
                    let clicked = ui
                        .add_enabled(valid, egui::Button::new(t.chat_send))
                        .clicked();
                    submit = valid
                        && (clicked
                            || (edit.lost_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Enter))));
                });
            });
            ui.add_space(8.0);
            if ui.available_height() >= HISTORY_MIN_HEIGHT {
                egui::ScrollArea::vertical()
                    .id_salt("chat_history")
                    .stick_to_bottom(true)
                    .min_scrolled_height(0.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if self.chat_messages.is_empty() {
                            ui.label(if available {
                                t.chat_empty_title
                            } else {
                                t.chat_unavailable_title
                            });
                            ui.label(if available {
                                t.chat_empty_body
                            } else {
                                t.chat_unavailable_body
                            });
                        }
                        for message in &self.chat_messages {
                            ui.label(
                                RichText::new(format!("{}  {}", message.ts, message.sender))
                                    .small()
                                    .color(theme::TEXT_3),
                            );
                            ui.label(&message.text);
                            ui.add_space(6.0);
                        }
                    });
            }
        });
        if submit {
            if let Some(channel) = self.chat_channel() {
                let id = format!(
                    "desktop-{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos()
                );
                if self
                    .bridge
                    .outgoing
                    .send(Command::Chat {
                        session_id: self.status.session_id.clone().unwrap_or_default(),
                        channel,
                        text: self.chat_draft.clone(),
                        request_id: id.clone(),
                    })
                    .is_ok()
                {
                    self.chat_pending = Some((id, Instant::now()));
                    self.chat_error = None;
                } else {
                    self.chat_error = Some(t.chat_uncertain.into());
                }
            }
        }
    }
}

// Status bar ---------------------------------------------------------------

impl App {
    /// Connection state, server and session time on the left; the push-to-talk
    /// key on the right. Latency is not exposed by the bridge, so it is absent.
    fn status_bar(&self, ui: &mut egui::Ui) {
        let t = self.t;
        let in_session = self.in_session();
        ui.horizontal_centered(|ui| {
            let (bars, _) = ui.allocate_exact_size(vec2(16.0, 14.0), Sense::hover());
            let ink = if in_session {
                theme::SUCCESS
            } else {
                theme::TEXT_3
            };
            for step in 0..3 {
                let height = 5.0 + 4.5 * step as f32;
                let x = bars.left() + step as f32 * 6.0;
                ui.painter().rect_filled(
                    Rect::from_min_max(
                        pos2(x, bars.bottom() - height),
                        pos2(x + 4.0, bars.bottom()),
                    ),
                    CornerRadius::same(1),
                    ink,
                );
            }
            let label = if in_session {
                t.voice_connected
            } else {
                t.disconnected
            };
            ui.label(
                RichText::new(label)
                    .font(FontId::new(13.0, theme::semibold()))
                    .color(ink),
            );
            if !in_session {
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

            if let Some(snapshot) = self.snapshot.as_ref().filter(|_| in_session) {
                ui.separator();
                ui.label(
                    RichText::new(&snapshot.server.name)
                        .small()
                        .color(theme::TEXT_2),
                );
                ui.label(
                    RichText::new(self.elapsed_label())
                        .font(FontId::new(12.0, egui::FontFamily::Monospace))
                        .color(theme::TEXT_3),
                );
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
}

// Settings and diagnostics -----------------------------------------------------

impl App {
    fn settings_window(&mut self, ctx: &egui::Context) {
        let t = self.t;
        let mut open = self.show_settings;
        egui::Window::new(t.settings_title)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(400.0)
            .show(ctx, |ui| {
                theme::section_label(ui, t.audio_section);
                ui.add_space(6.0);
                let level = self.audio.as_ref().and_then(|a| a.input_level_db);
                let transmitting = self.audio.as_ref().is_some_and(|a| a.transmitting);
                let mut threshold_changed = false;
                let audio = &mut self.store.data.audio;
                ui.checkbox(&mut audio.enabled, t.enable_audio);
                ui.add_enabled_ui(audio.enabled, |ui| {
                    device_combo(
                        ui,
                        t,
                        t.input,
                        "input",
                        &mut audio.input_device,
                        &self.devices.inputs,
                    );
                    device_combo(
                        ui,
                        t,
                        t.output,
                        "output",
                        &mut audio.output_device,
                        &self.devices.outputs,
                    );
                    ui.add_space(8.0);
                    ui.label(RichText::new(t.transmit_mode).small().color(theme::TEXT_2));
                    ui.horizontal(|ui| {
                        ui.radio_value(
                            &mut audio.mode,
                            TransmitMode::VoiceActivation,
                            t.mode_voice,
                        );
                        ui.radio_value(&mut audio.mode, TransmitMode::PushToTalk, t.mode_ptt);
                        ui.radio_value(
                            &mut audio.mode,
                            TransmitMode::Continuous,
                            t.mode_continuous,
                        );
                    });
                    ui.add_space(6.0);
                    match audio.mode {
                        TransmitMode::VoiceActivation => {
                            ui.label(RichText::new(t.vad_threshold).small().color(theme::TEXT_2));
                            level_meter(ui, level, audio.threshold_db, transmitting);
                            let slider = ui.add(
                                egui::Slider::new(&mut audio.threshold_db, VAD_MIN_DB..=VAD_MAX_DB)
                                    .suffix(" dB")
                                    .step_by(1.0),
                            );
                            threshold_changed = slider.changed();
                            ui.label(RichText::new(t.vad_hint).small().color(theme::TEXT_3));
                        }
                        TransmitMode::PushToTalk => {
                            field(
                                ui,
                                t.ptt_key,
                                egui::TextEdit::singleline(&mut audio.ptt).hint_text("F13"),
                            );
                            ui.label(RichText::new(t.ptt_hint).small().color(theme::TEXT_3));
                        }
                        TransmitMode::Continuous => {}
                    }
                    ui.add_space(8.0);
                    ui.label(RichText::new(t.noise_title).small().color(theme::TEXT_2));
                    let before = audio.noise;
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut audio.noise, NoiseLevel::Off, t.noise_off);
                        ui.radio_value(&mut audio.noise, NoiseLevel::Standard, t.noise_standard);
                        ui.radio_value(&mut audio.noise, NoiseLevel::Strong, t.noise_strong);
                    });
                    let hint = match audio.noise {
                        NoiseLevel::Off => t.noise_hint_off,
                        NoiseLevel::Standard => t.noise_hint_standard,
                        NoiseLevel::Strong => t.noise_hint_strong,
                    };
                    ui.label(RichText::new(hint).small().color(theme::TEXT_3));
                    // Applies live as well: no microphone restart.
                    threshold_changed |= audio.noise != before;
                });
                // The slider applies live: the engine changes the threshold
                // without restarting the microphone.
                if threshold_changed {
                    self.apply_audio();
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if theme::button(ui, t.apply_audio, None, Tone::Primary, 150.0).clicked() {
                        self.apply_audio();
                    }
                    if theme::button(ui, t.refresh_devices, None, Tone::Secondary, 170.0).clicked()
                    {
                        self.send(Command::Devices);
                    }
                });

                ui.add_space(16.0);
                theme::section_label(ui, t.profile_section);
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    theme::input_frame().show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.nickname)
                                .frame(Frame::NONE)
                                .char_limit(NICKNAME_MAX_CHARS)
                                .desired_width(200.0),
                        );
                    });
                    let can_save = self.in_session() && !self.nickname.trim().is_empty();
                    ui.add_enabled_ui(can_save, |ui| {
                        if theme::button(ui, t.save_nickname, None, Tone::Secondary, 140.0)
                            .clicked()
                        {
                            self.send(Command::Nick {
                                name: self.nickname.trim().to_string(),
                            });
                        }
                    });
                });

                ui.add_space(16.0);
                ui.checkbox(&mut self.show_logs, t.show_diagnostics);
                let version = match &self.bridge_version {
                    Some(v) => t.bridge_version(v),
                    None => t.bridge_offline_short.to_string(),
                };
                ui.label(RichText::new(version).small().color(theme::TEXT_3));
            });
        self.show_settings = open;
    }

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

/// Labelled text field with the app's input frame.
fn field(ui: &mut egui::Ui, label: &str, edit: egui::TextEdit<'_>) {
    ui.label(RichText::new(label).small().color(theme::TEXT_2));
    ui.add_space(2.0);
    theme::input_frame().show(ui, |ui| {
        ui.add(edit.frame(Frame::NONE).desired_width(f32::INFINITY));
    });
}

/// Horizontal microphone level bar with the voice activation threshold marked.
/// Green while transmitting, so the user sees exactly when voice goes out.
fn level_meter(ui: &mut egui::Ui, level_db: Option<f32>, threshold_db: f32, transmitting: bool) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 10.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(3), theme::SURFACE_RAISED);
    let span = VAD_MAX_DB - VAD_MIN_DB + 10.0;
    let to_x = |db: f32| rect.left() + rect.width() * ((db - VAD_MIN_DB) / span).clamp(0.0, 1.0);
    if let Some(level) = level_db {
        let fill = Rect::from_min_max(rect.left_top(), pos2(to_x(level), rect.bottom()));
        let color = if transmitting {
            theme::SUCCESS
        } else {
            theme::TEXT_3
        };
        painter.rect_filled(fill, CornerRadius::same(3), color);
    }
    let x = to_x(threshold_db);
    painter.line_segment(
        [pos2(x, rect.top() - 3.0), pos2(x, rect.bottom() + 3.0)],
        Stroke::new(2.0, theme::TEXT),
    );
}

fn device_combo(
    ui: &mut egui::Ui,
    t: &Strings,
    label: &str,
    salt: &str,
    selected: &mut String,
    devices: &[Device],
) {
    ui.horizontal(|ui| {
        // Same label width for every device row, so both selectors line up.
        ui.add_sized(
            vec2(84.0, 24.0),
            egui::Label::new(RichText::new(label).color(theme::TEXT_2)),
        );
        let shown = if selected.is_empty() {
            t.system_default.to_string()
        } else {
            selected.clone()
        };
        egui::ComboBox::from_id_salt(salt)
            .selected_text(shown)
            .width(260.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(selected, String::new(), t.system_default);
                for device in devices {
                    ui.selectable_value(selected, device.name.clone(), &device.name);
                }
            });
    });
}

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

/// Finds the visible part of the logo so it can be cropped and scaled
/// without the transparent margins around it.
fn logo_crop() -> Option<(Rect, f32)> {
    let image = image::load_from_memory_with_format(LOGO, image::ImageFormat::WebP)
        .ok()?
        .to_rgba8();
    let (width, height) = image.dimensions();
    let (mut x0, mut y0, mut x1, mut y1) = (width, height, 0u32, 0u32);
    for (x, y, pixel) in image.enumerate_pixels() {
        if pixel[3] > 16 {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    if x1 < x0 || y1 < y0 {
        return None;
    }
    let uv = Rect::from_min_max(
        pos2(x0 as f32 / width as f32, y0 as f32 / height as f32),
        pos2(
            (x1 + 1) as f32 / width as f32,
            (y1 + 1) as f32 / height as f32,
        ),
    );
    let aspect = (x1 + 1 - x0) as f32 / (y1 + 1 - y0) as f32;
    Some((uv, aspect))
}

#[cfg(test)]
mod tests {
    use super::logo_crop;

    #[test]
    fn logo_has_a_visible_crop() {
        let (uv, aspect) = logo_crop().expect("logo should contain visible pixels");
        assert!(uv.width() > 0.0 && uv.height() > 0.0);
        assert!(
            aspect > 1.0,
            "the wordmark is wider than tall, got {aspect}"
        );
    }
}
