//! Spike: ask to watch a stream someone else (e.g. the official TS6 client)
//! starts, and print only the shape of its SDP offer: media lines, codecs,
//! formats and feedback. Addresses, ICE credentials and fingerprints are never
//! printed. No answer is sent, so no media flows.

use super::stream_check::Owner;
use anyhow::{bail, Context, Result};
use std::{path::PathBuf, time::Duration};
use ts6_core::{streams::ControlKind, ClientConfig};

/// The SDP lines worth reporting; everything else may carry secrets.
fn summarize(sdp: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut candidates = std::collections::BTreeMap::<String, u32>::new();
    for line in sdp.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("a=candidate:") {
            // Only the candidate type (host/srflx/relay) and transport.
            let parts: Vec<&str> = rest.split_whitespace().collect();
            let kind = parts
                .iter()
                .position(|p| *p == "typ")
                .and_then(|i| parts.get(i + 1))
                .unwrap_or(&"?");
            let transport = parts.get(2).unwrap_or(&"?").to_ascii_lowercase();
            *candidates.entry(format!("{transport}/{kind}")).or_default() += 1;
            continue;
        }
        let keep = line.starts_with("m=")
            || line.starts_with("a=rtpmap:")
            || line.starts_with("a=fmtp:")
            || line.starts_with("a=rtcp-fb:")
            || line.starts_with("a=extmap:")
            || line.starts_with("a=group:")
            || line.starts_with("a=setup:")
            || line.starts_with("a=ice-options:")
            || matches!(
                line,
                "a=sendrecv" | "a=sendonly" | "a=recvonly" | "a=inactive" | "a=rtcp-mux"
            );
        if keep {
            lines.push(line.to_string());
        }
    }
    for (kind, count) in candidates {
        lines.push(format!("candidates {kind}: {count}"));
    }
    lines
}

pub async fn run(mut config: ClientConfig, directory: PathBuf) -> Result<()> {
    if config
        .channel
        .as_ref()
        .is_none_or(|channel| channel.trim().is_empty())
        || config.log_commands
    {
        bail!("stream-offer requires a channel and disabled verbose logging");
    }
    std::fs::create_dir_all(&directory)?;
    config.nickname = "Mirage-viewer-probe".into();
    config.identity_file = directory.join("viewer-probe.json");
    let mut viewer = Owner::start(config).await?;
    println!("ready: start a screen share in this channel now (waiting 75 s)");

    let result: Result<()> = async {
        let started = viewer
            .event_within(ControlKind::Started, None, None, Duration::from_secs(75))
            .await
            .context("no stream started in time")?;
        let id = started.control.stream_id;
        let publisher = started.control.client_id;
        println!("stream started by client {publisher}; asking to watch");
        viewer
            .send(
                "joinstreamrequest",
                vec![
                    ("id", id.clone()),
                    ("clid", publisher.to_string()),
                    ("msg", String::new()),
                    ("is_remove", "0".into()),
                ],
            )
            .await?;
        let answer = viewer
            .event_within(
                ControlKind::JoinResponded,
                Some(&id),
                Some(publisher),
                Duration::from_secs(30),
            )
            .await
            .context("the publisher did not answer the request")?;
        println!("admitted: {:?}", answer.control.accepted);
        if let Some(offer) = &answer.payload {
            println!("--- offer summary ({} bytes) ---", offer.len());
            for line in summarize(offer) {
                println!("{line}");
            }
        }
        // Count what the publisher signals next, without printing it.
        let mut signals = std::collections::BTreeMap::<String, u32>::new();
        while let Ok(event) = viewer
            .event_within(
                ControlKind::Signaling,
                Some(&id),
                Some(publisher),
                Duration::from_secs(5),
            )
            .await
        {
            let command = event
                .payload
                .and_then(|p| serde_json::from_str::<serde_json::Value>(&p).ok())
                .and_then(|v| v.get("cmd").and_then(|c| c.as_str()).map(str::to_owned))
                .unwrap_or_else(|| "?".into());
            *signals.entry(command).or_default() += 1;
        }
        println!("signaling after offer: {signals:?}");
        viewer
            .send(
                "joinstreamrequest",
                vec![
                    ("id", id),
                    ("clid", publisher.to_string()),
                    ("msg", String::new()),
                    ("is_remove", "1".into()),
                ],
            )
            .await?;
        Ok(())
    }
    .await;
    let stopped = viewer.stop().await;
    result?;
    stopped
}

#[cfg(test)]
mod tests {
    #[test]
    fn summary_keeps_codecs_and_hides_secrets() {
        let sdp = "v=0\r\no=- 1 2 IN IP4 10.0.0.5\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\nc=IN IP4 0.0.0.0\r\na=ice-ufrag:SECRETU\r\na=ice-pwd:SECRETP\r\na=fingerprint:sha-256 AA:BB\r\na=setup:actpass\r\na=rtpmap:96 H264/90000\r\na=fmtp:96 profile-level-id=42e01f\r\na=candidate:1 1 udp 2122260223 192.168.1.9 50000 typ host\r\n";
        let lines = super::summarize(sdp).join("\n");
        assert!(lines.contains("a=rtpmap:96 H264/90000"));
        assert!(lines.contains("candidates udp/host: 1"));
        for secret in ["SECRETU", "SECRETP", "AA:BB", "192.168", "10.0.0.5"] {
            assert!(!lines.contains(secret), "{secret}");
        }
    }
}
