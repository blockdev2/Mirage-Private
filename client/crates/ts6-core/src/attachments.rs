//! Chat attachments (photos, GIFs, files) and emoji over the classic TeamSpeak
//! text-message channel.
//!
//! Interoperability model, established from the official TS6 client bundle:
//!
//! * Server text messages carry BBCode. Images are `[IMG]…[/IMG]`; for files
//!   stored in a channel's file transfer the official client understands
//!   `ts3image://<file>?channel=<cid>&path=<dir>` (it takes the file name as the
//!   text between the 11-character scheme and `?`) and builds
//!   `ts3file://<host>?port=…&serverUID=…&channel=…&path=…&filename=…` links.
//! * The official client's own attachment button uploads to the central
//!   myTeamSpeak service and posts a Matrix `m.image`/`m.file` event instead.
//!   That needs an account and is out of scope here; this module is the
//!   account-free path that works on any server with file transfer.
//!
//! Everything here is pure so the parser, which handles text written by other
//! people, is fully unit-tested.

/// Largest attachment Mirage sends or downloads; matches the 5 MB limit the
/// official client enforces for chat attachments.
pub const MAX_ATTACHMENT_BYTES: u64 = 5 * 1000 * 1000;
/// Attachments recognised in one incoming message; the rest stay plain text.
pub const MAX_ATTACHMENTS_PER_MESSAGE: usize = 10;
/// Longest file name Mirage writes to a server.
const MAX_NAME_CHARS: usize = 96;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A still image (PNG, JPEG, WebP).
    Image,
    /// An animated or still GIF.
    Gif,
    /// Anything else.
    File,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Image => "image",
            Kind::Gif => "gif",
            Kind::File => "file",
        }
    }

    pub fn is_picture(self) -> bool {
        matches!(self, Kind::Image | Kind::Gif)
    }
}

/// Classifies by content, never by the claimed extension: a file named
/// `photo.png` that is not a PNG is sent and shown as a plain file.
pub fn sniff(bytes: &[u8]) -> (Kind, &'static str) {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        (Kind::Image, "image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        (Kind::Image, "image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        (Kind::Gif, "image/gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        (Kind::Image, "image/webp")
    } else {
        (Kind::File, "application/octet-stream")
    }
}

/// A file name that is safe on the server and on every client's disk: base
/// name only, no separators, wildcards or control characters, bounded length.
pub fn sanitize_name(name: &str) -> Option<String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| {
            if c.is_alphanumeric() || " -_.()+".contains(c) {
                c
            } else {
                '_'
            }
        })
        .take(MAX_NAME_CHARS)
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// A file stored in a channel's file transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelFile {
    pub channel_id: u64,
    /// Directory, always starting and ending with `/`.
    pub dir: String,
    pub name: String,
    pub size: Option<u64>,
}

impl ChannelFile {
    /// Validates a `(channel, path)` pair coming back from a frontend; the same
    /// rules as for paths parsed from messages apply.
    pub fn from_path(channel_id: u64, path: &str) -> Option<Self> {
        let (dir, name) = path.rsplit_once('/')?;
        file_from(&channel_id.to_string(), &format!("{dir}/"), name, None)
    }

    /// Full server path, as used by `ftinitdownload`.
    pub fn path(&self) -> String {
        format!("{}{}", self.dir, self.name)
    }
}

/// Where Mirage stores an outgoing attachment: the channel root, with a unique
/// prefix so two people sending `image.png` never overwrite each other.
pub fn upload_name(unique: &str, display_name: &str) -> Option<String> {
    let unique: String = unique
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(16)
        .collect();
    if unique.is_empty() {
        return None;
    }
    let name = sanitize_name(display_name)?;
    Some(format!("mirage-{unique}-{name}"))
}

/// The server a `ts3file://` link points to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerRef {
    pub host: String,
    pub port: u16,
    pub unique_id: String,
}

/// Message text announcing an uploaded attachment.
pub fn compose(kind: Kind, file: &ChannelFile, server: &ServerRef, unix_time: u64) -> String {
    let dir = encode(&file.dir);
    let name = encode(&file.name);
    if kind.is_picture() {
        format!(
            "[IMG]ts3image://{name}?channel={}&path={dir}[/IMG]",
            file.channel_id
        )
    } else {
        format!(
            "[URL=ts3file://{}?port={}&serverUID={}&channel={}&path={dir}&filename={name}&isDir=0&size={}&fileDateTime={unix_time}]{}[/URL]",
            server.host,
            server.port,
            encode(&server.unique_id),
            file.channel_id,
            file.size.unwrap_or(0),
            escape_bbcode(&file.name)
        )
    }
}

/// One piece of an incoming message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Text(String),
    /// A picture stored on this server's channel file transfer.
    Picture(ChannelFile),
    /// A file stored on this server's channel file transfer.
    File(ChannelFile),
    /// A picture on the web. Never fetched automatically: loading it would
    /// reveal this user's address to whoever chose the URL.
    WebPicture(String),
    /// A plain web link.
    Link {
        url: String,
        label: String,
    },
}

/// Splits BBCode text into segments. Unknown or malformed tags stay text.
pub fn parse(text: &str) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut rest = text;
    let mut attachments = 0;
    let mut plain = String::new();
    while let Some(open) = rest.find('[') {
        plain.push_str(&rest[..open]);
        rest = &rest[open..];
        match parse_tag(rest) {
            Some((segment, used)) if attachments < MAX_ATTACHMENTS_PER_MESSAGE => {
                if !plain.is_empty() {
                    segments.push(Segment::Text(std::mem::take(&mut plain)));
                }
                if !matches!(segment, Segment::Link { .. }) {
                    attachments += 1;
                }
                segments.push(segment);
                rest = &rest[used..];
            }
            _ => {
                plain.push('[');
                rest = &rest[1..];
            }
        }
    }
    plain.push_str(rest);
    if !plain.is_empty() {
        segments.push(Segment::Text(plain));
    }
    segments
}

fn parse_tag(text: &str) -> Option<(Segment, usize)> {
    let lower = text.get(..text.len().min(5))?.to_ascii_lowercase();
    if lower.starts_with("[img]") {
        let end = find_ci(text, "[/img]")?;
        let url = text[5..end].trim();
        let segment = if let Some(file) = parse_ts3image(url) {
            Segment::Picture(file)
        } else if is_web_url(url) {
            Segment::WebPicture(url.to_string())
        } else {
            return None;
        };
        return Some((segment, end + 6));
    }
    if lower.starts_with("[url]") || lower.starts_with("[url=") {
        let close = text.find(']')?;
        let end = find_ci(text, "[/url]")?;
        if end < close {
            return None;
        }
        let label = text[close + 1..end].to_string();
        let url = if lower.starts_with("[url=") {
            text[5..close].trim().to_string()
        } else {
            label.trim().to_string()
        };
        let segment = if let Some(file) = parse_ts3file(&url) {
            Segment::File(file)
        } else if is_web_url(&url) {
            Segment::Link { url, label }
        } else {
            return None;
        };
        return Some((segment, end + 6));
    }
    None
}

fn find_ci(text: &str, needle: &str) -> Option<usize> {
    text.to_ascii_lowercase().find(needle)
}

fn is_web_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://"))
        && url.len() <= 2048
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// `ts3image://<name>?channel=<cid>&path=<dir>`.
pub fn parse_ts3image(url: &str) -> Option<ChannelFile> {
    let rest = strip_scheme(url, "ts3image://")?;
    let (name, query) = rest.split_once('?')?;
    let params = query_params(query);
    file_from(
        params.get("channel")?,
        params.get("path").map_or("/", String::as_str),
        &decode(name)?,
        None,
    )
}

/// `ts3file://<host>?port=…&channel=…&path=…&filename=…&size=…`.
pub fn parse_ts3file(url: &str) -> Option<ChannelFile> {
    let rest = strip_scheme(url, "ts3file://")?;
    let (_, query) = rest.split_once('?')?;
    let params = query_params(query);
    if params.get("isDir").is_some_and(|v| v != "0") {
        return None;
    }
    file_from(
        params.get("channel")?,
        params.get("path").map_or("/", String::as_str),
        params.get("filename")?,
        params.get("size").and_then(|s| s.parse().ok()),
    )
}

fn strip_scheme<'a>(url: &'a str, scheme: &str) -> Option<&'a str> {
    let head = url.get(..scheme.len())?;
    head.eq_ignore_ascii_case(scheme)
        .then(|| &url[scheme.len()..])
}

fn query_params(query: &str) -> std::collections::HashMap<String, String> {
    query
        .split('&')
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            Some((key.to_string(), decode(value)?))
        })
        .collect()
}

fn file_from(channel: &str, dir: &str, name: &str, size: Option<u64>) -> Option<ChannelFile> {
    let channel_id = channel.parse().ok()?;
    let mut dir = dir.to_string();
    if !dir.starts_with('/') {
        dir.insert(0, '/');
    }
    if !dir.ends_with('/') {
        dir.push('/');
    }
    let safe_dir = !dir.split('/').any(|part| part == "..")
        && !dir.contains('\\')
        && !dir.chars().any(char::is_control)
        && dir.len() <= 512;
    let safe_name = !name.is_empty()
        && name.len() <= 255
        && !name.contains(['/', '\\'])
        && name != "."
        && name != ".."
        && !name.chars().any(char::is_control);
    (safe_dir && safe_name).then(|| ChannelFile {
        channel_id,
        dir,
        name: name.to_string(),
        size,
    })
}

/// Percent-encoding for URL components (RFC 3986 unreserved kept).
pub fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Percent-decoding; `None` for malformed escapes or invalid UTF-8.
pub fn decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = text.get(i + 1..i + 3)?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// Keeps a display name from closing or opening BBCode tags.
fn escape_bbcode(text: &str) -> String {
    text.replace(['[', ']'], "_")
}

/// `:shortcode:` emoji. A small, common set; unknown codes stay as typed.
const SHORTCODES: &[(&str, &str)] = &[
    ("smile", "😄"),
    ("grin", "😁"),
    ("joy", "😂"),
    ("rofl", "🤣"),
    ("wink", "😉"),
    ("blush", "😊"),
    ("heart_eyes", "😍"),
    ("kissing_heart", "😘"),
    ("thinking", "🤔"),
    ("neutral", "😐"),
    ("roll_eyes", "🙄"),
    ("sweat_smile", "😅"),
    ("cry", "😢"),
    ("sob", "😭"),
    ("angry", "😠"),
    ("rage", "😡"),
    ("scream", "😱"),
    ("sunglasses", "😎"),
    ("sleeping", "😴"),
    ("skull", "💀"),
    ("clown", "🤡"),
    ("eyes", "👀"),
    ("thumbsup", "👍"),
    ("+1", "👍"),
    ("thumbsdown", "👎"),
    ("-1", "👎"),
    ("ok_hand", "👌"),
    ("clap", "👏"),
    ("pray", "🙏"),
    ("wave", "👋"),
    ("muscle", "💪"),
    ("point_up", "☝️"),
    ("raised_hands", "🙌"),
    ("heart", "❤️"),
    ("broken_heart", "💔"),
    ("fire", "🔥"),
    ("star", "⭐"),
    ("sparkles", "✨"),
    ("100", "💯"),
    ("tada", "🎉"),
    ("rocket", "🚀"),
    ("check", "✅"),
    ("x", "❌"),
    ("warning", "⚠️"),
    ("question", "❓"),
    ("zzz", "💤"),
    ("headphones", "🎧"),
    ("microphone", "🎤"),
    ("video_game", "🎮"),
    ("trophy", "🏆"),
    ("gg", "🤝"),
    ("coffee", "☕"),
    ("beer", "🍺"),
    ("pizza", "🍕"),
    ("money", "💰"),
    ("ghost", "👻"),
    ("robot", "🤖"),
    ("cat", "🐱"),
    ("dog", "🐶"),
    ("poop", "💩"),
];

/// Replaces known `:shortcode:` occurrences with emoji. Text inside BBCode
/// URLs is left alone so links and attachment references stay intact.
pub fn expand_shortcodes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(':') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let code_end = after.find(':');
        let replaced = code_end.and_then(|end| {
            let code = &after[..end];
            (!code.is_empty() && code.len() <= 20 && !inside_bbcode_url(&out))
                .then(|| SHORTCODES.iter().find(|(name, _)| *name == code))
                .flatten()
                .map(|(_, emoji)| (*emoji, end))
        });
        match replaced {
            Some((emoji, end)) => {
                out.push_str(emoji);
                rest = &after[end + 1..];
            }
            None => {
                out.push(':');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// True while the text so far ends inside an `[URL=…]`/`[IMG]…` value.
fn inside_bbcode_url(so_far: &str) -> bool {
    let lower = so_far.to_ascii_lowercase();
    let open = lower.rfind("[url=").max(lower.rfind("[img]"));
    let close = lower
        .rfind(']')
        .max(lower.rfind("[/img]"))
        .max(lower.rfind("[/url]"));
    match (open, close) {
        (Some(open), Some(close)) => {
            open + 4 > close || lower[open..].starts_with("[img]") && close == open + 4
        }
        (Some(_), None) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> ServerRef {
        ServerRef {
            host: "voice.example.test".into(),
            port: 9987,
            unique_id: "abc+def=".into(),
        }
    }

    #[test]
    fn sniffs_content_not_names() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\nrest").0, Kind::Image);
        assert_eq!(sniff(&[0xff, 0xd8, 0xff, 0xe0]).1, "image/jpeg");
        assert_eq!(sniff(b"GIF89a....").0, Kind::Gif);
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 ").1, "image/webp");
        assert_eq!(sniff(b"PK\x03\x04zip").0, Kind::File);
        assert_eq!(sniff(b"").0, Kind::File);
    }

    #[test]
    fn sanitizes_hostile_names() {
        assert_eq!(
            sanitize_name("C:\\Users\\me\\photo 1.png").as_deref(),
            Some("photo 1.png")
        );
        assert_eq!(sanitize_name("../../etc/passwd").as_deref(), Some("passwd"));
        assert_eq!(
            sanitize_name("a*b?c<d>|e\"f.gif").as_deref(),
            Some("a_b_c_d__e_f.gif")
        );
        assert_eq!(
            sanitize_name("bell\u{7}name.txt").as_deref(),
            Some("bellname.txt")
        );
        assert_eq!(sanitize_name(".."), None);
        assert_eq!(sanitize_name("   "), None);
        assert_eq!(
            sanitize_name(&"x".repeat(500)).unwrap().chars().count(),
            MAX_NAME_CHARS
        );
        assert_eq!(
            upload_name("ab12../", "cat.gif").as_deref(),
            Some("mirage-ab12-cat.gif")
        );
        assert_eq!(upload_name("../", "cat.gif"), None);
    }

    #[test]
    fn composed_messages_parse_back_to_the_same_file() {
        let file = ChannelFile {
            channel_id: 7,
            dir: "/".into(),
            name: "mirage-ab-my photo [1].png".into(),
            size: Some(1234),
        };
        let image = compose(Kind::Image, &file, &server(), 1_700_000_000);
        assert!(image
            .starts_with("[IMG]ts3image://mirage-ab-my%20photo%20%5B1%5D.png?channel=7&path=%2F"));
        assert_eq!(
            parse(&image),
            vec![Segment::Picture(ChannelFile {
                size: None,
                ..file.clone()
            })]
        );

        let link = compose(Kind::File, &file, &server(), 1_700_000_000);
        assert!(link.contains("serverUID=abc%2Bdef%3D"));
        assert!(link.ends_with("]mirage-ab-my photo _1_.png[/URL]"));
        assert_eq!(parse(&link), vec![Segment::File(file)]);
    }

    #[test]
    fn parses_mixed_text_case_insensitively() {
        let text = "look [img]ts3image://a.gif?channel=3&path=%2Fpics%2F[/img] and [url]https://example.test/x[/url] :)";
        let segments = parse(text);
        assert_eq!(segments[0], Segment::Text("look ".into()));
        assert_eq!(
            segments[1],
            Segment::Picture(ChannelFile {
                channel_id: 3,
                dir: "/pics/".into(),
                name: "a.gif".into(),
                size: None
            })
        );
        assert_eq!(segments[2], Segment::Text(" and ".into()));
        assert!(
            matches!(&segments[3], Segment::Link { url, .. } if url == "https://example.test/x")
        );
        assert_eq!(segments[4], Segment::Text(" :)".into()));
        assert_eq!(
            parse("[IMG]https://media.example.test/cat.gif[/IMG]"),
            vec![Segment::WebPicture(
                "https://media.example.test/cat.gif".into()
            )]
        );
    }

    #[test]
    fn hostile_or_malformed_tags_stay_text() {
        for text in [
            "[img]ts3image://x.png?channel=1&path=%2F..%2F[/img]",
            "[img]ts3image://..?channel=1&path=%2F[/img]",
            "[img]ts3image://x.png?channel=abc&path=%2F[/img]",
            "[img]javascript:alert(1)[/img]",
            "[img]file:///C:/secret.png[/img]",
            "[url=ts3file://h?channel=1&path=%2F&filename=a%2Fb&isDir=0]x[/url]",
            "[url=ts3file://h?channel=1&path=%2F&filename=d&isDir=1]x[/url]",
            "[img]ts3image://x.png?channel=1&path=%2F",
            "[img]ts3image://bad%ZZ.png?channel=1&path=%2F[/img]",
            "[[[[",
        ] {
            assert!(
                parse(text).iter().all(|s| matches!(s, Segment::Text(_))),
                "{text}"
            );
        }
        let many = "[img]ts3image://a.png?channel=1&path=%2F[/img]".repeat(15);
        let pictures = parse(&many)
            .iter()
            .filter(|s| matches!(s, Segment::Picture(_)))
            .count();
        assert_eq!(pictures, MAX_ATTACHMENTS_PER_MESSAGE);
    }

    #[test]
    fn expands_shortcodes_but_not_inside_links() {
        assert_eq!(expand_shortcodes("gg :fire: :thumbsup:!"), "gg 🔥 👍!");
        assert_eq!(
            expand_shortcodes("time 10:30:45 :nope:"),
            "time 10:30:45 :nope:"
        );
        assert_eq!(expand_shortcodes("::"), "::");
        let link = "[URL=https://x.test/a:fire:b]x[/URL] :fire:";
        assert_eq!(
            expand_shortcodes(link),
            "[URL=https://x.test/a:fire:b]x[/URL] 🔥"
        );
    }

    #[test]
    fn frontend_paths_are_validated() {
        let file = ChannelFile::from_path(4, "/pics/a b.png").unwrap();
        assert_eq!(
            (file.dir.as_str(), file.name.as_str()),
            ("/pics/", "a b.png")
        );
        assert_eq!(file.path(), "/pics/a b.png");
        for bad in ["/../x.png", "x.png", "/dir/", "/a\\b.png", "/\x07bell.png"] {
            assert!(ChannelFile::from_path(4, bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn percent_coding_round_trips_unicode() {
        let name = "foto 😄 ação.png";
        assert_eq!(decode(&encode(name)).as_deref(), Some(name));
        assert_eq!(decode("%E0%A4"), None, "invalid UTF-8");
        assert_eq!(decode("%4"), None, "truncated escape");
    }
}
