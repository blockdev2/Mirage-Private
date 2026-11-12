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
const HISTORY: usize = 60;
const MENU_WIDTH: f32 = 272.0;
const INFO_WIDTH: f32 = 340.0;
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
            info.ping_ms.unwrap_or(0.0) as f32,
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
                    changed |= volume_slider(ui, &mut volume);
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
            });
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

    /// TeamSpeak-style connection info window for one participant.
    pub(super) fn connection_window(&mut self, ctx: &egui::Context) {
        let t = self.t;
        let Some(view) = self.info_view.as_mut() else {
            return;
        };
        let mut open = true;
        let title = format!("{} · {}", t.user_connection_info, view.name);
        egui::Window::new(title)
            .id(egui::Id::unique("connection-info"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(INFO_WIDTH)
            .show(ctx, |ui| {
                ui.set_width(INFO_WIDTH);
                let Some(info) = view.info.clone() else {
                    ui.label(RichText::new(t.info_waiting).color(theme::TEXT_3));
                    return;
                };
                let percent = |v: f32| format!("{:.1} %", v * 100.0);
                let series =
                    |index: usize| -> Vec<f32> { view.history.iter().map(|s| s[index]).collect() };
                info_row(
                    ui,
                    icon::ARROW_DOWN,
                    t.info_loss_in,
                    &info.loss_incoming.map_or("—".into(), percent),
                    Some((&series(0), loss_color)),
                );
                info_row(
                    ui,
                    icon::ARROW_UP,
                    t.info_loss_out,
                    &percent(info.loss_outgoing),
                    Some((&series(1), loss_color)),
                );
                let ping = match (info.ping_ms, info.ping_deviation_ms) {
                    (Some(p), Some(d)) => format!("{p:.0} ms ± {d:.1}"),
                    (Some(p), None) => format!("{p:.0} ms"),
                    _ => "—".into(),
                };
                info_row(
                    ui,
                    icon::PULSE,
                    t.info_ping,
                    &ping,
                    Some((&series(2), ping_color)),
                );
                ui.add_space(4.0);
                info_row(
                    ui,
                    icon::HOURGLASS,
                    t.info_idle,
                    &clock(info.idle_secs),
                    None,
                );
                info_row(
                    ui,
                    icon::CLOCK,
                    t.info_connected,
                    &info.connected_secs.map_or("—".into(), clock),
                    None,
                );
                info_row(
                    ui,
                    icon::ARROW_DOWN,
                    t.info_bytes_in,
                    &bytes(info.bytes_received),
                    None,
                );
                info_row(
                    ui,
                    icon::ARROW_UP,
                    t.info_bytes_out,
                    &bytes(info.bytes_sent),
                    None,
                );

                ui.add_space(6.0);
                ui.separator();
                ui.label(RichText::new(t.info_address).small().color(theme::TEXT_3));
                ui.horizontal(|ui| match &info.address {
                    Some(address) => {
                        ui.label(
                            RichText::new(address)
                                .font(FontId::new(14.0, egui::FontFamily::Monospace)),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let copied = view
                                .copied
                                .is_some_and(|at| at.elapsed() < Duration::from_secs(2));
                            let glyph = if copied { icon::CHECK } else { icon::COPY };
                            if theme::icon_button(ui, glyph, t.info_copy, false, 28.0).clicked() {
                                ui.ctx().copy_text(address.clone());
                                view.copied = Some(Instant::now());
                            }
                        });
                    }
                    None => {
                        ui.label(RichText::new(t.info_hidden).color(theme::TEXT_3));
                    }
                });
            });
        if !open {
            self.info_view = None;
            self.send(Command::ConnectionInfo { client: None });
        }
    }
}

/// Discord-style volume slider, 0–400%, with a mark at 100% and snapping near
/// it. Keyboard: arrows change by 5%. Returns true when the value changed.
fn volume_slider(ui: &mut egui::Ui, value: &mut f32) -> bool {
    let height = 22.0;
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click_and_drag());
    response.widget_info(|| egui::WidgetInfo::slider(true, f64::from(*value * 100.0), "Volume"));
    let track = Rect::from_center_size(rect.center(), vec2(rect.width() - 14.0, 6.0));
    let to_x = |v: f32| track.left() + track.width() * (v / MAX_PEER_VOLUME);
    let before = *value;

    if let Some(pointer) = response.interact_pointer_pos() {
        if response.dragged() || response.clicked() {
            let mut v =
                ((pointer.x - track.left()) / track.width()).clamp(0.0, 1.0) * MAX_PEER_VOLUME;
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
            *value = ((*value + step) * 100.0)
                .round()
                .clamp(0.0, MAX_PEER_VOLUME * 100.0)
                / 100.0;
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
fn menu_item(ui: &mut egui::Ui, glyph: &str, label: &str) -> egui::Response {
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
type Spark<'a> = (&'a [f32], fn(f32) -> Color32);

/// One statistic: icon, label, value, and optionally a sparkline under it.
fn info_row(ui: &mut egui::Ui, glyph: &str, label: &str, value: &str, spark: Option<Spark<'_>>) {
    let height = if spark.is_some() { 44.0 } else { 26.0 };
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let painter = ui.painter();
    let line_y = rect.top() + 12.0;
    painter.text(
        pos2(rect.left() + 8.0, line_y),
        Align2::CENTER_CENTER,
        glyph,
        FontId::new(14.0, theme::icons()),
        theme::TEXT_3,
    );
    painter.text(
        pos2(rect.left() + 24.0, line_y),
        Align2::LEFT_CENTER,
        label,
        FontId::new(13.0, egui::FontFamily::Proportional),
        theme::TEXT_2,
    );
    painter.text(
        pos2(rect.right(), line_y),
        Align2::RIGHT_CENTER,
        value,
        FontId::new(13.0, egui::FontFamily::Monospace),
        theme::TEXT,
    );
    if let Some((values, color)) = spark {
        let area = Rect::from_min_max(
            pos2(rect.left() + 24.0, rect.top() + 26.0),
            pos2(rect.right(), rect.bottom() - 4.0),
        );
        sparkline(painter, area, values, color);
    }
}

/// Small line graph; the newest value is on the right.
fn sparkline(painter: &egui::Painter, area: Rect, values: &[f32], color: fn(f32) -> Color32) {
    painter.line_segment(
        [area.left_bottom(), area.right_bottom()],
        Stroke::new(1.0, theme::BORDER_SOFT),
    );
    if values.len() < 2 {
        return;
    }
    let max = values
        .iter()
        .copied()
        .fold(0.0f32, f32::max)
        .max(f32::EPSILON);
    let step = area.width() / (HISTORY - 1) as f32;
    let start = area.right() - step * (values.len() - 1) as f32;
    let points: Vec<_> = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            pos2(
                start + step * i as f32,
                area.bottom() - (v / max).clamp(0.0, 1.0) * area.height(),
            )
        })
        .collect();
    let last = *values.last().unwrap_or(&0.0);
    painter.add(egui::Shape::line(points, Stroke::new(1.5, color(last))));
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

fn ping_color(ping_ms: f32) -> Color32 {
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
