//! Server administration in the window: moderation of people (poke, kick,
//! ban), channel management, a channel password prompt, and the window with
//! server groups, their permissions and the ban list.
//!
//! Every action is a bridge `admin` command; the server decides whether we may
//! do it, and its refusal is shown in plain Portuguese. This text lives here,
//! beside the screens it belongs to.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant};

use egui::{Align2, CornerRadius, FontId, Margin, RichText, Stroke};
use serde_json::{json, Value};

use super::home::dialog_window;
use super::peer::menu_item;
use super::App;
use crate::protocol::{ChannelInfo, Command};
use crate::strings::Strings;
use crate::theme::{self, icon, Tone};

pub(super) type Row = BTreeMap<String, String>;

const TOAST_SECS: u64 = 6;
/// More than this many unanswered requests means the bridge is not answering.
const PENDING_MAX: usize = 64;

/// How long a ban lasts, in the order of the dialog's list.
const BAN_CHOICES: [(&str, u64); 4] = [
    ("1 hora", 3_600),
    ("1 dia", 86_400),
    ("1 semana", 604_800),
    ("Para sempre", 0),
];

/// What to do with the answer to a request.
pub(super) enum Purpose {
    /// Say this when it worked.
    Done(&'static str),
    /// Say this, then read the group's permissions again.
    Edited(u64, &'static str),
    Groups,
    Catalog,
    GroupPermissions(u64),
    Bans,
    /// The server's regular groups, for a person's group dialog.
    DialogGroups,
    /// The groups this person (by database id) belongs to.
    PersonGroups(u64),
    /// Say this, then read the person's groups again.
    MemberChanged(u64, &'static str),
}

pub(super) enum AdminDialog {
    /// The server groups one person belongs to, each with a check box.
    Memberships {
        db_id: u64,
        name: String,
        groups: Vec<Row>,
        /// `None` until the server has answered.
        member: Option<HashSet<u64>>,
    },
    Poke {
        client: u16,
        name: String,
        text: String,
    },
    Kick {
        client: u16,
        name: String,
        from_server: bool,
        reason: String,
    },
    Ban {
        client: u16,
        name: String,
        choice: usize,
        reason: String,
    },
    ChannelCreate {
        parent: Option<u64>,
        parent_name: Option<String>,
        name: String,
        password: String,
    },
    ChannelEdit {
        id: u64,
        name: String,
        topic: String,
        password: String,
    },
    ChannelDelete {
        id: u64,
        name: String,
    },
    JoinPassword {
        channel: u64,
        name: String,
        password: String,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Groups,
    Bans,
}

pub(super) struct Panel {
    tab: Tab,
    groups: Vec<Row>,
    group: Option<u64>,
    permissions: Vec<Row>,
    bans: Vec<Row>,
    filter: String,
    new_name: String,
    new_value: i32,
}

struct Toast {
    text: String,
    error: bool,
    at: Instant,
}

#[derive(Default)]
pub(super) struct Admin {
    next: u64,
    pending: HashMap<String, Purpose>,
    pub dialog: Option<AdminDialog>,
    pub panel: Option<Panel>,
    toast: Option<Toast>,
    /// Permission id to name, once the server's list was read.
    catalog: HashMap<u32, String>,
    catalog_asked: bool,
}

fn field(row: &Row, key: &str) -> String {
    row.get(key).cloned().unwrap_or_default()
}

fn number(row: &Row, key: &str) -> Option<u64> {
    row.get(key).and_then(|v| v.parse().ok())
}

/// The permission number a refusal names ("missing permission Permission(24)").
fn missing_permission(raw: &str) -> Option<u32> {
    let tail = raw.split("Permission(").nth(1)?;
    tail.split(')').next()?.parse().ok()
}

impl Admin {
    /// Plain Portuguese for a refusal, naming the permission when known.
    fn explain(&self, raw: &str) -> String {
        if raw.contains("PermissionsClientInsufficient") || raw.contains("Insufficient") {
            return match missing_permission(raw) {
                Some(id) => match self.catalog.get(&id) {
                    Some(name) => format!("Sem permissão para isso (falta {name})."),
                    None => format!("Sem permissão para isso (permissão nº {id})."),
                },
                None => "Sem permissão para isso.".to_string(),
            };
        }
        for (code, text) in [
            (
                "PrivilegeKeyInvalid",
                "Chave de privilégio inválida ou já usada.",
            ),
            ("ChannelNameInuse", "Já existe um canal com esse nome."),
            ("ChannelInvalidName", "Esse nome de canal não é válido."),
            ("ClientInvalidId", "Essa pessoa não está mais no servidor."),
            ("ChannelInvalidId", "Esse canal não existe mais."),
            (
                "ParameterInvalidSize",
                "Um dos textos é curto ou longo demais.",
            ),
        ] {
            if raw.contains(code) {
                return text.to_string();
            }
        }
        if raw.contains("not connected") || raw.contains("Not connected") {
            return "Você não está conectado.".to_string();
        }
        format!("O servidor recusou: {raw}")
    }
}

impl App {
    /// Sends one administration operation; `args` must be a JSON object.
    pub(super) fn admin_send(&mut self, purpose: Purpose, action: &'static str, args: Value) {
        if self.admin.pending.len() >= PENDING_MAX {
            self.admin.pending.clear();
        }
        let request_id = format!("adm{}", self.admin.next);
        self.admin.next += 1;
        self.admin.pending.insert(request_id.clone(), purpose);
        self.send(Command::Admin {
            request_id,
            action,
            args,
        });
    }

    fn toast(&mut self, text: impl Into<String>, error: bool) {
        self.admin.toast = Some(Toast {
            text: text.into(),
            error,
            at: Instant::now(),
        });
    }

    /// The answer to one `admin` command.
    pub(super) fn on_admin_result(
        &mut self,
        request_id: &str,
        error: Option<String>,
        rows: Vec<Row>,
    ) {
        let Some(purpose) = self.admin.pending.remove(request_id) else {
            return;
        };
        if let Some(raw) = error {
            let text = self.admin.explain(&raw);
            // An empty list comes back as an error on some servers.
            let empty_list = raw.contains("EmptyResultSet");
            match purpose {
                Purpose::Groups
                | Purpose::Catalog
                | Purpose::GroupPermissions(_)
                | Purpose::Bans
                | Purpose::DialogGroups
                | Purpose::PersonGroups(_)
                    if empty_list =>
                {
                    self.fill(purpose, Vec::new());
                }
                _ => self.toast(text, true),
            }
            return;
        }
        match purpose {
            Purpose::Done(text) => self.toast(text, false),
            Purpose::Edited(group, text) => {
                self.toast(text, false);
                self.admin_send(
                    Purpose::GroupPermissions(group),
                    "server_group_permissions",
                    json!({ "group": group }),
                );
            }
            Purpose::MemberChanged(db_id, text) => {
                self.toast(text, false);
                self.admin_send(
                    Purpose::PersonGroups(db_id),
                    "client_server_groups",
                    json!({ "db_id": db_id }),
                );
            }
            other => self.fill(other, rows),
        }
    }

    /// Opens the group dialog of one person and asks for what it shows.
    pub(super) fn open_memberships(&mut self, db_id: u64, name: String) {
        self.admin.dialog = Some(AdminDialog::Memberships {
            db_id,
            name,
            groups: Vec::new(),
            member: None,
        });
        self.admin_send(Purpose::DialogGroups, "server_groups", json!({}));
        self.admin_send(
            Purpose::PersonGroups(db_id),
            "client_server_groups",
            json!({ "db_id": db_id }),
        );
    }

    fn fill(&mut self, purpose: Purpose, rows: Vec<Row>) {
        match purpose {
            Purpose::Catalog => {
                self.admin.catalog = rows
                    .iter()
                    .filter_map(|row| {
                        let id = number(row, "permid")? as u32;
                        let name = row.get("permname").or_else(|| row.get("permsid"))?;
                        Some((id, name.clone()))
                    })
                    .collect();
            }
            Purpose::Groups => {
                if let Some(panel) = self.admin.panel.as_mut() {
                    panel.groups = rows;
                    panel
                        .groups
                        .sort_by_key(|g| number(g, "sortid").unwrap_or(0));
                }
            }
            Purpose::GroupPermissions(group) => {
                if let Some(panel) = self.admin.panel.as_mut() {
                    if panel.group == Some(group) {
                        panel.permissions = rows;
                        panel.permissions.sort_by_key(|p| field(p, "permsid"));
                    }
                }
            }
            Purpose::Bans => {
                if let Some(panel) = self.admin.panel.as_mut() {
                    panel.bans = rows;
                }
            }
            Purpose::DialogGroups => {
                if let Some(AdminDialog::Memberships { groups, .. }) = self.admin.dialog.as_mut() {
                    // Type 1 is a regular group; templates and query groups
                    // are not assigned by hand.
                    *groups = rows
                        .into_iter()
                        .filter(|g| g.get("type").is_none_or(|t| t == "1"))
                        .collect();
                    groups.sort_by_key(|g| number(g, "sortid").unwrap_or(0));
                }
            }
            Purpose::PersonGroups(db_id) => {
                if let Some(AdminDialog::Memberships {
                    db_id: open,
                    member,
                    ..
                }) = self.admin.dialog.as_mut()
                {
                    if *open == db_id {
                        *member = Some(rows.iter().filter_map(|g| number(g, "sgid")).collect());
                    }
                }
            }
            Purpose::Done(_) | Purpose::Edited(..) | Purpose::MemberChanged(..) => {}
        }
    }

    /// The right-click menu of a channel: create a subchannel, edit, delete.
    pub(super) fn channel_menu(&mut self, response: &egui::Response, channel: &ChannelInfo) {
        let mut chosen: Option<AdminDialog> = None;
        egui::Popup::context_menu(response).show(|ui| {
            ui.set_width(220.0);
            if menu_item(ui, icon::PLUS, "Criar subcanal").clicked() {
                chosen = Some(AdminDialog::ChannelCreate {
                    parent: Some(channel.id),
                    parent_name: Some(channel.name.clone()),
                    name: String::new(),
                    password: String::new(),
                });
                ui.close();
            }
            if menu_item(ui, icon::PLUS, "Criar canal no topo").clicked() {
                chosen = Some(AdminDialog::ChannelCreate {
                    parent: None,
                    parent_name: None,
                    name: String::new(),
                    password: String::new(),
                });
                ui.close();
            }
            if menu_item(ui, icon::PENCIL, "Editar canal").clicked() {
                chosen = Some(AdminDialog::ChannelEdit {
                    id: channel.id,
                    name: channel.name.clone(),
                    topic: channel.topic.clone().unwrap_or_default(),
                    password: String::new(),
                });
                ui.close();
            }
            if menu_item(ui, icon::TRASH, "Excluir canal").clicked() {
                chosen = Some(AdminDialog::ChannelDelete {
                    id: channel.id,
                    name: channel.name.clone(),
                });
                ui.close();
            }
        });
        if chosen.is_some() {
            self.admin.dialog = chosen;
        }
    }

    /// Opens the groups, permissions and bans window.
    pub(super) fn open_admin_window(&mut self) {
        self.admin.panel = Some(Panel {
            tab: Tab::Groups,
            groups: Vec::new(),
            group: None,
            permissions: Vec::new(),
            bans: Vec::new(),
            filter: String::new(),
            new_name: String::new(),
            new_value: 1,
        });
        self.admin_send(Purpose::Groups, "server_groups", json!({}));
        if !self.admin.catalog_asked {
            self.admin.catalog_asked = true;
            self.admin_send(Purpose::Catalog, "permission_list", json!({}));
        }
    }

    /// Forget everything about the server when the session ends.
    pub(super) fn admin_reset(&mut self) {
        self.admin = Admin::default();
    }

    /// Dialogs and the toast; call once per frame.
    pub(super) fn admin_ui(&mut self, ctx: &egui::Context) {
        self.admin_dialogs(ctx);
        self.admin_window(ctx);
        self.admin_toast(ctx);
    }

    fn admin_toast(&mut self, ctx: &egui::Context) {
        let Some(toast) = &self.admin.toast else {
            return;
        };
        if toast.at.elapsed() > Duration::from_secs(TOAST_SECS) {
            self.admin.toast = None;
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(500));
        let ink = if toast.error {
            theme::DANGER
        } else {
            theme::SUCCESS
        };
        egui::Area::new(egui::Id::NULL.with("admin-toast"))
            .anchor(Align2::CENTER_BOTTOM, egui::vec2(0.0, -96.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(theme::SURFACE_RAISED)
                    .stroke(Stroke::new(1.0, ink))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::symmetric(16, 10))
                    .show(ui, |ui| {
                        ui.label(RichText::new(&toast.text).color(theme::TEXT));
                    });
            });
    }

    fn admin_dialogs(&mut self, ctx: &egui::Context) {
        let t = self.t;
        let Some(mut dialog) = self.admin.dialog.take() else {
            return;
        };
        let mut keep = true;
        let mut send: Option<(Purpose, &'static str, Value)> = None;
        let mut join: Option<(u64, String)> = None;
        match &mut dialog {
            AdminDialog::Memberships {
                db_id,
                name,
                groups,
                member,
            } => {
                dialog_window(ctx, &format!("Grupos de {name}"), |ui| {
                    ui.set_min_width(300.0);
                    match member {
                        None => {
                            ui.label(RichText::new("Carregando…").color(theme::TEXT_3));
                        }
                        Some(member) => {
                            egui::ScrollArea::vertical()
                                .max_height(300.0)
                                .show(ui, |ui| {
                                    for group in groups.iter() {
                                        let (Some(id), label) =
                                            (number(group, "sgid"), field(group, "name"))
                                        else {
                                            continue;
                                        };
                                        let mut on = member.contains(&id);
                                        if ui.checkbox(&mut on, label).changed() {
                                            let (action, text) = if on {
                                                (
                                                    "server_group_add_client",
                                                    "Pessoa adicionada ao grupo.",
                                                )
                                            } else {
                                                (
                                                    "server_group_remove_client",
                                                    "Pessoa removida do grupo.",
                                                )
                                            };
                                            send = Some((
                                                Purpose::MemberChanged(*db_id, text),
                                                action,
                                                json!({ "group": id, "db_id": *db_id }),
                                            ));
                                        }
                                    }
                                });
                        }
                    }
                    ui.add_space(12.0);
                    if theme::button(ui, "Fechar", None, Tone::Secondary, 110.0).clicked() {
                        keep = false;
                    }
                });
            }
            AdminDialog::Poke { client, name, text } => {
                dialog_window(ctx, &format!("Cutucar {name}"), |ui| {
                    ui.label("Mensagem");
                    first_field(ui, egui::TextEdit::singleline(text));
                    ui.add_space(12.0);
                    if buttons(ui, t, "Cutucar", Tone::Primary, &mut keep) {
                        send = Some((
                            Purpose::Done("Cutucada enviada."),
                            "poke",
                            json!({ "client": *client, "message": text.trim() }),
                        ));
                    }
                });
            }
            AdminDialog::Kick {
                client,
                name,
                from_server,
                reason,
            } => {
                let title = if *from_server {
                    format!("Expulsar {name} do servidor")
                } else {
                    format!("Expulsar {name} do canal")
                };
                dialog_window(ctx, &title, |ui| {
                    ui.label("Motivo (opcional)");
                    first_field(ui, egui::TextEdit::singleline(reason));
                    ui.add_space(12.0);
                    if buttons(ui, t, "Expulsar", Tone::Danger, &mut keep) {
                        send = Some((
                            Purpose::Done("Pessoa expulsa."),
                            "kick",
                            json!({
                                "client": *client,
                                "reason": reason.trim(),
                                "from_server": *from_server,
                            }),
                        ));
                    }
                });
            }
            AdminDialog::Ban {
                client,
                name,
                choice,
                reason,
            } => {
                dialog_window(ctx, &format!("Banir {name}"), |ui| {
                    ui.label("Por quanto tempo");
                    egui::ComboBox::from_id_salt("ban-length")
                        .selected_text(BAN_CHOICES[*choice].0)
                        .show_ui(ui, |ui| {
                            for (index, (label, _)) in BAN_CHOICES.iter().enumerate() {
                                ui.selectable_value(choice, index, *label);
                            }
                        });
                    ui.add_space(8.0);
                    ui.label("Motivo (opcional)");
                    ui.add(egui::TextEdit::singleline(reason).desired_width(f32::INFINITY));
                    ui.add_space(12.0);
                    if buttons(ui, t, "Banir", Tone::Danger, &mut keep) {
                        send = Some((
                            Purpose::Done("Pessoa banida."),
                            "ban",
                            json!({
                                "client": *client,
                                "seconds": BAN_CHOICES[*choice].1,
                                "reason": reason.trim(),
                            }),
                        ));
                    }
                });
            }
            AdminDialog::ChannelCreate {
                parent,
                parent_name,
                name,
                password,
            } => {
                let title = match parent_name {
                    Some(parent) => format!("Novo canal em {parent}"),
                    None => "Novo canal".to_string(),
                };
                dialog_window(ctx, &title, |ui| {
                    ui.label("Nome");
                    first_field(ui, egui::TextEdit::singleline(name));
                    ui.add_space(8.0);
                    ui.label("Senha (opcional)");
                    ui.add(
                        egui::TextEdit::singleline(password)
                            .password(true)
                            .desired_width(f32::INFINITY),
                    );
                    ui.add_space(12.0);
                    let go = buttons(ui, t, "Criar", Tone::Primary, &mut keep);
                    if go && name.trim().is_empty() {
                        keep = true; // a channel needs a name
                    } else if go {
                        let mut args = json!({ "name": name.trim() });
                        if let Some(parent) = parent {
                            args["parent"] = json!(parent);
                        }
                        if !password.is_empty() {
                            args["password"] = json!(password);
                        }
                        send = Some((Purpose::Done("Canal criado."), "channel_create", args));
                    }
                });
            }
            AdminDialog::ChannelEdit {
                id,
                name,
                topic,
                password,
            } => {
                dialog_window(ctx, "Editar canal", |ui| {
                    ui.label("Nome");
                    first_field(ui, egui::TextEdit::singleline(name));
                    ui.add_space(8.0);
                    ui.label("Tópico");
                    ui.add(egui::TextEdit::singleline(topic).desired_width(f32::INFINITY));
                    ui.add_space(8.0);
                    ui.label("Nova senha (vazio mantém a atual)");
                    ui.add(
                        egui::TextEdit::singleline(password)
                            .password(true)
                            .desired_width(f32::INFINITY),
                    );
                    ui.add_space(12.0);
                    let go = buttons(ui, t, t.save, Tone::Primary, &mut keep);
                    if go && name.trim().is_empty() {
                        keep = true; // a channel needs a name
                    } else if go {
                        let mut args =
                            json!({ "id": *id, "name": name.trim(), "topic": topic.trim() });
                        if !password.is_empty() {
                            args["password"] = json!(password);
                        }
                        send = Some((Purpose::Done("Canal atualizado."), "channel_edit", args));
                    }
                });
            }
            AdminDialog::ChannelDelete { id, name } => {
                dialog_window(ctx, "Excluir canal", |ui| {
                    ui.label(format!(
                        "Excluir \"{name}\" e tudo que há dentro dele? Isso não pode ser desfeito."
                    ));
                    ui.add_space(12.0);
                    if buttons(ui, t, t.delete, Tone::Danger, &mut keep) {
                        send = Some((
                            Purpose::Done("Canal excluído."),
                            "channel_delete",
                            json!({ "id": *id, "force": true }),
                        ));
                    }
                });
            }
            AdminDialog::JoinPassword {
                channel,
                name,
                password,
            } => {
                dialog_window(ctx, &format!("Entrar em {name}"), |ui| {
                    ui.label("Este canal tem senha.");
                    first_field(ui, egui::TextEdit::singleline(password).password(true));
                    ui.add_space(12.0);
                    if buttons(ui, t, "Entrar", Tone::Primary, &mut keep) {
                        join = Some((*channel, password.clone()));
                    }
                });
            }
        }
        if let Some((purpose, action, args)) = send {
            self.admin_send(purpose, action, args);
        }
        if let Some((channel, password)) = join {
            self.send(Command::Join {
                channel: channel.to_string(),
                password: Some(password),
            });
        }
        if keep {
            self.admin.dialog = Some(dialog);
        }
    }

    fn admin_window(&mut self, ctx: &egui::Context) {
        let Some(mut panel) = self.admin.panel.take() else {
            return;
        };
        let mut open = true;
        let mut requests: Vec<(Purpose, &'static str, Value)> = Vec::new();
        egui::Window::new("Administração do servidor")
            .open(&mut open)
            .collapsible(false)
            .default_size([780.0, 480.0])
            .max_size([1000.0, 640.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for (tab, label) in [
                        (Tab::Groups, "Grupos e permissões"),
                        (Tab::Bans, "Banimentos"),
                    ] {
                        if ui.selectable_label(panel.tab == tab, label).clicked() {
                            panel.tab = tab;
                            if tab == Tab::Bans {
                                requests.push((Purpose::Bans, "ban_list", json!({})));
                            }
                        }
                    }
                });
                ui.separator();
                match panel.tab {
                    Tab::Groups => self.groups_tab(ui, &mut panel, &mut requests),
                    Tab::Bans => bans_tab(ui, &panel, &mut requests),
                }
            });
        for (purpose, action, args) in requests {
            self.admin_send(purpose, action, args);
        }
        if open {
            self.admin.panel = Some(panel);
        }
    }

    fn groups_tab(
        &self,
        ui: &mut egui::Ui,
        panel: &mut Panel,
        requests: &mut Vec<(Purpose, &'static str, Value)>,
    ) {
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(210.0, 380.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("admin-groups")
                        .show(ui, |ui| {
                            if panel.groups.is_empty() {
                                ui.label(RichText::new("Nenhum grupo lido.").color(theme::TEXT_3));
                            }
                            for group in &panel.groups {
                                let Some(id) = number(group, "sgid") else {
                                    continue;
                                };
                                // Template groups (type 0) and query groups (2) are not ours to edit.
                                let regular = field(group, "type") == "1";
                                let label = if regular {
                                    field(group, "name")
                                } else {
                                    format!("{} (modelo)", field(group, "name"))
                                };
                                if ui
                                    .selectable_label(panel.group == Some(id), label)
                                    .clicked()
                                {
                                    panel.group = Some(id);
                                    panel.permissions.clear();
                                    requests.push((
                                        Purpose::GroupPermissions(id),
                                        "server_group_permissions",
                                        json!({ "group": id }),
                                    ));
                                }
                            }
                        });
                },
            );
            ui.separator();
            ui.vertical(|ui| {
                ui.set_min_width(500.0);
                let Some(group) = panel.group else {
                    ui.label(
                        RichText::new("Escolha um grupo para ver as permissões.")
                            .color(theme::TEXT_3),
                    );
                    return;
                };
                ui.horizontal(|ui| {
                    ui.label("Filtrar");
                    ui.add(egui::TextEdit::singleline(&mut panel.filter).desired_width(180.0));
                });
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .id_salt("admin-perms")
                    .max_height(280.0)
                    .show(ui, |ui| {
                        let filter = panel.filter.trim().to_lowercase();
                        egui::Grid::new("admin-perm-grid")
                            .num_columns(3)
                            .spacing([12.0, 6.0])
                            .show(ui, |ui| {
                                for row in &panel.permissions {
                                    let name = field(row, "permsid");
                                    if !filter.is_empty() && !name.to_lowercase().contains(&filter)
                                    {
                                        continue;
                                    }
                                    ui.label(&name);
                                    let mut value: i32 =
                                        field(row, "permvalue").parse().unwrap_or(0);
                                    let before = value;
                                    let response = ui.add(egui::DragValue::new(&mut value));
                                    if value != before
                                        && (response.drag_stopped() || response.lost_focus())
                                    {
                                        requests.push((
                                            Purpose::Edited(group, "Permissão alterada."),
                                            "server_group_set_permission",
                                            json!({
                                                "group": group,
                                                "permission": name,
                                                "value": value,
                                                "negated": field(row, "permnegated") == "1",
                                                "skip": field(row, "permskip") == "1",
                                            }),
                                        ));
                                    }
                                    if ui.small_button("Remover").clicked() {
                                        requests.push((
                                            Purpose::Edited(group, "Permissão removida."),
                                            "server_group_remove_permission",
                                            json!({ "group": group, "permission": name }),
                                        ));
                                    }
                                    ui.end_row();
                                }
                            });
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut panel.new_name)
                            .hint_text("nome da permissão, por exemplo b_client_kick_from_channel")
                            .desired_width(300.0),
                    );
                    ui.add(egui::DragValue::new(&mut panel.new_value));
                    let name = panel.new_name.trim().to_lowercase();
                    let known = self.admin.catalog.is_empty()
                        || self.admin.catalog.values().any(|n| *n == name);
                    let can = !name.is_empty() && known;
                    if ui.add_enabled(can, egui::Button::new("Definir")).clicked() {
                        requests.push((
                            Purpose::Edited(group, "Permissão definida."),
                            "server_group_set_permission",
                            json!({ "group": group, "permission": name, "value": panel.new_value }),
                        ));
                        panel.new_name.clear();
                    }
                });
            });
        });
    }
}

fn bans_tab(ui: &mut egui::Ui, panel: &Panel, requests: &mut Vec<(Purpose, &'static str, Value)>) {
    if panel.bans.is_empty() {
        ui.label(RichText::new("Nenhum banimento.").color(theme::TEXT_3));
        return;
    }
    egui::ScrollArea::vertical()
        .id_salt("admin-bans")
        .show(ui, |ui| {
            for ban in &panel.bans {
                let Some(id) = number(ban, "banid") else {
                    continue;
                };
                let who = [field(ban, "ip"), field(ban, "name"), field(ban, "uid")]
                    .into_iter()
                    .find(|s| !s.is_empty())
                    .unwrap_or_else(|| format!("#{id}"));
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(who)
                            .font(FontId::new(14.0, theme::semibold()))
                            .color(theme::TEXT),
                    );
                    let reason = field(ban, "reason");
                    if !reason.is_empty() {
                        ui.label(RichText::new(reason).color(theme::TEXT_2));
                    }
                    let seconds = number(ban, "duration").unwrap_or(0);
                    ui.label(
                        RichText::new(if seconds == 0 {
                            "para sempre".to_string()
                        } else {
                            format!("{} min", seconds.div_ceil(60))
                        })
                        .color(theme::TEXT_3),
                    );
                    if ui.small_button("Remover").clicked() {
                        requests.push((
                            Purpose::Done("Banimento removido."),
                            "ban_remove",
                            json!({ "id": id }),
                        ));
                        requests.push((Purpose::Bans, "ban_list", json!({})));
                    }
                });
            }
        });
}

/// A full-width text field that takes the keyboard when nothing else has it,
/// so a dialog can be typed into the moment it opens.
fn first_field(ui: &mut egui::Ui, field: egui::TextEdit<'_>) {
    let response = ui.add(field.desired_width(f32::INFINITY));
    if ui.memory(|m| m.focused().is_none()) {
        response.request_focus();
    }
}

/// Cancel and confirm buttons; true when confirmed. Cancel clears `keep`.
fn buttons(ui: &mut egui::Ui, t: &Strings, confirm: &str, tone: Tone, keep: &mut bool) -> bool {
    let mut confirmed = false;
    ui.horizontal(|ui| {
        if theme::button(ui, confirm, None, tone, 120.0).clicked() {
            confirmed = true;
            *keep = false;
        }
        if theme::button(ui, t.cancel, None, Tone::Secondary, 110.0).clicked() {
            *keep = false;
        }
    });
    confirmed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_missing_permission_is_read_from_a_refusal() {
        let raw = "server rejected `servergrouppermlist`: PermissionsClientInsufficient (missing permission Permission(24))";
        assert_eq!(missing_permission(raw), Some(24));
        assert_eq!(missing_permission("boom"), None);
    }

    #[test]
    fn a_refusal_names_the_permission_when_the_list_is_known() {
        let raw = "server rejected `x`: PermissionsClientInsufficient (missing permission Permission(24))";
        let mut admin = Admin::default();
        assert!(admin.explain(raw).contains("nº 24"));
        admin
            .catalog
            .insert(24, "b_virtualserver_servergroup_permission_list".into());
        assert!(admin
            .explain(raw)
            .contains("b_virtualserver_servergroup_permission_list"));
        assert!(admin.explain("other").starts_with("O servidor recusou"));
    }

    #[test]
    fn the_ban_lengths_end_with_forever() {
        assert_eq!(BAN_CHOICES[3].1, 0);
        assert!(BAN_CHOICES.windows(2).take(2).all(|w| w[0].1 < w[1].1));
    }
}
