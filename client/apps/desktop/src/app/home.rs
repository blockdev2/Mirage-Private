//! The home hub: saved servers, recent connections, quick connect and the
//! local profiles. Nothing here talks to a remote account service; everything
//! lives in [`crate::store`] on this computer.

use egui::{
    pos2, vec2, Align, Align2, Color32, CornerRadius, FontId, Layout, Margin, Rect, RichText,
    Sense, Stroke, StrokeKind, UiBuilder,
};

use super::{non_empty, App};
use crate::bridge::SocketState;
use crate::protocol::Command;
use crate::store::{
    self, normalize_address, validate_address, AddressProblem, AvatarError, Favorite, HistoryEntry,
    NICKNAME_MAX_CHARS,
};
use crate::strings::Strings;
use crate::theme::{self, icon, Tone};

use std::time::Instant;

/// Widest the hub content grows on large windows.
const HOME_MAX_WIDTH: f32 = 1480.0;
const SIDE_COLUMN_WIDTH: f32 = 344.0;
const COLUMN_GAP: f32 = 24.0;
/// Below this content width the two columns stack into one.
const STACK_BREAKPOINT: f32 = 860.0;
const SEARCH_WIDTH: f32 = 300.0;
/// Content width from which the header puts search beside the title.
const HEADER_SPLIT_WIDTH: f32 = 700.0;
const RECENTS_SHOWN: usize = 5;
const ROW_BUTTON: f32 = 32.0;

/// Which screen the main area shows.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Section {
    /// The voice session (only while connected).
    Voice,
    Home,
    History,
}

/// What a connect request will record once the server answers.
pub(super) struct PendingConnect {
    pub address: String,
    pub profile_id: Option<String>,
    pub save_favorite: bool,
}

/// A modal-style window opened over the hub.
pub(super) enum Dialog {
    Profile {
        id: Option<String>,
        nickname: String,
        make_default: bool,
        first_run: bool,
        show_required: bool,
        focus: bool,
        photo: PhotoEdit,
        photo_error: Option<AvatarError>,
        photo_failed: bool,
    },
    Favorite {
        id: Option<String>,
        name: String,
        address: String,
        problem: Option<AddressProblem>,
        focus: bool,
    },
    DeleteProfile(String),
    ClearHistory,
}

/// Pending change to a profile photo inside the profile dialog.
pub(super) enum PhotoEdit {
    Keep,
    /// Prepared square photo and its preview texture.
    New(image::RgbaImage, egui::TextureHandle),
    Remove,
}

fn load_photo_texture(
    ctx: &egui::Context,
    name: &str,
    image: &image::RgbaImage,
) -> egui::TextureHandle {
    let size = [image.width() as usize, image.height() as usize];
    let pixels = egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw());
    ctx.load_texture(name, pixels, egui::TextureOptions::LINEAR)
}

/// Actions offered by a server row's menu or connect button.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RowAction {
    Connect,
    Edit,
    ToggleFavorite,
    Remove,
}

struct RowSpec<'a> {
    id: String,
    name: &'a str,
    address: &'a str,
    meta: Option<String>,
    favorite: bool,
    selected: bool,
    compact: bool,
    /// No card background: the row sits inside a shared container.
    flat: bool,
    /// Hairline above the row, between rows of a shared container.
    separator: bool,
    connect_enabled: bool,
    menu: Vec<(RowAction, &'a str)>,
    connect_label: &'a str,
    menu_label: &'a str,
}

struct RowOutcome {
    clicked: bool,
    action: Option<RowAction>,
}

fn address_message(t: &Strings, problem: AddressProblem) -> &'static str {
    match problem {
        AddressProblem::Empty => t.address_empty,
        AddressProblem::HasSpaces => t.address_spaces,
        AddressProblem::BadPort => t.address_port,
    }
}

/// One server in a list: avatar, name, address, optional meta text, a connect
/// button and an options menu. Texts are cut with an ellipsis, never overlapped.
fn list_row(ui: &mut egui::Ui, spec: &RowSpec<'_>) -> RowOutcome {
    ui.push_id(&spec.id, |ui| {
        let mut outcome = RowOutcome {
            clicked: false,
            action: None,
        };
        let height = if spec.compact { 52.0 } else { 64.0 };
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, spec.name));
        let painter = ui.painter().clone();

        let hovered = response.hovered();
        let borderless = spec.compact || spec.flat;
        let fill = if spec.selected || hovered {
            theme::SURFACE_RAISED
        } else if borderless {
            Color32::TRANSPARENT
        } else {
            theme::SURFACE
        };
        painter.rect_filled(rect, CornerRadius::same(8), fill);
        if spec.selected {
            painter.rect_stroke(
                rect,
                CornerRadius::same(8),
                Stroke::new(1.0, theme::TEXT_3),
                StrokeKind::Inside,
            );
        } else if !borderless {
            painter.rect_stroke(
                rect,
                CornerRadius::same(8),
                Stroke::new(1.0, theme::BORDER_SOFT),
                StrokeKind::Inside,
            );
        }
        if spec.separator {
            painter.line_segment(
                [
                    pos2(rect.left() + 12.0, rect.top()),
                    pos2(rect.right() - 12.0, rect.top()),
                ],
                Stroke::new(1.0, theme::BORDER_SOFT),
            );
        }
        if response.has_focus() {
            theme::focus_ring(&painter, rect);
        }

        let pad = 12.0;
        let avatar = if spec.compact { 32.0 } else { 40.0 };
        theme::avatar_tiny(
            &painter,
            pos2(rect.left() + pad + avatar / 2.0, rect.center().y),
            avatar,
            spec.name,
            false,
        );
        let text_x = rect.left() + pad + avatar + 12.0;

        // Controls on the right, measured first so the text can stop before them.
        let has_menu = !spec.menu.is_empty();
        let mut edge = rect.right() - pad;
        let mut menu_rect = None;
        if has_menu {
            menu_rect = Some(Rect::from_center_size(
                pos2(edge - ROW_BUTTON / 2.0, rect.center().y),
                vec2(ROW_BUTTON, ROW_BUTTON),
            ));
            edge -= ROW_BUTTON + 8.0;
        }
        let connect_rect = (!spec.compact).then(|| {
            let r = Rect::from_center_size(
                pos2(edge - ROW_BUTTON / 2.0, rect.center().y),
                vec2(ROW_BUTTON, ROW_BUTTON),
            );
            edge -= ROW_BUTTON + 8.0;
            r
        });

        let mut text_right = edge;
        if let Some(meta) = &spec.meta {
            let galley = painter.layout_no_wrap(
                meta.clone(),
                FontId::new(12.5, theme::medium()),
                theme::TEXT_3,
            );
            // Hide the time on narrow rows rather than squeezing the name.
            if edge - text_x - galley.size().x - 16.0 >= 140.0 {
                painter.galley(
                    pos2(
                        edge - galley.size().x,
                        rect.center().y - galley.size().y / 2.0,
                    ),
                    galley.clone(),
                    theme::TEXT_3,
                );
                text_right = edge - galley.size().x - 16.0;
            }
        }

        let room = (text_right - text_x).max(0.0);
        let name_room = if spec.favorite { room - 20.0 } else { room };
        let name = theme::fit_text(
            &painter,
            spec.name,
            FontId::new(14.0, theme::semibold()),
            theme::TEXT,
            name_room.max(0.0),
        );
        let name_size = name.size();
        painter.galley(pos2(text_x, rect.center().y - 17.0), name, theme::TEXT);
        if spec.favorite && room > 24.0 {
            painter.text(
                pos2(
                    text_x + name_size.x + 6.0,
                    rect.center().y - 17.0 + name_size.y / 2.0,
                ),
                Align2::LEFT_CENTER,
                icon::BOOKMARK_SIMPLE,
                FontId::new(13.0, theme::icons()),
                theme::TEXT_2,
            );
        }
        let address = theme::fit_text(
            &painter,
            spec.address,
            FontId::new(12.5, egui::FontFamily::Proportional),
            theme::TEXT_3,
            room,
        );
        painter.galley(pos2(text_x, rect.center().y + 1.0), address, theme::TEXT_3);

        if let Some(connect_rect) = connect_rect {
            let clicked = ui
                .scope_builder(UiBuilder::new().max_rect(connect_rect), |ui| {
                    ui.add_enabled_ui(spec.connect_enabled, |ui| {
                        theme::icon_button(
                            ui,
                            icon::ARROW_RIGHT,
                            spec.connect_label,
                            false,
                            ROW_BUTTON,
                        )
                        .clicked()
                    })
                    .inner
                })
                .inner;
            if clicked {
                outcome.action = Some(RowAction::Connect);
            }
        }
        if let Some(menu_rect) = menu_rect {
            let more = ui
                .scope_builder(UiBuilder::new().max_rect(menu_rect), |ui| {
                    theme::icon_button(ui, icon::DOTS_THREE, spec.menu_label, false, ROW_BUTTON)
                })
                .inner;
            egui::Popup::menu(&more).show(|ui| {
                ui.set_min_width(190.0);
                for (action, label) in &spec.menu {
                    let enabled = *action != RowAction::Connect || spec.connect_enabled;
                    if ui.add_enabled(enabled, egui::Button::new(*label)).clicked() {
                        outcome.action = Some(*action);
                    }
                }
            });
        }
        outcome.clicked = response.clicked() && outcome.action.is_none();
        outcome
    })
    .inner
}

/// Full-width row that opens or closes a section: icon, label, caret.
fn disclosure_row(ui: &mut egui::Ui, glyph: &str, label: &str, open: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, open, label));
    let painter = ui.painter();
    let fill = if response.hovered() {
        theme::SURFACE_HOVER
    } else {
        theme::SURFACE_RAISED
    };
    painter.rect_filled(rect, CornerRadius::same(6), fill);
    painter.rect_stroke(
        rect,
        CornerRadius::same(6),
        Stroke::new(1.0, theme::BORDER),
        StrokeKind::Inside,
    );
    if response.has_focus() {
        theme::focus_ring(painter, rect);
    }
    painter.text(
        rect.left_center() + vec2(14.0, 0.0),
        Align2::LEFT_CENTER,
        glyph,
        FontId::new(16.0, theme::icons()),
        theme::TEXT_2,
    );
    painter.text(
        rect.left_center() + vec2(40.0, 0.0),
        Align2::LEFT_CENTER,
        label,
        FontId::new(14.0, egui::FontFamily::Proportional),
        theme::TEXT,
    );
    painter.text(
        rect.right_center() - vec2(14.0, 0.0),
        Align2::RIGHT_CENTER,
        if open {
            icon::CARET_UP
        } else {
            icon::CARET_DOWN
        },
        FontId::new(14.0, theme::icons()),
        theme::TEXT_2,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Labelled single-line field in the app's input frame.
fn labeled_input(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut String,
    hint: &str,
    char_limit: usize,
    password: bool,
) -> egui::Response {
    ui.label(
        RichText::new(label)
            .font(FontId::new(13.0, theme::medium()))
            .color(theme::TEXT),
    );
    ui.add_space(4.0);
    theme::input_frame()
        .show(ui, |ui| {
            // Without this the field is as tall as a button and the text sits on top.
            ui.spacing_mut().interact_size.y = 20.0;
            ui.add(
                egui::TextEdit::singleline(value)
                    .hint_text(hint)
                    .password(password)
                    .char_limit(char_limit)
                    .frame(egui::Frame::NONE)
                    .desired_width(f32::INFINITY),
            )
        })
        .inner
}

// Sidebar -------------------------------------------------------------------

impl App {
    /// Sidebar of the hub: navigation, favorites, add server and local profile.
    pub(super) fn home_sidebar(&mut self, ui: &mut egui::Ui) {
        let t = self.t;
        self.logo(ui);
        ui.add_space(20.0);

        let section = self.effective_section();
        ui.spacing_mut().item_spacing.y = 4.0;
        if self.nav_item(ui, icon::HOUSE, t.nav_home, section == Section::Home) {
            self.section = Section::Home;
        }
        if self.nav_item(
            ui,
            icon::CLOCK_COUNTER_CLOCKWISE,
            t.nav_history,
            section == Section::History,
        ) {
            self.section = Section::History;
        }
        if self.in_session() && self.nav_item(ui, icon::HEADPHONES, t.nav_voice, false) {
            self.section = Section::Voice;
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(icon::BOOKMARK_SIMPLE)
                    .font(FontId::new(15.0, theme::icons()))
                    .color(theme::TEXT_3),
            );
            theme::section_label(ui, &t.favorites_heading.to_uppercase());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if theme::icon_button(ui, icon::PLUS, t.add_server, false, 28.0).clicked() {
                    self.open_favorite_dialog(None);
                }
            });
        });
        ui.add_space(6.0);

        // The profile row is pinned to the bottom; favorites and "Adicionar
        // servidor" use the height in between.
        let reserved = 76.0 + 44.0;
        let remaining = (ui.available_height() - reserved).max(60.0);
        let favorites = self.store.data.favorites.clone();
        egui::ScrollArea::vertical()
            .id_salt("sidebar_favorites")
            .max_height(remaining)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                if favorites.is_empty() {
                    ui.label(RichText::new(t.no_favorites).small().color(theme::TEXT_3));
                }
                for favorite in &favorites {
                    self.favorite_row(ui, favorite);
                }
            });
        ui.add_space(4.0);
        if self.nav_item(ui, icon::PLUS, t.add_server, false) {
            self.open_favorite_dialog(None);
        }

        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            self.user_card(ui);
        });
    }

    fn favorite_row(&mut self, ui: &mut egui::Ui, favorite: &Favorite) {
        let t = self.t;
        let selected = self.selected_favorite.as_deref() == Some(favorite.id.as_str());
        let spec = RowSpec {
            id: format!("fav-{}", favorite.id),
            name: &favorite.name,
            address: &favorite.address,
            meta: None,
            favorite: false,
            selected,
            compact: true,
            flat: false,
            separator: false,
            connect_enabled: self.can_connect(),
            menu: vec![
                (RowAction::Connect, t.menu_connect),
                (RowAction::Edit, t.menu_edit),
                (RowAction::ToggleFavorite, t.menu_remove_favorite),
            ],
            connect_label: t.menu_connect,
            menu_label: t.more_options,
        };
        let outcome = list_row(ui, &spec);
        if outcome.clicked {
            self.select_server(&favorite.address);
            self.section = Section::Home;
        }
        if let Some(action) = outcome.action {
            self.row_action(action, &favorite.address, &favorite.name);
        }
    }
}

// Main screens ----------------------------------------------------------------

impl App {
    pub(super) fn home_view(&mut self, ui: &mut egui::Ui) {
        let t = self.t;
        egui::ScrollArea::vertical()
            .id_salt("home")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // Keep a gutter so cards never touch the scroll bar.
                let usable = ui.available_width() - 16.0;
                let width = usable.min(HOME_MAX_WIDTH);
                let pad = ((usable - width) / 2.0).max(0.0);
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.add_space(pad);
                    ui.vertical(|ui| {
                        ui.set_width(width);
                        self.page_header(ui, t.home_title, t.home_subtitle, width, true);
                        if self.store.save_failed {
                            ui.add_space(8.0);
                            ui.label(RichText::new(t.save_failed).small().color(theme::WARNING));
                        }
                        ui.add_space(20.0);

                        if width < STACK_BREAKPOINT {
                            self.quick_connect_card(ui);
                            ui.add_space(COLUMN_GAP);
                            self.profile_card(ui);
                            ui.add_space(COLUMN_GAP);
                            self.main_column(ui);
                        } else {
                            let main = width - SIDE_COLUMN_WIDTH - COLUMN_GAP;
                            ui.horizontal_top(|ui| {
                                ui.spacing_mut().item_spacing.x = COLUMN_GAP;
                                ui.allocate_ui_with_layout(
                                    vec2(main, 0.0),
                                    Layout::top_down(Align::Min),
                                    |ui| {
                                        ui.set_width(main);
                                        self.main_column(ui);
                                    },
                                );
                                ui.allocate_ui_with_layout(
                                    vec2(SIDE_COLUMN_WIDTH, 0.0),
                                    Layout::top_down(Align::Min),
                                    |ui| {
                                        ui.set_width(SIDE_COLUMN_WIDTH);
                                        self.quick_connect_card(ui);
                                        ui.add_space(COLUMN_GAP);
                                        self.profile_card(ui);
                                    },
                                );
                            });
                        }
                        ui.add_space(8.0);
                    });
                });
            });
    }

    pub(super) fn history_view(&mut self, ui: &mut egui::Ui) {
        let t = self.t;
        egui::ScrollArea::vertical()
            .id_salt("history")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let width = ui.available_width().min(HOME_MAX_WIDTH);
                let pad = ((ui.available_width() - width) / 2.0).max(0.0);
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.add_space(pad);
                    ui.vertical(|ui| {
                        ui.set_width(width);
                        self.page_header(ui, t.history_title, t.history_subtitle, width, true);
                        ui.add_space(16.0);

                        let history = self.filtered_history();
                        if self.store.data.history.is_empty() {
                            self.empty_history(ui);
                            return;
                        }
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("{}", history.len()))
                                    .font(FontId::new(13.0, theme::medium()))
                                    .color(theme::TEXT_3),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui
                                    .add(
                                        egui::Button::new(
                                            RichText::new(t.clear_history).color(theme::DANGER),
                                        )
                                        .frame(false),
                                    )
                                    .clicked()
                                {
                                    self.dialog = Some(Dialog::ClearHistory);
                                }
                            });
                        });
                        ui.add_space(8.0);
                        if history.is_empty() {
                            ui.label(RichText::new(t.no_results).color(theme::TEXT_3));
                        }
                        ui.spacing_mut().item_spacing.y = 8.0;
                        for entry in &history {
                            self.history_row(ui, entry, "history", false);
                        }
                    });
                });
            });
    }

    /// Title and description on the left, search on the right (below on narrow
    /// windows).
    fn page_header(
        &mut self,
        ui: &mut egui::Ui,
        title: &str,
        subtitle: &str,
        width: f32,
        search: bool,
    ) {
        let split = width >= HEADER_SPLIT_WIDTH;
        let crumb = if title == self.t.history_title {
            self.t.nav_history
        } else {
            self.t.nav_home
        };
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(RichText::new("Mirage").color(theme::TEXT_3));
            ui.label(RichText::new("/").color(theme::TEXT_3));
            ui.label(RichText::new(crumb).font(FontId::new(14.0, theme::medium())));
        });
        ui.add_space(12.0);
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                if split && search {
                    ui.set_max_width((width - SEARCH_WIDTH - 16.0).max(120.0));
                }
                ui.add(
                    egui::Label::new(RichText::new(title).font(FontId::new(32.0, theme::bold())))
                        .truncate(),
                );
                ui.add(egui::Label::new(RichText::new(subtitle).color(theme::TEXT_2)).truncate());
            });
            if split && search {
                ui.add_space((ui.available_width() - SEARCH_WIDTH).max(0.0));
                self.search_box(ui, SEARCH_WIDTH);
            }
        });
        if !split && search {
            ui.add_space(12.0);
            let full = ui.available_width();
            self.search_box(ui, full);
        }
    }

    fn search_box(&mut self, ui: &mut egui::Ui, width: f32) {
        let t = self.t;
        ui.allocate_ui_with_layout(vec2(width, 40.0), Layout::top_down(Align::Min), |ui| {
            theme::input_frame().show(ui, |ui| {
                ui.set_width(width - 26.0);
                ui.spacing_mut().interact_size.y = 20.0;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    ui.label(
                        RichText::new(icon::MAGNIFYING_GLASS)
                            .font(FontId::new(16.0, theme::icons()))
                            .color(theme::TEXT_3),
                    );
                    ui.add(
                        egui::TextEdit::singleline(&mut self.search)
                            .hint_text(t.search_placeholder)
                            .frame(egui::Frame::NONE)
                            .desired_width(f32::INFINITY),
                    );
                });
            });
        });
    }

    fn filtered_history(&self) -> Vec<HistoryEntry> {
        let query = self.search.trim();
        self.store
            .data
            .history
            .iter()
            .filter(|e| {
                store::matches(
                    query,
                    &self.store.display_name(&e.address, &e.name),
                    &e.address,
                )
            })
            .cloned()
            .collect()
    }

    /// Last connection, recents, or search results.
    fn main_column(&mut self, ui: &mut egui::Ui) {
        let t = self.t;
        if !self.search.trim().is_empty() {
            self.search_results(ui);
            return;
        }
        let history = self.store.data.history.clone();
        let Some((last, rest)) = history.split_first() else {
            self.empty_history(ui);
            return;
        };

        self.last_connection_card(ui, last);
        if rest.is_empty() {
            return;
        }
        ui.add_space(COLUMN_GAP);
        ui.horizontal(|ui| {
            ui.label(RichText::new(t.recents).font(FontId::new(22.0, theme::semibold())));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let label = format!("{}  {}", icon::ARROW_SQUARE_OUT, t.view_history);
                if ui
                    .add(egui::Button::new(RichText::new(label).color(theme::TEXT_2)).frame(false))
                    .clicked()
                {
                    self.section = Section::History;
                }
            });
        });
        ui.add_space(10.0);
        // One container for all recents, rows divided by hairlines.
        theme::card()
            .inner_margin(Margin::symmetric(6, 4))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 0.0;
                for (index, entry) in rest.iter().take(RECENTS_SHOWN).enumerate() {
                    self.history_row(ui, entry, "recent", index > 0);
                }
            });
    }

    /// Matches among saved and recent servers, each server once.
    fn search_results(&mut self, ui: &mut egui::Ui) {
        let t = self.t;
        let query = self.search.trim().to_string();
        let mut found: Vec<HistoryEntry> = self.filtered_history();
        for favorite in &self.store.data.favorites {
            let known = found
                .iter()
                .any(|e| normalize_address(&e.address) == normalize_address(&favorite.address));
            if !known && store::matches(&query, &favorite.name, &favorite.address) {
                found.push(HistoryEntry {
                    address: favorite.address.clone(),
                    name: favorite.name.clone(),
                    profile_id: None,
                    last_used: 0,
                });
            }
        }
        ui.label(RichText::new(t.results).font(FontId::new(16.0, theme::semibold())));
        ui.add_space(8.0);
        if found.is_empty() {
            ui.label(RichText::new(t.no_results).color(theme::TEXT_3));
            return;
        }
        ui.spacing_mut().item_spacing.y = 8.0;
        for entry in &found {
            self.history_row(ui, entry, "result", false);
        }
    }

    fn empty_history(&mut self, ui: &mut egui::Ui) {
        let t = self.t;
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add_space(12.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(icon::CLOCK_COUNTER_CLOCKWISE)
                        .font(FontId::new(28.0, theme::icons()))
                        .color(theme::TEXT_3),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(t.no_history_title).font(FontId::new(15.0, theme::semibold())),
                );
                ui.add_space(2.0);
                ui.label(
                    RichText::new(t.no_history_body)
                        .small()
                        .color(theme::TEXT_3),
                );
            });
            ui.add_space(12.0);
        });
    }

    /// One recent server, with its favorite state and actions.
    fn history_row(
        &mut self,
        ui: &mut egui::Ui,
        entry: &HistoryEntry,
        scope: &str,
        separator: bool,
    ) {
        let t = self.t;
        let favorite = self.store.favorite_for(&entry.address).cloned();
        let name = self.store.display_name(&entry.address, &entry.name);
        let ago = store::now_secs().saturating_sub(entry.last_used);
        let selected = self.address.trim() == entry.address.trim();
        let mut menu = Vec::new();
        if favorite.is_some() {
            menu.push((RowAction::ToggleFavorite, t.menu_remove_favorite));
            menu.push((RowAction::Edit, t.menu_edit));
        } else {
            menu.push((RowAction::ToggleFavorite, t.menu_add_favorite));
        }
        if entry.last_used != 0 {
            menu.push((RowAction::Remove, t.menu_remove_history));
        }
        let spec = RowSpec {
            id: format!("{scope}-{}", normalize_address(&entry.address)),
            name: &name,
            address: &entry.address,
            meta: (entry.last_used != 0).then(|| t.last_access(ago)),
            favorite: favorite.is_some(),
            selected,
            compact: false,
            flat: scope == "recent",
            separator,
            connect_enabled: self.can_connect(),
            menu,
            connect_label: t.menu_connect,
            menu_label: t.more_options,
        };
        let outcome = list_row(ui, &spec);
        if outcome.clicked {
            self.select_server(&entry.address);
        }
        if let Some(action) = outcome.action {
            self.row_action(action, &entry.address, &name);
        }
    }

    fn last_connection_card(&mut self, ui: &mut egui::Ui, entry: &HistoryEntry) {
        let t = self.t;
        let favorite = self.store.favorite_for(&entry.address).cloned();
        let name = self.store.display_name(&entry.address, &entry.name);
        let ago = store::now_secs().saturating_sub(entry.last_used);
        let enabled = self.can_connect();

        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            theme::section_label(ui, &t.last_connection.to_uppercase());
            ui.add_space(10.0);

            let inner = ui.available_width();
            // Wide: name and address on the left, favorite state and last
            // access in their own block on the right. Narrow: stacked.
            let wide = inner >= 620.0;
            // Narrow cards stack a third text line, so they get more height
            // before the buttons.
            let info_height = if wide { 64.0 } else { 86.0 };
            let (rect, _) =
                ui.allocate_exact_size(vec2(inner, info_height + 16.0 + 40.0), Sense::hover());
            let painter = ui.painter().clone();

            let avatar = 64.0;
            theme::avatar_tiny(
                &painter,
                pos2(rect.left() + avatar / 2.0, rect.top() + info_height / 2.0),
                avatar,
                &name,
                false,
            );
            let text_x = rect.left() + avatar + 20.0;
            let button_size = 40.0;

            let block_width = if wide { 250.0 } else { 0.0 };
            let divider_x = rect.right() - block_width;
            let room = (if wide { divider_x - 20.0 } else { rect.right() } - text_x).max(0.0);
            let top = rect.top() + 6.0;
            let name_galley = theme::fit_text(
                &painter,
                &name,
                FontId::new(22.0, theme::semibold()),
                theme::TEXT,
                room,
            );
            painter.galley(pos2(text_x, top), name_galley, theme::TEXT);
            let address = theme::fit_text(
                &painter,
                &entry.address,
                FontId::new(15.0, egui::FontFamily::Proportional),
                theme::TEXT_2,
                room,
            );
            painter.galley(pos2(text_x, top + 34.0), address, theme::TEXT_2);

            let access_text = t.last_access(ago);
            if wide {
                painter.line_segment(
                    [
                        pos2(divider_x, rect.top() + 6.0),
                        pos2(divider_x, rect.top() + info_height - 6.0),
                    ],
                    Stroke::new(1.0, theme::BORDER),
                );
                let block_x = divider_x + 20.0;
                let mut line_y = rect.top() + 8.0;
                if favorite.is_some() {
                    painter.text(
                        pos2(block_x, line_y + 9.0),
                        Align2::LEFT_CENTER,
                        icon::BOOKMARK_SIMPLE,
                        FontId::new(16.0, theme::icons()),
                        theme::SUCCESS,
                    );
                    painter.text(
                        pos2(block_x + 24.0, line_y + 9.0),
                        Align2::LEFT_CENTER,
                        t.favorited,
                        FontId::new(14.0, theme::medium()),
                        theme::TEXT,
                    );
                    line_y += 28.0;
                }
                let access = theme::fit_text(
                    &painter,
                    &access_text,
                    FontId::new(13.5, egui::FontFamily::Proportional),
                    theme::TEXT_2,
                    block_width - 24.0,
                );
                painter.galley(pos2(block_x, line_y + 2.0), access, theme::TEXT_2);
            } else {
                let line = if favorite.is_some() {
                    format!("{} · {access_text}", t.favorited)
                } else {
                    access_text
                };
                let galley = theme::fit_text(
                    &painter,
                    &line,
                    FontId::new(12.5, egui::FontFamily::Proportional),
                    theme::TEXT_3,
                    room,
                );
                painter.galley(pos2(text_x, top + 60.0), galley, theme::TEXT_3);
            }

            // Buttons: "Conectar novamente" and the options button, left aligned.
            let buttons_top = rect.top() + info_height + 16.0;
            let again = Rect::from_min_size(pos2(rect.left(), buttons_top), vec2(200.0, 40.0));
            let more_rect = Rect::from_min_size(
                pos2(again.right() + 8.0, buttons_top),
                vec2(button_size, button_size),
            );
            {
                let clicked = ui
                    .scope_builder(UiBuilder::new().max_rect(again), |ui| {
                        ui.add_enabled_ui(enabled, |ui| {
                            theme::button(ui, t.connect_again, None, Tone::Primary, again.width())
                                .clicked()
                        })
                        .inner
                    })
                    .inner;
                if clicked {
                    self.row_action(RowAction::Connect, &entry.address, &name);
                }
            }
            let more = ui
                .scope_builder(UiBuilder::new().max_rect(more_rect), |ui| {
                    theme::icon_button(ui, icon::DOTS_THREE, t.more_options, false, button_size)
                })
                .inner;
            let mut chosen = None;
            egui::Popup::menu(&more).show(|ui| {
                ui.set_min_width(190.0);
                let toggle = if favorite.is_some() {
                    t.menu_remove_favorite
                } else {
                    t.menu_add_favorite
                };
                if ui.button(toggle).clicked() {
                    chosen = Some(RowAction::ToggleFavorite);
                }
                if favorite.is_some() && ui.button(t.menu_edit).clicked() {
                    chosen = Some(RowAction::Edit);
                }
                if ui.button(t.menu_remove_history).clicked() {
                    chosen = Some(RowAction::Remove);
                }
            });
            if let Some(action) = chosen {
                self.row_action(action, &entry.address, &name);
            }
        });
    }
}

// Quick connect and profile panels ----------------------------------------------

impl App {
    fn quick_connect_card(&mut self, ui: &mut egui::Ui) {
        let t = self.t;
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(t.quick_connect).font(FontId::new(22.0, theme::semibold())));
            ui.label(RichText::new(t.quick_connect_intro).color(theme::TEXT_2));
            ui.add_space(14.0);

            // Address.
            let before = self.address.clone();
            labeled_input(
                ui,
                t.server_address,
                &mut self.address,
                t.address_hint,
                255,
                false,
            );
            if self.address != before {
                self.address_problem = None;
                self.selected_favorite =
                    self.store.favorite_for(&self.address).map(|f| f.id.clone());
            }
            if let Some(problem) = self.address_problem {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(address_message(t, problem))
                        .small()
                        .color(theme::DANGER),
                );
            } else if let Some(favorite) = self
                .selected_favorite
                .as_deref()
                .and_then(|id| self.store.data.favorites.iter().find(|f| f.id == id))
            {
                ui.add_space(4.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(t.selected_favorite(&favorite.name))
                            .small()
                            .color(theme::TEXT_3),
                    )
                    .truncate(),
                );
            }
            ui.add_space(12.0);

            // Profile.
            ui.label(
                RichText::new(t.join_as)
                    .font(FontId::new(13.0, theme::medium()))
                    .color(theme::TEXT),
            );
            ui.add_space(4.0);
            let active = self.store.active_profile_id();
            let shown = active
                .as_deref()
                .and_then(|id| self.store.profile(id))
                .map_or(t.no_profile.to_string(), |p| p.nickname.clone());
            let mut chosen = None;
            egui::ComboBox::from_id_salt("profile_select")
                .selected_text(shown)
                .width(ui.available_width())
                .show_ui(ui, |ui| {
                    for profile in &self.store.data.profiles {
                        let current = active.as_deref() == Some(profile.id.as_str());
                        if ui.selectable_label(current, &profile.nickname).clicked() {
                            chosen = Some(profile.id.clone());
                        }
                    }
                });
            if let Some(id) = chosen {
                self.store.set_last_profile(&id);
                self.store.save();
            }
            ui.add_space(2.0);
            ui.label(
                RichText::new(t.profile_saved_note)
                    .small()
                    .color(theme::TEXT_3),
            );
            ui.add_space(4.0);
            let toggle_label = if self.use_other_nickname {
                t.use_saved_profile
            } else {
                t.use_other_nickname
            };
            if theme::link(ui, None, toggle_label, 24.0).clicked() {
                self.use_other_nickname = !self.use_other_nickname;
            }
            if self.use_other_nickname {
                ui.add_space(4.0);
                labeled_input(
                    ui,
                    t.other_nickname_label,
                    &mut self.other_nickname,
                    "",
                    NICKNAME_MAX_CHARS,
                    false,
                );
            }
            ui.add_space(8.0);

            // Server password, collapsed until needed. Never written to disk.
            if disclosure_row(
                ui,
                icon::LOCK_SIMPLE,
                t.server_password_toggle,
                self.show_password,
            )
            .clicked()
            {
                self.show_password = !self.show_password;
            }
            if self.show_password {
                ui.add_space(8.0);
                labeled_input(
                    ui,
                    t.server_password_hint,
                    &mut self.password,
                    "",
                    128,
                    true,
                );
            }
            ui.add_space(12.0);

            // Favorite.
            let mut already = self.store.favorite_for(&self.address).is_some();
            ui.add_enabled_ui(!already, |ui| {
                let flag = if already {
                    &mut already
                } else {
                    &mut self.save_favorite
                };
                ui.checkbox(flag, t.save_to_favorites);
            });
            ui.add_space(14.0);

            // Connect.
            let connecting = self.is_connecting();
            let blocked = self.connect_blocker();
            let label = if connecting { t.connecting } else { t.connect };
            let width = ui.available_width();
            let enabled = self.can_connect();
            let clicked = ui
                .add_enabled_ui(enabled, |ui| {
                    theme::button(ui, label, None, Tone::Primary, width).clicked()
                })
                .inner;
            if clicked {
                let address = self.address.clone();
                self.start_connect(&address);
            }
            if let Some(text) = blocked.filter(|_| !connecting) {
                ui.add_space(8.0);
                ui.label(RichText::new(text).small().color(theme::TEXT_3));
            }
            if let Some(error) = self.status.error.clone() {
                ui.add_space(10.0);
                ui.label(
                    RichText::new(t.connect_error(&error))
                        .small()
                        .color(theme::DANGER),
                );
            }
        });
    }

    fn profile_card(&mut self, ui: &mut egui::Ui) {
        let t = self.t;
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new(t.profile_panel_title).font(FontId::new(22.0, theme::semibold())),
            );
            ui.label(RichText::new(t.profile_panel_body).color(theme::TEXT_2));
            ui.add_space(14.0);
            let active = self.store.active_profile_id();
            let profile = active
                .as_deref()
                .and_then(|id| self.store.profile(id))
                .cloned();
            match profile {
                Some(profile) => {
                    let is_default = self.store.is_default_profile(&profile.id);
                    let photo = self.profile_photo(ui.ctx(), &profile.id);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        theme::avatar_or_photo(ui, &profile.nickname, 48.0, photo.as_ref());
                        ui.vertical(|ui| {
                            ui.add_space(4.0);
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&profile.nickname)
                                        .font(FontId::new(15.0, theme::semibold())),
                                )
                                .truncate(),
                            );
                            if is_default {
                                ui.label(RichText::new(t.default_profile).color(theme::TEXT_2));
                            }
                        });
                    });
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 16.0;
                        if theme::button(ui, t.edit_profile, None, Tone::Secondary, 130.0).clicked()
                        {
                            self.open_profile_dialog(Some(profile.id.clone()));
                        }
                        // Same 40 px height as the button, so both share a center line.
                        if theme::link(ui, Some(icon::PLUS), t.new_profile, 40.0).clicked() {
                            self.open_profile_dialog(None);
                        }
                    });
                }
                None => {
                    let width = ui.available_width();
                    if theme::button(ui, t.create_profile, Some(icon::PLUS), Tone::Primary, width)
                        .clicked()
                    {
                        self.open_profile_dialog(None);
                    }
                }
            }
        });
    }
}

// Behavior --------------------------------------------------------------------

impl App {
    /// Why the connect button is disabled, if it is.
    fn connect_blocker(&self) -> Option<&'static str> {
        let t = self.t;
        if self.socket != SocketState::Open {
            Some(t.bridge_offline)
        } else if self.in_session() {
            Some(t.busy_in_call)
        } else if self.store.data.profiles.is_empty() {
            Some(t.need_profile)
        } else if self.use_other_nickname && self.other_nickname.trim().is_empty() {
            Some(t.nickname_required)
        } else {
            None
        }
    }

    pub(super) fn can_connect(&self) -> bool {
        !self.is_connecting() && self.connect_blocker().is_none()
    }

    /// The nickname for the next connection: the one-off override when set,
    /// otherwise the active profile's.
    fn effective_nickname(&self) -> Option<String> {
        if self.use_other_nickname {
            return non_empty(&self.other_nickname);
        }
        let id = self.store.active_profile_id()?;
        self.store.profile(&id).and_then(|p| non_empty(&p.nickname))
    }

    /// Puts a server into the quick-connect form without connecting.
    fn select_server(&mut self, address: &str) {
        self.address = address.trim().to_string();
        self.address_problem = None;
        self.selected_favorite = self.store.favorite_for(address).map(|f| f.id.clone());
    }

    pub(super) fn start_connect(&mut self, address: &str) {
        if !self.can_connect() {
            return;
        }
        if let Err(problem) = validate_address(address) {
            self.address_problem = Some(problem);
            return;
        }
        let Some(nickname) = self.effective_nickname() else {
            return;
        };
        let address = address.trim().to_string();
        self.pending = Some(PendingConnect {
            address: address.clone(),
            profile_id: self.store.active_profile_id(),
            save_favorite: self.save_favorite,
        });
        self.connect_started = Some(Instant::now());
        // Hide the previous attempt's error while this one is running.
        self.status.error = None;
        self.send(Command::Connect {
            address,
            nickname: Some(nickname),
            server_password: non_empty(&self.password),
        });
    }

    fn row_action(&mut self, action: RowAction, address: &str, name: &str) {
        match action {
            RowAction::Connect => {
                self.select_server(address);
                self.start_connect(address);
            }
            RowAction::Edit => {
                if let Some(id) = self.store.favorite_for(address).map(|f| f.id.clone()) {
                    self.open_favorite_dialog(Some(id));
                }
            }
            RowAction::ToggleFavorite => {
                match self.store.favorite_for(address).map(|f| f.id.clone()) {
                    Some(id) => self.store.remove_favorite(&id),
                    None => {
                        self.store.add_favorite(name, address);
                    }
                }
                self.store.save();
                self.selected_favorite =
                    self.store.favorite_for(&self.address).map(|f| f.id.clone());
            }
            RowAction::Remove => {
                self.store.remove_history(address);
                self.store.save();
            }
        }
    }

    /// Records a finished connection: history, optional favorite, password cleared.
    pub(super) fn finish_connection(&mut self, server_name: &str) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        self.store.record_connection(
            &pending.address,
            server_name,
            pending.profile_id,
            store::now_secs(),
        );
        if pending.save_favorite {
            self.store.add_favorite(server_name, &pending.address);
        }
        self.store.save();
        self.password.clear();
        self.save_favorite = false;
        self.selected_favorite = self
            .store
            .favorite_for(&pending.address)
            .map(|f| f.id.clone());
    }

    /// Opens the first-run profile dialog when there is no profile at all.
    pub(super) fn ensure_profile_dialog(&mut self) {
        if self.dialog.is_none() && self.store.data.profiles.is_empty() {
            self.open_profile_dialog(None);
        }
    }

    pub(super) fn open_profile_dialog(&mut self, id: Option<String>) {
        let first_run = self.store.data.profiles.is_empty();
        let (nickname, make_default) = match id.as_deref().and_then(|id| self.store.profile(id)) {
            Some(profile) => (
                profile.nickname.clone(),
                self.store.is_default_profile(&profile.id),
            ),
            None => (String::new(), first_run),
        };
        self.dialog = Some(Dialog::Profile {
            id,
            nickname,
            make_default,
            first_run,
            show_required: false,
            focus: true,
            photo: PhotoEdit::Keep,
            photo_error: None,
            photo_failed: false,
        });
    }

    /// Texture for an image file fetched from the server (avatar or icon).
    /// Loaded once per path; a file that cannot be decoded is remembered as
    /// missing, so it is not retried every frame.
    pub(super) fn media_texture(
        &mut self,
        ctx: &egui::Context,
        path: &str,
    ) -> Option<egui::TextureHandle> {
        if let Some(texture) = self.media_textures.get(path) {
            return texture.clone();
        }
        let texture = image::ImageReader::open(path)
            .and_then(|reader| reader.with_guessed_format())
            .ok()
            .and_then(|reader| reader.decode().ok())
            .map(|image| {
                // Server avatars can be large; a card never needs more than this.
                let image = image.thumbnail(crate::store::AVATAR_SIZE, crate::store::AVATAR_SIZE);
                load_photo_texture(ctx, path, &image.to_rgba8())
            });
        self.media_textures
            .insert(path.to_string(), texture.clone());
        texture
    }

    /// Photo of a participant: our own local photo for ourselves (shown at once,
    /// before the upload round trip), otherwise the avatar the server holds.
    pub(super) fn client_photo(
        &mut self,
        ctx: &egui::Context,
        client: &crate::protocol::ClientInfo,
    ) -> Option<egui::TextureHandle> {
        if client.is_self {
            if let Some(id) = self.store.active_profile_id() {
                if let Some(photo) = self.profile_photo(ctx, &id) {
                    return Some(photo);
                }
            }
        }
        let path = client
            .uid
            .as_ref()
            .and_then(|uid| self.media_avatars.get(uid))
            .cloned()?;
        self.media_texture(ctx, &path)
    }

    /// Sends the profile's photo to the server as our avatar, or clears it.
    pub(super) fn publish_avatar(&self, profile_id: Option<&str>) {
        let path = profile_id
            .and_then(|id| self.store.avatar_path(id))
            .map(|path| path.display().to_string());
        self.send(Command::SetAvatar { path });
    }

    /// Texture of a profile's saved photo, cached until the photo changes.
    pub(super) fn profile_photo(
        &mut self,
        ctx: &egui::Context,
        id: &str,
    ) -> Option<egui::TextureHandle> {
        let rev = self.store.profile(id)?.avatar_rev;
        if let Some((cached_rev, texture)) = self.photo_cache.get(id) {
            if *cached_rev == rev {
                return texture.clone();
            }
        }
        let texture = self
            .store
            .avatar_path(id)
            .and_then(|path| image::open(path).ok())
            .map(|image| load_photo_texture(ctx, &format!("avatar-{id}-{rev}"), &image.to_rgba8()));
        self.photo_cache
            .insert(id.to_string(), (rev, texture.clone()));
        texture
    }

    fn open_favorite_dialog(&mut self, id: Option<String>) {
        let (name, address) = match id
            .as_deref()
            .and_then(|id| self.store.data.favorites.iter().find(|f| f.id == id))
        {
            Some(favorite) => (favorite.name.clone(), favorite.address.clone()),
            // "+" starts from what the quick-connect form holds.
            None => (String::new(), self.address.trim().to_string()),
        };
        self.dialog = Some(Dialog::Favorite {
            id,
            name,
            address,
            problem: None,
            focus: true,
        });
    }
}

// Dialogs -----------------------------------------------------------------------

impl App {
    pub(super) fn dialogs(&mut self, ctx: &egui::Context) {
        let t = self.t;
        let Some(mut dialog) = self.dialog.take() else {
            return;
        };
        let mut keep = true;
        let mut replace: Option<Dialog> = None;
        match &mut dialog {
            Dialog::Profile {
                id,
                nickname,
                make_default,
                first_run,
                show_required,
                focus,
                photo,
                photo_error,
                photo_failed,
            } => {
                let title = if *first_run {
                    t.profile_first_title
                } else if id.is_some() {
                    t.edit_profile
                } else {
                    t.new_profile
                };
                let mut request_delete = None;
                dialog_window(ctx, title, |ui| {
                    if *first_run {
                        ui.label(RichText::new(t.profile_first_body).color(theme::TEXT_2));
                        ui.add_space(12.0);
                    }
                    let response = labeled_input(
                        ui,
                        t.nickname_label,
                        nickname,
                        "",
                        NICKNAME_MAX_CHARS,
                        false,
                    );
                    if *focus {
                        response.request_focus();
                        *focus = false;
                    }
                    let submit =
                        response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if *show_required && nickname.trim().is_empty() {
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(t.nickname_required)
                                .small()
                                .color(theme::DANGER),
                        );
                    }
                    ui.add_space(16.0);

                    // Photo: preview, choose/change, remove. Nothing touches the
                    // disk until "Salvar".
                    let saved = id
                        .as_deref()
                        .and_then(|id| self.profile_photo(ui.ctx(), id));
                    let preview = match photo {
                        PhotoEdit::Keep => saved.clone(),
                        PhotoEdit::New(_, texture) => Some(texture.clone()),
                        PhotoEdit::Remove => None,
                    };
                    let preview_name = if nickname.trim().is_empty() {
                        "?"
                    } else {
                        nickname.as_str()
                    };
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 16.0;
                        theme::avatar_or_photo(ui, preview_name, 72.0, preview.as_ref());
                        ui.vertical(|ui| {
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 14.0;
                                let pick = if preview.is_some() {
                                    t.change_photo
                                } else {
                                    t.choose_photo
                                };
                                if theme::button(ui, pick, None, Tone::Secondary, 130.0).clicked() {
                                    if let Some(path) = rfd::FileDialog::new()
                                        .set_title(t.photo_picker_title)
                                        .add_filter(t.photo_filter, crate::store::AVATAR_EXTENSIONS)
                                        .pick_file()
                                    {
                                        match crate::store::prepare_avatar(&path) {
                                            Ok(image) => {
                                                let texture = load_photo_texture(
                                                    ui.ctx(),
                                                    "avatar-preview",
                                                    &image,
                                                );
                                                *photo = PhotoEdit::New(image, texture);
                                                *photo_error = None;
                                            }
                                            Err(error) => *photo_error = Some(error),
                                        }
                                    }
                                }
                                if preview.is_some()
                                    && theme::link(ui, None, t.remove_photo, 40.0).clicked()
                                {
                                    *photo = PhotoEdit::Remove;
                                    *photo_error = None;
                                }
                            });
                            ui.add_space(4.0);
                            ui.label(RichText::new(t.photo_hint).small().color(theme::TEXT_3));
                        });
                    });
                    if let Some(error) = *photo_error {
                        ui.add_space(6.0);
                        let text = match error {
                            AvatarError::TooLarge => t.photo_too_large,
                            AvatarError::Unreadable => t.photo_unreadable,
                        };
                        ui.label(RichText::new(text).small().color(theme::DANGER));
                    }
                    // The first profile is always the default: no choice to show.
                    if !*first_run {
                        ui.add_space(10.0);
                        ui.checkbox(make_default, t.make_default);
                    }
                    if *photo_failed {
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new(t.photo_save_failed)
                                .small()
                                .color(theme::DANGER),
                        );
                    }
                    ui.add_space(18.0);

                    let mut save = submit;
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        if theme::button(ui, t.save, None, Tone::Primary, 110.0).clicked() {
                            save = true;
                        }
                        if !*first_run
                            && theme::button(ui, t.cancel, None, Tone::Secondary, 110.0).clicked()
                        {
                            keep = false;
                        }
                        if let Some(profile_id) = id.as_ref() {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if theme::button(ui, t.delete, None, Tone::Danger, 90.0).clicked() {
                                    request_delete = Some(profile_id.clone());
                                }
                            });
                        }
                    });
                    if save {
                        if nickname.trim().is_empty() {
                            *show_required = true;
                        } else {
                            let profile_id = match id.as_deref() {
                                Some(existing) => {
                                    self.store.update_profile(existing, nickname, *make_default);
                                    self.store.set_last_profile(existing);
                                    existing.to_string()
                                }
                                None => self.store.add_profile(nickname, *make_default),
                            };
                            // One photo file per profile: a new one overwrites
                            // the old, removing deletes it.
                            let mut photo_ok = true;
                            match photo {
                                PhotoEdit::Keep => {}
                                PhotoEdit::New(image, _) => {
                                    photo_ok = self.store.set_avatar(&profile_id, image);
                                }
                                PhotoEdit::Remove => self.store.remove_avatar(&profile_id),
                            }
                            self.store.save();
                            // In a call, the new photo reaches the server at once
                            // when it belongs to the profile in use.
                            let in_use = self.store.active_profile_id().as_deref()
                                == Some(profile_id.as_str());
                            if self.in_session() && in_use && !matches!(photo, PhotoEdit::Keep) {
                                self.publish_avatar(Some(&profile_id));
                            }
                            if photo_ok {
                                keep = false;
                            } else {
                                *id = Some(profile_id);
                                *photo_error = None;
                                *photo_failed = true;
                            }
                        }
                    }
                });
                if let Some(profile_id) = request_delete {
                    replace = Some(Dialog::DeleteProfile(profile_id));
                }
            }
            Dialog::Favorite {
                id,
                name,
                address,
                problem,
                focus,
            } => {
                let title = if id.is_some() {
                    t.favorite_edit_title
                } else {
                    t.favorite_add_title
                };
                dialog_window(ctx, title, |ui| {
                    let response =
                        labeled_input(ui, t.favorite_name, name, t.favorite_name_hint, 60, false);
                    if *focus {
                        response.request_focus();
                        *focus = false;
                    }
                    ui.add_space(10.0);
                    labeled_input(ui, t.server_address, address, t.address_hint, 255, false);
                    if let Some(p) = *problem {
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(address_message(t, p))
                                .small()
                                .color(theme::DANGER),
                        );
                    }
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        if theme::button(ui, t.save, None, Tone::Primary, 110.0).clicked() {
                            match validate_address(address) {
                                Err(p) => *problem = Some(p),
                                Ok(()) => {
                                    match id.as_deref() {
                                        Some(existing) => {
                                            self.store.update_favorite(existing, name, address)
                                        }
                                        None => {
                                            self.store.add_favorite(name, address);
                                        }
                                    }
                                    self.store.save();
                                    keep = false;
                                }
                            }
                        }
                        if theme::button(ui, t.cancel, None, Tone::Secondary, 110.0).clicked() {
                            keep = false;
                        }
                    });
                });
            }
            Dialog::DeleteProfile(profile_id) => {
                let name = self
                    .store
                    .profile(profile_id)
                    .map_or(String::new(), |p| p.nickname.clone());
                dialog_window(ctx, t.delete_profile, |ui| {
                    ui.label(RichText::new(&name).font(FontId::new(15.0, theme::semibold())));
                    ui.add_space(4.0);
                    ui.label(RichText::new(t.delete_profile_body).color(theme::TEXT_2));
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        if theme::button(ui, t.delete, None, Tone::Danger, 110.0).clicked() {
                            self.store.remove_profile(profile_id);
                            self.store.save();
                            keep = false;
                        }
                        if theme::button(ui, t.cancel, None, Tone::Secondary, 110.0).clicked() {
                            keep = false;
                        }
                    });
                });
            }
            Dialog::ClearHistory => {
                dialog_window(ctx, t.clear_history, |ui| {
                    ui.label(RichText::new(t.clear_history_body).color(theme::TEXT_2));
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        if theme::button(ui, t.clear_confirm, None, Tone::Danger, 110.0).clicked() {
                            self.store.clear_history();
                            self.store.save();
                            keep = false;
                        }
                        if theme::button(ui, t.cancel, None, Tone::Secondary, 110.0).clicked() {
                            keep = false;
                        }
                    });
                });
            }
        }
        if let Some(next) = replace {
            self.dialog = Some(next);
        } else if keep {
            self.dialog = Some(dialog);
        }
    }
}

/// A centered, fixed-width window used by every dialog.
fn dialog_window(ctx: &egui::Context, title: &str, add_contents: impl FnOnce(&mut egui::Ui)) {
    let frame = egui::Frame::NONE
        .fill(theme::SURFACE)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(22))
        .shadow(egui::Shadow {
            offset: [0, 8],
            blur: 24,
            spread: 0,
            color: Color32::from_black_alpha(140),
        });
    // A real modal: the hub behind is dimmed and does not take input.
    egui::Modal::new(egui::Id::unique("mirage_dialog"))
        .frame(frame)
        .backdrop_color(Color32::from_black_alpha(170))
        .show(ctx, |ui| {
            ui.set_width(400.0);
            ui.label(RichText::new(title).font(FontId::new(20.0, theme::semibold())));
            ui.add_space(12.0);
            add_contents(ui);
        });
}
