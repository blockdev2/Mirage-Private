//! Sharing our own screen: the button under the participants, the picker
//! dialog (screens and windows with live previews, quality, sound), the panel
//! the button opens while sharing, and the state the bridge reports back.

use super::App;
use crate::protocol::{Command, ShareState};
use crate::strings::Strings;
use crate::theme::{self, icon};
use egui::{pos2, vec2, Align2, Color32, CornerRadius, FontId, Rect, Sense, Stroke};
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

/// Where our own share stands, as far as the bridge has told us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Sharing {
    Starting,
    Live { viewers: usize },
}

/// What a share captures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Source {
    Monitor(usize),
    Window(i64),
}

/// One screen or window in the picker.
pub(super) struct Choice {
    pub source: Source,
    pub title: String,
    pub detail: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Tab {
    Screens,
    Windows,
}

/// The open picker dialog.
pub(super) struct Picker {
    tab: Tab,
    monitors: Vec<Choice>,
    windows: Vec<Choice>,
    selected: Option<Source>,
    previews: Previews,
}

/// Bridge names of the quality presets, in the order of `share_quality_levels`.
pub(super) const QUALITIES: [&str; 5] = ["light", "standard", "high", "max", "source"];
/// "standard": what a share used before there was a choice.
pub(super) const DEFAULT_QUALITY: usize = 1;

/// Longest window title shown, in characters; the rest is cut with an ellipsis.
const WINDOW_TITLE_MAX: usize = 44;
/// Previews are taken again this often while they are on screen.
const PREVIEW_EVERY: Duration = Duration::from_secs(2);
/// Preview pictures are at most this big (they are drawn smaller).
const PREVIEW_SIZE: (u32, u32) = (384, 216);
const PICKER_WIDTH: f32 = 640.0;
const LIVE_PANEL_WIDTH: f32 = 320.0;
const COLUMNS: usize = 3;

/// Small still pictures of sources, taken on a helper thread so a slow
/// window never stalls the interface.
#[derive(Default)]
pub(super) struct Previews {
    textures: HashMap<Source, egui::TextureHandle>,
    pending: Option<Receiver<Vec<(Source, egui::ColorImage)>>>,
    taken_at: Option<Instant>,
}

impl Previews {
    /// Picks up finished pictures and starts the next round when due.
    fn refresh(&mut self, ctx: &egui::Context, sources: Vec<Source>) {
        if let Some(pending) = &self.pending {
            match pending.try_recv() {
                Ok(pictures) => {
                    for (source, image) in pictures {
                        let name = format!("share-preview-{source:?}");
                        match self.textures.get_mut(&source) {
                            Some(texture) => texture.set(image, egui::TextureOptions::LINEAR),
                            None => {
                                let texture =
                                    ctx.load_texture(name, image, egui::TextureOptions::LINEAR);
                                self.textures.insert(source, texture);
                            }
                        }
                    }
                    self.pending = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.pending = None,
            }
        }
        let due = self.taken_at.is_none_or(|at| at.elapsed() >= PREVIEW_EVERY);
        if self.pending.is_none() && due && !sources.is_empty() {
            self.taken_at = Some(Instant::now());
            self.pending = take_previews(ctx.clone(), sources);
        }
        ctx.request_repaint_after(Duration::from_millis(250));
    }

    fn get(&self, source: Source) -> Option<&egui::TextureHandle> {
        self.textures.get(&source)
    }
}

#[cfg(windows)]
fn take_previews(
    ctx: egui::Context,
    sources: Vec<Source>,
) -> Option<Receiver<Vec<(Source, egui::ColorImage)>>> {
    use ts6_bridge::share_media::{thumbnail, CaptureSource};
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("share-previews".into())
        .spawn(move || {
            let pictures = sources
                .into_iter()
                .filter_map(|source| {
                    let capture = match source {
                        Source::Monitor(index) => CaptureSource::Monitor(Some(index)),
                        Source::Window(id) => CaptureSource::Window(id),
                    };
                    let picture = thumbnail(capture, PREVIEW_SIZE.0, PREVIEW_SIZE.1)?;
                    let size = [picture.width as usize, picture.height as usize];
                    Some((
                        source,
                        egui::ColorImage::from_rgba_unmultiplied(size, &picture.rgba),
                    ))
                })
                .collect();
            let _ = sender.send(pictures);
            ctx.request_repaint();
        })
        .ok()?;
    Some(receiver)
}

#[cfg(not(windows))]
fn take_previews(
    _ctx: egui::Context,
    _sources: Vec<Source>,
) -> Option<Receiver<Vec<(Source, egui::ColorImage)>>> {
    None
}

/// "Title", shortened when long.
pub(super) fn window_title(title: &str) -> String {
    let title = title.trim();
    let mut shown: String = title.chars().take(WINDOW_TITLE_MAX).collect();
    if title.chars().count() > WINDOW_TITLE_MAX {
        shown.push('…');
    }
    shown
}

/// "app.exe · 1280×720".
pub(super) fn window_detail(process: &str, width: u32, height: u32) -> String {
    if process.is_empty() {
        format!("{width}×{height}")
    } else {
        format!("{process} · {width}×{height}")
    }
}

/// "1920×1080 (principal)".
pub(super) fn monitor_detail(t: &Strings, width: u32, height: u32, primary: bool) -> String {
    let mut detail = format!("{width}×{height}");
    if primary {
        detail.push(' ');
        detail.push_str(t.share_primary);
    }
    detail
}

/// The short name of a quality level ("Padrão" of "Padrão · 720p …").
fn quality_name(label: &str) -> &str {
    label.split(" · ").next().unwrap_or(label)
}

/// The user-facing text for a failure code from the bridge.
pub(super) fn failure_text(t: &Strings, reason: Option<&str>) -> &'static str {
    match reason {
        Some("unsupported") => t.share_unsupported,
        Some("timeout") => t.share_timeout,
        Some("offline") => t.share_offline,
        Some("busy") => t.share_busy,
        _ => t.share_capture,
    }
}

#[cfg(windows)]
fn list_sources(t: &Strings) -> (Vec<Choice>, Vec<Choice>) {
    let monitors = ts6_bridge::share_media::monitors()
        .into_iter()
        .map(|m| Choice {
            source: Source::Monitor(m.index),
            title: m.name.clone(),
            detail: monitor_detail(t, m.width, m.height, m.primary),
        })
        .collect();
    let windows = ts6_bridge::share_media::windows()
        .into_iter()
        .map(|w| Choice {
            source: Source::Window(w.id),
            title: window_title(&w.title),
            detail: window_detail(&w.process, w.width, w.height),
        })
        .collect();
    (monitors, windows)
}

#[cfg(not(windows))]
fn list_sources(_t: &Strings) -> (Vec<Choice>, Vec<Choice>) {
    (Vec::new(), Vec::new())
}

impl Picker {
    fn open(t: &Strings, current: Option<Source>) -> Self {
        // Monitors and windows change (docks, cables, apps): list them anew.
        let (monitors, windows) = list_sources(t);
        let tab = match current {
            Some(Source::Window(_)) => Tab::Windows,
            _ => Tab::Screens,
        };
        let primary = monitors
            .iter()
            .find(|c| c.detail.ends_with(t.share_primary))
            .or(monitors.first())
            .map(|c| c.source);
        Self {
            tab,
            monitors,
            windows,
            selected: current.or(primary),
            previews: Previews::default(),
        }
    }

    fn shown(&self) -> &[Choice] {
        match self.tab {
            Tab::Screens => &self.monitors,
            Tab::Windows => &self.windows,
        }
    }
}

/// A segment of a tab or option row: the app's selected look, not a color.
fn segment(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    let font = FontId::new(13.0, theme::medium());
    let color = if selected { theme::TEXT } else { theme::TEXT_2 };
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, color);
    let (rect, response) = ui.allocate_exact_size(galley.size() + vec2(24.0, 14.0), Sense::click());
    response
        .widget_info(|| egui::WidgetInfo::selected(egui::Role::RadioButton, true, selected, label));
    let fill = if selected {
        theme::ACCENT_SOFT
    } else if response.hovered() {
        theme::SURFACE_HOVER
    } else {
        Color32::TRANSPARENT
    };
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(6), fill);
    if selected {
        painter.rect_stroke(
            rect,
            CornerRadius::same(6),
            Stroke::new(1.0, theme::BORDER),
            egui::StrokeKind::Inside,
        );
    }
    painter.galley(rect.center() - galley.size() / 2.0, galley, color);
    if response.has_focus() {
        theme::focus_ring(painter, rect);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn pill_galley(painter: &egui::Painter, text: &str) -> std::sync::Arc<egui::Galley> {
    painter.layout_no_wrap(
        text.to_owned(),
        FontId::new(11.5, theme::bold()),
        Color32::WHITE,
    )
}

fn pill_size(painter: &egui::Painter, text: &str) -> egui::Vec2 {
    pill_galley(painter, text).size() + vec2(16.0, 8.0)
}

/// A small label drawn over the preview, like the live badges of the cards.
fn overlay_pill(painter: &egui::Painter, at: egui::Pos2, text: &str, fill: Color32) {
    let galley = pill_galley(painter, text);
    let rect = Rect::from_min_size(at, galley.size() + vec2(16.0, 8.0));
    painter.rect_filled(rect, CornerRadius::same(4), fill);
    painter.galley(rect.center() - galley.size() / 2.0, galley, Color32::WHITE);
}

/// One row of equal options on a shared track; returns a newly picked index.
fn segmented(
    ui: &mut egui::Ui,
    width: f32,
    labels: &[&str],
    selected: usize,
    hint: &str,
) -> Option<usize> {
    let (track, _) = ui.allocate_exact_size(vec2(width, 34.0), Sense::hover());
    ui.painter()
        .rect_filled(track, CornerRadius::same(7), theme::BG);
    ui.painter().rect_stroke(
        track,
        CornerRadius::same(7),
        Stroke::new(1.0, theme::BORDER_SOFT),
        egui::StrokeKind::Inside,
    );
    let inner = track.shrink(3.0);
    let step = inner.width() / labels.len().max(1) as f32;
    let mut picked = None;
    for (index, label) in labels.iter().enumerate() {
        let rect = Rect::from_min_size(
            pos2(inner.left() + step * index as f32, inner.top()),
            vec2(step, inner.height()),
        );
        let is_selected = index == selected;
        let response = ui.interact(rect, ui.scope_id().with(("segment", index)), Sense::click());
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::Role::RadioButton, true, is_selected, *label)
        });
        let painter = ui.painter();
        if is_selected {
            painter.rect_filled(rect, CornerRadius::same(5), theme::ACCENT_SOFT);
            painter.rect_stroke(
                rect,
                CornerRadius::same(5),
                Stroke::new(1.0, theme::BORDER),
                egui::StrokeKind::Inside,
            );
        } else if response.hovered() {
            painter.rect_filled(rect, CornerRadius::same(5), theme::SURFACE_RAISED);
        }
        let (color, family) = if is_selected {
            (theme::TEXT, theme::semibold())
        } else {
            (theme::TEXT_2, theme::medium())
        };
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            *label,
            FontId::new(13.0, family),
            color,
        );
        if response.has_focus() {
            theme::focus_ring(painter, rect);
        }
        let response = response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(hint);
        if response.clicked() && !is_selected {
            picked = Some(index);
        }
    }
    picked
}

/// A picture fitted (letterboxed) inside `area`, or a placeholder glyph.
fn paint_preview(
    painter: &egui::Painter,
    area: Rect,
    texture: Option<&egui::TextureHandle>,
    glyph: &str,
) {
    painter.rect_filled(area, CornerRadius::same(6), theme::BG);
    match texture {
        Some(texture) => {
            let size = texture.size_vec2();
            let scale = (area.width() / size.x).min(area.height() / size.y);
            let rect = Rect::from_center_size(area.center(), size * scale);
            painter.image(
                texture.id(),
                rect,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        None => {
            painter.text(
                area.center(),
                Align2::CENTER_CENTER,
                glyph,
                FontId::new(26.0, theme::icons()),
                theme::TEXT_3,
            );
        }
    }
}

/// One card of the grid: preview on top, title and detail under it.
fn source_card(
    ui: &mut egui::Ui,
    choice: &Choice,
    width: f32,
    selected: bool,
    texture: Option<&egui::TextureHandle>,
) -> egui::Response {
    let preview_height = width * 9.0 / 16.0;
    let (rect, response) =
        ui.allocate_exact_size(vec2(width, preview_height + 52.0), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::RadioButton, true, selected, &choice.title)
    });
    let fill = if selected {
        theme::ACCENT_SOFT
    } else if response.hovered() {
        theme::SURFACE_HOVER
    } else {
        theme::SURFACE_RAISED
    };
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(8), fill);
    let stroke = if selected {
        Stroke::new(1.5, theme::ACCENT)
    } else {
        Stroke::new(1.0, theme::BORDER)
    };
    painter.rect_stroke(
        rect,
        CornerRadius::same(8),
        stroke,
        egui::StrokeKind::Inside,
    );
    let inner = rect.shrink(6.0);
    let preview = Rect::from_min_size(inner.min, vec2(inner.width(), preview_height - 6.0));
    paint_preview(painter, preview, texture, icon::SHARE_SCREEN);
    let text_width = inner.width() - 4.0;
    let title = theme::fit_text(
        painter,
        &choice.title,
        FontId::new(13.0, theme::medium()),
        theme::TEXT,
        text_width,
    );
    let detail = theme::fit_text(
        painter,
        &choice.detail,
        FontId::new(12.0, egui::FontFamily::Proportional),
        theme::TEXT_3,
        text_width,
    );
    let text_top = preview.bottom() + 8.0;
    painter.galley(pos2(inner.left() + 2.0, text_top), title, theme::TEXT);
    painter.galley(
        pos2(inner.left() + 2.0, text_top + 19.0),
        detail,
        theme::TEXT_3,
    );
    if response.has_focus() {
        theme::focus_ring(painter, rect);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

impl App {
    /// Applies a report from the bridge about our own share.
    pub(super) fn on_share_state(&mut self, update: ShareState) {
        let t = self.t;
        match update.state.as_str() {
            "starting" => self.sharing = Some(Sharing::Starting),
            "live" => {
                self.sharing = Some(Sharing::Live {
                    viewers: update.viewers,
                })
            }
            "stopped" => {
                if let Some(source) = self.share_restart.take() {
                    // We stopped to change the source or the quality.
                    self.send_share_start(source);
                } else if self.sharing.take().is_some() {
                    // Stopped by us clears the state first; reaching here with
                    // a share still on means the server or a disconnect ended it.
                    self.chat_notice = Some(t.share_ended);
                }
            }
            "failed" => {
                self.sharing = None;
                self.share_restart = None;
                self.chat_notice = Some(failure_text(t, update.reason.as_deref()));
            }
            _ => {}
        }
    }

    pub(super) fn stop_sharing(&mut self) {
        self.share_restart = None;
        if self.sharing.take().is_some() {
            self.send(Command::ShareStop);
        }
    }

    fn send_share_start(&mut self, source: Source) {
        let (monitor, window) = match source {
            Source::Monitor(index) => (Some(index), None),
            Source::Window(id) => (None, Some(id)),
        };
        self.sharing = Some(Sharing::Starting);
        self.share_source = Some(source);
        self.send(Command::ShareStart {
            monitor,
            window,
            quality: QUALITIES.get(self.share_quality).copied(),
            audio: self.share_audio,
        });
    }

    /// Starts sharing `source`; a running share is stopped first and the new
    /// one starts once the bridge confirms the stop.
    fn share(&mut self, source: Source) {
        if self.sharing.is_some() {
            self.share_restart = Some(source);
            self.sharing = None;
            self.send(Command::ShareStop);
        } else {
            self.send_share_start(source);
        }
    }

    /// The share button: opens the picker, or, while sharing, a panel with a
    /// preview, the quality and the stop button.
    pub(super) fn share_button(&mut self, ui: &mut egui::Ui, size: f32) {
        let t = self.t;
        let Some(state) = self.sharing else {
            if theme::icon_button(ui, icon::SHARE_SCREEN, t.share_screen, false, size).clicked() {
                self.share_picker = Some(Picker::open(t, self.share_source));
            }
            return;
        };
        let label = match state {
            Sharing::Starting => t.share_starting.to_owned(),
            Sharing::Live { viewers: 0 } => t.share_live.to_owned(),
            Sharing::Live { viewers } => {
                format!("{} ({viewers} {})", t.share_live, t.share_watching)
            }
        };
        let response = theme::icon_button(ui, icon::SHARE_SCREEN, &label, true, size);
        let mut action = None;
        egui::Popup::from_toggle_button_response(&response)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| action = self.live_panel(ui, state));
        match action {
            Some(LiveAction::Stop) => {
                self.stop_sharing();
                egui::Popup::close_all(ui.ctx());
            }
            Some(LiveAction::Change) => {
                self.share_picker = Some(Picker::open(t, self.share_source));
                egui::Popup::close_all(ui.ctx());
            }
            Some(LiveAction::Quality) => {
                if let Some(quality) = QUALITIES.get(self.share_quality) {
                    self.send(Command::ShareQuality { quality });
                }
            }
            None => {}
        }
    }

    fn live_panel(&mut self, ui: &mut egui::Ui, state: Sharing) -> Option<LiveAction> {
        let t = self.t;
        let mut action = None;
        let width = LIVE_PANEL_WIDTH;
        ui.set_width(width);
        ui.spacing_mut().item_spacing.y = 0.0;
        let sources: Vec<Source> = self.share_source.into_iter().collect();
        self.share_previews.refresh(ui.ctx(), sources);

        // The picture leads: what viewers see, with the live state on it.
        let (area, _) = ui.allocate_exact_size(vec2(width, width * 9.0 / 16.0), Sense::hover());
        let texture = self.share_source.and_then(|s| self.share_previews.get(s));
        let painter = ui.painter();
        paint_preview(painter, area, texture, icon::SHARE_SCREEN);
        let (badge, badge_fill) = match state {
            Sharing::Starting => (t.share_starting_short, theme::SURFACE_HOVER),
            Sharing::Live { .. } => (t.share_live_badge, theme::DANGER),
        };
        overlay_pill(
            painter,
            area.left_top() + vec2(10.0, 10.0),
            badge,
            badge_fill,
        );
        if let Sharing::Live { viewers } = state {
            let text = format!("{viewers} {}", t.share_watching);
            let size = pill_size(painter, &text);
            let at = area.right_bottom() - size - vec2(10.0, 10.0);
            overlay_pill(painter, at, &text, Color32::from_black_alpha(170));
        }

        ui.add_space(12.0);
        let title = if self.share_source_title.is_empty() {
            t.share_screen
        } else {
            self.share_source_title.as_str()
        };
        let galley = theme::fit_text(
            ui.painter(),
            title,
            FontId::new(15.0, theme::semibold()),
            theme::TEXT,
            width,
        );
        let (rect, _) = ui.allocate_exact_size(vec2(width, galley.size().y), Sense::hover());
        ui.painter().galley(rect.min, galley, theme::TEXT);

        ui.add_space(16.0);
        theme::section_label(ui, t.share_quality);
        ui.add_space(6.0);
        let names: Vec<&str> = t
            .share_quality_levels
            .iter()
            .map(|l| quality_name(l))
            .collect();
        if let Some(index) = segmented(
            ui,
            width,
            &names,
            self.share_quality,
            t.share_quality_restart,
        ) {
            self.share_quality = index;
            action = Some(LiveAction::Quality);
        }
        ui.add_space(6.0);
        if let Some(label) = t.share_quality_levels.get(self.share_quality) {
            let detail = label.split_once(" · ").map_or(*label, |(_, rest)| rest);
            ui.label(egui::RichText::new(detail).color(theme::TEXT_3).size(12.0));
        }

        ui.add_space(16.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let half = (width - 8.0) / 2.0;
            if theme::button(ui, t.share_change, None, theme::Tone::Secondary, half).clicked() {
                action = Some(LiveAction::Change);
            }
            if theme::button(ui, t.share_stop_short, None, theme::Tone::Danger, half)
                .on_hover_text(t.share_stop)
                .clicked()
            {
                action = Some(LiveAction::Stop);
            }
        });
        action
    }

    /// The picker dialog, while it is open.
    pub(super) fn share_picker_window(&mut self, ctx: &egui::Context) {
        let t = self.t;
        let Some(picker) = self.share_picker.as_mut() else {
            return;
        };
        let sources: Vec<Source> = picker.shown().iter().map(|c| c.source).collect();
        picker.previews.refresh(ctx, sources);
        let mut close = false;
        let mut start = None;
        let modal = egui::Modal::new(egui::Id::unique("share-picker"))
            .frame(
                theme::card()
                    .fill(theme::SURFACE)
                    .inner_margin(egui::Margin::same(20)),
            )
            .show(ctx, |ui| {
                ui.set_width(PICKER_WIDTH);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(t.share_screen)
                            .font(FontId::new(18.0, theme::semibold()))
                            .color(theme::TEXT),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::glyph_button(ui, icon::X, t.close, 28.0, 0.0).clicked() {
                            close = true;
                        }
                    });
                });
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for (tab, label) in [
                        (Tab::Screens, t.share_screens),
                        (Tab::Windows, t.share_windows),
                    ] {
                        if segment(ui, label, picker.tab == tab).clicked() {
                            picker.tab = tab;
                        }
                    }
                });
                ui.add_space(12.0);

                let gap = 12.0;
                let card_width = (PICKER_WIDTH - gap * (COLUMNS as f32 - 1.0)) / COLUMNS as f32;
                let card_height = card_width * 9.0 / 16.0 + 52.0;
                egui::ScrollArea::vertical()
                    .max_height(card_height * 2.0 + gap + 4.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        let shown = picker.shown();
                        if shown.is_empty() {
                            let empty = match picker.tab {
                                Tab::Screens => t.share_no_monitor,
                                Tab::Windows => t.share_no_window,
                            };
                            ui.add_space(24.0);
                            ui.vertical_centered(|ui| {
                                ui.label(egui::RichText::new(empty).color(theme::TEXT_3))
                            });
                            ui.add_space(24.0);
                        }
                        let mut clicked = None;
                        for row in shown.chunks(COLUMNS) {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = gap;
                                for choice in row {
                                    let selected = picker.selected == Some(choice.source);
                                    let texture = picker.previews.get(choice.source);
                                    let response =
                                        source_card(ui, choice, card_width, selected, texture);
                                    if response.double_clicked() {
                                        start = Some(choice.source);
                                    } else if response.clicked() {
                                        clicked = Some(choice.source);
                                    }
                                }
                            });
                            ui.add_space(gap);
                        }
                        if clicked.is_some() {
                            picker.selected = clicked;
                        }
                    });

                ui.add_space(4.0);
                ui.separator();
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(t.share_quality).color(theme::TEXT_2));
                    ui.add_space(8.0);
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for (index, label) in t.share_quality_levels.iter().enumerate() {
                        if segment(ui, quality_name(label), self.share_quality == index)
                            .on_hover_text(*label)
                            .clicked()
                        {
                            self.share_quality = index;
                        }
                    }
                });
                if let Some(label) = t.share_quality_levels.get(self.share_quality) {
                    ui.label(egui::RichText::new(*label).color(theme::TEXT_3).size(12.0));
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    theme::toggle(ui, &mut self.share_audio, t.share_audio)
                        .on_hover_text(t.share_audio_hint);
                    ui.label(egui::RichText::new(t.share_audio).color(theme::TEXT_2))
                        .on_hover_text(t.share_audio_hint);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let go = ui
                            .add_enabled_ui(picker.selected.is_some(), |ui| {
                                theme::button(ui, t.share_start, None, theme::Tone::Primary, 140.0)
                            })
                            .inner;
                        if go.clicked() {
                            start = picker.selected;
                        }
                        if theme::button(ui, t.cancel, None, theme::Tone::Secondary, 110.0)
                            .clicked()
                        {
                            close = true;
                        }
                    });
                });
            });
        if modal.should_close() {
            close = true;
        }
        if let Some(source) = start {
            if let Some(picker) = self.share_picker.take() {
                self.share_source_title = picker
                    .monitors
                    .iter()
                    .chain(&picker.windows)
                    .find(|c| c.source == source)
                    .map(|c| c.title.clone())
                    .unwrap_or_default();
            }
            self.share(source);
        } else if close {
            self.share_picker = None;
        }
    }
}

enum LiveAction {
    Stop,
    Change,
    Quality,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strings::PT;

    #[test]
    fn quality_names_match_the_bridge_and_the_labels() {
        assert_eq!(QUALITIES.len(), PT.share_quality_levels.len());
        assert_eq!(QUALITIES[DEFAULT_QUALITY], "standard");
        let mut unique = QUALITIES.to_vec();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), QUALITIES.len());
        let names: Vec<&str> = PT
            .share_quality_levels
            .iter()
            .map(|l| quality_name(l))
            .collect();
        assert_eq!(names, ["Leve", "Padrão", "Alta", "Máxima", "Original"]);
    }

    #[test]
    fn window_titles_are_cut_and_details_keep_the_app_and_size() {
        assert_eq!(window_title(" Notas "), "Notas");
        assert_eq!(
            window_detail("notepad.exe", 800, 600),
            "notepad.exe · 800×600"
        );
        assert_eq!(window_detail("", 640, 480), "640×480");
        let long = "á".repeat(60);
        let title = window_title(&long);
        assert!(title.starts_with(&"á".repeat(44)));
        assert!(title.ends_with('…'));
        assert!(!title.contains(&"á".repeat(45)));
    }

    #[test]
    fn every_failure_code_has_its_own_text() {
        let texts: Vec<&str> = ["unsupported", "timeout", "offline", "busy", "capture"]
            .into_iter()
            .map(|code| failure_text(&PT, Some(code)))
            .collect();
        let mut unique = texts.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), texts.len());
        // An unknown code is still a capture problem, never a blank notice.
        assert_eq!(failure_text(&PT, Some("new")), PT.share_capture);
        assert_eq!(failure_text(&PT, None), PT.share_capture);
    }

    #[test]
    fn monitor_details_name_size_and_primary() {
        assert_eq!(
            monitor_detail(&PT, 3840, 2160, true),
            "3840×2160 (principal)"
        );
        assert_eq!(monitor_detail(&PT, 1920, 1080, false), "1920×1080");
    }
}
