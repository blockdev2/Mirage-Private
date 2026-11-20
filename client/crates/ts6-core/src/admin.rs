//! Server administration: channels, moderation, groups and permissions.
//!
//! Every operation is a typed [`AdminOp`] that maps to one TeamSpeak command
//! ([`AdminOp::request`]). Arguments are validated here, never passed through
//! raw, so a frontend cannot send an arbitrary command. The server decides
//! whether the client may do it: a refusal comes back as the command's error
//! (for example "insufficient client permissions").
//!
//! List commands answer with rows before the result. The library hands those
//! rows over as unknown commands; [`parse_rows`] turns them into key/value rows.

use std::collections::BTreeMap;

use serde::Deserialize;

/// One answer row: argument name to unescaped value.
pub type Row = BTreeMap<String, String>;

/// Longest free text (name, topic, reason, message) the server accepts is far
/// below this; it only bounds what a frontend can make us send.
const TEXT_MAX: usize = 512;
const PERMISSION_NAME_MAX: usize = 64;
/// A reply is cut here: the server's own permission list is a few hundred rows.
const ROWS_MAX: usize = 4_000;

/// A command to send and the reply commands to collect its rows from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub name: &'static str,
    pub args: Vec<(&'static str, String)>,
    pub replies: &'static [&'static str],
}

impl Request {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            args: Vec::new(),
            replies: &[],
        }
    }

    fn arg(mut self, key: &'static str, value: impl ToString) -> Self {
        self.args.push((key, value.to_string()));
        self
    }

    fn text(self, key: &'static str, value: &str) -> Result<Self, String> {
        Ok(self.arg(key, check_text(key, value)?))
    }

    fn opt_text(self, key: &'static str, value: &Option<String>) -> Result<Self, String> {
        match value {
            Some(value) => self.text(key, value),
            None => Ok(self),
        }
    }

    fn perm(self, value: &str) -> Result<Self, String> {
        Ok(self.arg("permsid", check_permission(value)?))
    }

    fn replies(mut self, replies: &'static [&'static str]) -> Self {
        self.replies = replies;
        self
    }
}

fn check_text<'a>(what: &str, value: &'a str) -> Result<&'a str, String> {
    if value.chars().count() > TEXT_MAX {
        return Err(format!("`{what}` is longer than {TEXT_MAX} characters"));
    }
    Ok(value)
}

/// Permission names are lower-case words joined by underscores
/// (`b_client_ban_create`); anything else is refused before it is sent.
fn check_permission(name: &str) -> Result<&str, String> {
    let ok = !name.is_empty()
        && name.len() <= PERMISSION_NAME_MAX
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if ok {
        Ok(name)
    } else {
        Err(format!("`{name}` is not a permission name"))
    }
}

fn default_true() -> bool {
    true
}

/// What a frontend may ask for. Ids are the server's own (channel id, client
/// id `clid`, client database id `db_id`, group id).
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum AdminOp {
    ChannelCreate {
        name: String,
        #[serde(default)]
        parent: Option<u64>,
        #[serde(default)]
        topic: Option<String>,
        #[serde(default)]
        password: Option<String>,
        /// Kept after the server restarts (default); otherwise it is removed
        /// when the last client leaves.
        #[serde(default = "default_true")]
        permanent: bool,
        /// `None` or a negative number: unlimited.
        #[serde(default)]
        max_clients: Option<i32>,
    },
    ChannelEdit {
        id: u64,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        topic: Option<String>,
        /// An empty string removes the password.
        #[serde(default)]
        password: Option<String>,
        #[serde(default)]
        max_clients: Option<i32>,
    },
    ChannelDelete {
        id: u64,
        /// Also remove clients and subchannels it still holds.
        #[serde(default)]
        force: bool,
    },
    ChannelMove {
        id: u64,
        parent: u64,
    },
    /// Throw a client out of its channel, or off the server.
    Kick {
        client: u16,
        #[serde(default)]
        reason: String,
        #[serde(default)]
        from_server: bool,
    },
    /// Ban a connected client; `seconds` 0 is permanent.
    Ban {
        client: u16,
        #[serde(default)]
        seconds: u64,
        #[serde(default)]
        reason: String,
    },
    BanList,
    BanRemove {
        id: u64,
    },
    /// Move another client (moving ourselves is `join`).
    MoveClient {
        client: u16,
        channel: u64,
        #[serde(default)]
        password: Option<String>,
    },
    Poke {
        client: u16,
        message: String,
    },
    ServerEdit {
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        welcome_message: Option<String>,
        #[serde(default)]
        max_clients: Option<u32>,
        #[serde(default)]
        password: Option<String>,
    },
    /// Redeem a privilege key (the first `ServerAdmin` key, or a group token).
    UseToken {
        token: String,
    },
    ServerGroups,
    /// A new regular server group (no permissions yet).
    ServerGroupCreate {
        name: String,
    },
    ServerGroupRename {
        group: u64,
        name: String,
    },
    /// `force` also removes the group from the people who still have it.
    ServerGroupDelete {
        group: u64,
        #[serde(default)]
        force: bool,
    },
    ChannelGroups,
    ServerGroupMembers {
        group: u64,
    },
    ClientServerGroups {
        db_id: u64,
    },
    ServerGroupAddClient {
        group: u64,
        db_id: u64,
    },
    ServerGroupRemoveClient {
        group: u64,
        db_id: u64,
    },
    SetChannelGroup {
        group: u64,
        channel: u64,
        db_id: u64,
    },
    /// Every permission the server knows (name and description).
    PermissionList,
    ServerGroupPermissions {
        group: u64,
    },
    ServerGroupSetPermission {
        group: u64,
        permission: String,
        value: i32,
        #[serde(default)]
        negated: bool,
        #[serde(default)]
        skip: bool,
    },
    ServerGroupRemovePermission {
        group: u64,
        permission: String,
    },
    ChannelPermissions {
        channel: u64,
    },
    ChannelSetPermission {
        channel: u64,
        permission: String,
        value: i32,
    },
    ChannelRemovePermission {
        channel: u64,
        permission: String,
    },
    ChannelGroupPermissions {
        group: u64,
    },
    ChannelGroupSetPermission {
        group: u64,
        permission: String,
        value: i32,
    },
    ChannelGroupRemovePermission {
        group: u64,
        permission: String,
    },
    ClientPermissions {
        db_id: u64,
    },
    ClientSetPermission {
        db_id: u64,
        permission: String,
        value: i32,
        #[serde(default)]
        skip: bool,
    },
    ClientRemovePermission {
        db_id: u64,
        permission: String,
    },
}

/// `channel_maxclients` and its "unlimited" flag for a channel command.
fn with_max_clients(request: Request, max_clients: Option<i32>) -> Request {
    match max_clients {
        None => request,
        Some(n) if n < 0 => request.arg("channel_flag_maxclients_unlimited", 1),
        Some(n) => request
            .arg("channel_flag_maxclients_unlimited", 0)
            .arg("channel_maxclients", n),
    }
}

impl AdminOp {
    /// The command to send for this operation, or why it is refused locally.
    pub fn request(&self) -> Result<Request, String> {
        use AdminOp::*;
        Ok(match self {
            ChannelCreate {
                name,
                parent,
                topic,
                password,
                permanent,
                max_clients,
            } => {
                if name.trim().is_empty() {
                    return Err("a channel needs a name".into());
                }
                let request = Request::new("channelcreate")
                    .text("channel_name", name)?
                    .opt_text("channel_topic", topic)?
                    .opt_text("channel_password", password)?
                    .arg("channel_flag_permanent", u8::from(*permanent))
                    .arg("channel_flag_semi_permanent", u8::from(!*permanent));
                let request = match parent {
                    Some(parent) => request.arg("cpid", parent),
                    None => request,
                };
                with_max_clients(request, *max_clients)
            }
            ChannelEdit {
                id,
                name,
                topic,
                password,
                max_clients,
            } => {
                if name.as_deref().is_some_and(|n| n.trim().is_empty()) {
                    return Err("a channel needs a name".into());
                }
                let request = Request::new("channeledit")
                    .arg("cid", id)
                    .opt_text("channel_name", name)?
                    .opt_text("channel_topic", topic)?
                    .opt_text("channel_password", password)?;
                with_max_clients(request, *max_clients)
            }
            ChannelDelete { id, force } => Request::new("channeldelete")
                .arg("cid", id)
                .arg("force", u8::from(*force)),
            ChannelMove { id, parent } => Request::new("channelmove")
                .arg("cid", id)
                .arg("cpid", parent)
                .arg("order", 0),
            Kick {
                client,
                reason,
                from_server,
            } => Request::new("clientkick")
                .arg("clid", client)
                // 4: out of the channel, 5: off the server.
                .arg("reasonid", if *from_server { 5 } else { 4 })
                .text("reasonmsg", reason)?,
            Ban {
                client,
                seconds,
                reason,
            } => Request::new("banclient")
                .arg("clid", client)
                .arg("time", seconds)
                .text("banreason", reason)?,
            BanList => Request::new("banlist").replies(&["notifybanlist"]),
            BanRemove { id } => Request::new("bandel").arg("banid", id),
            MoveClient {
                client,
                channel,
                password,
            } => Request::new("clientmove")
                .arg("clid", client)
                .arg("cid", channel)
                .opt_text("cpw", password)?,
            Poke { client, message } => Request::new("clientpoke")
                .arg("clid", client)
                .text("msg", message)?,
            ServerEdit {
                name,
                welcome_message,
                max_clients,
                password,
            } => {
                let mut request = Request::new("serveredit")
                    .opt_text("virtualserver_name", name)?
                    .opt_text("virtualserver_welcomemessage", welcome_message)?
                    .opt_text("virtualserver_password", password)?;
                if let Some(max) = max_clients {
                    request = request.arg("virtualserver_maxclients", max);
                }
                request
            }
            UseToken { token } => Request::new("privilegekeyuse").text("token", token)?,
            ServerGroups => Request::new("servergrouplist").replies(&["notifyservergrouplist"]),
            ServerGroupCreate { name } => {
                if name.trim().is_empty() {
                    return Err("a group needs a name".into());
                }
                // 1: a regular group (0 is a template, 2 a query group).
                Request::new("servergroupadd")
                    .text("name", name)?
                    .arg("type", 1)
            }
            ServerGroupRename { group, name } => {
                if name.trim().is_empty() {
                    return Err("a group needs a name".into());
                }
                Request::new("servergrouprename")
                    .arg("sgid", group)
                    .text("name", name)?
            }
            ServerGroupDelete { group, force } => Request::new("servergroupdel")
                .arg("sgid", group)
                .arg("force", u8::from(*force)),
            ChannelGroups => Request::new("channelgrouplist").replies(&["notifychannelgrouplist"]),
            ServerGroupMembers { group } => Request::new("servergroupclientlist")
                .arg("sgid", group)
                .arg("-names", "")
                .replies(&["notifyservergroupclientlist"]),
            ClientServerGroups { db_id } => Request::new("servergroupsbyclientid")
                .arg("cldbid", db_id)
                .replies(&["notifyservergroupsbyclientid"]),
            ServerGroupAddClient { group, db_id } => Request::new("servergroupaddclient")
                .arg("sgid", group)
                .arg("cldbid", db_id),
            ServerGroupRemoveClient { group, db_id } => Request::new("servergroupdelclient")
                .arg("sgid", group)
                .arg("cldbid", db_id),
            SetChannelGroup {
                group,
                channel,
                db_id,
            } => Request::new("setclientchannelgroup")
                .arg("cgid", group)
                .arg("cid", channel)
                .arg("cldbid", db_id),
            PermissionList => Request::new("permissionlist").replies(&["notifypermissionlist"]),
            ServerGroupPermissions { group } => Request::new("servergrouppermlist")
                .arg("sgid", group)
                .arg("-permsid", "")
                .replies(&["notifyservergrouppermlist"]),
            ServerGroupSetPermission {
                group,
                permission,
                value,
                negated,
                skip,
            } => Request::new("servergroupaddperm")
                .arg("sgid", group)
                .perm(permission)?
                .arg("permvalue", value)
                .arg("permnegated", u8::from(*negated))
                .arg("permskip", u8::from(*skip)),
            ServerGroupRemovePermission { group, permission } => Request::new("servergroupdelperm")
                .arg("sgid", group)
                .perm(permission)?,
            ChannelPermissions { channel } => Request::new("channelpermlist")
                .arg("cid", channel)
                .arg("-permsid", "")
                .replies(&["notifychannelpermlist"]),
            ChannelSetPermission {
                channel,
                permission,
                value,
            } => Request::new("channeladdperm")
                .arg("cid", channel)
                .perm(permission)?
                .arg("permvalue", value),
            ChannelRemovePermission {
                channel,
                permission,
            } => Request::new("channeldelperm")
                .arg("cid", channel)
                .perm(permission)?,
            ChannelGroupPermissions { group } => Request::new("channelgrouppermlist")
                .arg("cgid", group)
                .arg("-permsid", "")
                .replies(&["notifychannelgrouppermlist"]),
            ChannelGroupSetPermission {
                group,
                permission,
                value,
            } => Request::new("channelgroupaddperm")
                .arg("cgid", group)
                .perm(permission)?
                .arg("permvalue", value),
            ChannelGroupRemovePermission { group, permission } => {
                Request::new("channelgroupdelperm")
                    .arg("cgid", group)
                    .perm(permission)?
            }
            ClientPermissions { db_id } => Request::new("clientpermlist")
                .arg("cldbid", db_id)
                .arg("-permsid", "")
                .replies(&["notifyclientpermlist"]),
            ClientSetPermission {
                db_id,
                permission,
                value,
                skip,
            } => Request::new("clientaddperm")
                .arg("cldbid", db_id)
                .perm(permission)?
                .arg("permvalue", value)
                .arg("permskip", u8::from(*skip)),
            ClientRemovePermission { db_id, permission } => Request::new("clientdelperm")
                .arg("cldbid", db_id)
                .perm(permission)?,
        })
    }
}

/// Turns one reply command (`name a=1 b=x|a=2 b=y`) into rows.
pub fn parse_rows(content: &str) -> Vec<Row> {
    use tsproto_packets::commands::{CommandItem, CommandParser};

    let (_, items) = CommandParser::new(content.as_bytes());
    let mut rows = Vec::new();
    let mut current = Row::new();
    for item in items {
        match item {
            CommandItem::NextCommand => {
                if rows.len() >= ROWS_MAX {
                    break;
                }
                rows.push(std::mem::take(&mut current));
            }
            CommandItem::Argument(argument) => {
                let key = String::from_utf8_lossy(argument.name()).into_owned();
                let value = argument
                    .value()
                    .get_str()
                    .map(|v| v.into_owned())
                    .unwrap_or_default();
                current.insert(key, value);
            }
        }
    }
    rows.push(current);
    rows.retain(|row| !row.is_empty());
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(json: &str) -> AdminOp {
        serde_json::from_str(json).unwrap()
    }

    fn arg<'a>(request: &'a Request, key: &str) -> Option<&'a str> {
        request
            .args
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn rows_are_split_on_pipes_and_unescaped() {
        let rows = parse_rows(
            "servergrouplist sgid=6 name=Server\\sAdmin type=1|sgid=7 name=Normal\\pGuest type=1",
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["name"], "Server Admin");
        assert_eq!(rows[1]["sgid"], "7");
        assert_eq!(rows[1]["name"], "Normal|Guest");
        assert!(parse_rows("servergrouplist").is_empty());
    }

    #[test]
    fn creating_a_channel_sends_flags_and_limits() {
        let request =
            op(r#"{"action":"channel_create","name":"Sala","parent":3,"max_clients":-1}"#)
                .request()
                .unwrap();
        assert_eq!(request.name, "channelcreate");
        assert_eq!(arg(&request, "channel_name"), Some("Sala"));
        assert_eq!(arg(&request, "cpid"), Some("3"));
        assert_eq!(arg(&request, "channel_flag_permanent"), Some("1"));
        assert_eq!(
            arg(&request, "channel_flag_maxclients_unlimited"),
            Some("1")
        );
        assert_eq!(arg(&request, "channel_maxclients"), None);
        let temporary = op(r#"{"action":"channel_create","name":"Temp","permanent":false}"#)
            .request()
            .unwrap();
        assert_eq!(arg(&temporary, "channel_flag_semi_permanent"), Some("1"));
        assert_eq!(arg(&temporary, "cpid"), None);
    }

    #[test]
    fn server_groups_can_be_created_renamed_and_deleted() {
        let create = op(r#"{"action":"server_group_create","name":"Moderadores"}"#)
            .request()
            .unwrap();
        assert_eq!(create.name, "servergroupadd");
        assert_eq!(arg(&create, "name"), Some("Moderadores"));
        assert_eq!(arg(&create, "type"), Some("1"));
        let rename = op(r#"{"action":"server_group_rename","group":9,"name":"Mods"}"#)
            .request()
            .unwrap();
        assert_eq!(rename.name, "servergrouprename");
        assert_eq!(arg(&rename, "sgid"), Some("9"));
        let delete = op(r#"{"action":"server_group_delete","group":9,"force":true}"#)
            .request()
            .unwrap();
        assert_eq!(delete.name, "servergroupdel");
        assert_eq!(arg(&delete, "force"), Some("1"));
        assert_eq!(
            arg(
                &op(r#"{"action":"server_group_delete","group":9}"#)
                    .request()
                    .unwrap(),
                "force"
            ),
            Some("0")
        );
        for json in [
            r#"{"action":"server_group_create","name":"  "}"#,
            r#"{"action":"server_group_rename","group":1,"name":""}"#,
        ] {
            assert!(op(json).request().is_err(), "{json}");
        }
    }

    #[test]
    fn a_persons_channel_group_is_set_per_channel() {
        let set = op(r#"{"action":"set_channel_group","group":5,"channel":4,"db_id":7}"#)
            .request()
            .unwrap();
        assert_eq!(set.name, "setclientchannelgroup");
        assert_eq!(arg(&set, "cgid"), Some("5"));
    }

    #[test]
    fn kick_picks_the_reason_id_for_channel_or_server() {
        let channel = op(r#"{"action":"kick","client":5,"reason":"calma"}"#)
            .request()
            .unwrap();
        assert_eq!(arg(&channel, "reasonid"), Some("4"));
        let server = op(r#"{"action":"kick","client":5,"from_server":true}"#)
            .request()
            .unwrap();
        assert_eq!(arg(&server, "reasonid"), Some("5"));
    }

    #[test]
    fn list_operations_name_the_reply_to_collect() {
        let groups = op(r#"{"action":"server_groups"}"#).request().unwrap();
        assert_eq!(groups.name, "servergrouplist");
        assert_eq!(groups.replies, &["notifyservergrouplist"]);
        let perms = op(r#"{"action":"channel_permissions","channel":9}"#)
            .request()
            .unwrap();
        assert_eq!(arg(&perms, "cid"), Some("9"));
        assert_eq!(perms.replies, &["notifychannelpermlist"]);
    }

    #[test]
    fn permission_names_and_text_are_checked_before_sending() {
        let ok = op(
            r#"{"action":"server_group_set_permission","group":6,"permission":"b_client_ban_create","value":1}"#,
        )
        .request()
        .unwrap();
        assert_eq!(ok.name, "servergroupaddperm");
        assert_eq!(arg(&ok, "permsid"), Some("b_client_ban_create"));
        assert_eq!(arg(&ok, "permnegated"), Some("0"));
        for bad in ["", "B_Upper", "has space", "semi;colon", &"a".repeat(65)] {
            let json = serde_json::json!({
                "action": "server_group_remove_permission", "group": 1, "permission": bad
            });
            let result = serde_json::from_value::<AdminOp>(json).unwrap().request();
            assert!(result.is_err(), "{bad:?} was accepted");
        }
        let long = serde_json::json!({"action": "poke", "client": 1, "message": "x".repeat(513)});
        assert!(serde_json::from_value::<AdminOp>(long)
            .unwrap()
            .request()
            .is_err());
        assert!(op(r#"{"action":"channel_create","name":"  "}"#)
            .request()
            .is_err());
    }

    #[test]
    fn unknown_actions_do_not_parse() {
        assert!(serde_json::from_str::<AdminOp>(r#"{"action":"rm_rf"}"#).is_err());
        assert!(serde_json::from_str::<AdminOp>(r#"{"action":"kick"}"#).is_err());
    }
}
