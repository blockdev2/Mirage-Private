//! Watching a participant's screen stream: the "AO VIVO" badge on their
//! card, the stream view that takes the participants' area, and its states.
//! Frames come from the embedded bridge's in-process slot, never the socket.

use egui::{
    pos2, vec2, Align, Align2, Color32, CornerRadius, FontId, Layout, Rect, Sense, Stroke,
    UiBuilder,
};
use ts6_bridge::stream_media::frame::frames;
use ts6_bridge::stream_media::{stream_audio, MAX_STREAM_VOLUME};

use super::peer::volume_slider;
use super::App;
use crate::protocol::{ClientInfo, Command, StreamState};
use crate::strings::Strings;
use crate::theme::{self, icon};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WatchState {
    Connecting,
    Playing,
    Failed(&'static str),
}

pub(super) struct Watching {
    pub client: u16,
    pub state: WatchState,
    /// The stream fills the whole window.
    pub fullscreen: bool,
    texture: Option<egui::TextureHandle>,
    /// Sequence of the last frame shown.
    seen: u64,
}

/// The user-facing text for a failure code from the bridge.
pub(super) fn failure_text(t: &Strings, reason: Option<&str>) -> &'static str {
    match reason {
        Some("refused") => t.stream_refused,
        Some("unsupported") => t.stream_unsupported,
        Some("network") => t.stream_network,
        Some("no_decoder") => t.stream_no_decoder,
        _ => t.stream_timeout,
    }
}

/// Repaints the window whenever the bridge publishes a frame.
pub(super) fn install_waker(ctx: &egui::Context) {
    let ctx = ctx.clone();
    frames().set_waker(Box::new(move || ctx.request_repaint()));
}

/// Height of the participants strip under the stream.
const STRIP_HEIGHT: f32 = 96.0;
const STRIP_GAP: f32 = 12.0;

impl App {
    pub(super) fn watch_stream(&mut self, client: u16) {
        if self.watching.as_ref().is_some_and(|w| w.client == client) {
            return;
        }
        self.watching = Some(Watching {
            client,
            state: WatchState::Connecting,
            fullscreen: false,
            texture: None,
            seen: 0,
        });
        self.send(Command::WatchStream { client });
    }

    pub(super) fn stop_watching(&mut self) {
        if self.watching.take().is_some() {
            self.send(Command::StopWatching);
        }
    }

    pub(super) fn on_stream_state(&mut self, update: StreamState) {
        let t = self.t;
        let Some(watching) = self.watching.as_mut().filter(|w| w.client == update.client) else {
            return;
        };
        match update.state.as_str() {
            "connecting" => watching.state = WatchState::Connecting,
            "playing" => watching.state = WatchState::Playing,
            "ended" => {
                self.watching = None;
                self.chat_notice = Some(t.stream_ended);
            }
            "failed" => {
                watching.state = WatchState::Failed(failure_text(t, update.reason.as_deref()));
            }
            _ => {}
        }
    }

    /// Closes the view when the watched person left our channel; the bridge
    /// ends the stream on its own side.
    pub(super) fn check_watched(&mut self) {
        let Some(client) = self.watching.as_ref().map(|w| w.client) else {
            return;
        };
        let present = self.snapshot.as_ref().is_some_and(|snapshot| {
            let own = snapshot
                .clients
                .iter()
                .find(|c| c.is_self)
                .map(|c| c.channel_id);
            snapshot
                .clients
                .iter()
                .any(|c| c.id == client && Some(c.channel_id) == own)
        });
        if !present {
            self.stop_watching();
        }
    }

    /// The stream above a strip of small participant cards.
    pub(super) fn watching_area(
        &mut self,
        ui: &mut egui::Ui,
        participants: &[&ClientInfo],
        room: f32,
    ) {
        let width = ui.available_width();
        let stage_height = (room - STRIP_HEIGHT - STRIP_GAP).max(STRIP_HEIGHT);
        let (stage, _) = ui.allocate_exact_size(vec2(width, stage_height), Sense::hover());
        self.stream_stage(ui, stage);
        ui.add_space(STRIP_GAP);
        self.participants_content(ui, participants, STRIP_HEIGHT, None);
    }

    /// Puts the window in or out of fullscreen to match the stream view.
    pub(super) fn sync_stream_fullscreen(&mut self, ctx: &egui::Context) {
        let wanted = self.watching.as_ref().is_some_and(|w| w.fullscreen);
        if wanted != self.window_fullscreen {
            self.window_fullscreen = wanted;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(wanted));
        }
    }

    pub(super) fn stream_stage(&mut self, ui: &mut egui::Ui, stage: Rect) {
        let t = self.t;
        let name = self
            .watching
            .as_ref()
            .and_then(|w| {
                let snapshot = self.snapshot.as_ref()?;
                snapshot.clients.iter().find(|c| c.id == w.client)
            })
            .map(|c| c.name.clone())
            .unwrap_or_default();
        let Some(watching) = self.watching.as_mut() else {
            return;
        };
        if let Some((sequence, frame)) = frames().latest(watching.seen) {
            watching.seen = sequence;
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [frame.width as usize, frame.height as usize],
                &frame.rgba,
            );
            match &mut watching.texture {
                Some(texture) => texture.set(image, egui::TextureOptions::LINEAR),
                None => {
                    watching.texture = Some(ui.ctx().load_texture(
                        "stream",
                        image,
                        egui::TextureOptions::LINEAR,
                    ))
                }
            }
        }

        let painter = ui.painter().with_clip_rect(stage);
        painter.rect_filled(stage, CornerRadius::same(8), Color32::from_rgb(8, 9, 11));
        match (watching.state, &watching.texture) {
            (WatchState::Failed(text), _) => {
                painter.text(
                    stage.center() - vec2(0.0, 12.0),
                    Align2::CENTER_CENTER,
                    icon::SHARE_SCREEN,
                    FontId::new(32.0, theme::icons()),
                    theme::TEXT_3,
                );
                painter.text(
                    stage.center() + vec2(0.0, 24.0),
                    Align2::CENTER_CENTER,
                    text,
                    FontId::new(14.0, theme::medium()),
                    theme::TEXT_2,
                );
            }
            (_, Some(texture)) => {
                // Contain: the whole screen, letterboxed, never distorted.
                let size = texture.size_vec2();
                let scale = (stage.width() / size.x).min(stage.height() / size.y);
                let shown = Rect::from_center_size(stage.center(), size * scale);
                painter.image(
                    texture.id(),
                    shown,
                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            (_, None) => {
                let spin = ui.input(|i| i.time) as f32 * 4.0;
                let center = stage.center() - vec2(0.0, 12.0);
                for dot in 0..8 {
                    let angle = spin + dot as f32 * std::f32::consts::TAU / 8.0;
                    let alpha = (dot as f32 + 1.0) / 8.0;
                    painter.circle_filled(
                        center + vec2(angle.cos(), angle.sin()) * 14.0,
                        3.0,
                        theme::TEXT_2.gamma_multiply(alpha),
                    );
                }
                painter.text(
                    stage.center() + vec2(0.0, 24.0),
                    Align2::CENTER_CENTER,
                    t.stream_connecting,
                    FontId::new(14.0, theme::medium()),
                    theme::TEXT_2,
                );
                ui.ctx().request_repaint();
            }
        }

        // Double click toggles fullscreen; Esc leaves it. Registered before the
        // buttons so they win where they overlap.
        let stage_response = ui.interact(stage, ui.scope_id().with("stream-stage"), Sense::click());
        let mut toggle_fullscreen = stage_response.double_clicked();
        if watching.fullscreen && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            toggle_fullscreen = true;
        }
        let fullscreen = watching.fullscreen;

        // Who is live, top-left.
        let badge = live_badge(&painter, stage.left_top() + vec2(12.0, 12.0), t.live_badge);
        painter.text(
            pos2(badge.right() + 8.0, badge.center().y),
            Align2::LEFT_CENTER,
            &name,
            FontId::new(13.0, theme::semibold()),
            Color32::WHITE,
        );

        // Close, top-right.
        let close = Rect::from_min_size(stage.right_top() + vec2(-44.0, 8.0), vec2(36.0, 36.0));
        painter.rect(
            close,
            CornerRadius::same(8),
            Color32::from_black_alpha(140),
            Stroke::NONE,
            egui::StrokeKind::Inside,
        );
        let mut tools = ui.new_child(UiBuilder::new().max_rect(close));
        if theme::glyph_button(&mut tools, icon::X, t.stop_watching, 36.0, 0.0)
            .on_hover_text(t.stop_watching)
            .clicked()
        {
            self.stop_watching();
            return;
        }

        // Volume, mute and fullscreen, bottom-right.
        let sink = stream_audio();
        let bar = Rect::from_min_size(
            stage.right_bottom() - vec2(8.0 + 216.0, 8.0 + 40.0),
            vec2(216.0, 40.0),
        );
        painter.rect(
            bar,
            CornerRadius::same(8),
            Color32::from_black_alpha(140),
            Stroke::NONE,
            egui::StrokeKind::Inside,
        );
        let mut controls = ui.new_child(
            UiBuilder::new()
                .max_rect(bar.shrink2(vec2(4.0, 2.0)))
                .layout(Layout::left_to_right(Align::Center)),
        );
        let (muted, mut volume) = (sink.muted(), sink.volume());
        let glyph = if muted || volume <= 0.0 {
            icon::SPEAKER_SLASH
        } else if volume < 0.5 {
            icon::SPEAKER_LOW
        } else {
            icon::SPEAKER
        };
        let label = if muted {
            t.stream_unmute
        } else {
            t.stream_mute
        };
        if theme::glyph_button(&mut controls, glyph, label, 36.0, 0.0)
            .on_hover_text(label)
            .clicked()
        {
            sink.set_muted(!muted);
        }
        controls.allocate_ui(vec2(104.0, 36.0), |ui| {
            ui.centered_and_justified(|ui| {
                if volume_slider(ui, &mut volume, MAX_STREAM_VOLUME) {
                    sink.set_volume(volume);
                    sink.set_muted(false);
                }
            })
            .response
            .on_hover_text(t.stream_volume);
        });
        let label = if fullscreen {
            t.fullscreen_exit
        } else {
            t.fullscreen_enter
        };
        if fullscreen_button(&mut controls, fullscreen, label)
            .on_hover_text(label)
            .clicked()
        {
            toggle_fullscreen = true;
        }
        if toggle_fullscreen {
            if let Some(watching) = self.watching.as_mut() {
                watching.fullscreen = !watching.fullscreen;
            }
        }
    }
}

/// Four corner brackets: pointing outward to enter fullscreen, inward to leave.
fn fullscreen_button(ui: &mut egui::Ui, leave: bool, label: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    let ink = if response.hovered() || response.has_focus() {
        Color32::WHITE
    } else {
        theme::TEXT_3
    };
    let stroke = Stroke::new(1.6, ink);
    let (edge, arm) = if leave { (3.0, 5.0) } else { (8.0, 5.0) };
    // Entering: brackets sit at the outer corners with arms toward the center.
    // Leaving: brackets sit near the center with arms pointing out.
    let direction = if leave { 1.0 } else { -1.0 };
    for (sx, sy) in [(-1.0f32, -1.0f32), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
        let corner = rect.center() + vec2(sx * edge, sy * edge);
        let horizontal = corner + vec2(sx * arm * direction, 0.0);
        let vertical = corner + vec2(0.0, sy * arm * direction);
        ui.painter().line_segment([corner, horizontal], stroke);
        ui.painter().line_segment([corner, vertical], stroke);
    }
    response
}

/// The red "AO VIVO" pill; returns its rect.
pub(super) fn live_badge(painter: &egui::Painter, at: egui::Pos2, text: &str) -> Rect {
    let galley = painter.layout_no_wrap(
        text.to_string(),
        FontId::new(10.5, theme::semibold()),
        Color32::WHITE,
    );
    let rect = Rect::from_min_size(at, galley.size() + vec2(12.0, 6.0));
    painter.rect_filled(rect, CornerRadius::same(4), theme::DANGER);
    painter.galley(rect.min + vec2(6.0, 3.0), galley, Color32::WHITE);
    rect
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strings::PT;

    #[test]
    fn every_failure_code_has_its_own_text() {
        let t = &PT;
        let texts: Vec<&str> = ["refused", "timeout", "unsupported", "network", "no_decoder"]
            .into_iter()
            .map(|code| failure_text(t, Some(code)))
            .collect();
        let mut unique = texts.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), texts.len());
        assert_eq!(
            failure_text(t, Some("network")),
            "Não foi possível conectar à transmissão (rede bloqueada?)."
        );
    }
}
