//! Design tokens, typography and reusable components.
//!
//! Colors are tuned for contrast on a near-black base: primary text is about
//! 17:1 against the background, secondary text about 8:1, and the single violet
//! accent is used only for the active state and the primary action.

use std::sync::Arc;

use egui::{
    pos2, vec2, Align2, Color32, Context, CornerRadius, CursorIcon, FontData, FontDefinitions,
    FontFamily, FontId, Frame, Margin, Response, Sense, Stroke, TextStyle, Ui, Visuals,
};

// Surfaces, from the back of the window to the front.
pub const BG: Color32 = Color32::from_rgb(16, 18, 22);
pub const SIDEBAR: Color32 = Color32::from_rgb(20, 22, 27);
pub const SURFACE: Color32 = Color32::from_rgb(27, 30, 37);
pub const SURFACE_RAISED: Color32 = Color32::from_rgb(32, 36, 44);
pub const SURFACE_HOVER: Color32 = Color32::from_rgb(40, 45, 55);

pub const BORDER: Color32 = Color32::from_rgb(43, 48, 57);
pub const BORDER_SOFT: Color32 = Color32::from_rgb(34, 38, 46);

// Text.
pub const TEXT: Color32 = Color32::from_rgb(240, 241, 243);
pub const TEXT_2: Color32 = Color32::from_rgb(165, 173, 186);
pub const TEXT_3: Color32 = Color32::from_rgb(122, 130, 144);

// Accent and states.
pub const ACCENT: Color32 = Color32::from_rgb(236, 239, 244);
pub const ACCENT_HOVER: Color32 = Color32::from_rgb(205, 211, 221);
pub const ACCENT_SOFT: Color32 = Color32::from_rgb(38, 43, 54);
/// Background of selected text: a muted blue, clearly visible on every surface
/// while keeping white text readable on top of it.
pub const SELECTION: Color32 = Color32::from_rgb(52, 84, 140);
pub const SUCCESS: Color32 = Color32::from_rgb(61, 214, 140);
pub const WARNING: Color32 = Color32::from_rgb(245, 185, 74);
pub const DANGER: Color32 = Color32::from_rgb(229, 72, 77);
pub const DANGER_HOVER: Color32 = Color32::from_rgb(240, 96, 101);
pub const DANGER_SOFT: Color32 = Color32::from_rgba_premultiplied(40, 13, 15, 255);

/// Avatar fills, picked by a hash of the user's name.
pub const AVATAR_PALETTE: [Color32; 6] = [
    Color32::from_rgb(196, 170, 146),
    Color32::from_rgb(150, 160, 176),
    Color32::from_rgb(190, 196, 206),
    Color32::from_rgb(128, 150, 140),
    Color32::from_rgb(168, 150, 184),
    Color32::from_rgb(190, 140, 130),
];

// Phosphor icon codepoints (regular weight), from the egui-phosphor crate.
pub mod icon {
    pub const MICROPHONE: &str = "\u{E326}";
    pub const MICROPHONE_SLASH: &str = "\u{E328}";
    pub const HEADPHONES: &str = "\u{E2A6}";
    pub const SHARE_SCREEN: &str = "\u{E58A}";
    pub const PHONE_DISCONNECT: &str = "\u{E3BC}";
    pub const GEAR: &str = "\u{E270}";
    pub const PENCIL: &str = "\u{E3B4}";
    pub const PLUS: &str = "\u{E3D4}";
    pub const SEND: &str = "\u{E396}";
    pub const USER_PLUS: &str = "\u{E4D0}";
    pub const SPEAKER: &str = "\u{E44A}";
    pub const WAVEFORM: &str = "\u{E802}";
    pub const HOUSE: &str = "\u{E2C2}";
    pub const CARET_DOWN: &str = "\u{E136}";
    pub const CARET_UP: &str = "\u{E13C}";
    pub const CARET_RIGHT: &str = "\u{E13A}";
    pub const DOTS_THREE: &str = "\u{E1FE}";
    pub const LIST: &str = "\u{E2F0}";
    pub const SQUARES_FOUR: &str = "\u{E464}";
    pub const LOCK_SIMPLE: &str = "\u{E308}";
    pub const SPEAKER_LOW: &str = "\u{E44C}";
    pub const SPEAKER_SLASH: &str = "\u{E45A}";
    pub const INFO: &str = "\u{E2CE}";
    pub const ARROW_COUNTER_CLOCKWISE: &str = "\u{E038}";
    pub const ARROW_SQUARE_OUT: &str = "\u{E5DE}";
    pub const BOOKMARK_SIMPLE: &str = "\u{E0EA}";
    pub const CLOCK_COUNTER_CLOCKWISE: &str = "\u{E1A0}";
    pub const ARROW_RIGHT: &str = "\u{E06C}";
    pub const MAGNIFYING_GLASS: &str = "\u{E30C}";
    pub const EYE: &str = "\u{E220}";
    pub const IMAGE: &str = "\u{E2CA}";
    pub const EYE_SLASH: &str = "\u{E224}";
}

const INTER_REGULAR: &[u8] = include_bytes!("../assets/fonts/Inter-Regular.ttf");
const INTER_MEDIUM: &[u8] = include_bytes!("../assets/fonts/Inter-Medium.ttf");
const INTER_SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/Inter-SemiBold.ttf");
const INTER_BOLD: &[u8] = include_bytes!("../assets/fonts/Inter-Bold.ttf");
const PHOSPHOR: &[u8] = include_bytes!("../assets/fonts/Phosphor.ttf");

/// Font family names used by the views.
pub fn medium() -> FontFamily {
    FontFamily::Name(Arc::from("inter-medium"))
}

pub fn semibold() -> FontFamily {
    FontFamily::Name(Arc::from("inter-semibold"))
}

pub fn bold() -> FontFamily {
    FontFamily::Name(Arc::from("inter-bold"))
}

/// Icon-only family. Use it for every Phosphor glyph: Inter also defines some
/// private-use codepoints (for example the gear and house), and Inter comes
/// first in the text families, so icons drawn with those families show Inter's
/// glyphs instead of Phosphor's.
pub fn icons() -> FontFamily {
    FontFamily::Name(Arc::from("phosphor"))
}

/// Installs fonts, colors, spacing and text styles. Call once at startup.
pub fn apply(ctx: &Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "inter-regular".into(),
        Arc::new(FontData::from_static(INTER_REGULAR)),
    );
    fonts.font_data.insert(
        "inter-medium".into(),
        Arc::new(FontData::from_static(INTER_MEDIUM)),
    );
    fonts.font_data.insert(
        "inter-semibold".into(),
        Arc::new(FontData::from_static(INTER_SEMIBOLD)),
    );
    fonts.font_data.insert(
        "inter-bold".into(),
        Arc::new(FontData::from_static(INTER_BOLD)),
    );
    fonts
        .font_data
        .insert("phosphor".into(), Arc::new(FontData::from_static(PHOSPHOR)));

    // Phosphor is appended to every family so icons render inline in any label.
    let with_icons = |first: &str| vec![first.to_string(), "phosphor".to_string()];
    fonts
        .families
        .insert(FontFamily::Proportional, with_icons("inter-regular"));
    fonts.families.insert(medium(), with_icons("inter-medium"));
    fonts
        .families
        .insert(semibold(), with_icons("inter-semibold"));
    fonts.families.insert(bold(), with_icons("inter-bold"));
    fonts.families.insert(icons(), vec!["phosphor".to_string()]);
    ctx.set_fonts(fonts);

    ctx.global_style_mut(|style| {
        style.text_styles = [
            (TextStyle::Heading, FontId::new(26.0, semibold())),
            (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(14.0, semibold())),
            (
                TextStyle::Small,
                FontId::new(12.0, FontFamily::Proportional),
            ),
            (
                TextStyle::Monospace,
                FontId::new(13.0, FontFamily::Monospace),
            ),
        ]
        .into();

        style.spacing.item_spacing = vec2(10.0, 8.0);
        style.spacing.button_padding = vec2(14.0, 8.0);
        style.spacing.interact_size.y = 34.0;
        // Checkboxes: a visible box and room before the label.
        style.spacing.icon_width = 18.0;
        style.spacing.icon_width_inner = 10.0;
        style.spacing.icon_spacing = 10.0;

        let mut visuals = Visuals::dark();
        visuals.panel_fill = BG;
        visuals.window_fill = SURFACE;
        visuals.window_stroke = Stroke::new(1.0, BORDER);
        visuals.window_corner_radius = CornerRadius::same(8);
        visuals.extreme_bg_color = SIDEBAR;
        visuals.faint_bg_color = SURFACE;
        visuals.override_text_color = Some(TEXT);
        // Text selection must stand out from input fills (SURFACE_RAISED);
        // ACCENT_SOFT was almost the same shade, so selections were invisible.
        visuals.selection.bg_fill = SELECTION;
        visuals.selection.stroke = Stroke::new(1.0, ACCENT);
        visuals.hyperlink_color = ACCENT;
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER_SOFT);
        visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_2);
        for widget in [
            &mut visuals.widgets.inactive,
            &mut visuals.widgets.hovered,
            &mut visuals.widgets.active,
        ] {
            widget.corner_radius = CornerRadius::same(6);
        }
        visuals.widgets.inactive.weak_bg_fill = SURFACE_RAISED;
        visuals.widgets.inactive.bg_fill = SURFACE_RAISED;
        visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
        visuals.widgets.hovered.weak_bg_fill = SURFACE_HOVER;
        visuals.widgets.hovered.bg_fill = SURFACE_HOVER;
        visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, BORDER);
        visuals.widgets.active.weak_bg_fill = ACCENT_SOFT;
        visuals.widgets.active.bg_fill = ACCENT_SOFT;
        style.visuals = visuals;
    });
}

// Components -----------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Primary,
    Secondary,
    Ghost,
    Danger,
}

/// A labelled button with an optional leading icon. Respects `ui.is_enabled()`,
/// so wrap it in `add_enabled_ui` to disable it.
pub fn button(ui: &mut Ui, label: &str, icon: Option<&str>, tone: Tone, width: f32) -> Response {
    let enabled = ui.is_enabled();
    let (rect, response) = ui.allocate_exact_size(
        vec2(width, 40.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let hovered = enabled && response.hovered();

    let (fill, text, border) = match (tone, hovered) {
        (Tone::Primary, false) => (ACCENT, BG, None),
        (Tone::Primary, true) => (ACCENT_HOVER, BG, None),
        (Tone::Secondary, false) => (SURFACE_RAISED, TEXT, Some(BORDER)),
        (Tone::Secondary, true) => (SURFACE_HOVER, TEXT, Some(BORDER)),
        (Tone::Ghost, false) => (Color32::TRANSPARENT, TEXT_2, None),
        (Tone::Ghost, true) => (SURFACE_RAISED, TEXT, None),
        (Tone::Danger, false) => (DANGER_SOFT, DANGER, Some(DANGER)),
        (Tone::Danger, true) => (DANGER, Color32::WHITE, Some(DANGER)),
    };
    // Disabled keeps the button's shape (fill and border) so it still reads
    // as a button, just inactive.
    let (fill, text, border) = if enabled {
        (fill, text, border)
    } else {
        (SURFACE_RAISED, TEXT_3, Some(BORDER))
    };

    let painter = ui.painter();
    if response.has_focus() {
        focus_ring(painter, rect);
    }
    painter.rect_filled(rect, CornerRadius::same(6), fill);
    if let Some(color) = border {
        painter.rect_stroke(
            rect,
            CornerRadius::same(6),
            Stroke::new(1.0, color),
            egui::StrokeKind::Inside,
        );
    }

    let font = FontId::new(14.0, semibold());
    match icon {
        Some(glyph) => {
            let icon_font = FontId::new(17.0, icons());
            let label_galley = painter.layout_no_wrap(label.to_string(), font.clone(), text);
            let icon_galley = painter.layout_no_wrap(glyph.to_string(), icon_font, text);
            let gap = 8.0;
            let total = icon_galley.size().x + gap + label_galley.size().x;
            let start = rect.center().x - total / 2.0;
            painter.galley(
                pos2(start, rect.center().y - icon_galley.size().y / 2.0),
                icon_galley,
                text,
            );
            painter.galley(
                pos2(
                    start + total - label_galley.size().x,
                    rect.center().y - label_galley.size().y / 2.0,
                ),
                label_galley,
                text,
            );
        }
        None => {
            painter.text(rect.center(), Align2::CENTER_CENTER, label, font, text);
        }
    }

    if enabled {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    }
}

/// Lays out `text` on one line and ends it with an ellipsis when it is wider
/// than `max_width`, so a long name never runs into its neighbours.
pub fn fit_text(
    painter: &egui::Painter,
    text: &str,
    font: FontId,
    color: Color32,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap.max_width = max_width.max(0.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    painter.layout_job(job)
}

/// Draws the keyboard-focus outline shared by every custom widget.
pub fn focus_ring(painter: &egui::Painter, rect: egui::Rect) {
    painter.rect_stroke(
        rect.expand(2.0),
        CornerRadius::same(8),
        Stroke::new(2.0, ACCENT),
        egui::StrokeKind::Outside,
    );
}

/// Square icon-only button. `active_danger` shows the danger tint used for
/// "muted" states. `label` is the accessible name and the hover tooltip.
pub fn icon_button(
    ui: &mut Ui,
    glyph: &str,
    label: &str,
    active_danger: bool,
    size: f32,
) -> Response {
    let enabled = ui.is_enabled();
    let (rect, response) = ui.allocate_exact_size(
        vec2(size, size),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));
    let hovered = enabled && response.hovered();
    let (fill, color) = if !enabled {
        (SURFACE, TEXT_3)
    } else if active_danger {
        (DANGER_SOFT, DANGER)
    } else if hovered {
        (SURFACE_HOVER, TEXT)
    } else {
        (SURFACE_RAISED, TEXT_2)
    };
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(6), fill);
    if enabled {
        let stroke = if active_danger { DANGER } else { BORDER };
        painter.rect_stroke(
            rect,
            CornerRadius::same(6),
            Stroke::new(1.0, stroke),
            egui::StrokeKind::Inside,
        );
    }
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        glyph,
        FontId::new(size * 0.45, icons()),
        color,
    );
    if response.has_focus() {
        focus_ring(painter, rect);
    }
    if enabled {
        response
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text(label)
    } else {
        response
    }
}

/// Solid red icon button, reserved for ending the call.
pub fn icon_button_danger(ui: &mut Ui, glyph: &str, label: &str, size: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    let fill = if response.hovered() {
        DANGER_HOVER
    } else {
        DANGER
    };
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(6), fill);
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        glyph,
        FontId::new(size * 0.45, icons()),
        Color32::WHITE,
    );
    if response.has_focus() {
        focus_ring(painter, rect);
    }
    response
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text(label)
}

/// Small rounded label, used for status, counts and key hints.
pub fn chip(ui: &mut Ui, text: &str, color: Color32, background: Color32) {
    let font = FontId::new(12.0, semibold());
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, color);
    let size = galley.size() + vec2(16.0, 8.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(4), background);
    let pos = rect.center() - galley.size() / 2.0;
    ui.painter().galley(pos, galley, color);
}

/// Width of the segmented control: two 100 px segments, a 3 px gap, 3 px padding
/// on each side and the 1 px border.
pub const SEGMENTED_WIDTH: f32 = 211.0;

/// Two-option switch (used for the list/grid view toggle). Each option is an
/// (icon, label) pair. Returns the index clicked this frame, if any.
pub fn segmented(ui: &mut Ui, options: [(&str, &str); 2], selected: usize) -> Option<usize> {
    let mut clicked = None;
    Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(3.0, 0.0);
            ui.horizontal(|ui| {
                for (index, (glyph, label)) in options.iter().enumerate() {
                    let active = index == selected;
                    let (rect, response) =
                        ui.allocate_exact_size(vec2(100.0, 30.0), Sense::click());
                    response.widget_info(|| {
                        egui::WidgetInfo::selected(egui::Role::Button, true, active, *label)
                    });
                    let (fill, ink) = if active {
                        (SURFACE_HOVER, TEXT)
                    } else if response.hovered() {
                        (SURFACE_RAISED, TEXT)
                    } else {
                        (Color32::TRANSPARENT, TEXT_2)
                    };
                    let painter = ui.painter();
                    painter.rect_filled(rect, CornerRadius::same(4), fill);
                    let icon_galley =
                        painter.layout_no_wrap(glyph.to_string(), FontId::new(15.0, icons()), ink);
                    let label_galley = painter.layout_no_wrap(
                        label.to_string(),
                        FontId::new(13.0, semibold()),
                        ink,
                    );
                    let gap = 6.0;
                    let total = icon_galley.size().x + gap + label_galley.size().x;
                    let start = rect.center().x - total / 2.0;
                    painter.galley(
                        pos2(start, rect.center().y - icon_galley.size().y / 2.0),
                        icon_galley.clone(),
                        ink,
                    );
                    painter.galley(
                        pos2(
                            start + icon_galley.size().x + gap,
                            rect.center().y - label_galley.size().y / 2.0,
                        ),
                        label_galley,
                        ink,
                    );
                    if response.has_focus() {
                        focus_ring(painter, rect);
                    }
                    if response.clicked() && !active {
                        clicked = Some(index);
                    }
                }
            });
        });
    clicked
}

/// Dark surface with a hairline border, used for every content block.
pub fn card() -> Frame {
    Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, BORDER_SOFT))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(16))
}

/// Frame for text fields, so inputs look like inputs rather than default boxes.
pub fn input_frame() -> Frame {
    Frame::new()
        .fill(SURFACE_RAISED)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(12, 9))
}

/// Bare glyph button, with no box behind it: the glyph rests in the muted text
/// color and, on hover or focus, turns fully white while it makes a partial
/// turn. Both ease in and out, and the motion settles by itself, so it only
/// answers the pointer. `label` is the accessible name and the tooltip.
/// `turn_degrees` should be a multiple of the glyph's own rotational symmetry
/// (45 for the eight-tooth gear), or the resting hover pose looks crooked.
pub fn glyph_button(
    ui: &mut Ui,
    glyph: &str,
    label: &str,
    size: f32,
    turn_degrees: f32,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    let hot = response.hovered() || response.has_focus();
    let progress = ui.ctx().animate_bool_with_time_and_easing(
        response.id,
        hot,
        0.22,
        egui::emath::easing::cubic_out,
    );
    let ink = lerp_color(TEXT_3, Color32::WHITE, progress);
    // A press leans a little further into the turn.
    let pressed = if response.is_pointer_button_down_on() {
        8.0
    } else {
        0.0
    };
    let angle = (turn_degrees * progress + pressed).to_radians();

    let galley =
        ui.painter()
            .layout_no_wrap(glyph.to_string(), FontId::new(size * 0.7, icons()), ink);
    // Rotate about the glyph's own center so it spins in place.
    let center = rect.center();
    let half = galley.size() / 2.0;
    let (sin, cos) = angle.sin_cos();
    let rotated_half = vec2(half.x * cos - half.y * sin, half.x * sin + half.y * cos);
    ui.painter()
        .add(egui::epaint::TextShape::new(center - rotated_half, galley, ink).with_angle(angle));
    if response.has_focus() {
        focus_ring(ui.painter(), rect);
    }
    response
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text(label)
}

/// Draws a server address that can be hidden. `reveal` runs from 0 (fully
/// censored) to 1 (readable) and the two states cross-fade. The censor bar is
/// exactly as wide as the text it covers, so nothing moves when it toggles,
/// and its hatching keeps it reading as a deliberate redaction, not a glitch.
/// `lift` (0 to 1) brightens the bar, for a hover. Returns the area taken.
pub fn masked_text(
    painter: &egui::Painter,
    pos: egui::Pos2,
    galley: std::sync::Arc<egui::Galley>,
    color: Color32,
    reveal: f32,
    lift: f32,
) -> egui::Rect {
    let rect = egui::Rect::from_min_size(pos, galley.size());
    let reveal = reveal.clamp(0.0, 1.0);
    if reveal > 0.0 {
        painter.galley_with_override_text_color(pos, galley, color.gamma_multiply(reveal));
    }
    let hidden = 1.0 - reveal;
    if hidden > 0.0 {
        let bar = rect.expand2(vec2(4.0, -1.0));
        let fill = lerp_color(SURFACE_RAISED, SURFACE_HOVER, lift);
        painter.rect_filled(bar, CornerRadius::same(5), fill.gamma_multiply(hidden));
        painter.rect_stroke(
            bar,
            CornerRadius::same(5),
            Stroke::new(1.0, BORDER.gamma_multiply(hidden)),
            egui::StrokeKind::Inside,
        );
        // Fine diagonal hatching, kept inside the bar.
        let clip = painter.with_clip_rect(bar.shrink(1.5));
        let ink = lerp_color(BORDER, TEXT_3, 0.35 + 0.35 * lift).gamma_multiply(hidden);
        let step = 5.0;
        let mut x = bar.left() - bar.height();
        while x < bar.right() {
            clip.line_segment(
                [pos2(x, bar.bottom()), pos2(x + bar.height(), bar.top())],
                Stroke::new(1.0, ink),
            );
            x += step;
        }
    }
    rect
}

/// Linear blend of two colors, `amount` 0 giving `from` and 1 giving `to`.
pub fn lerp_color(from: Color32, to: Color32, amount: f32) -> Color32 {
    let mix =
        |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * amount.clamp(0.0, 1.0)).round() as u8;
    Color32::from_rgb(
        mix(from.r(), to.r()),
        mix(from.g(), to.g()),
        mix(from.b(), to.b()),
    )
}

/// On/off switch. The knob slides and the track changes color together. Returns
/// the response; read `*on` after it for the new value.
pub fn toggle(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let enabled = ui.is_enabled();
    let (rect, mut response) = ui.allocate_exact_size(
        vec2(40.0, 22.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Switch, enabled, *on, label));
    let progress = ui.ctx().animate_bool_with_time(response.id, *on, 0.16);
    let (off_track, on_track) = if enabled {
        (SURFACE_HOVER, ACCENT)
    } else {
        (SURFACE_RAISED, SURFACE_HOVER)
    };
    let painter = ui.painter();
    let track = lerp_color(off_track, on_track, progress);
    painter.rect_filled(rect, CornerRadius::same(11), track);
    if !*on && progress < 0.5 {
        painter.rect_stroke(
            rect,
            CornerRadius::same(11),
            Stroke::new(1.0, BORDER),
            egui::StrokeKind::Inside,
        );
    }
    let travel = rect.width() - rect.height();
    let knob_x = rect.left() + rect.height() / 2.0 + travel * progress;
    let knob = lerp_color(TEXT_2, BG, progress);
    painter.circle_filled(
        pos2(knob_x, rect.center().y),
        rect.height() / 2.0 - 4.0,
        knob,
    );
    if response.has_focus() {
        focus_ring(painter, rect);
    }
    if enabled {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    }
}

/// Uppercase-style section label.
pub fn section_label(ui: &mut Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .font(FontId::new(12.0, medium()))
            .color(TEXT_3),
    );
}

/// Circular avatar with the name's initial, colored from a stable palette.
/// `ring` draws an accent outline, used for the speaking state.
pub fn avatar(ui: &mut Ui, name: &str, size: f32, ring: Option<Color32>) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    let painter = ui.painter();
    // A corner radius of half the size makes the square a circle.
    let corner = (size / 2.0) as u8;
    if let Some(color) = ring {
        painter.rect_stroke(
            rect.expand(2.0),
            CornerRadius::same(corner + 2),
            Stroke::new(2.0, color),
            egui::StrokeKind::Outside,
        );
    }
    painter.rect_filled(rect, CornerRadius::same(corner), avatar_color(name));
    let initial = name
        .chars()
        .next()
        .map_or("?".to_string(), |c| c.to_uppercase().collect());
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        initial,
        FontId::new(size * 0.40, semibold()),
        Color32::from_rgb(20, 22, 28),
    );
    response
}

/// Circular avatar showing a photo when there is one, the initial otherwise.
pub fn avatar_or_photo(
    ui: &mut Ui,
    name: &str,
    size: f32,
    photo: Option<&egui::TextureHandle>,
) -> Response {
    match photo {
        Some(texture) => {
            let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
            egui::Image::new((texture.id(), vec2(size, size)))
                .corner_radius(CornerRadius::same((size / 2.0) as u8))
                .paint_at(ui, rect);
            response
        }
        None => avatar(ui, name, size, None),
    }
}

/// Paints a photo as a circle filling `rect`, with an optional ring (speaking).
pub fn photo_at(ui: &Ui, rect: egui::Rect, texture: &egui::TextureHandle, ring: Option<Color32>) {
    let corner = (rect.width() / 2.0) as u8;
    if let Some(color) = ring {
        ui.painter().rect_stroke(
            rect.expand(1.5),
            CornerRadius::same(corner.saturating_add(2)),
            Stroke::new(1.5, color),
            egui::StrokeKind::Outside,
        );
    }
    egui::Image::new((texture.id(), rect.size()))
        .corner_radius(CornerRadius::same(corner))
        .paint_at(ui, rect);
}

/// Text link with an optional leading icon. The underline covers the text
/// only, and icon and text share one vertical center, so it lines up with
/// buttons of the same `height`.
pub fn link(ui: &mut Ui, glyph: Option<&str>, label: &str, height: f32) -> Response {
    let font = FontId::new(14.0, medium());
    let icon_font = FontId::new(15.0, icons());
    let painter = ui.painter();
    let text = painter.layout_no_wrap(label.to_string(), font, TEXT);
    let icon = glyph.map(|g| painter.layout_no_wrap(g.to_string(), icon_font, TEXT));
    let gap = 6.0;
    let icon_width = icon.as_ref().map_or(0.0, |g| g.size().x + gap);
    let size = vec2(icon_width + text.size().x + 4.0, height);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Link, true, label));
    let ink = if response.hovered() {
        ACCENT_HOVER
    } else {
        TEXT
    };
    let painter = ui.painter();
    let mut x = rect.left() + 2.0;
    if let Some(icon) = icon {
        painter.galley(
            pos2(x, rect.center().y - icon.size().y / 2.0),
            icon.clone(),
            ink,
        );
        x += icon.size().x + gap;
    }
    let text_top = rect.center().y - text.size().y / 2.0;
    let underline_y = (text_top + text.size().y).round() + 0.5;
    painter.galley(pos2(x, text_top), text.clone(), ink);
    painter.line_segment(
        [pos2(x, underline_y), pos2(x + text.size().x, underline_y)],
        Stroke::new(1.0, ink),
    );
    if response.has_focus() {
        focus_ring(painter, rect);
    }
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// Small avatar drawn directly with a painter, for dense lists.
pub fn avatar_tiny(painter: &egui::Painter, center: egui::Pos2, size: f32, name: &str, ring: bool) {
    let rect = egui::Rect::from_center_size(center, vec2(size, size));
    let corner = (size / 2.0) as u8;
    if ring {
        painter.rect_stroke(
            rect.expand(1.5),
            CornerRadius::same(corner + 2),
            Stroke::new(1.5, SUCCESS),
            egui::StrokeKind::Outside,
        );
    }
    painter.rect_filled(rect, CornerRadius::same(corner), avatar_color(name));
    let initial = name
        .chars()
        .next()
        .map_or("?".to_string(), |c| c.to_uppercase().collect());
    painter.text(
        center,
        Align2::CENTER_CENTER,
        initial,
        FontId::new(size * 0.45, semibold()),
        Color32::from_rgb(20, 22, 28),
    );
}

/// Stable avatar color for a name, so a participant keeps the same color.
pub fn avatar_color(name: &str) -> Color32 {
    let hash = name.bytes().fold(0usize, |acc, byte| {
        acc.wrapping_mul(31).wrapping_add(byte as usize)
    });
    AVATAR_PALETTE[hash % AVATAR_PALETTE.len()]
}

#[cfg(test)]
mod tests {
    use super::avatar_color;

    #[test]
    fn avatar_color_is_stable_for_a_name() {
        assert_eq!(avatar_color("shaha"), avatar_color("shaha"));
    }
}
