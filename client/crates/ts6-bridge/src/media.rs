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
        }
    }

    /// Forget everything tied to the previous connection.
    pub fn reset(&mut self) {
        self.avatars.clear();
        self.server_icon = None;
        self.in_flight.clear();
        self.attempted.clear();
        self.uploads.clear();
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
        let Some(target) = self.in_flight.remove(&handle) else {
            return;
        };
        let (Some(dir), Some(name)) = (self.dir.clone(), target.file_name()) else {
            return;
        };
        let done = done.clone();
        tokio::spawn(async move {
            let path = dir.join(name);
            let saved = tokio::time::timeout(TRANSFER_TIMEOUT, receive(stream, size, &path))
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

    pub fn on_failed(&mut self, handle: u16, error: &str) -> Option<String> {
        if let Some(target) = self.in_flight.remove(&handle) {
            tracing::debug!(?target, %error, "file download failed");
            return None;
        }
        self.uploads
            .remove(&handle)
            .map(|_| format!("avatar upload failed: {error}"))
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

    /// The server accepted an upload: write it in a task that reports back.
    pub fn on_upload(
        &mut self,
        handle: u16,
        mut stream: tokio::net::TcpStream,
        done: &mpsc::Sender<Command>,
    ) {
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

/// Reads exactly `size` bytes into `path` through a temporary file.
async fn receive(mut stream: tokio::net::TcpStream, size: u64, path: &Path) -> std::io::Result<()> {
    if size == 0 || size > MAX_MEDIA_BYTES {
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
}
