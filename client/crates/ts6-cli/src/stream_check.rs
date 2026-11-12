//! Two continuously polled diagnostic peers; no media or capture devices.

use std::{path::PathBuf, thread, time::Duration};

use anyhow::{anyhow, bail, Context, Result};
use futures::future::poll_fn;
use tokio::sync::{mpsc, oneshot};
use ts6_core::{
    streams::{ControlKind, ControlNotice, StreamEvent},
    ClientConfig, Item, Session, Wake,
};

struct Request {
    name: &'static str,
    args: Vec<(&'static str, String)>,
    reply: oneshot::Sender<bool>,
}

pub(super) struct Owner {
    pub(super) id: u16,
    commands: mpsc::Sender<Request>,
    notices: mpsc::Receiver<StreamEvent>,
    thread: thread::JoinHandle<Result<()>>,
}

impl Owner {
    pub(super) async fn start(config: ClientConfig) -> Result<Self> {
        let (commands, mut incoming) = mpsc::channel::<Request>(8);
        let (outgoing, notices) = mpsc::channel(64);
        let (ready, started) = oneshot::channel();
        let thread = thread::Builder::new()
            .name("stream-control-owner".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                runtime.block_on(async move {
                    let mut session = Session::connect(config.clone())
                        .await
                        .map_err(|_| anyhow!("diagnostic connection failed"))?;
                    let result: Result<()> = async {
                        let snapshot = session.snapshot()?;
                        let own = snapshot
                            .clients
                            .iter()
                            .find(|client| client.is_self)
                            .context("diagnostic client absent from snapshot")?;
                        let expected = snapshot
                            .find_channel(config.channel.as_deref().unwrap_or_default())
                            .context("test channel absent")?;
                        if own.channel_id != expected.id {
                            bail!("diagnostic joined a different channel");
                        }
                        let _ = ready.send(own.id);
                        let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
                        loop {
                            if tokio::time::Instant::now() >= deadline {
                                bail!("diagnostic owner deadline exceeded");
                            }
                            let wake = tokio::time::timeout_at(
                                deadline,
                                poll_fn(|cx| session.poll_with(None, Some(&mut incoming), cx)),
                            )
                            .await
                            .context("diagnostic owner deadline exceeded")?;
                            match wake {
                                Some(Wake::Command(request)) => {
                                    let accepted = session
                                        .send_command(request.name, &request.args)
                                        .await
                                        .is_ok();
                                    let _ = request.reply.send(accepted);
                                }
                                Some(Wake::Item(Item::UnknownCommand { content, .. })) => {
                                    if let Some(notice) = StreamEvent::parse(&content) {
                                        outgoing.try_send(notice).map_err(|_| {
                                            anyhow!("diagnostic notice queue unavailable")
                                        })?;
                                    }
                                }
                                Some(_) => {}
                                None => break,
                            }
                        }
                        Ok(())
                    }
                    .await;
                    let disconnected = session.disconnect().await;
                    result?;
                    disconnected.map_err(|_| anyhow!("diagnostic disconnect failed"))
                })
            })?;
        let id = match tokio::time::timeout(Duration::from_secs(25), started).await {
            Ok(Ok(id)) => id,
            _ => {
                // Joining also waits for disconnect when channel selection fails.
                drop(commands);
                tokio::task::spawn_blocking(move || thread.join())
                    .await?
                    .map_err(|_| anyhow!("diagnostic owner panicked during startup"))?
                    .context("diagnostic connection or channel selection failed")?;
                bail!("diagnostic startup deadline exceeded or readiness was not reported");
            }
        };
        Ok(Self {
            id,
            commands,
            notices,
            thread,
        })
    }

    pub(super) async fn send(
        &self,
        name: &'static str,
        args: Vec<(&'static str, String)>,
    ) -> Result<()> {
        let (reply, result) = oneshot::channel();
        self.commands
            .try_send(Request { name, args, reply })
            .map_err(|_| anyhow!("diagnostic command queue unavailable"))?;
        let accepted = tokio::time::timeout(Duration::from_secs(15), result)
            .await
            .context("diagnostic command deadline exceeded")?
            .context("diagnostic command owner ended")?;
        if !accepted {
            bail!("stream control command refused or timed out");
        }
        Ok(())
    }

    async fn notice(
        &mut self,
        kind: ControlKind,
        stream: Option<&str>,
        client: u16,
    ) -> Result<ControlNotice> {
        Ok(self.event(kind, stream, client).await?.control)
    }

    pub(super) async fn event(
        &mut self,
        kind: ControlKind,
        stream: Option<&str>,
        client: u16,
    ) -> Result<StreamEvent> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if tokio::time::Instant::now() >= deadline {
                bail!("stream notification deadline exceeded");
            }
            let notice = tokio::time::timeout_at(deadline, self.notices.recv())
                .await
                .context("stream notification deadline exceeded")?
                .context("stream notification owner ended")?;
            if notice.control.kind == kind
                && notice.control.client_id == client
                && stream.is_none_or(|stream| notice.control.stream_id == stream)
            {
                return Ok(notice);
            }
        }
    }

    pub(super) async fn stop(self) -> Result<()> {
        drop(self.commands);
        tokio::task::spawn_blocking(move || self.thread.join())
            .await?
            .map_err(|_| anyhow!("diagnostic owner panicked"))?
    }
}

pub async fn run(mut config: ClientConfig, directory: PathBuf) -> Result<()> {
    if config
        .channel
        .as_ref()
        .is_none_or(|channel| channel.trim().is_empty())
    {
        bail!("stream-check requires an explicit test channel");
    }
    if config.log_commands {
        bail!("stream-check does not permit verbose protocol logging");
    }
    std::fs::create_dir_all(&directory)?;
    config.nickname = "Mirage-stream-publisher".into();
    config.identity_file = directory.join("publisher.json");
    let mut publisher = Owner::start(config.clone()).await?;
    config.nickname = "Mirage-stream-viewer".into();
    config.identity_file = directory.join("viewer.json");
    let viewer = Owner::start(config).await;
    let mut viewer = match viewer {
        Ok(viewer) => viewer,
        Err(error) => {
            publisher.stop().await?;
            return Err(error);
        }
    };
    let mut active = None;
    let result: Result<()> = async {
        publisher
            .send(
                "setupstream",
                vec![
                    ("name", "Mirage control fixture".into()),
                    ("type", "2".into()),
                    ("bitrate", "800".into()),
                    ("accessibility", "1".into()),
                    ("mode", "1".into()),
                    ("viewer_limit", "1".into()),
                    ("audio", "0".into()),
                ],
            )
            .await?;
        let started = publisher
            .notice(ControlKind::Started, None, publisher.id)
            .await?;
        active = Some(started.stream_id.clone());
        let id = started.stream_id;
        viewer
            .notice(ControlKind::Started, Some(&id), publisher.id)
            .await?;
        viewer
            .send(
                "requeststreaminfo",
                vec![("clid", publisher.id.to_string())],
            )
            .await?;
        viewer
            .notice(ControlKind::Info, Some(&id), publisher.id)
            .await?;
        viewer
            .send(
                "joinstreamrequest",
                vec![
                    ("id", id.clone()),
                    ("clid", publisher.id.to_string()),
                    ("msg", String::new()),
                    ("is_remove", "0".into()),
                ],
            )
            .await?;
        publisher
            .notice(ControlKind::JoinRequested, Some(&id), viewer.id)
            .await?;
        publisher
            .send(
                "respondjoinstreamrequest",
                vec![
                    ("id", id.clone()),
                    ("clid", viewer.id.to_string()),
                    ("msg", String::new()),
                    ("offer", String::new()),
                    ("decision", "0".into()),
                ],
            )
            .await?;
        let response = viewer
            .notice(ControlKind::JoinResponded, Some(&id), publisher.id)
            .await?;
        if response.accepted != Some(false) {
            bail!("unexpected viewer admission result");
        }
        publisher
            .send(
                "stopstream",
                vec![("id", id.clone()), ("reason", "1".into())],
            )
            .await?;
        viewer
            .notice(ControlKind::Stopped, Some(&id), publisher.id)
            .await?;
        active = None;
        Ok(())
    }
    .await;
    if let Some(id) = active {
        // A failed experiment must still attempt to remove its announcement.
        if publisher
            .send("stopstream", vec![("id", id), ("reason", "1".into())])
            .await
            .is_err()
        {
            eprintln!("diagnostic stop command failed; disconnect cleanup follows");
        }
    }
    let publisher_stopped = publisher.stop().await;
    let viewer_stopped = viewer.stop().await;
    result?;
    publisher_stopped?;
    viewer_stopped?;
    println!(
        "{}",
        serde_json::json!({"announcement_observed":true,"viewer_request_observed":true,
        "info_observed":true,"refusal_observed":true,"explicit_stop_observed":true,"media_transmitted":false})
    );
    Ok(())
}
