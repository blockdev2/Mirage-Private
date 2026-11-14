//! Sharing our own screen: the button under the participants, the picker of
//! monitors and windows it opens, and the state the bridge reports back.

use super::App;
use crate::protocol::{Command, ShareState};
use crate::strings::Strings;
use crate::theme::{self, icon};

/// Where our own share stands, as far as the bridge has told us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Sharing {
    Starting,
    Live { viewers: usize },
}

/// One monitor in the picker.
pub(super) struct MonitorChoice {
    pub index: usize,
    pub label: String,
}

/// Bridge names of the quality presets, in the order of `share_quality_levels`.
pub(super) const QUALITIES: [&str; 4] = ["light", "standard", "high", "max"];
/// "standard": what a share used before there was a choice.
pub(super) const DEFAULT_QUALITY: usize = 1;

/// One window in the picker.
pub(super) struct WindowChoice {
    pub id: i64,
    pub label: String,
}

/// Longest window title shown, in characters; the rest is cut with an ellipsis.
const WINDOW_TITLE_MAX: usize = 44;

/// "Title · app.exe · 1280×720", with a long title shortened.
pub(super) fn window_label(title: &str, process: &str, width: u32, height: u32) -> String {
    let title = title.trim();
    let mut shown: String = title.chars().take(WINDOW_TITLE_MAX).collect();
    if title.chars().count() > WINDOW_TITLE_MAX {
        shown.push('…');
    }
    if process.is_empty() {
        format!("{shown} · {width}×{height}")
    } else {
        format!("{shown} · {process} · {width}×{height}")
    }
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

/// "Name · 1920×1080 (principal)".
pub(super) fn monitor_label(
    t: &Strings,
    name: &str,
    width: u32,
    height: u32,
    primary: bool,
) -> String {
    let mut label = format!("{name} · {width}×{height}");
    if primary {
        label.push(' ');
        label.push_str(t.share_primary);
    }
    label
}

#[cfg(windows)]
fn list_monitors(t: &Strings) -> Vec<MonitorChoice> {
    ts6_bridge::share_media::monitors()
        .into_iter()
        .map(|m| MonitorChoice {
            index: m.index,
            label: monitor_label(t, &m.name, m.width, m.height, m.primary),
        })
        .collect()
}

#[cfg(not(windows))]
fn list_monitors(_t: &Strings) -> Vec<MonitorChoice> {
    Vec::new()
}

#[cfg(windows)]
fn list_windows() -> Vec<WindowChoice> {
    ts6_bridge::share_media::windows()
        .into_iter()
        .map(|w| WindowChoice {
            id: w.id,
            label: window_label(&w.title, &w.process, w.width, w.height),
        })
        .collect()
}

#[cfg(not(windows))]
fn list_windows() -> Vec<WindowChoice> {
    Vec::new()
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
                // Stopped by us clears the state first; reaching here with a
                // share still on means the server or the disconnect ended it.
                if self.sharing.take().is_some() {
                    self.chat_notice = Some(t.share_ended);
                }
            }
            "failed" => {
                self.sharing = None;
                self.chat_notice = Some(failure_text(t, update.reason.as_deref()));
            }
            _ => {}
        }
    }

    pub(super) fn stop_sharing(&mut self) {
        if self.sharing.take().is_some() {
            self.send(Command::ShareStop);
        }
    }

    /// The share button: stops a running share, or opens the monitor picker.
    pub(super) fn share_button(&mut self, ui: &mut egui::Ui, size: f32) {
        let t = self.t;
        match self.sharing {
            Some(state) => {
                let label = match state {
                    Sharing::Starting => t.share_starting.to_owned(),
                    Sharing::Live { viewers: 0 } => t.share_stop.to_owned(),
                    Sharing::Live { viewers } => {
                        format!("{} ({viewers} {})", t.share_stop, t.share_watching)
                    }
                };
                if theme::icon_button(ui, icon::SHARE_SCREEN, &label, true, size).clicked() {
                    self.stop_sharing();
                }
            }
            None => {
                let response =
                    theme::icon_button(ui, icon::SHARE_SCREEN, t.share_screen, false, size);
                if response.clicked() {
                    // Monitors change (docks, cables): look again each time.
                    self.share_monitors = list_monitors(t);
                    self.share_windows = list_windows();
                }
                // (monitor index, window handle): exactly one is set.
                let mut chosen: Option<(Option<usize>, Option<i64>)> = None;
                // Picking a quality must not close the picker; picking what to
                // share starts the share, which replaces this button anyway.
                egui::Popup::from_toggle_button_response(&response)
                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                    .show(|ui| {
                        ui.set_min_width(260.0);
                        ui.label(egui::RichText::new(t.share_quality).color(theme::TEXT_2));
                        for (index, label) in t.share_quality_levels.iter().enumerate() {
                            ui.selectable_value(&mut self.share_quality, index, *label);
                        }
                        ui.add_space(4.0);
                        ui.checkbox(&mut self.share_audio, t.share_audio)
                            .on_hover_text(t.share_audio_hint);
                        ui.add_space(6.0);
                        ui.separator();
                        ui.label(egui::RichText::new(t.share_pick).color(theme::TEXT_2));
                        egui::ScrollArea::vertical()
                            .max_height(360.0)
                            .show(ui, |ui| {
                                ui.label(egui::RichText::new(t.share_screens).color(theme::TEXT_3));
                                if self.share_monitors.is_empty() {
                                    ui.label(t.share_no_monitor);
                                }
                                for monitor in &self.share_monitors {
                                    if ui.button(&monitor.label).clicked() {
                                        chosen = Some((Some(monitor.index), None));
                                    }
                                }
                                ui.add_space(6.0);
                                ui.label(egui::RichText::new(t.share_windows).color(theme::TEXT_3));
                                if self.share_windows.is_empty() {
                                    ui.label(t.share_no_window);
                                }
                                for window in &self.share_windows {
                                    if ui.button(&window.label).clicked() {
                                        chosen = Some((None, Some(window.id)));
                                    }
                                }
                            });
                    });
                if let Some((monitor, window)) = chosen {
                    self.sharing = Some(Sharing::Starting);
                    self.send(Command::ShareStart {
                        monitor,
                        window,
                        quality: QUALITIES.get(self.share_quality).copied(),
                        audio: self.share_audio,
                    });
                }
            }
        }
    }
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
    }

    #[test]
    fn window_labels_cut_long_titles_and_keep_the_app_and_size() {
        assert_eq!(
            window_label("Notas", "notepad.exe", 800, 600),
            "Notas · notepad.exe · 800×600"
        );
        assert_eq!(window_label(" Sem app ", "", 640, 480), "Sem app · 640×480");
        let long = "á".repeat(60);
        let label = window_label(&long, "app.exe", 1, 1);
        assert!(label.starts_with(&"á".repeat(44)));
        assert!(label.contains('…'));
        assert!(!label.contains(&"á".repeat(45)));
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
    fn monitor_labels_name_size_and_primary() {
        assert_eq!(
            monitor_label(&PT, "Dell U2720Q", 3840, 2160, true),
            "Dell U2720Q · 3840×2160 (principal)"
        );
        assert_eq!(
            monitor_label(&PT, "DISPLAY2", 1920, 1080, false),
            "DISPLAY2 · 1920×1080"
        );
    }
}
