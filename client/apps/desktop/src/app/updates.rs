//! The updates page of the settings window.
//!
//! One panel: the app's icon and version, a chip with where things stand, when
//! it last looked, and the one button that moves things forward. A progress
//! track appears only while something is happening: a sweeping segment while
//! it looks, a filling bar while it downloads. Everything slow runs in the
//! updater's worker; this page only reads its state.

use egui::{
    pos2, vec2, Align, CornerRadius, FontId, Frame, Layout, Margin, RichText, Sense, Stroke, Ui,
};

use super::settings::{group, setting_row};
use super::App;
use crate::theme::{self, Tone};
use crate::updater::{Release, UpdateState, DEFAULT_SOURCE};

/// What a click on the page's main button should do.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Check,
    Download,
    Restart,
    OpenDownloads,
}

impl App {
    /// A newer version has been found (or downloaded) and not yet installed.
    pub(super) fn update_waiting(&self) -> bool {
        matches!(
            self.updater.state(),
            UpdateState::Available(_) | UpdateState::Ready(_)
        )
    }

    /// Once per frame: the one automatic check a few seconds after start-up,
    /// and closing the window when the updater is about to relaunch the app.
    pub(super) fn update_housekeeping(&mut self, ctx: &egui::Context) {
        const START_UP_DELAY: std::time::Duration = std::time::Duration::from_secs(4);
        if !self.startup_update_check {
            match START_UP_DELAY.checked_sub(self.opened_at.elapsed()) {
                Some(wait) => ctx.request_repaint_after(wait),
                None => {
                    self.startup_update_check = true;
                    if !self.store.data.skip_update_check {
                        self.check_updates();
                    }
                }
            }
        } else if !self.store.data.skip_update_check
            && crate::store::now_secs().saturating_sub(self.store.data.last_update_check)
                >= DAILY_CHECK_SECS
            && matches!(
                self.updater.state(),
                UpdateState::Idle | UpdateState::UpToDate | UpdateState::Failed(_)
            )
        {
            // Left open for days: look again once a day.
            self.check_updates();
        }
        if self.updater.state() == UpdateState::Restarting {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// Looks for an update and remembers when, for the daily check and the page.
    pub(super) fn check_updates(&mut self) {
        self.updater.check();
        self.store.data.last_update_check = crate::store::now_secs();
        self.store.save();
    }

    pub(super) fn updates_page(&mut self, ui: &mut Ui) {
        let t = self.t;
        let state = self.updater.state();
        // Looking when the page opens is what people expect; once per opening.
        if state == UpdateState::Idle && !self.profile_edit.updates_checked {
            self.profile_edit.updates_checked = true;
            self.check_updates();
        }
        let mut action = None;
        ui.add_space(14.0);

        if state == UpdateState::Unavailable {
            panel(ui, |ui| {
                ui.label(
                    RichText::new(t.upd_portable_title).font(FontId::new(18.0, theme::semibold())),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(t.upd_portable_body)
                        .size(13.5)
                        .color(theme::TEXT_2),
                );
                ui.add_space(14.0);
                if theme::button(ui, t.upd_get_installer, None, Tone::Primary, 250.0).clicked() {
                    action = Some(Action::OpenDownloads);
                }
            });
        } else {
            let checked = self.store.data.last_update_check;
            panel(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Image::new(egui::include_image!("../../assets/icon.png"))
                            .fit_to_exact_size(vec2(64.0, 64.0))
                            .corner_radius(14),
                    );
                    ui.add_space(14.0);
                    ui.vertical(|ui| {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("Mirage {}", self.updater.current_version()))
                                    .font(FontId::new(22.0, theme::semibold()))
                                    .color(theme::TEXT),
                            );
                            ui.add_space(6.0);
                            state_chip(ui, t, &state);
                        });
                        ui.add_space(2.0);
                        let when = if checked == 0 {
                            t.upd_never_checked.to_string()
                        } else {
                            t.upd_checked_ago(crate::store::now_secs().saturating_sub(checked))
                        };
                        ui.label(RichText::new(when).size(12.5).color(theme::TEXT_3));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        action = main_button(ui, t, &state);
                    });
                });
                if matches!(
                    state,
                    UpdateState::Checking | UpdateState::Downloading { .. }
                ) {
                    ui.add_space(18.0);
                    progress_track(ui, &state);
                }
                ui.add_space(16.0);
                status_line(ui, t, &state);
            });
        }

        if let UpdateState::Available(release) | UpdateState::Ready(release) = &state {
            release_notes(ui, t, release);
        }
        if matches!(state, UpdateState::Ready(_)) {
            ui.add_space(10.0);
            ui.label(
                RichText::new(t.upd_restart_note)
                    .size(13.0)
                    .color(theme::TEXT_3),
            );
        }

        group(ui, t.group_update_prefs, |ui| {
            setting_row(ui, t.upd_auto, Some(t.upd_auto_hint), |ui| {
                let mut on = !self.store.data.skip_update_check;
                if theme::toggle(ui, &mut on, t.upd_auto).changed() {
                    self.store.data.skip_update_check = !on;
                    self.store.save();
                }
            });
            setting_row(ui, t.upd_source, None, |ui| {
                ui.label(RichText::new(t.upd_source_value).color(theme::TEXT_2));
            });
        });

        match action {
            Some(Action::Check) => self.check_updates(),
            Some(Action::Download) => self.updater.download(),
            Some(Action::Restart) => self.updater.apply(),
            Some(Action::OpenDownloads) => ui.ctx().open_url(egui::OpenUrl::new_tab(format!(
                "{DEFAULT_SOURCE}/releases/latest"
            ))),
            None => {}
        }
    }
}

/// The big rounded panel the state lives in.
fn panel(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui)) {
    Frame::new()
        .fill(theme::SURFACE)
        .stroke(Stroke::new(1.0, theme::BORDER_SOFT))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(24))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui);
        });
}

/// The one button that moves things forward, or a disabled one while waiting.
fn main_button(ui: &mut Ui, t: &crate::strings::Strings, state: &UpdateState) -> Option<Action> {
    let (label, action, tone, enabled) = match state {
        UpdateState::Idle | UpdateState::UpToDate => (
            t.upd_check_again,
            Some(Action::Check),
            Tone::Secondary,
            true,
        ),
        UpdateState::Checking => (t.upd_checking, None, Tone::Secondary, false),
        UpdateState::Available(_) => (t.upd_download, Some(Action::Download), Tone::Primary, true),
        UpdateState::Downloading { .. } => (t.upd_downloading, None, Tone::Secondary, false),
        UpdateState::Ready(_) => (t.upd_restart, Some(Action::Restart), Tone::Primary, true),
        UpdateState::Restarting => (t.upd_restarting, None, Tone::Secondary, false),
        UpdateState::Failed(_) => (t.upd_retry, Some(Action::Check), Tone::Primary, true),
        UpdateState::Unavailable => return None,
    };
    let mut result = None;
    ui.add_enabled_ui(enabled, |ui| {
        if theme::button(ui, label, None, tone, 210.0).clicked() {
            result = action;
        }
    });
    result
}

/// A dot and one line of what is going on.
fn status_line(ui: &mut Ui, t: &crate::strings::Strings, state: &UpdateState) {
    let (color, text) = match state {
        UpdateState::Idle => (theme::TEXT_3, String::new()),
        UpdateState::Checking => (theme::TEXT_2, t.upd_checking.to_string()),
        UpdateState::UpToDate => (theme::SUCCESS, t.upd_up_to_date.to_string()),
        UpdateState::Available(r) => (theme::WARNING, t.upd_available(&r.version, r.size_bytes)),
        UpdateState::Downloading { percent } => (theme::TEXT_2, t.upd_progress(*percent)),
        UpdateState::Ready(r) => (theme::SUCCESS, t.upd_ready(&r.version)),
        UpdateState::Restarting => (theme::TEXT_2, t.upd_restarting.to_string()),
        UpdateState::Failed(_) => (theme::DANGER, t.upd_failed.to_string()),
        UpdateState::Unavailable => return,
    };
    if text.is_empty() {
        return;
    }
    ui.horizontal(|ui| {
        let (dot, _) = ui.allocate_exact_size(vec2(10.0, 16.0), Sense::hover());
        ui.painter().circle_filled(dot.center(), 3.5, color);
        ui.label(RichText::new(text).size(13.5).color(theme::TEXT));
    });
    if let UpdateState::Failed(detail) = state {
        if !detail.is_empty() {
            ui.label(RichText::new(detail).small().color(theme::TEXT_3));
        }
    }
}

/// The release notes, in a quiet block under the panel.
fn release_notes(ui: &mut Ui, t: &crate::strings::Strings, release: &Release) {
    let notes = release.notes.trim();
    if notes.is_empty() {
        return;
    }
    ui.add_space(18.0);
    ui.label(RichText::new(t.upd_notes).font(FontId::new(15.0, theme::semibold())));
    ui.add_space(6.0);
    let shown: String = notes.chars().take(1200).collect();
    ui.label(RichText::new(shown).size(13.5).color(theme::TEXT_2));
}

/// Seconds between automatic checks while the app stays open.
const DAILY_CHECK_SECS: u64 = 24 * 60 * 60;

/// A small pill naming where things stand.
fn state_chip(ui: &mut Ui, t: &crate::strings::Strings, state: &UpdateState) {
    let (text, ink, fill) = match state {
        UpdateState::UpToDate => (t.upd_chip_current, theme::SUCCESS, theme::SUCCESS_SOFT),
        UpdateState::Checking => (t.upd_chip_checking, theme::TEXT_2, theme::SURFACE_HOVER),
        UpdateState::Available(_) => (t.upd_chip_new, theme::WARNING, theme::WARNING_SOFT),
        UpdateState::Downloading { .. } => {
            (t.upd_chip_downloading, theme::TEXT, theme::SURFACE_HOVER)
        }
        UpdateState::Ready(_) | UpdateState::Restarting => {
            (t.upd_chip_ready, theme::SUCCESS, theme::SUCCESS_SOFT)
        }
        UpdateState::Failed(_) => (t.upd_chip_failed, theme::DANGER, theme::DANGER_SOFT),
        UpdateState::Idle | UpdateState::Unavailable => return,
    };
    theme::chip(ui, text, ink, fill);
}

/// Only while something happens: a segment that sweeps across while it looks
/// (it has no length to show), a bar that fills while it downloads.
fn progress_track(ui: &mut Ui, state: &UpdateState) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 6.0), Sense::hover());
    let painter = ui.painter().with_clip_rect(rect);
    let round = CornerRadius::same(3);
    painter.rect_filled(rect, round, theme::BG);
    match state {
        UpdateState::Checking => {
            ui.ctx().request_repaint();
            let time = ui.input(|i| i.time) as f32;
            // 1.4 s per sweep, eased so it starts and lands softly.
            let phase = (time / 1.4).fract();
            let eased = phase * phase * (3.0 - 2.0 * phase);
            let width = rect.width() * 0.28;
            let left = rect.left() - width + (rect.width() + width) * eased;
            let segment =
                egui::Rect::from_min_size(pos2(left, rect.top()), vec2(width, rect.height()));
            painter.rect_filled(segment, round, theme::ACCENT);
        }
        UpdateState::Downloading { percent } => {
            let p = f32::from(*percent).clamp(0.0, 100.0) / 100.0;
            let filled = egui::Rect::from_min_size(rect.min, vec2(rect.width() * p, rect.height()));
            painter.rect_filled(filled, round, theme::SUCCESS);
        }
        _ => {}
    }
}
