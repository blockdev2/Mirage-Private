//! Checks a stream offer before any WebRTC work: it must fit the size bound
//! and offer H.264 with packetization-mode=1, the one codec Mirage decodes.

/// Largest offer accepted from a publisher.
pub const MAX_OFFER_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfferError {
    TooLarge,
    NoH264,
}

pub fn check_offer(sdp: &str) -> Result<(), OfferError> {
    if sdp.len() > MAX_OFFER_BYTES {
        return Err(OfferError::TooLarge);
    }
    let lines: Vec<&str> = sdp.lines().map(str::trim).collect();
    let h264 = lines.iter().filter_map(|line| {
        let (pt, codec) = line.strip_prefix("a=rtpmap:")?.split_once(' ')?;
        codec.eq_ignore_ascii_case("H264/90000").then_some(pt)
    });
    let usable = h264.into_iter().any(|pt| {
        let prefix = format!("a=fmtp:{pt} ");
        lines.iter().any(|line| {
            line.strip_prefix(&prefix).is_some_and(|params| {
                params
                    .split(';')
                    .any(|p| p.trim() == "packetization-mode=1")
            })
        })
    });
    if usable {
        Ok(())
    } else {
        Err(OfferError::NoH264)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The official TS6 client's offer shape (spike of 2026-11-13), with
    /// credentials and addresses replaced.
    const OFFICIAL_SHAPE: &str = "v=0\r\n\
o=- 1 2 IN IP4 127.0.0.1\r\n\
s=-\r\n\
t=0 0\r\n\
a=group:BUNDLE 0 1\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 39 102 104 98 96 97 103 107 40 99 116 117 118\r\n\
c=IN IP4 0.0.0.0\r\n\
a=ice-ufrag:x\r\n\
a=ice-pwd:x\r\n\
a=fingerprint:sha-256 x\r\n\
a=setup:actpass\r\n\
a=mid:0\r\n\
a=sendonly\r\n\
a=rtcp-mux\r\n\
a=rtpmap:39 AV1/90000\r\n\
a=fmtp:39 level-idx=5;profile=0;tier=0\r\n\
a=rtpmap:102 H264/90000\r\n\
a=fmtp:102 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=640c1f\r\n\
a=rtpmap:104 H264/90000\r\n\
a=fmtp:104 level-asymmetry-allowed=1;packetization-mode=0;profile-level-id=640c1f\r\n\
a=rtpmap:98 VP9/90000\r\n\
a=fmtp:98 profile-id=0\r\n\
a=rtpmap:96 VP8/90000\r\n\
m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
a=mid:1\r\n\
a=sendonly\r\n\
a=rtpmap:111 opus/48000/2\r\n";

    fn without(lines: &[&str]) -> String {
        OFFICIAL_SHAPE
            .split("\r\n")
            .filter(|line| !lines.iter().any(|drop| line.starts_with(drop)))
            .collect::<Vec<_>>()
            .join("\r\n")
    }

    #[test]
    fn accepts_official_offer() {
        assert_eq!(check_offer(OFFICIAL_SHAPE), Ok(()));
    }

    #[test]
    fn rejects_without_h264() {
        let offer = without(&["a=rtpmap:102", "a=rtpmap:104"]);
        assert_eq!(check_offer(&offer), Err(OfferError::NoH264));
    }

    #[test]
    fn rejects_only_packetization_mode_0() {
        let offer = without(&["a=rtpmap:102", "a=fmtp:102"]);
        assert_eq!(check_offer(&offer), Err(OfferError::NoH264));
    }

    #[test]
    fn rejects_oversized() {
        let offer = format!("v=0\r\n{}", "a=x\r\n".repeat(4000));
        assert_eq!(check_offer(&offer), Err(OfferError::TooLarge));
    }
}
