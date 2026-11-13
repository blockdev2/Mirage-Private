//! The updates page of the settings window.
//!
//! One panel says what is installed and what is happening, with a thin line
//! like a horizon and a small sun on it: it travels while the app looks for an
//! update, fills while one downloads, and rests at the end when it is ready.
//! Everything slow runs in the updater's worker; this page only reads its state.

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
                        self.updater.check();
                    }
                }
            }
        }
        if self.updater.state() == UpdateState::Restarting {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    pub(super) fn updates_page(&mut self, ui: &mut Ui) {
        let t = self.t;
        let state = self.updater.state();
        // Looking when the page opens is what people expect; once per opening.
        if state == UpdateState::Idle && !self.profile_edit.updates_checked {
            self.profile_edit.updates_checked = true;
            self.updater.check();
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
            panel(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new(t.upd_installed)
                                .size(12.5)
                                .color(theme::TEXT_3),
                        );
                        ui.label(
                            RichText::new(self.updater.current_version())
                                .font(FontId::new(40.0, theme::semibold())),
                        );
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        action = main_button(ui, t, &state);
                    });
                });
                ui.add_space(14.0);
                horizon(ui, &state);
                ui.add_space(12.0);
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
            Some(Action::Check) => self.updater.check(),
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

/// The horizon: a hairline across the panel and a small sun.
fn horizon(ui: &mut Ui, state: &UpdateState) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
    let y = rect.center().y;
    let (left, right) = (rect.left() + 6.0, rect.right() - 6.0);
    let span = right - left;
    let time = ui.input(|i| i.time) as f32;
    let (position, fill, color) = match state {
        UpdateState::Checking => {
            // Back and forth until the answer comes.
            ui.ctx().request_repaint();
            ((time * 0.9).sin() * 0.5 + 0.5, 0.0, theme::TEXT)
        }
        UpdateState::Downloading { percent } => {
            let p = f32::from(*percent) / 100.0;
            (p, p, theme::ACCENT)
        }
        UpdateState::Ready(_) | UpdateState::Restarting => (1.0, 1.0, theme::SUCCESS),
        UpdateState::UpToDate => (1.0, 0.0, theme::SUCCESS),
        UpdateState::Available(_) => (0.0, 0.0, theme::WARNING),
        _ => (0.0, 0.0, theme::TEXT_3),
    };
    let painter = ui.painter();
    painter.line_segment(
        [pos2(left, y), pos2(right, y)],
        Stroke::new(1.5, theme::BORDER),
    );
    if fill > 0.0 {
        painter.line_segment(
            [pos2(left, y), pos2(left + span * fill, y)],
            Stroke::new(2.0, color),
        );
    }
    let sun = pos2(left + span * position, y);
    painter.circle_filled(sun, 11.0, color.gamma_multiply(0.12));
    painter.circle_filled(sun, 6.0, color);
}
