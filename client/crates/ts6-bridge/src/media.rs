//! Avatars and the server icon: fetched over the server's file transfer and
//! cached on disk, so the UI receives file paths instead of image bytes (the
//! WebSocket is limited to 16 KiB per message).
//!
//! The transfer itself is a separate TCP connection. Reading or writing it runs
//! in a spawned task; when it ends, the task reports back through the bridge's
//! own command channel, which the session loop already polls. No new receiver
//! competes with the session poll.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::protocol::Command;
use ts6_core::attachments::{self, ChannelFile, ServerRef, MAX_ATTACHMENT_BYTES};
use ts6_core::media::{avatar_flag, avatar_path, icon_path, MAX_MEDIA_BYTES};
use ts6_core::{Session, Snapshot};

/// Transfers in flight at once; the rest wait for the next state change.
const MAX_IN_FLIGHT: usize = 4;
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(20);
/// Cached files untouched for this long are removed at startup.
const CACHE_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 3600);

/// What a transfer is for.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// Avatar of the client with this uid, at this flag (MD5).
    Avatar {
        uid: String,
        hash: String,
    },
    /// myTeamSpeak avatar of the client with this uid, at this HTTPS address.
    Web {
        uid: String,
        url: String,
    },
    ServerIcon(u32),
}

impl Target {
    /// Cache file name. Only hex/digits reach the file system.
    fn file_name(&self) -> Option<String> {
        match self {
            Target::Avatar { hash, .. } => {
                let safe = hash.chars().all(|c| c.is_ascii_hexdigit()) && !hash.is_empty();
                safe.then(|| format!("avatar_{}", hash.to_ascii_lowercase()))
            }
            Target::ServerIcon(id) => Some(format!("icon_{id}")),
            // Named by a hash of the address, so a new picture is a new file.
            Target::Web { url, .. } => Some(format!("myts_{}", avatar_flag(url.as_bytes()))),
        }
    }
}

pub struct Media {
    dir: Option<PathBuf>,
    /// Avatar file by client uid.
    avatars: HashMap<String, PathBuf>,
    server_icon: Option<PathBuf>,
    in_flight: HashMap<u16, Target>,
    /// Targets already tried in this session, so a failure is not retried in a loop.
    attempted: HashSet<Target>,
    /// Bytes waiting for the server to accept an upload, by handle.
    uploads: HashMap<u16, (Vec<u8>, String)>,
    /// Chat attachments waiting for the server to accept their upload.
    chat_uploads: HashMap<u16, PendingAttachment>,
    /// Chat attachment downloads in flight: (channel, server path, cache file).
    chat_downloads: HashMap<u16, (u64, String, PathBuf)>,
}

/// An outgoing chat attachment between the upload request and the message.
struct PendingAttachment {
    bytes: Vec<u8>,
    request_id: String,
    channel: u64,
    message: String,
}

/// Why the server refused an attachment upload. Default servers give the
/// guest group download power but no upload power, so a permission refusal
/// is common and deserves its own explanation.
pub fn upload_refusal(error: &str) -> (&'static str, &'static str) {
    if error.contains("Permission") {
        (
            "upload_permission",
            "This server does not let you upload files to this channel. Ask an admin for upload permission.",
        )
    } else {
        (
            "upload_refused",
            "The server refused the file (quota, size limit or file transfer disabled).",
        )
    }
}

/// Which outstanding request a failed transfer belonged to.
pub enum Failed {
    Avatar(String),
    Attachment {
        request_id: String,
        /// Stable code and English message for `chat_result`.
        refusal: (&'static str, &'static str),
    },
    ChatFile {
        channel: u64,
        path: String,
    },
    Unknown,
}

impl Media {
    pub fn new() -> Self {
        let dir = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from))
            .map(|base| base.join("Mirage").join("cache").join("media"));
        if let Some(dir) = &dir {
            prune(dir);
        }
        Self {
            dir,
            avatars: HashMap::new(),
            server_icon: None,
            in_flight: HashMap::new(),
            attempted: HashSet::new(),
            uploads: HashMap::new(),
            chat_uploads: HashMap::new(),
            chat_downloads: HashMap::new(),
        }
    }

    /// Forget everything tied to the previous connection.
    pub fn reset(&mut self) {
        self.avatars.clear();
        self.server_icon = None;
        self.in_flight.clear();
        self.attempted.clear();
        self.uploads.clear();
        self.chat_uploads.clear();
        self.chat_downloads.clear();
    }

    pub fn avatars(&self) -> HashMap<String, String> {
        self.avatars
            .iter()
            .map(|(uid, path)| (uid.clone(), path.display().to_string()))
            .collect()
    }

    pub fn server_icon(&self) -> Option<String> {
        self.server_icon.as_ref().map(|p| p.display().to_string())
    }

    /// Requests whatever the snapshot shows that is not cached yet. Returns true
    /// when the set of known files changed (something was found in the cache).
    pub fn sync(
        &mut self,
        session: &mut Session,
        snapshot: &Snapshot,
        done: &mpsc::Sender<Command>,
    ) -> bool {
        let Some(dir) = self.dir.clone() else {
            return false;
        };
        let mut changed = false;
        let mut wanted = Vec::new();

        // Drop avatars of clients that left or removed their picture.
        let before = self.avatars.len();
        self.avatars.retain(|uid, _| {
            snapshot.clients.iter().any(|c| {
                c.uid.as_deref() == Some(uid.as_str())
                    && (c.avatar_hash.is_some() || c.myteamspeak_avatar.is_some())
            })
        });
        changed |= self.avatars.len() != before;

        for client in &snapshot.clients {
            let Some(uid) = &client.uid else {
                continue;
            };
            // A file-transfer avatar wins; official TS6 clients usually only
            // announce their myTeamSpeak picture.
            let target = match (&client.avatar_hash, &client.myteamspeak_avatar) {
                (Some(hash), _) => Target::Avatar {
                    uid: uid.clone(),
                    hash: hash.clone(),
                },
                (None, Some(url)) => Target::Web {
                    uid: uid.clone(),
                    url: url.clone(),
                },
                (None, None) => continue,
            };
            let Some(name) = target.file_name() else {
                continue;
            };
            let path = dir.join(&name);
            if path.is_file() {
                if self.avatars.get(uid) != Some(&path) {
                    self.avatars.insert(uid.clone(), path);
                    changed = true;
                }
            } else {
                wanted.push(target);
            }
        }

        let icon = snapshot.server.icon_id;
        if icon_path(icon).is_some() {
            let target = Target::ServerIcon(icon);
            if let Some(name) = target.file_name() {
                let path = dir.join(name);
                if path.is_file() {
                    if self.server_icon.as_ref() != Some(&path) {
                        self.server_icon = Some(path);
                        changed = true;
                    }
                } else {
                    wanted.push(target);
                }
            }
        } else if self.server_icon.take().is_some() {
            changed = true;
        }

        for target in wanted {
            if self.in_flight.len() >= MAX_IN_FLIGHT {
                break;
            }
            if self.attempted.contains(&target) || self.in_flight.values().any(|t| t == &target) {
                continue;
            }
            if let Target::Web { url, .. } = &target {
                self.attempted.insert(target.clone());
                fetch_web(url.clone(), target.clone(), &dir, done);
                continue;
            }
            let remote = match &target {
                Target::Avatar { uid, .. } => avatar_path(uid),
                Target::ServerIcon(id) => icon_path(*id),
                Target::Web { .. } => None,
            };
            let Some(remote) = remote else {
                self.attempted.insert(target);
                continue;
            };
            self.attempted.insert(target.clone());
            match session.request_download(&remote) {
                Ok(handle) => {
                    self.in_flight.insert(handle, target);
                }
                Err(error) => tracing::debug!(%error, "download request failed"),
            }
        }
        changed
    }

    /// The server opened a download: read it in a task that reports back.
    pub fn on_download(
        &mut self,
        handle: u16,
        size: u64,
        stream: tokio::net::TcpStream,
        done: &mpsc::Sender<Command>,
    ) {
        if let Some((channel, path, cache)) = self.chat_downloads.remove(&handle) {
            let done = done.clone();
            tokio::spawn(async move {
                let saved = tokio::time::timeout(
                    TRANSFER_TIMEOUT,
                    receive(stream, size, MAX_ATTACHMENT_BYTES, &cache),
                )
                .await
                .ok()
                .and_then(Result::ok)
                .map(|()| cache.display().to_string());
                let _ = done
                    .send(Command::ChatFileDone {
                        channel,
                        path,
                        file: saved,
                    })
                    .await;
            });
            return;
        }
        let Some(target) = self.in_flight.remove(&handle) else {
            return;
        };
        let (Some(dir), Some(name)) = (self.dir.clone(), target.file_name()) else {
            return;
        };
        let done = done.clone();
        tokio::spawn(async move {
            let path = dir.join(name);
            let saved = tokio::time::timeout(
                TRANSFER_TIMEOUT,
                receive(stream, size, MAX_MEDIA_BYTES, &path),
            )
            .await
            .ok()
            .and_then(Result::ok)
            .map(|()| path.display().to_string());
            let _ = done
                .send(Command::MediaDone {
                    target,
                    path: saved,
                })
                .await;
        });
    }

    pub fn on_failed(&mut self, handle: u16, error: &str) -> Failed {
        if let Some(target) = self.in_flight.remove(&handle) {
            tracing::debug!(?target, %error, "file download failed");
            return Failed::Unknown;
        }
        if let Some(pending) = self.chat_uploads.remove(&handle) {
            tracing::debug!(%error, "chat attachment upload refused");
            return Failed::Attachment {
                request_id: pending.request_id,
                refusal: upload_refusal(error),
            };
        }
        if let Some((channel, path, _)) = self.chat_downloads.remove(&handle) {
            tracing::debug!(%error, "chat attachment download refused");
            return Failed::ChatFile { channel, path };
        }
        match self.uploads.remove(&handle) {
            Some(_) => Failed::Avatar(format!("avatar upload failed: {error}")),
            None => Failed::Unknown,
        }
    }

    /// A finished download, reported by the reading task.
    pub fn on_done(&mut self, target: Target, path: Option<String>) -> bool {
        let Some(path) = path.map(PathBuf::from) else {
            return false;
        };
        match target {
            Target::Avatar { uid, .. } | Target::Web { uid, .. } => {
                self.avatars.insert(uid, path);
            }
            Target::ServerIcon(_) => self.server_icon = Some(path),
        }
        true
    }

    /// Starts publishing our avatar: the server answers with an upload stream.
    pub fn start_upload(&mut self, session: &mut Session, file: &Path) -> anyhow::Result<()> {
        let bytes = std::fs::read(file)?;
        if bytes.is_empty() || bytes.len() as u64 > MAX_MEDIA_BYTES {
            anyhow::bail!("avatar file is empty or larger than 2 MiB");
        }
        let flag = avatar_flag(&bytes);
        let handle = session.request_upload("/avatar", bytes.len() as u64)?;
        self.uploads.insert(handle, (bytes, flag));
        Ok(())
    }

    /// Starts sending a chat attachment: uploads it to the channel's file
    /// transfer, then (in `on_upload`) asks the loop to post the message.
    pub fn start_attachment(
        &mut self,
        session: &mut Session,
        channel: u64,
        file: &Path,
        request_id: String,
    ) -> anyhow::Result<()> {
        let size = std::fs::metadata(file)?.len();
        if size == 0 || size > MAX_ATTACHMENT_BYTES {
            anyhow::bail!("attachments must be between 1 byte and 5 MB");
        }
        let bytes = std::fs::read(file)?;
        if bytes.len() as u64 != size {
            anyhow::bail!("file changed while it was being read");
        }
        let (kind, _) = attachments::sniff(&bytes);
        let display = file
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let name = attachments::upload_name(&unique_token(), &display)
            .ok_or_else(|| anyhow::anyhow!("unusable file name"))?;
        let target = ChannelFile {
            channel_id: channel,
            dir: "/".into(),
            name,
            size: Some(size),
        };
        let (host, port) = split_address(session.address());
        let server = ServerRef {
            host,
            port,
            unique_id: session.server_unique_id().unwrap_or_default(),
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        let message = attachments::compose(kind, &target, &server, now);
        let handle = session.request_channel_upload(channel, &target.path(), size)?;
        self.chat_uploads.insert(
            handle,
            PendingAttachment {
                bytes,
                request_id,
                channel,
                message,
            },
        );
        Ok(())
    }

    /// Fetches a chat attachment into the cache. Returns the cached file at
    /// once when it is already there; otherwise the result arrives later as
    /// `Command::ChatFileDone`.
    pub fn fetch_chat_file(
        &mut self,
        session: &mut Session,
        file: &ChannelFile,
    ) -> anyhow::Result<Option<String>> {
        let dir = self
            .dir
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no cache directory"))?;
        let path = file.path();
        // Keyed by server, channel and path, so equal names on two servers
        // never collide. Only hex reaches the file system.
        let key = format!(
            "{}|{}|{path}",
            session.server_unique_id().unwrap_or_default(),
            file.channel_id
        );
        let cache = dir.join(format!("chat_{}", avatar_flag(key.as_bytes())));
        if cache.is_file() {
            return Ok(Some(cache.display().to_string()));
        }
        let already = self
            .chat_downloads
            .values()
            .any(|(channel, p, _)| *channel == file.channel_id && *p == path);
        if !already {
            if self.chat_downloads.len() >= MAX_IN_FLIGHT * 2 {
                anyhow::bail!("too many attachment downloads in flight");
            }
            let handle = session.request_channel_download(file.channel_id, &path)?;
            self.chat_downloads
                .insert(handle, (file.channel_id, path, cache));
        }
        Ok(None)
    }

    /// The server accepted an upload: write it in a task that reports back.
    pub fn on_upload(
        &mut self,
        handle: u16,
        mut stream: tokio::net::TcpStream,
        done: &mpsc::Sender<Command>,
    ) {
        if let Some(pending) = self.chat_uploads.remove(&handle) {
            let done = done.clone();
            tokio::spawn(async move {
                let sent = tokio::time::timeout(TRANSFER_TIMEOUT, async {
                    stream.write_all(&pending.bytes).await?;
                    stream.shutdown().await
                })
                .await
                .is_ok_and(|result| result.is_ok());
                let _ = done
                    .send(Command::AttachmentSent {
                        request_id: pending.request_id,
                        channel: pending.channel,
                        message: sent.then_some(pending.message),
                    })
                    .await;
            });
            return;
        }
        let Some((bytes, flag)) = self.uploads.remove(&handle) else {
            return;
        };
        let done = done.clone();
        tokio::spawn(async move {
            let sent = tokio::time::timeout(TRANSFER_TIMEOUT, async {
                stream.write_all(&bytes).await?;
                stream.shutdown().await
            })
            .await
            .is_ok_and(|result| result.is_ok());
            let _ = done
                .send(Command::AvatarUploaded {
                    flag: sent.then_some(flag),
                })
                .await;
        });
    }
}

/// A short unique token for attachment names (time plus a process counter).
fn unique_token() -> String {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let counter = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{:x}{counter:x}", nanos & 0xffff_ffff_ffff)
}

/// `host:port` (or `[v6]:port`); TeamSpeak's default voice port otherwise.
fn split_address(address: &str) -> (String, u16) {
    if let Some(rest) = address.strip_prefix('[') {
        if let Some((host, tail)) = rest.split_once(']') {
            let port = tail.strip_prefix(':').and_then(|p| p.parse().ok());
            return (host.to_string(), port.unwrap_or(9987));
        }
    }
    match address.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => {
            (host.to_string(), port.parse().unwrap_or(9987))
        }
        _ => (address.to_string(), 9987),
    }
}

/// Reads exactly `size` bytes into `path` through a temporary file.
async fn receive(
    mut stream: tokio::net::TcpStream,
    size: u64,
    limit: u64,
    path: &Path,
) -> std::io::Result<()> {
    if size == 0 || size > limit {
        return Err(std::io::Error::other("unexpected media size"));
    }
    let mut bytes = Vec::with_capacity(size as usize);
    (&mut stream).take(size).read_to_end(&mut bytes).await?;
    if bytes.len() as u64 != size {
        return Err(std::io::Error::other("short transfer"));
    }
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let temporary = path.with_extension("part");
    tokio::fs::write(&temporary, &bytes).await?;
    tokio::fs::rename(&temporary, path).await
}

/// Fetches a myTeamSpeak avatar over HTTPS in a task that reports back. The
/// address was already restricted to the avatar bucket by the core.
fn fetch_web(url: String, target: Target, dir: &Path, done: &mpsc::Sender<Command>) {
    let Some(name) = target.file_name() else {
        return;
    };
    let path = dir.join(name);
    let done = done.clone();
    tokio::spawn(async move {
        let saved = match tokio::time::timeout(TRANSFER_TIMEOUT, fetch(&url, &path)).await {
            Ok(Ok(())) => Some(path.display().to_string()),
            Ok(Err(error)) => {
                tracing::debug!(%error, "myTeamSpeak avatar fetch failed");
                None
            }
            Err(_) => None,
        };
        let _ = done
            .send(Command::MediaDone {
                target,
                path: saved,
            })
            .await;
    });
}

/// Downloads an HTTPS image with a size cap and no redirects.
async fn fetch(url: &str, path: &Path) -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(TRANSFER_TIMEOUT)
        .build()?;
    let mut response = client.get(url).send().await?.error_for_status()?;
    if response
        .content_length()
        .is_some_and(|len| len > MAX_MEDIA_BYTES)
    {
        anyhow::bail!("avatar too large");
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        bytes.extend_from_slice(&chunk);
        if bytes.len() as u64 > MAX_MEDIA_BYTES {
            anyhow::bail!("avatar too large");
        }
    }
    if bytes.is_empty() {
        anyhow::bail!("empty avatar");
    }
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let temporary = path.with_extension("part");
    tokio::fs::write(&temporary, &bytes).await?;
    tokio::fs::rename(&temporary, path).await?;
    Ok(())
}

/// Removes stale cache files so the folder does not grow forever.
fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > CACHE_MAX_AGE);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_names_never_contain_path_characters() {
        let bad = Target::Avatar {
            uid: "x".into(),
            hash: "../../evil".into(),
        };
        assert_eq!(bad.file_name(), None);
        let good = Target::Avatar {
            uid: "x".into(),
            hash: "ABCDEF0123".into(),
        };
        assert_eq!(good.file_name().as_deref(), Some("avatar_abcdef0123"));
        assert_eq!(
            Target::ServerIcon(1234).file_name().as_deref(),
            Some("icon_1234")
        );
    }

    #[test]
    fn classifies_upload_refusals() {
        // A default server gives guests download but no upload power.
        assert_eq!(
            upload_refusal("PermissionsClientInsufficient").0,
            "upload_permission"
        );
        assert_eq!(
            upload_refusal("PermissionsInsufficientGroupPower").0,
            "upload_permission"
        );
        assert_eq!(
            upload_refusal("FileTransferQuotaExceeded").0,
            "upload_refused"
        );
        assert_eq!(upload_refusal("").0, "upload_refused");
    }

    #[test]
    fn splits_server_addresses() {
        assert_eq!(
            split_address("voice.test:9988"),
            ("voice.test".into(), 9988)
        );
        assert_eq!(split_address("voice.test"), ("voice.test".into(), 9987));
        assert_eq!(split_address("[::1]:1234"), ("::1".into(), 1234));
        assert_eq!(split_address("::1"), ("::1".into(), 9987));
        assert_ne!(unique_token(), unique_token());
    }
}
