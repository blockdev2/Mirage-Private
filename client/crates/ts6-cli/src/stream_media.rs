//! Optional synthetic media experiment. SDP/ICE stay in private subprocess pipes.

use super::stream_check::Owner;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout},
};
use ts6_core::{streams::ControlKind, ClientConfig};

struct Controller {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    broken: bool,
}

impl Controller {
    async fn start(directory: &std::path::Path) -> Result<Self> {
        let runtime = directory.join("browser-runtime");
        std::fs::create_dir_all(&runtime)?;
        let mut command = tokio::process::Command::new("python");
        #[cfg(windows)]
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW for the pipe controller.
        let mut child = command
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../../scripts/stream_webrtc_controller.py"
            ))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env("TEMP", &runtime)
            .env("TMP", &runtime)
            .env("TMPDIR", &runtime)
            .kill_on_drop(true)
            .spawn()
            .context("cannot start optional browser controller")?;
        let input = child
            .stdin
            .take()
            .context("missing controller input pipe")?;
        let output = BufReader::new(
            child
                .stdout
                .take()
                .context("missing controller output pipe")?,
        );
        let mut controller = Self {
            child,
            input,
            output,
            broken: false,
        };
        if controller.call(json!({"op":"init"})).await.is_err() {
            let _ = controller.stop().await;
            bail!("browser controller initialization failed; check Playwright and Chromium installation");
        }
        Ok(controller)
    }

    async fn call(&mut self, command: Value) -> Result<Value> {
        if self.broken {
            bail!("browser pipe is unusable after an earlier failure");
        }
        let mut bytes = serde_json::to_vec(&command)?;
        if bytes.len() >= 32768 {
            bail!("browser command exceeds local limit");
        }
        bytes.push(b'\n');
        // A cancelled read must never be followed by another request on this pipe.
        self.broken = true;
        let result = tokio::time::timeout(Duration::from_secs(15), async {
            self.input.write_all(&bytes).await?;
            self.input.flush().await?;
            let mut line = String::new();
            (&mut self.output).take(32769).read_line(&mut line).await?;
            if line.len() > 32768 || !line.ends_with('\n') {
                bail!("invalid browser response frame");
            }
            let response: Value = serde_json::from_str(&line)
                .map_err(|_| anyhow!("invalid browser response JSON"))?;
            if response.get("ok") != Some(&Value::Bool(true)) {
                bail!("browser operation failed");
            }
            Ok(response.get("data").cloned().unwrap_or(Value::Null))
        })
        .await
        .context("browser controller deadline exceeded")?;
        if result.is_ok() {
            self.broken = false;
        }
        result
    }

    async fn stop(&mut self) -> Result<()> {
        if self.broken {
            let _ = self.child.kill().await;
            tokio::time::timeout(Duration::from_secs(5), self.child.wait())
                .await
                .context("failed browser cleanup deadline exceeded")??;
            return Ok(());
        }
        let shutdown = self.call(json!({"op":"shutdown"})).await;
        if shutdown.is_err() {
            let _ = self.child.kill().await;
        }
        let status = match tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await {
            Ok(status) => status?,
            Err(_) => {
                self.child.kill().await?;
                self.child.wait().await?
            }
        };
        shutdown?;
        if !status.success() {
            bail!("browser controller exited unsuccessfully");
        }
        Ok(())
    }
}

fn string_field(value: &Value, name: &str) -> Result<String> {
    value
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("browser response missing required string"))
}

pub async fn run(mut config: ClientConfig, directory: PathBuf) -> Result<()> {
    if config
        .channel
        .as_ref()
        .is_none_or(|channel| channel.trim().is_empty())
        || config.log_commands
    {
        bail!("stream-media-check requires a test channel and disabled verbose logging");
    }
    std::fs::create_dir_all(&directory)?;
    config.nickname = "Mirage-video-publisher".into();
    config.identity_file = directory.join("media-publisher.json");
    let mut publisher = Owner::start(config.clone()).await?;
    config.nickname = "Mirage-video-viewer".into();
    config.identity_file = directory.join("media-viewer.json");
    let mut viewer = match Owner::start(config).await {
        Ok(viewer) => viewer,
        Err(error) => {
            publisher.stop().await?;
            return Err(error);
        }
    };
    let mut browser = match Controller::start(&directory).await {
        Ok(browser) => browser,
        Err(error) => {
            let a = publisher.stop().await;
            let b = viewer.stop().await;
            a?;
            b?;
            return Err(error);
        }
    };
    let mut active = None;
    let experiment: Result<Value> = async {
        publisher.send("setupstream", vec![
            ("name","Mirage synthetic video".into()),("type","2".into()),("bitrate","800".into()),
            ("accessibility","1".into()),("mode","1".into()),("viewer_limit","1".into()),("audio","0".into()),
        ]).await.context("video announcement rejected")?;
        let event=publisher.event(ControlKind::Started,None,publisher.id).await?;
        let id=event.control.stream_id;
        active=Some(id.clone());
        viewer.event(ControlKind::Started,Some(&id),publisher.id).await?;
        viewer.send("joinstreamrequest",vec![("id",id.clone()),("clid",publisher.id.to_string()),
            ("msg",String::new()),("is_remove","0".into())]).await?;
        publisher.event(ControlKind::JoinRequested,Some(&id),viewer.id).await?;
        let offer=string_field(&browser.call(json!({"op":"offer"})).await?,"sdp")?;
        publisher.send("respondjoinstreamrequest",vec![("id",id.clone()),("clid",viewer.id.to_string()),
            ("msg",String::new()),("offer",offer),("decision","1".into())]).await.context("SDP admission rejected")?;
        let received=viewer.event(ControlKind::JoinResponded,Some(&id),publisher.id).await?;
        if received.control.accepted!=Some(true) { bail!("viewer was not admitted"); }
        let answer=string_field(&browser.call(json!({"op":"accept_offer","sdp":received.payload.context("offer missing")?})).await
            .context("browser rejected relayed offer")?,"sdp")?;
        viewer.send("streamsignaling",vec![("id",id.clone()),("clid",publisher.id.to_string()),
            ("json",json!({"cmd":"answer","args":{"answer":answer}}).to_string())]).await?;
        let received=publisher.event(ControlKind::Signaling,Some(&id),viewer.id).await?;
        let answer:Value=serde_json::from_str(&received.payload.context("answer missing")?)
            .map_err(|_| anyhow!("invalid relayed answer JSON"))?;
        if answer.get("cmd").and_then(Value::as_str)!=Some("answer") { bail!("unexpected signaling command"); }
        browser.call(json!({"op":"accept_answer","sdp":string_field(&answer["args"],"answer")?})).await
            .context("browser rejected relayed answer")?;
        let deadline=tokio::time::Instant::now()+Duration::from_secs(20);
        let mut ice_relayed=0;
        let stats=loop {
            if tokio::time::Instant::now()>=deadline { bail!("decoded video fixture deadline exceeded"); }
            let candidates=browser.call(json!({"op":"candidates"})).await?;
            for role in ["publisher","viewer"] {
                let list=candidates[role].as_array().context("invalid candidate response")?;
                if list.len()>64 { bail!("candidate count exceeds local bound"); }
                for candidate in list {
                    let (sender,receiver,target)=if role=="publisher" {
                        (&publisher,&mut viewer,"viewer")
                    } else { (&viewer,&mut publisher,"publisher") };
                    let payload=json!({"cmd":"iceCandidate","args":{
                        "sdp":candidate["candidate"],"mid":candidate["sdpMid"],"mLine":candidate["sdpMLineIndex"]}});
                    sender.send("streamsignaling",vec![("id",id.clone()),("clid",receiver.id.to_string()),("json",payload.to_string())]).await?;
                    let event=receiver.event(ControlKind::Signaling,Some(&id),sender.id).await?;
                    let signal:Value=serde_json::from_str(&event.payload.context("ICE payload missing")?)
                        .map_err(|_| anyhow!("invalid relayed ICE JSON"))?;
                    if signal["cmd"]!="iceCandidate" { bail!("unexpected ICE signaling command"); }
                    browser.call(json!({"op":"ice","role":target,"candidate":signal["args"]})).await?;
                    ice_relayed+=1;
                    tokio::time::sleep(Duration::from_millis(75)).await;
                }
            }
            let stats=browser.call(json!({"op":"stats"})).await?;
            if stats["frames_decoded"].as_u64().unwrap_or(0)>=5
                && stats["fixture_pixel_matches"]==true && stats["connection_state"]=="connected" { break stats; }
            tokio::time::sleep(Duration::from_millis(250)).await;
        };
        browser.call(json!({"op":"snapshot"})).await?;
        viewer.send("joinstreamrequest",vec![("id",id.clone()),("clid",publisher.id.to_string()),
            ("msg",String::new()),("is_remove","1".into())]).await?;
        publisher.send("stopstream",vec![("id",id.clone()),("reason","1".into())]).await?;
        viewer.event(ControlKind::Stopped,Some(&id),publisher.id).await?;
        active=None;
        Ok(json!({"synthetic_video":stats,"ice_candidates_relayed":ice_relayed,
            "signaling_via_teamspeak":true,"media_peers_same_host":true,"screen_captured":false,
            "explicit_stop_observed":true}))
    }.await;
    if let Some(id) = active {
        if publisher
            .send("stopstream", vec![("id", id), ("reason", "1".into())])
            .await
            .is_err()
        {
            eprintln!("media diagnostic stop failed; disconnect cleanup follows");
        }
    }
    let browser_stopped = browser.stop().await;
    let publisher_stopped = publisher.stop().await;
    let viewer_stopped = viewer.stop().await;
    if browser_stopped.is_err() || publisher_stopped.is_err() || viewer_stopped.is_err() {
        eprintln!("media diagnostic cleanup reported a failure");
    }
    let summary = experiment?;
    browser_stopped?;
    publisher_stopped?;
    viewer_stopped?;
    println!("{summary}");
    Ok(())
}
