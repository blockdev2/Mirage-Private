//! Local data kept on this computer: profiles, favorites and connection history.
//!
//! A profile is only a saved nickname. It does not authenticate anywhere, and
//! it never touches the technical identity (`identity.json`) that the voice
//! core manages. Server passwords are deliberately not stored here.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// TeamSpeak's limit for a nickname.
pub const NICKNAME_MAX_CHARS: usize = 30;
/// Oldest history entries beyond this count are dropped.
pub const MAX_HISTORY: usize = 50;
const FILE_NAME: &str = "profiles.json";

/// Seconds since the Unix epoch.
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// A stable local identifier. It is unique on this computer, not secret.
pub fn new_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    format!("{nanos:x}-{:x}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

// Addresses -----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressProblem {
    Empty,
    HasSpaces,
    BadPort,
}

/// Checks the shape of `host`, `host:port` or `[ipv6]:port`. It does not
/// resolve the name; the connection attempt reports unreachable servers.
pub fn validate_address(address: &str) -> Result<(), AddressProblem> {
    let address = address.trim();
    if address.is_empty() {
        return Err(AddressProblem::Empty);
    }
    if address.chars().any(char::is_whitespace) {
        return Err(AddressProblem::HasSpaces);
    }
    if let Some(port) = port_part(address) {
        match port.parse::<u16>() {
            Ok(number) if number > 0 => {}
            _ => return Err(AddressProblem::BadPort),
        }
    }
    Ok(())
}

fn port_part(address: &str) -> Option<&str> {
    if let Some(rest) = address.strip_prefix('[') {
        return rest.split_once("]:").map(|(_, port)| port);
    }
    // A bare IPv6 literal has several colons and no port.
    if address.matches(':').count() == 1 {
        return address.split_once(':').map(|(_, port)| port);
    }
    None
}

/// Key used to recognise the same server written in different ways.
pub fn normalize_address(address: &str) -> String {
    let lower = address.trim().to_lowercase();
    match lower.strip_suffix(":9987") {
        Some(host) => host.to_string(),
        None => lower,
    }
}

/// Case-insensitive match used by the search box, over local data only.
pub fn matches(query: &str, name: &str, address: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty()
        || name.to_lowercase().contains(&query)
        || address.to_lowercase().contains(&query)
}

fn clean_nickname(nickname: &str) -> String {
    nickname.trim().chars().take(NICKNAME_MAX_CHARS).collect()
}

// Data ------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub id: String,
    pub nickname: String,
    /// Bumped whenever the photo changes, so cached textures are refreshed.
    pub avatar_rev: u64,
}

// Profile photos --------------------------------------------------------------

/// Side of the square photo kept on disk. Larger sources are scaled down.
pub const AVATAR_SIZE: u32 = 256;
/// Extensions offered by the file picker. Detection uses the file content.
pub const AVATAR_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "jfif", "gif", "bmp", "webp", "tif", "tiff", "ico",
];
/// Files above this size are refused before decoding.
const AVATAR_MAX_BYTES: u64 = 25 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvatarError {
    TooLarge,
    Unreadable,
}

/// Loads any supported image, crops it to a centered square and scales it to
/// [`AVATAR_SIZE`]. Nothing is written to disk here.
pub fn prepare_avatar(path: &std::path::Path) -> Result<image::RgbaImage, AvatarError> {
    Ok(square_avatar(&load_avatar_source(path)?))
}

/// Loads any supported image for the crop editor, refusing huge files.
pub fn load_avatar_source(path: &std::path::Path) -> Result<image::DynamicImage, AvatarError> {
    let size = fs::metadata(path)
        .map_err(|_| AvatarError::Unreadable)?
        .len();
    if size > AVATAR_MAX_BYTES {
        return Err(AvatarError::TooLarge);
    }
    image::ImageReader::open(path)
        .and_then(|reader| reader.with_guessed_format())
        .map_err(|_| AvatarError::Unreadable)?
        .decode()
        .map_err(|_| AvatarError::Unreadable)
}

fn square_avatar(image: &image::DynamicImage) -> image::RgbaImage {
    let (width, height) = (image.width(), image.height());
    let side = width.min(height).max(1);
    crop_avatar(image, (width - side) / 2, (height - side) / 2, side)
}

/// Cuts the `side` square at (`x`, `y`) out of `image` (clamped to fit) and
/// scales it to [`AVATAR_SIZE`].
pub fn crop_avatar(image: &image::DynamicImage, x: u32, y: u32, side: u32) -> image::RgbaImage {
    let side = side.clamp(1, image.width().min(image.height()).max(1));
    let x = x.min(image.width().saturating_sub(side));
    let y = y.min(image.height().saturating_sub(side));
    let square = image.crop_imm(x, y, side, side);
    let target = AVATAR_SIZE.min(side);
    square
        .resize_exact(target, target, image::imageops::FilterType::Lanczos3)
        .to_rgba8()
}

/// File name for a profile photo; ids are generated locally, but anything
/// outside `[0-9a-z-]` is dropped so a hand-edited id cannot escape the folder.
fn avatar_file_name(id: &str) -> Option<String> {
    let safe: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    (!safe.is_empty()).then(|| format!("{safe}.png"))
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Favorite {
    pub id: String,
    pub name: String,
    pub address: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HistoryEntry {
    pub address: String,
    pub name: String,
    pub profile_id: Option<String>,
    pub last_used: u64,
}

/// How the microphone decides when to transmit, as in TeamSpeak.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransmitMode {
    /// Transmit while the voice level is above the threshold.
    #[default]
    VoiceActivation,
    /// Transmit while the key is held.
    PushToTalk,
    /// Always transmit.
    Continuous,
}

/// Microphone noise suppression in the voice engine (RNNoise or DPDFNet).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoiseLevel {
    Off,
    /// Removes steady noise: fans, hum, hiss.
    #[default]
    Standard,
    /// Also mutes everything that is not speech: keyboard, clicks, breathing.
    Strong,
    /// DPDFNet, a neural speech enhancer that keeps the voice and removes the rest.
    Advanced,
}

impl NoiseLevel {
    /// Wire value understood by the voice engine.
    pub fn level(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::Standard => 1,
            Self::Strong => 2,
            Self::Advanced => 3,
        }
    }
}

/// Highest per-user playback volume (400%).
pub const MAX_PEER_VOLUME: f32 = 4.0;

/// Default voice activation threshold in dBFS (matches the bridge's default).
pub const DEFAULT_VAD_DB: f32 = -42.0;
pub const VAD_MIN_DB: f32 = -70.0;
pub const VAD_MAX_DB: f32 = -10.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSettings {
    pub enabled: bool,
    pub mode: TransmitMode,
    pub threshold_db: f32,
    pub noise: NoiseLevel,
    pub ptt: String,
    pub input_device: String,
    pub output_device: String,
    /// Not saved: hearing yourself is a quick check, never a startup surprise.
    #[serde(skip)]
    pub monitor: bool,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: TransmitMode::VoiceActivation,
            threshold_db: DEFAULT_VAD_DB,
            noise: NoiseLevel::Standard,
            ptt: String::new(),
            input_device: String::new(),
            output_device: String::new(),
            monitor: false,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Data {
    pub version: u32,
    pub profiles: Vec<Profile>,
    pub default_profile: Option<String>,
    pub last_profile: Option<String>,
    pub favorites: Vec<Favorite>,
    pub history: Vec<HistoryEntry>,
    pub audio: AudioSettings,
    /// Playback volume per TeamSpeak user (unique id), 1.0 = 100%.
    pub volumes: std::collections::HashMap<String, f32>,
}

pub struct Store {
    pub data: Data,
    path: Option<PathBuf>,
    /// True when the last attempt to write the file failed.
    pub save_failed: bool,
}

fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("Mirage").join(FILE_NAME))
}

impl Store {
    /// Opens the per-user file. A missing file means a first run; an unreadable
    /// one is set aside as `.bad` and the app starts empty instead of failing.
    pub fn open() -> Self {
        Self::open_at(default_path())
    }

    pub fn open_at(path: Option<PathBuf>) -> Self {
        let mut data = Data::default();
        if let Some(path) = &path {
            if let Ok(text) = fs::read_to_string(path) {
                match serde_json::from_str::<Data>(&text) {
                    Ok(parsed) => data = parsed,
                    Err(_) => {
                        let _ = fs::rename(path, path.with_extension("json.bad"));
                    }
                }
            }
        }
        let mut store = Self {
            data,
            path,
            save_failed: false,
        };
        store.sanitize();
        store
    }

    /// Drops entries that cannot be used and fixes dangling references, so
    /// hand-edited or partly corrupt data never blocks the app.
    fn sanitize(&mut self) {
        let mut seen = std::collections::HashSet::new();
        self.data.profiles.retain_mut(|profile| {
            profile.nickname = clean_nickname(&profile.nickname);
            !profile.id.is_empty()
                && !profile.nickname.is_empty()
                && seen.insert(profile.id.clone())
        });
        let exists = |id: &Option<String>, profiles: &[Profile]| {
            id.as_ref()
                .is_some_and(|id| profiles.iter().any(|p| &p.id == id))
        };
        if !exists(&self.data.default_profile, &self.data.profiles) {
            self.data.default_profile = self.data.profiles.first().map(|p| p.id.clone());
        }
        if !exists(&self.data.last_profile, &self.data.profiles) {
            self.data.last_profile = None;
        }
        self.data
            .favorites
            .retain(|f| !f.id.is_empty() && validate_address(&f.address).is_ok());
        let profiles = self.data.profiles.clone();
        for entry in &mut self.data.history {
            if !exists(&entry.profile_id, &profiles) {
                entry.profile_id = None;
            }
        }
        let mut keys = std::collections::HashSet::new();
        self.data.history.retain(|e| {
            validate_address(&e.address).is_ok() && keys.insert(normalize_address(&e.address))
        });
        self.data
            .history
            .sort_by_key(|entry| std::cmp::Reverse(entry.last_used));
        self.data.history.truncate(MAX_HISTORY);
        let threshold = self.data.audio.threshold_db;
        self.data.audio.threshold_db = if threshold.is_finite() {
            threshold.clamp(VAD_MIN_DB, VAD_MAX_DB)
        } else {
            DEFAULT_VAD_DB
        };
        self.data
            .volumes
            .retain(|uid, volume| !uid.is_empty() && volume.is_finite());
        for volume in self.data.volumes.values_mut() {
            *volume = volume.clamp(0.0, MAX_PEER_VOLUME);
        }
        self.data.version = 1;
    }

    /// Writes the file through a temporary one, so a crash cannot leave it
    /// half written. Failure is recorded in `save_failed` for the UI.
    pub fn save(&mut self) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let result = (|| -> io::Result<()> {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let json = serde_json::to_string_pretty(&self.data).map_err(io::Error::other)?;
            let temporary = path.with_extension("json.tmp");
            fs::write(&temporary, json)?;
            fs::rename(&temporary, &path)
        })();
        self.save_failed = result.is_err();
    }

    // Profile photos ----------------------------------------------------------

    fn avatar_file(&self, id: &str) -> Option<PathBuf> {
        let dir = self.path.as_ref()?.parent()?.join("avatars");
        Some(dir.join(avatar_file_name(id)?))
    }

    /// The saved photo of a profile, if it has one.
    pub fn avatar_path(&self, id: &str) -> Option<PathBuf> {
        self.avatar_file(id).filter(|path| path.is_file())
    }

    /// Stores the profile's photo. There is one file per profile: a new photo
    /// replaces the previous one instead of piling up copies.
    pub fn set_avatar(&mut self, id: &str, photo: &image::RgbaImage) -> bool {
        let Some(path) = self.avatar_file(id) else {
            return false;
        };
        let written = (|| -> Option<()> {
            fs::create_dir_all(path.parent()?).ok()?;
            let temporary = path.with_extension("png.tmp");
            photo
                .save_with_format(&temporary, image::ImageFormat::Png)
                .ok()?;
            fs::rename(&temporary, &path).ok()
        })()
        .is_some();
        if written {
            self.bump_avatar(id);
        }
        written
    }

    pub fn remove_avatar(&mut self, id: &str) {
        if let Some(path) = self.avatar_file(id) {
            let _ = fs::remove_file(path);
        }
        self.bump_avatar(id);
    }

    fn bump_avatar(&mut self, id: &str) {
        if let Some(profile) = self.data.profiles.iter_mut().find(|p| p.id == id) {
            profile.avatar_rev = profile.avatar_rev.wrapping_add(1);
        }
    }

    // Profiles ----------------------------------------------------------------

    pub fn profile(&self, id: &str) -> Option<&Profile> {
        self.data.profiles.iter().find(|p| p.id == id)
    }

    /// The profile to use now: the last one picked, else the default, else the first.
    pub fn active_profile_id(&self) -> Option<String> {
        [&self.data.last_profile, &self.data.default_profile]
            .into_iter()
            .flatten()
            .find(|id| self.profile(id).is_some())
            .cloned()
            .or_else(|| self.data.profiles.first().map(|p| p.id.clone()))
    }

    pub fn is_default_profile(&self, id: &str) -> bool {
        self.data.default_profile.as_deref() == Some(id)
    }

    pub fn set_last_profile(&mut self, id: &str) {
        if self.profile(id).is_some() {
            self.data.last_profile = Some(id.to_string());
        }
    }

    /// Creates a profile and makes it the active one. The first profile is
    /// always the default.
    pub fn add_profile(&mut self, nickname: &str, make_default: bool) -> String {
        let id = new_id();
        let first = self.data.profiles.is_empty();
        self.data.profiles.push(Profile {
            id: id.clone(),
            nickname: clean_nickname(nickname),
            avatar_rev: 0,
        });
        if first || make_default {
            self.data.default_profile = Some(id.clone());
        }
        self.data.last_profile = Some(id.clone());
        id
    }

    pub fn update_profile(&mut self, id: &str, nickname: &str, make_default: bool) {
        if let Some(profile) = self.data.profiles.iter_mut().find(|p| p.id == id) {
            profile.nickname = clean_nickname(nickname);
            if make_default {
                self.data.default_profile = Some(id.to_string());
            }
        }
    }

    /// Removes a profile. The default moves to another profile when needed,
    /// and history entries that used it fall back to the default.
    pub fn remove_profile(&mut self, id: &str) {
        // The photo goes with the profile; nothing is left behind on disk.
        if let Some(path) = self.avatar_file(id) {
            let _ = fs::remove_file(path);
        }
        self.data.profiles.retain(|p| p.id != id);
        if self.data.default_profile.as_deref() == Some(id) {
            self.data.default_profile = self.data.profiles.first().map(|p| p.id.clone());
        }
        if self.data.last_profile.as_deref() == Some(id) {
            self.data.last_profile = self.data.default_profile.clone();
        }
        for entry in &mut self.data.history {
            if entry.profile_id.as_deref() == Some(id) {
                entry.profile_id = None;
            }
        }
    }

    // Favorites -----------------------------------------------------------------

    pub fn favorite_for(&self, address: &str) -> Option<&Favorite> {
        let key = normalize_address(address);
        self.data
            .favorites
            .iter()
            .find(|f| normalize_address(&f.address) == key)
    }

    /// Adds a favorite and returns its id. The same server is never added
    /// twice: the existing id is returned instead.
    pub fn add_favorite(&mut self, name: &str, address: &str) -> String {
        if let Some(existing) = self.favorite_for(address) {
            return existing.id.clone();
        }
        let id = new_id();
        self.data.favorites.push(Favorite {
            id: id.clone(),
            name: name_or_address(name, address),
            address: address.trim().to_string(),
        });
        id
    }

    pub fn update_favorite(&mut self, id: &str, name: &str, address: &str) {
        if let Some(favorite) = self.data.favorites.iter_mut().find(|f| f.id == id) {
            favorite.name = name_or_address(name, address);
            favorite.address = address.trim().to_string();
        }
    }

    pub fn remove_favorite(&mut self, id: &str) {
        self.data.favorites.retain(|f| f.id != id);
    }

    // History -------------------------------------------------------------------

    /// Records a successful connection at the front of the history. The same
    /// server appears only once, with its latest time and profile.
    pub fn record_connection(
        &mut self,
        address: &str,
        name: &str,
        profile_id: Option<String>,
        now: u64,
    ) {
        let key = normalize_address(address);
        self.data
            .history
            .retain(|e| normalize_address(&e.address) != key);
        self.data.history.insert(
            0,
            HistoryEntry {
                address: address.trim().to_string(),
                name: name_or_address(name, address),
                profile_id,
                last_used: now,
            },
        );
        self.data.history.truncate(MAX_HISTORY);
    }

    pub fn remove_history(&mut self, address: &str) {
        let key = normalize_address(address);
        self.data
            .history
            .retain(|e| normalize_address(&e.address) != key);
    }

    /// Clears the history only. Favorites are kept.
    pub fn clear_history(&mut self) {
        self.data.history.clear();
    }

    /// The name to show for a server: the favorite's custom name when it is
    /// one, otherwise the name the server reported.
    pub fn display_name(&self, address: &str, reported: &str) -> String {
        match self.favorite_for(address) {
            Some(favorite) => favorite.name.clone(),
            None => name_or_address(reported, address),
        }
    }
}

fn name_or_address(name: &str, address: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        address.trim().to_string()
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(image: &image::RgbaImage, format: image::ImageFormat, path: &std::path::Path) {
        image::DynamicImage::ImageRgba8(image.clone())
            .to_rgb8()
            .save_with_format(path, format)
            .unwrap();
    }

    #[test]
    fn photos_of_common_formats_become_one_square_file_per_profile() {
        let path = temp_file();
        let dir = path.parent().unwrap().to_path_buf();
        fs::create_dir_all(&dir).unwrap();
        let mut store = Store::open_at(Some(path.clone()));
        let id = store.add_profile("ts6client", true);

        // A wide image is cropped to a square and scaled down.
        let wide = image::RgbaImage::from_pixel(800, 400, image::Rgba([200, 30, 30, 255]));
        let formats = [
            ("a.png", image::ImageFormat::Png),
            ("b.jpg", image::ImageFormat::Jpeg),
            ("c.bmp", image::ImageFormat::Bmp),
            ("d.gif", image::ImageFormat::Gif),
            ("e.tiff", image::ImageFormat::Tiff),
            ("f.webp", image::ImageFormat::WebP),
        ];
        for (name, format) in formats {
            let source = dir.join(name);
            encode(&wide, format, &source);
            let photo = prepare_avatar(&source).unwrap_or_else(|_| panic!("{name} should load"));
            assert_eq!(photo.dimensions(), (AVATAR_SIZE, AVATAR_SIZE));
            assert!(store.set_avatar(&id, &photo));
        }
        // A wrong extension does not matter: the content decides the format.
        let renamed = dir.join("photo.png");
        fs::copy(dir.join("b.jpg"), &renamed).unwrap();
        assert!(prepare_avatar(&renamed).is_ok());

        // Six photo changes left exactly one stored file.
        let stored: Vec<_> = fs::read_dir(dir.join("avatars")).unwrap().collect();
        assert_eq!(stored.len(), 1);
        assert_eq!(store.profile(&id).unwrap().avatar_rev, 6);

        // Removing the profile removes its photo.
        store.remove_profile(&id);
        assert_eq!(fs::read_dir(dir.join("avatars")).unwrap().count(), 0);

        // Not an image at all.
        let junk = dir.join("junk.png");
        fs::write(&junk, b"not an image").unwrap();
        assert_eq!(prepare_avatar(&junk), Err(AvatarError::Unreadable));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn crop_avatar_clamps_to_the_image() {
        let image = image::DynamicImage::new_rgba8(400, 300);
        // A square that would run off the bottom-right is pulled back inside.
        let photo = crop_avatar(&image, 350, 250, 1000);
        assert_eq!(photo.dimensions(), (AVATAR_SIZE, AVATAR_SIZE));
    }

    fn temp_file() -> PathBuf {
        std::env::temp_dir()
            .join(format!("mirage-test-{}", new_id()))
            .join(FILE_NAME)
    }

    #[test]
    fn validates_addresses() {
        assert_eq!(validate_address(""), Err(AddressProblem::Empty));
        assert_eq!(validate_address("   "), Err(AddressProblem::Empty));
        assert_eq!(validate_address("a b"), Err(AddressProblem::HasSpaces));
        assert_eq!(validate_address("host:abc"), Err(AddressProblem::BadPort));
        assert_eq!(validate_address("host:0"), Err(AddressProblem::BadPort));
        assert_eq!(validate_address("host:70000"), Err(AddressProblem::BadPort));
        assert_eq!(validate_address("host:"), Err(AddressProblem::BadPort));
        assert_eq!(validate_address("voz.exemplo.com"), Ok(()));
        assert_eq!(validate_address("voz.exemplo.com:9987"), Ok(()));
        assert_eq!(validate_address("10.0.0.5:9988"), Ok(()));
        assert_eq!(validate_address("[::1]:9987"), Ok(()));
        assert_eq!(validate_address("::1"), Ok(()));
    }

    #[test]
    fn normalizes_the_default_port_and_case() {
        assert_eq!(
            normalize_address(" Voz.Exemplo.com:9987 "),
            "voz.exemplo.com"
        );
        assert_eq!(normalize_address("voz.exemplo.com"), "voz.exemplo.com");
        assert_eq!(
            normalize_address("voz.exemplo.com:9988"),
            "voz.exemplo.com:9988"
        );
    }

    #[test]
    fn search_is_case_insensitive_over_name_and_address() {
        assert!(matches("", "x", "y"));
        assert!(matches("GALERA", "Servidor da galera", "voz.exemplo.com"));
        assert!(matches("exemplo", "Servidor", "voz.exemplo.com"));
        assert!(!matches("zzz", "Servidor", "voz.exemplo.com"));
    }

    #[test]
    fn history_has_one_entry_per_server_newest_first() {
        let mut store = Store::open_at(None);
        store.record_connection("a.com", "A", None, 10);
        store.record_connection("b.com", "B", None, 20);
        store.record_connection("A.com:9987", "A novo", None, 30);
        let addresses: Vec<&str> = store
            .data
            .history
            .iter()
            .map(|e| e.address.as_str())
            .collect();
        assert_eq!(addresses, ["A.com:9987", "b.com"]);
        assert_eq!(store.data.history[0].name, "A novo");
    }

    #[test]
    fn history_is_capped() {
        let mut store = Store::open_at(None);
        for index in 0..(MAX_HISTORY + 10) {
            store.record_connection(&format!("s{index}.com"), "S", None, index as u64);
        }
        assert_eq!(store.data.history.len(), MAX_HISTORY);
        assert_eq!(
            store.data.history[0].address,
            format!("s{}.com", MAX_HISTORY + 9)
        );
    }

    #[test]
    fn clearing_history_keeps_favorites() {
        let mut store = Store::open_at(None);
        store.add_favorite("Fav", "fav.com");
        store.record_connection("fav.com", "Fav", None, 1);
        store.clear_history();
        assert!(store.data.history.is_empty());
        assert_eq!(store.data.favorites.len(), 1);
    }

    #[test]
    fn favorites_are_not_duplicated() {
        let mut store = Store::open_at(None);
        let first = store.add_favorite("Um", "voz.com");
        let second = store.add_favorite("Dois", "VOZ.com:9987");
        assert_eq!(first, second);
        assert_eq!(store.data.favorites.len(), 1);
        assert_eq!(store.display_name("voz.com", "Reportado"), "Um");
        assert_eq!(store.display_name("outro.com", "Reportado"), "Reportado");
    }

    #[test]
    fn first_profile_is_the_default_and_nicknames_are_limited() {
        let mut store = Store::open_at(None);
        let long = "x".repeat(100);
        let first = store.add_profile(&long, false);
        assert!(store.is_default_profile(&first));
        assert_eq!(
            store.profile(&first).unwrap().nickname.chars().count(),
            NICKNAME_MAX_CHARS
        );
        let second = store.add_profile("Outro", false);
        assert!(store.is_default_profile(&first));
        assert_eq!(store.active_profile_id().as_deref(), Some(second.as_str()));
        store.update_profile(&second, "Outro", true);
        assert!(store.is_default_profile(&second));
    }

    #[test]
    fn removing_a_profile_moves_the_default_and_history_references() {
        let mut store = Store::open_at(None);
        let first = store.add_profile("Um", false);
        let second = store.add_profile("Dois", false);
        store.record_connection("a.com", "A", Some(first.clone()), 1);
        store.remove_profile(&first);
        assert!(store.is_default_profile(&second));
        assert_eq!(store.data.history[0].profile_id, None);
        store.remove_profile(&second);
        assert_eq!(store.active_profile_id(), None);
        assert!(store.data.profiles.is_empty());
    }

    #[test]
    fn data_survives_a_restart() {
        let path = temp_file();
        {
            let mut store = Store::open_at(Some(path.clone()));
            let id = store.add_profile("ts6client", true);
            store.add_favorite("Galera", "voz.galera.net");
            store.record_connection("voz.galera.net", "Galera", Some(id), 42);
            store.save();
            assert!(!store.save_failed);
        }
        let reopened = Store::open_at(Some(path.clone()));
        assert_eq!(reopened.data.profiles.len(), 1);
        assert_eq!(reopened.data.profiles[0].nickname, "ts6client");
        assert_eq!(reopened.data.favorites[0].name, "Galera");
        assert_eq!(reopened.data.history[0].last_used, 42);
        assert!(reopened.active_profile_id().is_some());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_corrupt_file_is_set_aside_and_the_app_starts_empty() {
        let path = temp_file();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{ not json").unwrap();
        let store = Store::open_at(Some(path.clone()));
        assert!(store.data.profiles.is_empty());
        assert!(path.with_extension("json.bad").exists());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn dangling_references_are_repaired_on_load() {
        let path = temp_file();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{
                "profiles": [{"id": "p1", "nickname": "Ok"}, {"id": "", "nickname": "Sem id"}],
                "default_profile": "gone",
                "last_profile": "gone",
                "favorites": [{"id": "f1", "name": "Ruim", "address": "tem espaco"}],
                "history": [{"address": "a.com", "name": "A", "profile_id": "gone", "last_used": 5}]
            }"#,
        )
        .unwrap();
        let store = Store::open_at(Some(path.clone()));
        assert_eq!(store.data.profiles.len(), 1);
        assert_eq!(store.data.default_profile.as_deref(), Some("p1"));
        assert_eq!(store.data.last_profile, None);
        assert!(store.data.favorites.is_empty());
        assert_eq!(store.data.history[0].profile_id, None);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}
