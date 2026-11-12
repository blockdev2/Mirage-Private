//! The settings window: your profile on top, then audio.
//!
//! Your photo and name are shown large and edited where they are: click the
//! photo to pick a new one, click the name to type a new one. Both apply at
//! once, to the saved profile, the sidebar and, in a call, the server.

use egui::{
    pos2, vec2, Align, Align2, Color32, CornerRadius, FontId, Frame, Id, Layout, Margin, Rect,
    RichText, Sense, Stroke, Ui,
};

use super::App;
use crate::protocol::{Command, Device};
use crate::store::{
    AvatarError, NoiseLevel, TransmitMode, AVATAR_EXTENSIONS, NICKNAME_MAX_CHARS, VAD_MAX_DB,
    VAD_MIN_DB,
};
use crate::strings::Strings;
use crate::theme::{self, icon, Tone};

const WINDOW_WIDTH: f32 = 520.0;
const AVATAR_SIZE: f32 = 76.0;
/// Width of the label column of a settings row.
const LABEL_WIDTH: f32 = 96.0;

/// State of the profile block that lives only while the window is open.
#[derive(Default)]
pub struct ProfileEdit {
    /// The name being typed, while the name is being edited.
    pub name: Option<String>,
    /// Focus the name field on its first frame.
    pub focus: bool,
    pub photo_error: Option<AvatarError>,
    /// A chosen photo waiting to be framed and confirmed.
    pub crop: Option<PhotoCrop>,
}

/// The crop editor's state: the full image (cropped from on apply), a
/// preview texture, and how the user framed it.
pub struct PhotoCrop {
    image: image::DynamicImage,
    texture: egui::TextureHandle,
    /// 1 = the short side exactly fills the circle.
    zoom: f32,
    /// Image center relative to the circle center, in points on screen.
    offset: egui::Vec2,
}

const CROP_STAGE: f32 = 420.0;
const CROP_CIRCLE: f32 = 360.0;
const CROP_MAX_ZOOM: f32 = 4.0;

impl App {
    pub(super) fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let max_height = (ctx.content_rect().height() - 56.0).max(240.0);
        // The window is the sidebar's color, the sections are the same cards
        // the rest of the app uses, so it reads as part of the app and not as
        // a foreign panel laid over it. The soft, long shadow lifts it off the
        // dimmed page without a glow.
        let frame = Frame::new()
            .fill(theme::SIDEBAR)
            .stroke(Stroke::new(1.0, theme::BORDER))
            .corner_radius(CornerRadius::same(14))
            .shadow(egui::Shadow {
                offset: [0, 18],
                blur: 48,
                spread: 0,
                color: Color32::from_black_alpha(150),
            })
            .inner_margin(Margin::symmetric(24, 22));

        // Framing a new photo gets its own, larger window in place of the
        // settings, so the scroll wheel zooms instead of scrolling the list.
        if self.profile_edit.crop.is_some() {
            let modal = egui::Modal::new(Id::unique("photo-crop"))
                .frame(frame)
                .backdrop_color(Color32::from_black_alpha(140))
                .show(ctx, |ui| {
                    ui.set_width(CROP_STAGE);
                    self.crop_editor(ui);
                });
            if modal.should_close() {
                self.profile_edit.crop = None;
            }
            return;
        }

        let modal = egui::Modal::new(Id::unique("settings"))
            .frame(frame)
            .backdrop_color(Color32::from_black_alpha(140))
            .show(ctx, |ui| {
                ui.set_width(WINDOW_WIDTH);
                self.settings_header(ui);
                ui.add_space(18.0);
                // A solid bar that is always drawn, so it is plain that the
                // list goes on below.
                ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
                ui.spacing_mut().scroll.bar_width = 7.0;
                egui::ScrollArea::vertical()
                    .max_height(max_height - 96.0)
                    .auto_shrink([false, true])
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                    .show(ui, |ui| {
                        // The bar sits in its own column to the right.
                        ui.spacing_mut().item_spacing.y = 12.0;
                        ui.set_width(ui.available_width() - 10.0);
                        section(ui, |ui| self.profile_block(ui));
                        section(ui, |ui| self.audio_block(ui));
                        self.footer_block(ui);
                    });
            });

        // Escape first cancels an open name edit; it only closes the window
        // when nothing is being edited.
        if modal.should_close() && self.profile_edit.name.is_none() {
            self.show_settings = false;
            self.profile_edit = ProfileEdit::default();
        }
    }

    fn settings_header(&mut self, ui: &mut Ui) {
        let t = self.t;
        ui.horizontal(|ui| {
            ui.label(RichText::new(t.settings_title).font(FontId::new(22.0, theme::semibold())));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if close_button(ui, t.close).clicked() {
                    self.show_settings = false;
                    self.profile_edit = ProfileEdit::default();
                }
            });
        });
    }

    // Profile ---------------------------------------------------------------

    fn profile_block(&mut self, ui: &mut Ui) {
        let t = self.t;
        let profile_id = self.store.active_profile_id();
        let profile_name = profile_id
            .as_deref()
            .and_then(|id| self.store.profile(id).map(|p| p.nickname.clone()));
        let in_session = self.in_session();
        // In a call the server's nickname, otherwise the local profile's.
        let name = if in_session && !self.nickname.is_empty() {
            self.nickname.clone()
        } else {
            profile_name
                .clone()
                .unwrap_or_else(|| t.no_profile.to_string())
        };
        let photo = profile_id
            .as_deref()
            .and_then(|id| self.profile_photo(ui.ctx(), id));

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 18.0;
            if editable_avatar(ui, &name, photo.as_ref(), t).clicked() && profile_id.is_some() {
                self.pick_photo(ui.ctx());
            }
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                self.name_field(ui, &name);
                let (status, color) = if in_session {
                    (t.available, theme::SUCCESS)
                } else {
                    (t.local_profile, theme::TEXT_3)
                };
                ui.horizontal(|ui| {
                    let (dot, _) = ui.allocate_exact_size(vec2(8.0, 14.0), Sense::hover());
                    ui.painter().circle_filled(dot.center(), 3.5, color);
                    ui.label(RichText::new(status).small().color(theme::TEXT_2));
                });
                if photo.is_some()
                    && profile_id.is_some()
                    && quiet_link(ui, t.remove_photo).clicked()
                {
                    self.remove_photo();
                }
            });
        });
        if let Some(error) = self.profile_edit.photo_error {
            ui.add_space(8.0);
            let text = match error {
                AvatarError::TooLarge => t.photo_too_large,
                AvatarError::Unreadable => t.photo_unreadable,
            };
            ui.label(RichText::new(text).small().color(theme::DANGER));
        }
    }

    /// The name, big. Click to edit it in place: Enter or clicking away saves,
    /// Escape cancels.
    fn name_field(&mut self, ui: &mut Ui, name: &str) {
        let t = self.t;
        let field_id = Id::unique("settings_name");
        let Some(draft) = self.profile_edit.name.as_mut() else {
            if editable_name(ui, name, t.edit_name).clicked() {
                self.profile_edit.name = Some(name.to_string());
                self.profile_edit.focus = true;
            }
            return;
        };

        let mut commit = false;
        let mut cancel = false;
        theme::input_frame().show(ui, |ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(draft)
                    .id(field_id)
                    .frame(Frame::NONE)
                    .font(FontId::new(20.0, theme::semibold()))
                    .char_limit(NICKNAME_MAX_CHARS)
                    .desired_width(260.0),
            );
            if self.profile_edit.focus {
                edit.request_focus();
                self.profile_edit.focus = false;
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                cancel = true;
            } else if edit.lost_focus() {
                commit = true;
            }
        });
        if cancel {
            ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
            self.profile_edit.name = None;
        } else if commit {
            let new_name = self.profile_edit.name.take().unwrap_or_default();
            self.commit_name(&new_name, name);
        }
    }

    /// Saves a new name to the profile and, in a call, to the server.
    fn commit_name(&mut self, typed: &str, current: &str) {
        let name: String = typed.trim().chars().take(NICKNAME_MAX_CHARS).collect();
        if name.is_empty() || name == current {
            return;
        }
        match self.store.active_profile_id() {
            Some(id) => self.store.update_profile(&id, &name, false),
            None => {
                self.store.add_profile(&name, true);
            }
        }
        self.store.save();
        if self.in_session() {
            self.nickname = name.clone();
            self.send(Command::Nick { name });
        }
    }

    fn pick_photo(&mut self, ctx: &egui::Context) {
        let t = self.t;
        let Some(id) = self.store.active_profile_id() else {
            return;
        };
        let Some(path) = rfd::FileDialog::new()
            .set_title(t.photo_picker_title)
            .add_filter(t.photo_filter, AVATAR_EXTENSIONS)
            .pick_file()
        else {
            return;
        };
        let _ = id;
        match crate::store::load_avatar_source(&path) {
            Ok(image) => {
                // The preview only needs screen resolution; the crop uses the original.
                let preview = image.thumbnail(1024, 1024).to_rgba8();
                let size = [preview.width() as usize, preview.height() as usize];
                let pixels = egui::ColorImage::from_rgba_unmultiplied(size, preview.as_raw());
                let texture = ctx.load_texture("avatar-crop", pixels, egui::TextureOptions::LINEAR);
                self.profile_edit.photo_error = None;
                self.profile_edit.crop = Some(PhotoCrop {
                    image,
                    texture,
                    zoom: 1.0,
                    offset: egui::Vec2::ZERO,
                });
            }
            Err(error) => self.profile_edit.photo_error = Some(error),
        }
        ctx.request_repaint();
    }

    /// Saves the framed square as the profile photo.
    fn apply_crop(&mut self) {
        let Some(crop) = self.profile_edit.crop.take() else {
            return;
        };
        let Some(id) = self.store.active_profile_id() else {
            return;
        };
        let (width, height) = (crop.image.width() as f32, crop.image.height() as f32);
        let scale = crop_scale(&crop);
        let side = CROP_CIRCLE / scale;
        let center_x = width / 2.0 - crop.offset.x / scale;
        let center_y = height / 2.0 - crop.offset.y / scale;
        let photo = crate::store::crop_avatar(
            &crop.image,
            (center_x - side / 2.0).max(0.0).round() as u32,
            (center_y - side / 2.0).max(0.0).round() as u32,
            side.round() as u32,
        );
        if self.store.set_avatar(&id, &photo) {
            self.store.save();
            if self.in_session() {
                self.publish_avatar(Some(&id));
            }
        } else {
            self.profile_edit.photo_error = Some(AvatarError::Unreadable);
        }
    }

    /// The framing step after a photo is chosen: drag to move, scroll or the
    /// slider to zoom; nothing changes until "Aplicar foto".
    fn crop_editor(&mut self, ui: &mut Ui) {
        let t = self.t;
        let Some(crop) = self.profile_edit.crop.as_mut() else {
            return;
        };
        ui.label(RichText::new(t.crop_title).font(FontId::new(22.0, theme::semibold())));
        ui.label(RichText::new(t.crop_hint).small().color(theme::TEXT_3));
        ui.add_space(14.0);

        let mut apply = false;
        let mut cancel = false;
        ui.vertical_centered(|ui| {
            let (stage, response) =
                ui.allocate_exact_size(vec2(CROP_STAGE, CROP_STAGE), Sense::drag());
            if response.dragged() {
                crop.offset += response.drag_delta();
            }
            if response.hovered() {
                // Taken from the input, so nothing behind also scrolls.
                let scroll = ui.input_mut(|i| std::mem::take(&mut i.smooth_scroll_delta).y);
                if scroll != 0.0 {
                    crop.zoom = (crop.zoom * (1.0 + scroll * 0.002)).clamp(1.0, CROP_MAX_ZOOM);
                }
            }
            clamp_offset(crop);
            let grabbing = response.dragged();
            response.on_hover_cursor(if grabbing {
                egui::CursorIcon::Grabbing
            } else {
                egui::CursorIcon::Grab
            });

            let painter = ui.painter_at(stage);
            painter.rect_filled(stage, CornerRadius::same(12), theme::SURFACE_RAISED);
            let scale = crop_scale(crop);
            let size = vec2(crop.image.width() as f32, crop.image.height() as f32) * scale;
            let image_rect = Rect::from_center_size(stage.center() + crop.offset, size);
            painter.image(
                crop.texture.id(),
                image_rect,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
            dim_outside_circle(&painter, stage, CROP_CIRCLE / 2.0);
            painter.circle_stroke(
                stage.center(),
                CROP_CIRCLE / 2.0,
                Stroke::new(2.0, Color32::from_white_alpha(220)),
            );

            ui.add_space(14.0);
            // Zoom: a small and a large photo glyph either side of the slider.
            ui.allocate_ui_with_layout(
                vec2(CROP_STAGE, 24.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    ui.label(
                        RichText::new(icon::IMAGE)
                            .font(FontId::new(13.0, theme::icons()))
                            .color(theme::TEXT_3),
                    );
                    ui.spacing_mut().slider_width = CROP_STAGE - 60.0;
                    ui.add(
                        egui::Slider::new(&mut crop.zoom, 1.0..=CROP_MAX_ZOOM)
                            .show_value(false)
                            .logarithmic(true),
                    )
                    .on_hover_text(t.crop_zoom);
                    ui.label(
                        RichText::new(icon::IMAGE)
                            .font(FontId::new(19.0, theme::icons()))
                            .color(theme::TEXT_3),
                    );
                },
            );
            clamp_offset(crop);
            ui.add_space(16.0);
            ui.allocate_ui_with_layout(
                vec2(CROP_STAGE, 40.0),
                Layout::right_to_left(Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if theme::button(ui, t.crop_apply, None, Tone::Primary, 136.0).clicked() {
                        apply = true;
                    }
                    if theme::button(ui, t.cancel, None, Tone::Ghost, 100.0).clicked() {
                        cancel = true;
                    }
                },
            );
        });
        if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            apply = true;
        }
        if apply {
            self.apply_crop();
        } else if cancel {
            self.profile_edit.crop = None;
        }
    }

    fn remove_photo(&mut self) {
        let Some(id) = self.store.active_profile_id() else {
            return;
        };
        self.store.remove_avatar(&id);
        self.store.save();
        self.profile_edit.photo_error = None;
        if self.in_session() {
            self.publish_avatar(Some(&id));
        }
    }

    // Audio -----------------------------------------------------------------

    fn audio_block(&mut self, ui: &mut Ui) {
        let t = self.t;
        let level = self.audio.as_ref().and_then(|a| a.input_level_db);
        let transmitting = self.audio.as_ref().is_some_and(|a| a.transmitting);
        let mut changed_live = false;
        let audio = &mut self.store.data.audio;

        ui.horizontal(|ui| {
            ui.label(RichText::new(t.audio_section).font(FontId::new(16.0, theme::semibold())));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                theme::toggle(ui, &mut audio.enabled, t.enable_audio);
                ui.label(RichText::new(t.enable_audio).color(theme::TEXT_2));
            });
        });
        ui.add_space(14.0);

        ui.add_enabled_ui(audio.enabled, |ui| {
            device_row(
                ui,
                t,
                t.input,
                "input",
                &mut audio.input_device,
                &self.devices.inputs,
            );
            device_row(
                ui,
                t,
                t.output,
                "output",
                &mut audio.output_device,
                &self.devices.outputs,
            );
            ui.add_space(14.0);

            sub_heading(ui, t.transmit_mode);
            let modes = [
                (TransmitMode::VoiceActivation, t.mode_voice),
                (TransmitMode::PushToTalk, t.mode_ptt),
                (TransmitMode::Continuous, t.mode_continuous),
            ];
            let selected = modes
                .iter()
                .position(|(mode, _)| *mode == audio.mode)
                .unwrap_or(0);
            if let Some(index) = segmented(ui, &modes.map(|(_, label)| label), selected) {
                audio.mode = modes[index].0;
            }
            ui.add_space(12.0);
            match audio.mode {
                TransmitMode::VoiceActivation => {
                    sub_heading(ui, t.vad_threshold);
                    level_meter(ui, level, audio.threshold_db, transmitting);
                    ui.add_space(6.0);
                    ui.spacing_mut().slider_width = ui.available_width() - 110.0;
                    let slider = ui.add(
                        egui::Slider::new(&mut audio.threshold_db, VAD_MIN_DB..=VAD_MAX_DB)
                            .suffix(" dB")
                            .step_by(1.0),
                    );
                    changed_live |= slider.changed();
                    ui.label(RichText::new(t.vad_hint).small().color(theme::TEXT_3));
                }
                TransmitMode::PushToTalk => {
                    sub_heading(ui, t.ptt_key);
                    theme::input_frame().show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut audio.ptt)
                                .hint_text("F13")
                                .frame(Frame::NONE)
                                .desired_width(f32::INFINITY),
                        );
                    });
                    ui.add_space(4.0);
                    ui.label(RichText::new(t.ptt_hint).small().color(theme::TEXT_3));
                }
                TransmitMode::Continuous => {}
            }

            ui.add_space(16.0);
            sub_heading(ui, t.noise_title);
            let before = audio.noise;
            noise_choices(ui, t, &mut audio.noise);
            // Applies live as well: no microphone restart.
            changed_live |= audio.noise != before;

            ui.add_space(16.0);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new(t.monitor_title).color(theme::TEXT));
                    ui.label(RichText::new(t.monitor_hint).small().color(theme::TEXT_3));
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    changed_live |=
                        theme::toggle(ui, &mut audio.monitor, t.monitor_title).changed();
                });
            });
        });

        if changed_live {
            self.apply_audio();
        }
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if theme::button(ui, t.apply_audio, None, Tone::Primary, 150.0).clicked() {
                self.apply_audio();
            }
            if theme::button(ui, t.refresh_devices, None, Tone::Secondary, 190.0).clicked() {
                self.send(Command::Devices);
            }
        });
    }

    fn footer_block(&mut self, ui: &mut Ui) {
        let t = self.t;
        ui.horizontal(|ui| {
            ui.label(RichText::new(t.show_diagnostics).color(theme::TEXT_2));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                theme::toggle(ui, &mut self.show_logs, t.show_diagnostics);
            });
        });
        let version = match &self.bridge_version {
            Some(v) => t.bridge_version(v),
            None => t.bridge_offline_short.to_string(),
        };
        ui.add_space(4.0);
        ui.label(RichText::new(version).small().color(theme::TEXT_3));
    }
}

// Pieces --------------------------------------------------------------------

/// Screen points per image pixel for the current zoom.
fn crop_scale(crop: &PhotoCrop) -> f32 {
    let short = crop.image.width().min(crop.image.height()).max(1) as f32;
    CROP_CIRCLE / short * crop.zoom
}

/// Keeps the circle covered by the image, so the photo never has empty edges.
fn clamp_offset(crop: &mut PhotoCrop) {
    let scale = crop_scale(crop);
    let room = |len: u32| ((len as f32 * scale - CROP_CIRCLE) / 2.0).max(0.0);
    let (x, y) = (room(crop.image.width()), room(crop.image.height()));
    crop.offset = vec2(crop.offset.x.clamp(-x, x), crop.offset.y.clamp(-y, y));
}

/// Darkens the stage outside the circle: a ring of quads from the circle out
/// to the stage edge (egui has no inverse fill).
fn dim_outside_circle(painter: &egui::Painter, stage: Rect, radius: f32) {
    let shade = Color32::from_black_alpha(150);
    let center = stage.center();
    let reach = stage.width().max(stage.height());
    let steps = 96;
    let mut mesh = egui::Mesh::default();
    for i in 0..=steps {
        let angle = i as f32 / steps as f32 * std::f32::consts::TAU;
        let dir = vec2(angle.cos(), angle.sin());
        mesh.colored_vertex(center + dir * radius, shade);
        mesh.colored_vertex(center + dir * reach, shade);
        if i > 0 {
            let base = (i as u32 - 1) * 2;
            mesh.add_triangle(base, base + 1, base + 2);
            mesh.add_triangle(base + 1, base + 3, base + 2);
        }
    }
    painter.add(mesh);
}

/// One block of the window: the same card as the app's content blocks, with a
/// slightly rounder corner because the window around it is larger.
fn section(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui)) {
    Frame::new()
        .fill(theme::SURFACE)
        .stroke(Stroke::new(1.0, theme::BORDER_SOFT))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(18))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui);
        });
}

/// Small muted text that lights up under the pointer.
fn quiet_link(ui: &mut Ui, text: &str) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(
        text.to_string(),
        FontId::new(12.5, theme::medium()),
        theme::TEXT_3,
    );
    let (rect, response) = ui.allocate_exact_size(galley.size() + vec2(0.0, 6.0), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Link, true, text));
    let amount = ui.ctx().animate_bool_with_time(
        response.id,
        response.hovered() || response.has_focus(),
        0.12,
    );
    let ink = theme::lerp_color(theme::TEXT_3, Color32::WHITE, amount);
    ui.painter().galley(
        pos2(rect.left(), rect.center().y - galley.size().y / 2.0),
        galley,
        ink,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn sub_heading(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .font(FontId::new(13.0, theme::medium()))
            .color(theme::TEXT_2),
    );
    ui.add_space(6.0);
}

/// A plain X, painted so it does not depend on an icon font. Shows a soft
/// round highlight under the pointer.
pub(super) fn close_button(ui: &mut Ui, label: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    let hot = response.hovered() || response.has_focus();
    let amount = ui.ctx().animate_bool_with_time(response.id, hot, 0.12);
    let painter = ui.painter();
    painter.circle_filled(
        rect.center(),
        16.0,
        theme::lerp_color(theme::SIDEBAR, theme::SURFACE_HOVER, amount),
    );
    let ink = theme::lerp_color(theme::TEXT_3, Color32::WHITE, amount);
    let reach = 5.0;
    let stroke = Stroke::new(1.6, ink);
    let c = rect.center();
    painter.line_segment([c + vec2(-reach, -reach), c + vec2(reach, reach)], stroke);
    painter.line_segment([c + vec2(-reach, reach), c + vec2(reach, -reach)], stroke);
    if response.has_focus() {
        theme::focus_ring(painter, rect);
    }
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(label)
}

/// The profile photo as a button: a pencil appears over it on hover.
fn editable_avatar(
    ui: &mut Ui,
    name: &str,
    photo: Option<&egui::TextureHandle>,
    t: &Strings,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(AVATAR_SIZE, AVATAR_SIZE), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, t.change_photo));
    match photo {
        Some(texture) => theme::photo_at(ui, rect, texture, None),
        None => theme::avatar_tiny(ui.painter(), rect.center(), AVATAR_SIZE, name, false),
    }
    let hot = response.hovered() || response.has_focus();
    let amount = ui.ctx().animate_bool_with_time(response.id, hot, 0.15);
    if amount > 0.0 {
        let painter = ui.painter();
        painter.circle_filled(
            rect.center(),
            AVATAR_SIZE / 2.0,
            Color32::from_black_alpha((150.0 * amount) as u8),
        );
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            icon::PENCIL,
            FontId::new(24.0, theme::icons()),
            Color32::from_white_alpha((255.0 * amount) as u8),
        );
    }
    if response.has_focus() {
        theme::focus_ring(ui.painter(), rect);
    }
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(t.change_photo)
}

/// The name as large text; a pencil fades in beside it on hover.
fn editable_name(ui: &mut Ui, name: &str, label: &str) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(
        name.to_string(),
        FontId::new(22.0, theme::semibold()),
        theme::TEXT,
    );
    let max = (ui.available_width() - 34.0).max(60.0);
    let text_width = galley.size().x.min(max);
    let size = vec2(text_width + 34.0, 34.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    let hot = response.hovered() || response.has_focus();
    let amount = ui.ctx().animate_bool_with_time(response.id, hot, 0.15);
    let painter = ui.painter().with_clip_rect(rect);
    painter.galley(
        pos2(rect.left(), rect.center().y - galley.size().y / 2.0),
        galley,
        theme::TEXT,
    );
    painter.text(
        pos2(rect.left() + text_width + 10.0, rect.center().y),
        Align2::LEFT_CENTER,
        icon::PENCIL,
        FontId::new(16.0, theme::icons()),
        theme::lerp_color(theme::SURFACE, Color32::WHITE, amount),
    );
    if response.has_focus() {
        theme::focus_ring(ui.painter(), rect);
    }
    response
        .on_hover_cursor(egui::CursorIcon::Text)
        .on_hover_text(label)
}

/// Equal-width options in one bar. Returns the index clicked this frame.
fn segmented(ui: &mut Ui, labels: &[&str], selected: usize) -> Option<usize> {
    let mut clicked = None;
    Frame::new()
        .fill(theme::SURFACE_RAISED)
        .stroke(Stroke::new(1.0, theme::BORDER_SOFT))
        .corner_radius(CornerRadius::same(7))
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            let width =
                (ui.available_width() - 3.0 * (labels.len() as f32 - 1.0)) / labels.len() as f32;
            ui.horizontal(|ui| {
                for (index, label) in labels.iter().enumerate() {
                    let active = index == selected;
                    let (rect, response) =
                        ui.allocate_exact_size(vec2(width, 32.0), Sense::click());
                    response.widget_info(|| {
                        egui::WidgetInfo::selected(egui::Role::RadioButton, true, active, *label)
                    });
                    let amount = ui.ctx().animate_bool_with_time(response.id, active, 0.14);
                    let hover = response.hovered() && !active;
                    let fill = if hover {
                        theme::SURFACE_HOVER
                    } else {
                        theme::lerp_color(theme::SURFACE_RAISED, theme::ACCENT, amount)
                    };
                    let ink = theme::lerp_color(
                        if hover { theme::TEXT } else { theme::TEXT_2 },
                        theme::BG,
                        amount,
                    );
                    let painter = ui.painter();
                    painter.rect_filled(rect, CornerRadius::same(5), fill);
                    painter.text(
                        rect.center(),
                        Align2::CENTER_CENTER,
                        label,
                        FontId::new(13.0, theme::semibold()),
                        ink,
                    );
                    if response.has_focus() {
                        theme::focus_ring(painter, rect);
                    }
                    if response.clicked() && !active {
                        clicked = Some(index);
                    }
                }
            });
        });
    clicked
}

/// The noise suppression options as a list: each row names the setting, then
/// the engine behind it in parentheses, and the chosen one explains itself.
fn noise_choices(ui: &mut Ui, t: &Strings, current: &mut NoiseLevel) {
    let options = [
        (NoiseLevel::Off, t.noise_off, None, t.noise_hint_off),
        (
            NoiseLevel::Standard,
            t.noise_standard,
            Some(t.noise_engine_standard),
            t.noise_hint_standard,
        ),
        (
            NoiseLevel::Strong,
            t.noise_strong,
            Some(t.noise_engine_strong),
            t.noise_hint_strong,
        ),
        (
            NoiseLevel::Advanced,
            t.noise_advanced,
            Some(t.noise_engine_advanced),
            t.noise_hint_advanced,
        ),
    ];
    Frame::new()
        .fill(theme::SURFACE_RAISED)
        .stroke(Stroke::new(1.0, theme::BORDER_SOFT))
        .corner_radius(CornerRadius::same(7))
        .inner_margin(Margin::same(4))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for (level, name, engine, hint) in options {
                let active = *current == level;
                let height = if active { 52.0 } else { 34.0 };
                let (rect, response) =
                    ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
                response.widget_info(|| {
                    egui::WidgetInfo::selected(egui::Role::RadioButton, true, active, name)
                });
                let amount =
                    ui.ctx()
                        .animate_bool_with_time(response.id.with("active"), active, 0.14);
                let painter = ui.painter();
                let fill = if active {
                    theme::SURFACE_HOVER
                } else if response.hovered() {
                    theme::lerp_color(theme::SURFACE_RAISED, theme::SURFACE_HOVER, 0.5)
                } else {
                    theme::SURFACE_RAISED
                };
                painter.rect_filled(rect, CornerRadius::same(5), fill);

                // Radio mark.
                let mark = pos2(rect.left() + 18.0, rect.top() + 17.0);
                painter.circle_stroke(
                    mark,
                    7.0,
                    Stroke::new(1.5, theme::lerp_color(theme::TEXT_3, theme::TEXT, amount)),
                );
                painter.circle_filled(mark, 3.5 * amount, theme::TEXT);

                let mut x = rect.left() + 36.0;
                let title = painter.layout_no_wrap(
                    name.to_string(),
                    FontId::new(14.0, theme::semibold()),
                    theme::TEXT,
                );
                let title_pos = pos2(x, mark.y - title.size().y / 2.0);
                x += title.size().x + 6.0;
                painter.galley(title_pos, title, theme::TEXT);
                if let Some(engine) = engine {
                    let tag = painter.layout_no_wrap(
                        format!("({engine})"),
                        FontId::new(13.0, theme::medium()),
                        theme::TEXT_3,
                    );
                    painter.galley(pos2(x, mark.y - tag.size().y / 2.0), tag, theme::TEXT_3);
                }
                if active {
                    let note = painter.layout(
                        hint.to_string(),
                        FontId::new(12.5, egui::FontFamily::Proportional),
                        theme::TEXT_2,
                        rect.width() - 52.0,
                    );
                    painter.galley(
                        pos2(rect.left() + 36.0, rect.top() + 31.0),
                        note,
                        theme::TEXT_2,
                    );
                }
                if response.has_focus() {
                    theme::focus_ring(painter, rect);
                }
                if response.clicked() {
                    *current = level;
                }
                response.on_hover_cursor(egui::CursorIcon::PointingHand);
            }
        });
}

/// Label on the left, selector filling the rest of the row.
fn device_row(
    ui: &mut Ui,
    t: &Strings,
    label: &str,
    salt: &str,
    selected: &mut String,
    devices: &[Device],
) {
    ui.horizontal(|ui| {
        ui.add_sized(
            vec2(LABEL_WIDTH, 28.0),
            egui::Label::new(RichText::new(label).color(theme::TEXT_2)),
        );
        let shown = if selected.is_empty() {
            t.system_default.to_string()
        } else {
            selected.clone()
        };
        egui::ComboBox::from_id_salt(salt)
            .selected_text(shown)
            .width(ui.available_width() - 14.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(selected, String::new(), t.system_default);
                for device in devices {
                    ui.selectable_value(selected, device.name.clone(), &device.name);
                }
            });
    });
}

/// Horizontal microphone level bar with the voice activation threshold marked.
/// Green while transmitting, so the user sees exactly when voice goes out.
fn level_meter(ui: &mut Ui, level_db: Option<f32>, threshold_db: f32, transmitting: bool) {
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
