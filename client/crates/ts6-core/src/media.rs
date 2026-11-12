//! Avatars and icons stored on the server's file transfer.
//!
//! TeamSpeak keeps a client's avatar in channel 0 under `/avatar_<key>`, where
//! `<key>` is the client's unique id (base64) decoded to bytes and written with
//! one letter per nibble (`0..=15` -> `a..=p`). Server and channel icons live at
//! `/icon_<id>`; ids below 1000 are built-in group icons that the official
//! client ships locally, so they are not downloadable.
//!
//! The avatar flag (`client_flag_avatar`) is the lowercase hex MD5 of the image
//! bytes; other clients compare it to decide whether to download again.

use md5::{Digest, Md5};

/// Largest avatar or icon the bridge accepts from a server or uploads.
pub const MAX_MEDIA_BYTES: u64 = 2 * 1024 * 1024;
/// Built-in icon ids are below this value and cannot be downloaded.
pub const FIRST_CUSTOM_ICON: u32 = 1000;

/// File-transfer path of a client's avatar, or `None` for a malformed uid.
pub fn avatar_path(uid: &str) -> Option<String> {
    let bytes = decode_base64(uid)?;
    if bytes.is_empty() {
        return None;
    }
    let mut key = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        key.push(char::from(b'a' + (byte >> 4)));
        key.push(char::from(b'a' + (byte & 0x0f)));
    }
    Some(format!("/avatar_{key}"))
}

/// File-transfer path of a custom icon.
pub fn icon_path(icon_id: u32) -> Option<String> {
    (icon_id >= FIRST_CUSTOM_ICON).then(|| format!("/icon_{icon_id}"))
}

/// Host that serves myTeamSpeak avatars. Only this host is fetched: the value
/// comes from other users, and an arbitrary URL would let any peer make this
/// computer request internal or third-party addresses.
pub const MYTEAMSPEAK_AVATAR_HOST: &str = "storage.googleapis.com";
const MYTEAMSPEAK_AVATAR_PREFIX: &str = "/ts-sys-myts-avatars/";

/// Extracts the image address from `client_myteamspeak_avatar`.
///
/// Observed on a TS6 server: `2,https://storage.googleapis.com/ts-sys-myts-avatars/<uuid>/<n>`.
/// The leading number looks like a format/source marker; the part after the
/// first comma is the URL. Anything that is not an HTTPS URL on the
/// myTeamSpeak avatar bucket is ignored.
pub fn myteamspeak_avatar_url(value: &str) -> Option<String> {
    let url = value.split_once(',').map_or(value, |(_, rest)| rest).trim();
    let rest = url.strip_prefix("https://")?;
    let (host, path) = rest.split_once('/')?;
    let path = format!("/{path}");
    let clean = host.eq_ignore_ascii_case(MYTEAMSPEAK_AVATAR_HOST)
        && path.starts_with(MYTEAMSPEAK_AVATAR_PREFIX)
        && path.len() > MYTEAMSPEAK_AVATAR_PREFIX.len()
        && !path.contains("..")
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/-_.".contains(c));
    clean.then(|| format!("https://{MYTEAMSPEAK_AVATAR_HOST}{path}"))
}

/// Value for `client_flag_avatar`: hex MD5 of the image bytes.
pub fn avatar_flag(bytes: &[u8]) -> String {
    let digest = Md5::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Standard base64 with optional padding; `None` on any invalid character.
fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0u8;
    for ch in text.trim().trim_end_matches('=').bytes() {
        let value = match ch {
            b'A'..=b'Z' => ch - b'A',
            b'a'..=b'z' => ch - b'a' + 26,
            b'0'..=b'9' => ch - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avatar_path_maps_each_nibble_to_a_letter() {
        // "AAEC/w==" decodes to [0x00, 0x01, 0x02, 0xff].
        assert_eq!(avatar_path("AAEC/w==").as_deref(), Some("/avatar_aaabacpp"));
        // A real-shaped uid: 20 bytes -> 40 letters.
        let path = avatar_path("bmhSEGnqDsHQ3wrZv2XWx0Oe0eE=").unwrap();
        assert_eq!(path.len(), "/avatar_".len() + 40);
        assert!(path["/avatar_".len()..]
            .bytes()
            .all(|b| (b'a'..=b'p').contains(&b)));
    }

    #[test]
    fn malformed_uids_have_no_avatar_path() {
        assert_eq!(avatar_path(""), None);
        assert_eq!(avatar_path("not base64!"), None);
    }

    #[test]
    fn builtin_icons_are_not_downloaded() {
        assert_eq!(icon_path(0), None);
        assert_eq!(icon_path(500), None);
        assert_eq!(icon_path(1234).as_deref(), Some("/icon_1234"));
    }

    #[test]
    fn myteamspeak_avatar_accepts_only_the_avatar_bucket() {
        let observed = "2,https://storage.googleapis.com/ts-sys-myts-avatars/e060f53a-e917-407d-9d6a-80c9fa90cb35/1888088201";
        assert_eq!(
            myteamspeak_avatar_url(observed).as_deref(),
            Some("https://storage.googleapis.com/ts-sys-myts-avatars/e060f53a-e917-407d-9d6a-80c9fa90cb35/1888088201")
        );
        for bad in [
            "",
            "2,http://storage.googleapis.com/ts-sys-myts-avatars/a/1",
            "2,https://evil.example/ts-sys-myts-avatars/a/1",
            "2,https://storage.googleapis.com/other-bucket/a",
            "2,https://storage.googleapis.com@127.0.0.1/ts-sys-myts-avatars/a",
            "2,https://storage.googleapis.com/ts-sys-myts-avatars/../x",
            "2,https://storage.googleapis.com/ts-sys-myts-avatars/a?x=1",
        ] {
            assert_eq!(myteamspeak_avatar_url(bad), None, "{bad}");
        }
    }

    #[test]
    fn avatar_flag_is_hex_md5() {
        assert_eq!(avatar_flag(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(avatar_flag(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
    }
}
