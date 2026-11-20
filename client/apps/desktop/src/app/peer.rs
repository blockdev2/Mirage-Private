//! Per-participant actions: the right-click menu (volume, local mute,
//! connection info) and the connection info window.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use egui::{pos2, vec2, Align2, Color32, CornerRadius, FontId, Rect, RichText, Sense, Stroke};

use super::App;
use crate::protocol::{ClientInfo, Command, ConnectionInfo};
use crate::store::MAX_PEER_VOLUME;
use crate::theme::{self, icon};

/// Samples kept for the sparklines (one per second).
pub(super) const HISTORY: usize = 60;
const MENU_WIDTH: f32 = 272.0;
const INFO_WIDTH: f32 = 320.0;
/// Around 100% the slider snaps, so "back to normal" is easy to hit.
const SNAP: f32 = 0.04;

/// One sample of the connection graphs: loss in, loss out (0..1), ping (ms).
pub(super) type InfoSample = [f32; 3];

pub(super) struct InfoView {
    pub client: u16,
    pub name: String,
    pub info: Option<ConnectionInfo>,
    pub history: VecDeque<InfoSample>,
    pub copied: Option<Instant>,
}

impl InfoView {
    pub fn new(client: u16, name: String) -> Self {
        Self {
            client,
            name,
            info: None,
            history: VecDeque::new(),
            copied: None,
        }
    }

    pub fn push(&mut self, info: ConnectionInfo) {
        let sample = [
            info.loss_incoming.unwrap_or(0.0),
            info.loss_outgoing,
            info.ping_ms.map_or(f32::NAN, |p| p as f32),
        ];
        if self.history.len() >= HISTORY {
            self.history.pop_front();
        }
        self.history.push_back(sample);
        self.info = Some(info);
    }
}

impl App {
    /// Volume saved for a participant (by unique id), 1.0 when none.
    fn saved_volume(&self, client: &ClientInfo) -> f32 {
        client
            .uid
            .as_ref()
            .and_then(|uid| self.store.data.volumes.get(uid))
            .copied()
            .unwrap_or(1.0)
    }

    fn set_saved_volume(&mut self, client: &ClientInfo, volume: f32) {
        let volume = volume.clamp(0.0, MAX_PEER_VOLUME);
        if let Some(uid) = &client.uid {
            if (volume - 1.0).abs() < f32::EPSILON {
                self.store.data.volumes.remove(uid);
            } else {
                self.store.data.volumes.insert(uid.clone(), volume);
            }
        }
        self.volumes_sent.insert(client.id, volume);
        self.send(Command::Volume {
            client: client.id,
            volume,
        });
    }

    /// Sends the saved volume of every participant the engine does not have
    /// yet (new joiners, reconnects). Called on every state update.
    pub(super) fn sync_volumes(&mut self) {
        let Some(snapshot) = self.snapshot.clone() else {
            return;
        };
        for client in snapshot.clients.iter().filter(|c| !c.is_self) {
            let volume = self.saved_volume(client);
            let known = self.volumes_sent.get(&client.id).copied();
            let needs = match known {
                Some(sent) => (sent - volume).abs() > f32::EPSILON,
                None => (volume - 1.0).abs() > f32::EPSILON,
            };
            if needs {
                self.volumes_sent.insert(client.id, volume);
                self.send(Command::Volume {
                    client: client.id,
                    volume,
                });
            }
        }
    }

    /// The right-click menu of a participant.
    pub(super) fn peer_menu(&mut self, response: &egui::Response, client: &ClientInfo) {
        let t = self.t;
        let photo = self.client_photo(&response.ctx, client);
        let mut volume = self.saved_volume(client);
        let mut changed = false;
        let mut open_info = false;
        let mut moderation: Option<super::admin::AdminDialog> = None;
        let mut write_to: Option<u16> = None;
        let mut groups_of: Option<(u64, String)> = None;
        let mut channel_group_of: Option<(u64, String, u64)> = None;
        let mut permissions_of: Option<u64> = None;
        let mut whisper_toggle: Option<u16> = None;
        // Clicks inside the menu (slider, % field, buttons) must keep it open;
        // only a click outside closes it.
        egui::Popup::context_menu(response)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                ui.set_width(MENU_WIDTH);
                ui.spacing_mut().item_spacing.y = 4.0;

                // Header: who this menu is about.
                // A fixed-height row with centered items, so the name sits on the
                // avatar's center line instead of its top.
                ui.allocate_ui_with_layout(
                    vec2(ui.available_width(), 36.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        theme::avatar_or_photo(ui, &client.name, 32.0, photo.as_ref());
                        ui.add(
                            egui::Label::new(
                                RichText::new(&client.name)
                                    .font(FontId::new(14.0, theme::semibold())),
                            )
                            .truncate(),
                        );
                        if client.is_self {
                            ui.label(RichText::new(t.you).small().color(theme::TEXT_3));
                        }
                    },
                );
                ui.add_space(4.0);
                ui.separator();

                if !client.is_self {
                    // Volume: icon, label, value; then the slider.
                    ui.horizontal(|ui| {
                        let glyph = if volume <= 0.0 {
                            icon::SPEAKER_SLASH
                        } else if volume < 1.0 {
                            icon::SPEAKER_LOW
                        } else {
                            icon::SPEAKER
                        };
                        ui.label(
                            RichText::new(glyph)
                                .font(FontId::new(16.0, theme::icons()))
                                .color(theme::TEXT_2),
                        );
                        ui.label(RichText::new(t.user_volume).color(theme::TEXT));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let color = if volume > 1.0 + SNAP {
                                theme::SUCCESS
                            } else if volume <= 0.0 {
                                theme::DANGER
                            } else {
                                theme::TEXT
                            };
                            changed |= percent_field(ui, client.id, &mut volume, color);
                        });
                    });
                    changed |= volume_slider(ui, &mut volume, MAX_PEER_VOLUME);
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        let muted = volume <= 0.0;
                        let (glyph, label) = if muted {
                            (icon::SPEAKER, t.user_unmute_local)
                        } else {
                            (icon::SPEAKER_SLASH, t.user_mute_local)
                        };
                        if chip_button(ui, glyph, label, muted).clicked() {
                            volume = if muted { 1.0 } else { 0.0 };
                            changed = true;
                        }
                        let reset_enabled = (volume - 1.0).abs() > f32::EPSILON;
                        let reset = ui.add_enabled_ui(reset_enabled, |ui| {
                            chip_button(
                                ui,
                                icon::ARROW_COUNTER_CLOCKWISE,
                                t.user_reset_volume,
                                false,
                            )
                        });
                        if reset.inner.clicked() {
                            volume = 1.0;
                            changed = true;
                        }
                    });
                    ui.add_space(2.0);
                    ui.separator();
                }

                if menu_item(ui, icon::INFO, t.user_connection_info).clicked() {
                    open_info = true;
                    ui.close();
                }
                if !client.is_self {
                    if menu_item(ui, icon::REPLY, t.user_message).clicked() {
                        write_to = Some(client.id);
                        ui.close();
                    }
                    if client.db_id != 0
                        && menu_item(ui, icon::USER_PLUS, "Grupos do servidor").clicked()
                    {
                        groups_of = Some((client.db_id, client.name.clone()));
                        ui.close();
                    }
                    let whispering = self.whisper.contains(&client.id);
                    let whisper_label = if whispering {
                        t.user_whisper_stop
                    } else {
                        t.user_whisper
                    };
                    if menu_item(ui, icon::SPEAKER_LOW, whisper_label).clicked() {
                        whisper_toggle = Some(client.id);
                        ui.close();
                    }
                    if client.db_id != 0
                        && menu_item(ui, icon::INFO, "Permissões da pessoa").clicked()
                    {
                        permissions_of = Some(client.db_id);
                        ui.close();
                    }
                    if client.db_id != 0
                        && menu_item(ui, icon::USER_PLUS, "Grupo no canal").clicked()
                    {
                        channel_group_of =
                            Some((client.db_id, client.name.clone(), client.channel_id));
                        ui.close();
                    }
                    ui.add_space(2.0);
                    ui.separator();
                    moderation = moderation_items(ui, client);
                }
            });
        if moderation.is_some() {
            self.admin.dialog = moderation;
        }
        if let Some((db_id, name)) = groups_of {
            self.open_memberships(db_id, name);
        }
        if let Some(id) = whisper_toggle {
            self.toggle_whisper(id);
        }
        if let Some(db_id) = permissions_of {
            self.open_permissions(super::admin::Kind::ClientPerm, db_id);
        }
        if let Some((db_id, name, channel)) = channel_group_of {
            self.open_channel_group(db_id, name, channel);
        }
        if let Some(id) = write_to {
            self.chat_to = super::chat::Target::Person(id);
        }
        if changed {
            self.set_saved_volume(client, volume);
        }
        // Written to disk once the pointer is released, not on every drag step.
        self.volumes_dirty |= changed;
        if self.volumes_dirty && !response.ctx.input(|i| i.pointer.any_down()) {
            self.store.save();
            self.volumes_dirty = false;
        }
        if open_info {
            self.store.save();
            self.info_view = Some(InfoView::new(client.id, client.name.clone()));
            self.send(Command::ConnectionInfo {
                client: Some(client.id),
            });
        }
    }

    /// Connection info window for one participant: the ping graph is the
    /// one picture, everything else is a quiet list of label and value.
    pub(super) fn connection_window(&mut self, ctx: &egui::Context) {
        let t = self.t;
        let show_addresses = self.show_addresses;
        let Some(view) = self.info_view.as_mut() else {
            return;
        };
        let mut open = true;
        let frame = egui::Frame::new()
            .fill(theme::SIDEBAR)
            .stroke(Stroke::new(1.0, theme::BORDER))
            .corner_radius(CornerRadius::same(14))
            .shadow(egui::Shadow {
                offset: [0, 18],
                blur: 48,
                spread: 0,
                color: Color32::from_black_alpha(150),
            })
            .inner_margin(egui::Margin::same(20));
        egui::Window::new(t.user_connection_info)
            .id(egui::Id::unique("connection-info"))
            .title_bar(false)
            .frame(frame)
            .resizable(false)
            .fixed_size(vec2(INFO_WIDTH, 0.0))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                // Header: who, then what this is.
                let (head, _) = ui.allocate_exact_size(vec2(INFO_WIDTH, 40.0), Sense::hover());
                ui.painter().text(
                    head.left_top(),
                    Align2::LEFT_TOP,
                    &view.name,
                    FontId::new(17.0, theme::semibold()),
                    theme::TEXT,
                );
                ui.painter().text(
                    head.left_top() + vec2(0.0, 23.0),
                    Align2::LEFT_TOP,
                    t.user_connection_info,
                    FontId::new(12.0, egui::FontFamily::Proportional),
                    theme::TEXT_3,
                );
                let close =
                    Rect::from_min_size(head.right_top() - vec2(30.0, 2.0), vec2(32.0, 32.0));
                if ui
                    .put(close, |ui: &mut egui::Ui| {
                        super::settings::close_button(ui, t.close)
                    })
                    .clicked()
                {
                    open = false;
                }
                ui.add_space(14.0);

                let Some(info) = view.info.clone() else {
                    ui.label(RichText::new(t.info_waiting).color(theme::TEXT_3));
                    return;
                };

                // The ping graph, with the reading set into its corner.
                let (graph, _) = ui.allocate_exact_size(vec2(INFO_WIDTH, 108.0), Sense::hover());
                let painter = ui.painter();
                painter.rect_filled(graph, CornerRadius::same(10), theme::SURFACE);
                let (reading, color) = match info.ping_ms {
                    Some(p) => (format!("{p:.0}"), ping_color(p as f32)),
                    None => ("—".into(), theme::TEXT_3),
                };
                let number = painter.text(
                    graph.left_top() + vec2(14.0, 10.0),
                    Align2::LEFT_TOP,
                    reading,
                    FontId::new(26.0, theme::semibold()),
                    color,
                );
                let unit = match info.ping_deviation_ms {
                    Some(d) => format!("ms  ±{d:.1}"),
                    None => "ms".into(),
                };
                painter.text(
                    pos2(number.right() + 5.0, number.bottom() - 5.0),
                    Align2::LEFT_BOTTOM,
                    unit,
                    FontId::new(12.0, egui::FontFamily::Proportional),
                    theme::TEXT_3,
                );
                painter.text(
                    graph.right_top() + vec2(-14.0, 14.0),
                    Align2::RIGHT_TOP,
                    t.info_ping,
                    FontId::new(12.0, egui::FontFamily::Proportional),
                    theme::TEXT_3,
                );
                let pings: Vec<f32> = view.history.iter().map(|s| s[2]).collect();
                sparkline(
                    painter,
                    Rect::from_min_max(
                        graph.left_top() + vec2(14.0, 52.0),
                        graph.right_bottom() - vec2(14.0, 12.0),
                    ),
                    &pings,
                    ping_color,
                );
                ui.add_space(10.0);

                // Everything else: a plain list, hairlines between rows.
                let percent = |v: f32| format!("{:.1}%", v * 100.0);
                let loss = |v: Option<f32>| match v {
                    Some(v) => (
                        percent(v),
                        if v > 0.0 { loss_color(v) } else { theme::TEXT },
                    ),
                    None => ("—".into(), theme::TEXT_3),
                };
                let address = match &info.address {
                    Some(a) if show_addresses => (a.clone(), theme::TEXT),
                    Some(a) => ("•".repeat(a.len().min(15)), theme::TEXT_2),
                    None => (t.info_hidden.to_string(), theme::TEXT_3),
                };
                let rows = [
                    (t.info_loss_in, loss(info.loss_incoming)),
                    (t.info_loss_out, loss(Some(info.loss_outgoing))),
                    (
                        t.info_connected,
                        (info.connected_secs.map_or("—".into(), clock), theme::TEXT),
                    ),
                    (t.info_idle, (clock(info.idle_secs), theme::TEXT)),
                    (t.info_bytes_in, (bytes(info.bytes_received), theme::TEXT)),
                    (t.info_bytes_out, (bytes(info.bytes_sent), theme::TEXT)),
                    (t.info_address, address),
                ];
                for (i, (label, (value, color))) in rows.into_iter().enumerate() {
                    let (row, response) =
                        ui.allocate_exact_size(vec2(INFO_WIDTH, 34.0), Sense::click());
                    let painter = ui.painter();
                    if i > 0 {
                        painter.line_segment(
                            [row.left_top(), row.right_top()],
                            Stroke::new(1.0, theme::BORDER_SOFT),
                        );
                    }
                    painter.text(
                        row.left_center(),
                        Align2::LEFT_CENTER,
                        label,
                        FontId::new(13.0, egui::FontFamily::Proportional),
                        theme::TEXT_2,
                    );
                    painter.text(
                        row.right_center(),
                        Align2::RIGHT_CENTER,
                        value,
                        FontId::new(13.0, theme::medium()),
                        color,
                    );
                    // The address row copies the address on click.
                    if let (Some(addr), true) = (&info.address, label == t.info_address) {
                        let copied = view
                            .copied
                            .is_some_and(|at| at.elapsed() < Duration::from_secs(2));
                        let response = response
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .on_hover_text(if copied { "✓" } else { t.info_copy });
                        if response.clicked() {
                            ui.ctx().copy_text(addr.clone());
                            view.copied = Some(Instant::now());
                        }
                    }
                }
            });
        if !open {
            self.info_view = None;
            self.send(Command::ConnectionInfo { client: None });
        }
    }
}

/// Discord-style volume slider, 0 to `max` (400% for people, 200% for a
/// stream), with a mark at 100% and snapping near
/// it. Keyboard: arrows change by 5%. Returns true when the value changed.
pub(super) fn volume_slider(ui: &mut egui::Ui, value: &mut f32, max: f32) -> bool {
    let height = 22.0;
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click_and_drag());
    response.widget_info(|| egui::WidgetInfo::slider(true, f64::from(*value * 100.0), "Volume"));
    let track = Rect::from_center_size(rect.center(), vec2(rect.width() - 14.0, 6.0));
    let to_x = |v: f32| track.left() + track.width() * (v / max);
    let before = *value;

    if let Some(pointer) = response.interact_pointer_pos() {
        if response.dragged() || response.clicked() {
            let mut v = ((pointer.x - track.left()) / track.width()).clamp(0.0, 1.0) * max;
            if (v - 1.0).abs() < SNAP {
                v = 1.0;
            }
            *value = (v * 100.0).round() / 100.0;
        }
    }
    if response.has_focus() {
        let step = ui.input(|i| {
            if i.key_pressed(egui::Key::ArrowRight) || i.key_pressed(egui::Key::ArrowUp) {
                0.05
            } else if i.key_pressed(egui::Key::ArrowLeft) || i.key_pressed(egui::Key::ArrowDown) {
                -0.05
            } else {
                0.0
            }
        });
        if step != 0.0 {
            *value = ((*value + step) * 100.0).round().clamp(0.0, max * 100.0) / 100.0;
        }
    }

    let painter = ui.painter();
    painter.rect_filled(track, CornerRadius::same(3), theme::SURFACE_HOVER);
    // Up to 100% in the normal ink, boost above it in green.
    let normal_end = to_x(value.min(1.0));
    painter.rect_filled(
        Rect::from_min_max(track.left_top(), pos2(normal_end, track.bottom())),
        CornerRadius::same(3),
        theme::TEXT,
    );
    if *value > 1.0 {
        painter.rect_filled(
            Rect::from_min_max(
                pos2(to_x(1.0), track.top()),
                pos2(to_x(*value), track.bottom()),
            ),
            CornerRadius::same(3),
            theme::SUCCESS,
        );
    }
    let mark = to_x(1.0);
    painter.line_segment(
        [
            pos2(mark, track.top() - 4.0),
            pos2(mark, track.bottom() + 4.0),
        ],
        Stroke::new(1.5, theme::TEXT_3),
    );
    let knob = pos2(to_x(*value), track.center().y);
    let hovered = response.hovered() || response.dragged();
    painter.circle_filled(knob, if hovered { 8.0 } else { 7.0 }, Color32::WHITE);
    painter.circle_stroke(knob, 7.0, Stroke::new(1.0, theme::BORDER));
    if response.has_focus() {
        theme::focus_ring(painter, rect);
    }
    let changed = (*value - before).abs() > f32::EPSILON;
    if changed {
        response.clone().mark_changed();
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand);
    changed
}

/// The volume percentage: a label that turns into a text field when clicked.
/// Enter or clicking away applies the typed number (clamped to 0–400%);
/// Escape cancels. Returns true when the volume changed.
fn percent_field(ui: &mut egui::Ui, client: u16, volume: &mut f32, color: Color32) -> bool {
    let id = egui::Id::unique(("volume-percent", client));
    let font = FontId::new(13.0, egui::FontFamily::Monospace);
    let editing: Option<String> = ui.data(|d| d.get_temp(id));
    let Some(mut text) = editing else {
        let label = ui
            .add(
                egui::Label::new(
                    RichText::new(format!("{:.0}%", *volume * 100.0))
                        .font(font)
                        .color(color),
                )
                .sense(Sense::click()),
            )
            .on_hover_cursor(egui::CursorIcon::Text);
        if label.clicked() {
            let start = format!("{:.0}", *volume * 100.0);
            ui.data_mut(|d| d.insert_temp(id, start));
        }
        return false;
    };

    // One compact pill: the number and its % sign inside the same box, with
    // an accent outline while editing.
    let response = egui::Frame::new()
        .fill(theme::SURFACE_RAISED)
        .stroke(Stroke::new(1.0, theme::TEXT_3))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(6, 1))
        .show(ui, |ui| {
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                ui.spacing_mut().interact_size.y = 20.0;
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut text)
                        .id_salt(("volume-percent-edit", client))
                        .font(font.clone())
                        .char_limit(3)
                        .frame(egui::Frame::NONE)
                        .align(Align2::RIGHT_CENTER)
                        .desired_width(28.0),
                );
                ui.label(RichText::new("%").font(font).color(theme::TEXT_2));
                edit
            })
            .inner
        })
        .inner;
    // Digits only, so the field can never hold something unparsable.
    text.retain(|c| c.is_ascii_digit());
    if !response.has_focus() && !response.lost_focus() {
        response.request_focus();
    }

    let cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));
    if cancel {
        ui.data_mut(|d| d.remove::<String>(id));
        return false;
    }
    if response.lost_focus() {
        ui.data_mut(|d| d.remove::<String>(id));
        return match text.parse::<f32>() {
            Ok(percent) => {
                let value = (percent.min(MAX_PEER_VOLUME * 100.0) / 100.0).max(0.0);
                let changed = (value - *volume).abs() > f32::EPSILON;
                *volume = value;
                changed
            }
            Err(_) => false,
        };
    }
    ui.data_mut(|d| d.insert_temp(id, text));
    false
}

/// Full-width menu row with a leading icon.
pub(super) fn menu_item(ui: &mut egui::Ui, glyph: &str, label: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    let painter = ui.painter();
    if response.hovered() || response.has_focus() {
        painter.rect_filled(rect, CornerRadius::same(6), theme::SURFACE_HOVER);
    }
    painter.text(
        rect.left_center() + vec2(12.0, 0.0),
        Align2::LEFT_CENTER,
        glyph,
        FontId::new(16.0, theme::icons()),
        theme::TEXT_2,
    );
    painter.text(
        rect.left_center() + vec2(38.0, 0.0),
        Align2::LEFT_CENTER,
        label,
        FontId::new(14.0, theme::medium()),
        theme::TEXT,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Small rounded action with an icon, used under the volume slider.
fn chip_button(ui: &mut egui::Ui, glyph: &str, label: &str, active: bool) -> egui::Response {
    let enabled = ui.is_enabled();
    let font = FontId::new(12.5, theme::medium());
    let text = ui
        .painter()
        .layout_no_wrap(label.to_string(), font, theme::TEXT);
    let size = vec2(text.size().x + 38.0, 28.0);
    let (rect, response) = ui.allocate_exact_size(
        size,
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));
    let (fill, ink) = if !enabled {
        (theme::SURFACE, theme::TEXT_3)
    } else if active {
        (theme::DANGER_SOFT, theme::DANGER)
    } else if response.hovered() {
        (theme::SURFACE_HOVER, theme::TEXT)
    } else {
        (theme::SURFACE_RAISED, theme::TEXT_2)
    };
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(6), fill);
    painter.text(
        rect.left_center() + vec2(15.0, 0.0),
        Align2::CENTER_CENTER,
        glyph,
        FontId::new(14.0, theme::icons()),
        ink,
    );
    painter.galley(
        pos2(rect.left() + 28.0, rect.center().y - text.size().y / 2.0),
        text,
        ink,
    );
    if response.has_focus() {
        theme::focus_ring(painter, rect);
    }
    if enabled {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        response
    }
}

/// Values of a sparkline and the color for its latest value.
/// Small area graph; the newest value is on the right. Missing samples (NaN)
/// leave gaps. The scale spans the recent range with some headroom, so a
/// steady value sits mid-height instead of hugging the top or the floor.
pub(super) fn sparkline(
    painter: &egui::Painter,
    area: Rect,
    values: &[f32],
    color: fn(f32) -> Color32,
) {
    painter.line_segment(
        [area.left_bottom(), area.right_bottom()],
        Stroke::new(1.0, theme::BORDER_SOFT),
    );
    let known = || values.iter().copied().filter(|v| v.is_finite());
    let Some(last) = values.iter().rev().copied().find(|v| v.is_finite()) else {
        return;
    };
    let max = known().fold(f32::MIN, f32::max);
    let min = known().fold(f32::MAX, f32::min);
    let top = (max * 1.5).max(min + 1e-3).max(f32::EPSILON);
    let step = area.width() / (values.len().clamp(20, HISTORY) - 1) as f32;
    let start = area.right() - step * (values.len().saturating_sub(1)) as f32;
    let ink = color(last);
    let y = |v: f32| area.bottom() - (v / top).clamp(0.0, 1.0) * area.height();
    let mut run: Vec<egui::Pos2> = Vec::new();
    let flush = |run: &mut Vec<egui::Pos2>| {
        if run.len() >= 2 {
            // Filled column by column: egui only fills convex polygons.
            for pair in run.windows(2) {
                painter.add(egui::Shape::convex_polygon(
                    vec![
                        pair[0],
                        pair[1],
                        pos2(pair[1].x, area.bottom()),
                        pos2(pair[0].x, area.bottom()),
                    ],
                    ink.gamma_multiply(0.14),
                    Stroke::NONE,
                ));
            }
            painter.add(egui::Shape::line(run.clone(), Stroke::new(1.5, ink)));
        } else if let [point] = run[..] {
            painter.circle_filled(point, 2.0, ink);
        }
        run.clear();
    };
    for (i, v) in values.iter().enumerate() {
        if v.is_finite() {
            run.push(pos2(start + step * i as f32, y(*v)));
        } else {
            flush(&mut run);
        }
    }
    flush(&mut run);
}

fn loss_color(loss: f32) -> Color32 {
    if loss < 0.01 {
        theme::SUCCESS
    } else if loss < 0.05 {
        theme::WARNING
    } else {
        theme::DANGER
    }
}

pub(super) fn ping_color(ping_ms: f32) -> Color32 {
    if ping_ms < 80.0 {
        theme::SUCCESS
    } else if ping_ms < 160.0 {
        theme::WARNING
    } else {
        theme::DANGER
    }
}

/// `HH:MM:SS`.
fn clock(secs: u64) -> String {
    format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

fn bytes(value: u64) -> String {
    let v = value as f64;
    if v >= 1024.0 * 1024.0 {
        format!("{:.2} MiB", v / (1024.0 * 1024.0))
    } else if v >= 1024.0 {
        format!("{:.2} KiB", v / 1024.0)
    } else {
        format!("{value} B")
    }
}

/// Poke, kick and ban entries of a participant's menu; the chosen one opens its dialog.
fn moderation_items(ui: &mut egui::Ui, client: &ClientInfo) -> Option<super::admin::AdminDialog> {
    use super::admin::AdminDialog;
    let name = client.name.clone();
    let id = client.id;
    let mut chosen = None;
    if menu_item(ui, icon::ARROW_RIGHT, "Cutucar").clicked() {
        chosen = Some(AdminDialog::Poke {
            client: id,
            name: name.clone(),
            text: String::new(),
        });
        ui.close();
    }
    if menu_item(ui, icon::X, "Expulsar do canal").clicked() {
        chosen = Some(AdminDialog::Kick {
            client: id,
            name: name.clone(),
            from_server: false,
            reason: String::new(),
        });
        ui.close();
    }
    if menu_item(ui, icon::X, "Expulsar do servidor").clicked() {
        chosen = Some(AdminDialog::Kick {
            client: id,
            name: name.clone(),
            from_server: true,
            reason: String::new(),
        });
        ui.close();
    }
    if menu_item(ui, icon::TRASH, "Banir").clicked() {
        chosen = Some(AdminDialog::Ban {
            client: id,
            name,
            choice: 1,
            reason: String::new(),
        });
        ui.close();
    }
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_like_teamspeak() {
        assert_eq!(clock(94), "00:01:34");
        assert_eq!(bytes(222_310), "217.10 KiB");
        assert_eq!(bytes(512), "512 B");
    }
}
