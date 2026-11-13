//! Rewrites the SDP webrtc-rs writes into the layout Chrome/libwebrtc writes.
//!
//! Why: the official TeamSpeak 6 client logs `HandleRemoteAnswer` /
//! `HandleRemoteOffer ENTRY` for our SDP and then never calls
//! `SetRemoteDescription`, while it does for every Chrome-written SDP (and a
//! real Chromium accepts ours). Something in its own pre-processing rejects
//! the webrtc-rs layout, silently. Only the text sent to the other side is
//! rewritten; ICE credentials, fingerprint, payload types and directions are
//! never changed, so the local peer keeps working from its own description.
//!
//! The levels are cumulative. Measured with the official client's log: an
//! answer at level 5 got `SetRemoteDescription ... OK`, answers at levels 3 and
//! 4 never reached it (offers were accepted at any level tried). So
//! `next_level` always returns the highest level; `MIRAGE_SDP_LEVEL` pins
//! another one for experiments.
//!
//! shortcut: one accepted answer against two refused ones is thin evidence;
//! the lower levels stay only so this can be bisected again.

use std::sync::atomic::{AtomicU32, Ordering};

/// The highest level; each one adds to the previous.
///
/// 0 unchanged; 1 Chrome's `o=` line; 2 `a=msid-semantic: WMS`; 3 `ice-options:trickle`
/// and `a=rtcp:` per media; 4 Chrome's attribute order with the fingerprint per
/// media; 5 `rtcp-xr`, and `rrtr` feedback on the payloads.
pub const MAX_LEVEL: u8 = 5;

static ATTEMPTS: AtomicU32 = AtomicU32::new(0);

/// The level for the next negotiation and the session version to write.
pub fn next_level() -> (u8, u32) {
    let attempt = ATTEMPTS.fetch_add(1, Ordering::Relaxed);
    let version = 2 + attempt;
    let level = std::env::var("MIRAGE_SDP_LEVEL")
        .ok()
        .and_then(|v| v.parse::<u8>().ok())
        .map_or(MAX_LEVEL, |level| level.min(MAX_LEVEL));
    (level, version)
}

fn attr_name(line: &str) -> &str {
    let rest = line.strip_prefix("a=").unwrap_or("");
    rest.split([':', ' ']).next().unwrap_or("")
}

fn media_rank(line: &str) -> u32 {
    if line.starts_with("c=") {
        return 0;
    }
    match attr_name(line) {
        "rtcp" => 1,
        "ice-ufrag" => 2,
        "ice-pwd" => 3,
        "ice-options" => 4,
        "fingerprint" => 5,
        "setup" => 6,
        "mid" => 7,
        "extmap" => 8,
        "sendonly" | "recvonly" | "sendrecv" | "inactive" => 9,
        "msid" => 10,
        "rtcp-mux" => 11,
        "rtcp-rsize" => 12,
        "rtcp-xr" => 13,
        "ssrc-group" => 100,
        "ssrc" => 101,
        _ => 50,
    }
}

/// Payload type of an `a=rtpmap|fmtp|rtcp-fb:<pt> ...` line.
fn payload(line: &str) -> Option<u32> {
    let rest = line.strip_prefix("a=")?;
    let (name, value) = rest.split_once(':')?;
    if !matches!(name, "rtpmap" | "fmtp" | "rtcp-fb") {
        return None;
    }
    value.split(' ').next()?.parse().ok()
}

/// Header lines first, then per payload type (in order of first appearance)
/// rtpmap, rtcp-fb, fmtp, as Chrome writes them.
fn order_media(lines: Vec<String>, level: u8) -> Vec<String> {
    let (codec, mut rest): (Vec<_>, Vec<_>) = lines.into_iter().partition(|l| payload(l).is_some());
    rest.sort_by_key(|l| media_rank(l));
    let mut order = Vec::new();
    for line in &codec {
        let pt = payload(line).unwrap_or(0);
        if !order.contains(&pt) {
            order.push(pt);
        }
    }
    let mut out = rest;
    // Everything ranked after the codec block (ssrc lines) goes after it.
    let tail_at = out
        .iter()
        .position(|l| media_rank(l) >= 100)
        .unwrap_or(out.len());
    let tail = out.split_off(tail_at);
    for pt in order {
        let of_kind = |kind: &str| {
            codec
                .iter()
                .filter(|l| payload(l) == Some(pt) && attr_name(l) == kind)
                .cloned()
                .collect::<Vec<_>>()
        };
        out.extend(of_kind("rtpmap"));
        if level >= 5 {
            let feedback = format!("a=rtcp-fb:{pt} rrtr");
            if !codec.contains(&feedback) {
                out.push(feedback);
            }
        }
        out.extend(of_kind("rtcp-fb"));
        out.extend(of_kind("fmtp"));
    }
    out.extend(tail);
    out
}

/// `sdp` rewritten for `level` (see [`MAX_LEVEL`]); `version` is the `o=` session version.
pub fn shape(sdp: &str, level: u8, version: u32) -> String {
    if level == 0 {
        return sdp.to_owned();
    }
    let mut session: Vec<String> = Vec::new();
    let mut media: Vec<Vec<String>> = Vec::new();
    for line in sdp.split("\r\n").filter(|l| !l.is_empty()) {
        if line.starts_with("m=") {
            media.push(vec![line.to_owned()]);
        } else if let Some(current) = media.last_mut() {
            current.push(line.to_owned());
        } else {
            session.push(line.to_owned());
        }
    }
    // Session level: Chrome's `o=`, then group, extmap-allow-mixed, msid-semantic.
    let stream = media
        .iter()
        .flatten()
        .find_map(|l| l.strip_prefix("a=msid:"))
        .and_then(|v| v.split(' ').next())
        .unwrap_or("")
        .to_owned();
    let fingerprint = session
        .iter()
        .find(|l| l.starts_with("a=fingerprint:"))
        .cloned();
    let mut out_session: Vec<String> = Vec::new();
    for line in &session {
        match attr_name(line) {
            _ if line.starts_with("o=") && level >= 1 => {
                let parts: Vec<&str> = line[2..].split(' ').collect();
                out_session.push(if parts.len() >= 6 {
                    format!("o={} {} {} IN IP4 127.0.0.1", parts[0], parts[1], version)
                } else {
                    line.clone()
                });
            }
            "msid-semantic" if level >= 2 => {}
            "fingerprint" if level >= 4 => {}
            _ => out_session.push(line.clone()),
        }
    }
    if level >= 2 {
        out_session.push(format!(
            "a=msid-semantic: WMS{}",
            if stream.is_empty() {
                String::new()
            } else {
                format!(" {stream}")
            }
        ));
    }
    if level >= 4 {
        // Chrome: v, o, s, t, group, extmap-allow-mixed, msid-semantic.
        let rank = |l: &String| match l.split(['=', ':']).next().unwrap_or("") {
            "v" => 0,
            "o" => 1,
            "s" => 2,
            "t" => 3,
            "a" => match attr_name(l) {
                "group" => 4,
                "extmap-allow-mixed" => 5,
                "msid-semantic" => 6,
                _ => 7,
            },
            _ => 8,
        };
        out_session.sort_by_key(rank);
    }
    let mut out = out_session;
    for mut lines in media {
        let header = lines.remove(0);
        if level >= 3 {
            if !lines.iter().any(|l| attr_name(l) == "ice-options") {
                lines.push("a=ice-options:trickle".to_owned());
            }
            if !lines.iter().any(|l| attr_name(l) == "rtcp") {
                lines.push("a=rtcp:9 IN IP4 0.0.0.0".to_owned());
            }
        }
        if level >= 4 {
            if let Some(print) = &fingerprint {
                if !lines.iter().any(|l| attr_name(l) == "fingerprint") {
                    lines.push(print.clone());
                }
            }
        }
        if level >= 5 && !lines.iter().any(|l| attr_name(l) == "rtcp-xr") {
            lines.push("a=rtcp-xr:rcvr-rtt=all".to_owned());
        }
        if level >= 4 {
            lines = order_media(lines, level);
        }
        out.push(header);
        out.extend(lines);
    }
    let mut text = out.join("\r\n");
    text.push_str("\r\n");
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANSWER: &str = "v=0\r\no=- 7914140926160298549 970995000 IN IP4 0.0.0.0\r\ns=-\r\nt=0 0\r\n\
a=msid-semantic:WMS *\r\na=fingerprint:sha-256 AA:BB\r\na=extmap-allow-mixed\r\na=group:BUNDLE 0 1\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 102\r\nc=IN IP4 0.0.0.0\r\na=setup:active\r\na=mid:0\r\n\
a=ice-ufrag:uf\r\na=ice-pwd:pw\r\na=rtcp-mux\r\na=rtcp-rsize\r\na=rtpmap:102 H264/90000\r\n\
a=fmtp:102 packetization-mode=1\r\na=rtcp-fb:102 nack\r\na=extmap:9 urn:ietf:params:rtp-hdrext:sdes:mid\r\na=recvonly\r\n\
m=audio 9 UDP/TLS/RTP/SAVPF 111\r\nc=IN IP4 0.0.0.0\r\na=setup:active\r\na=mid:1\r\n\
a=ice-ufrag:uf\r\na=ice-pwd:pw\r\na=rtcp-mux\r\na=rtpmap:111 opus/48000/2\r\na=recvonly\r\n";

    fn lines(text: &str) -> Vec<&str> {
        text.split("\r\n").filter(|l| !l.is_empty()).collect()
    }

    #[test]
    fn level_zero_is_untouched() {
        assert_eq!(shape(ANSWER, 0, 2), ANSWER);
    }

    #[test]
    fn credentials_fingerprint_and_payloads_never_change() {
        for level in 0..=MAX_LEVEL {
            let shaped = shape(ANSWER, level, 3);
            let l = lines(&shaped);
            assert_eq!(l.iter().filter(|x| **x == "a=ice-ufrag:uf").count(), 2);
            assert_eq!(l.iter().filter(|x| **x == "a=ice-pwd:pw").count(), 2);
            assert!(
                l.contains(&"a=rtpmap:102 H264/90000") && l.contains(&"a=rtpmap:111 opus/48000/2")
            );
            assert!(l.contains(&"a=fmtp:102 packetization-mode=1"));
            assert!(shaped.contains("a=fingerprint:sha-256 AA:BB"));
            assert_eq!(
                l.iter().filter(|x| **x == "a=recvonly").count(),
                2,
                "level {level}"
            );
            assert_eq!(l.iter().filter(|x| x.starts_with("m=")).count(), 2);
        }
    }

    #[test]
    fn chrome_layout_at_the_top_level() {
        let shaped = shape(ANSWER, MAX_LEVEL, 2);
        let l = lines(&shaped);
        assert_eq!(l[0], "v=0");
        assert_eq!(l[1], "o=- 7914140926160298549 2 IN IP4 127.0.0.1");
        assert_eq!(&l[2..4], ["s=-", "t=0 0"]);
        assert_eq!(
            &l[4..7],
            [
                "a=group:BUNDLE 0 1",
                "a=extmap-allow-mixed",
                "a=msid-semantic: WMS"
            ]
        );
        let video = l.iter().position(|x| x.starts_with("m=video")).unwrap();
        assert_eq!(
            &l[video + 1..video + 9],
            [
                "c=IN IP4 0.0.0.0",
                "a=rtcp:9 IN IP4 0.0.0.0",
                "a=ice-ufrag:uf",
                "a=ice-pwd:pw",
                "a=ice-options:trickle",
                "a=fingerprint:sha-256 AA:BB",
                "a=setup:active",
                "a=mid:0"
            ]
        );
        let map = l
            .iter()
            .position(|x| *x == "a=rtpmap:102 H264/90000")
            .unwrap();
        assert_eq!(
            &l[map..map + 4],
            [
                "a=rtpmap:102 H264/90000",
                "a=rtcp-fb:102 rrtr",
                "a=rtcp-fb:102 nack",
                "a=fmtp:102 packetization-mode=1"
            ]
        );
        assert!(shaped.ends_with("\r\n") && !shaped.contains("WMS *"));
    }

    #[test]
    fn levels_are_cumulative() {
        let two = shape(ANSWER, 2, 2);
        assert!(two.contains("a=msid-semantic: WMS\r\n") && !two.contains("ice-options"));
        let three = shape(ANSWER, 3, 2);
        assert_eq!(three.matches("a=ice-options:trickle").count(), 2);
    }

    #[test]
    fn the_stream_id_of_an_offer_names_the_msid_semantic() {
        let offer = "v=0\r\no=- 1 1 IN IP4 0.0.0.0\r\ns=-\r\nt=0 0\r\nm=video 9 UDP/TLS/RTP/SAVPF 102\r\nc=IN IP4 0.0.0.0\r\na=msid:mirage-stream mirage-video\r\na=ssrc:1 cname:x\r\n";
        assert!(shape(offer, 2, 2).contains("a=msid-semantic: WMS mirage-stream"));
    }
}
