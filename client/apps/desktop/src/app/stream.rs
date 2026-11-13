//! Watching a participant's screen stream: the "AO VIVO" badge on their
//! card, the stream view that takes the participants' area, and its states.
//! Frames come from the embedded bridge's in-process slot, never the socket.

use egui::{pos2, vec2, Align2, Color32, CornerRadius, FontId, Rect, Sense, Stroke, UiBuilder};
use ts6_bridge::stream_media::frame::frames;

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
        self.stream_stage(ui, stage, participants);
        ui.add_space(STRIP_GAP);
        self.participants_content(ui, participants, STRIP_HEIGHT, None);
    }

    fn stream_stage(&mut self, ui: &mut egui::Ui, stage: Rect, participants: &[&ClientInfo]) {
        let t = self.t;
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

        // Who is live, top-left.
        let name = participants
            .iter()
            .find(|c| c.id == watching.client)
            .map_or("", |c| c.name.as_str());
        let badge = live_badge(&painter, stage.left_top() + vec2(12.0, 12.0), t.live_badge);
        painter.text(
            pos2(badge.right() + 8.0, badge.center().y),
            Align2::LEFT_CENTER,
            name,
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
        }
    }
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
