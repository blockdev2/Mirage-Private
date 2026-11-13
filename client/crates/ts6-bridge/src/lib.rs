//! `ts6-bridge` - owns one TS6 session and exposes it to the desktop UI over a
//! local WebSocket.
//!
//! Why a bridge instead of driving the session from the UI process: the session is
//! single-threaded (the protocol library is `!Send`), the audio pipeline lives on
//! its own OS threads, and keeping them out of the UI process makes the same React
//! app usable inside Tauri later without rewriting the transport.
//!
//! Run:  cargo run -p ts6-bridge -- [--port 8787] [--ui <dir>]

mod audio;
mod media;
mod probe;
mod protocol;
pub mod stream_media;
mod streams;

/// Chat attachment parsing, shared with the native frontend.
pub use ts6_core::attachments;

use std::net::SocketAddr;
use std::path::PathBuf;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use futures::{SinkExt, StreamExt};
use tokio::sync::{broadcast, mpsc};
use tower_http::services::ServeDir;

use protocol::{Command, Event};
use ts6_core::{Item, Session, Wake};

#[derive(Clone)]
struct App {
    port: u16,
    commands: mpsc::Sender<Command>,
    events: broadcast::Sender<String>,
}

pub fn main_entry() -> anyhow::Result<()> {
    // Another subscriber may already be installed when hosted inside the desktop.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,ts6_bridge=debug")),
        )
        .try_init();

    let port: u16 = std::env::var("TS6_BRIDGE_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .or_else(|| arg_value("--port").and_then(|value| value.parse().ok()))
        .unwrap_or(8787);
    let ui_dir = std::env::var("TS6_UI_DIR")
        .ok()
        .map(PathBuf::from)
        .or_else(|| arg_value("--ui").map(PathBuf::from))
        .unwrap_or_else(default_ui_dir);

    // Session-only diagnostic mode (docs/verification.md V-23).
    if let Some(address) = arg_value("--probe") {
        let options = probe::Options {
            address,
            seconds: arg_value("--seconds")
                .and_then(|value| value.parse().ok())
                .unwrap_or(20),
            tone: !has_flag("--no-tone"),
            commands: !has_flag("--no-commands"),
        };
        let runtime = if has_flag("--current-thread") {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
        } else {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()?
        };
        return runtime.block_on(probe::run(options));
    }

    // The protocol session is not `Send`, so it stays on the thread that runs this
    // future (`Runtime::block_on`). Worker threads are still needed: the library
    // drives socket I/O from the poll of its event stream. Historical receive
    // delays had several confounding factors (docs/verification.md V-23/V-25);
    // they do not establish that a single-thread runtime caused the symptom.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    runtime.block_on(async move { serve(port, ui_dir).await })
}

fn arg_value(flag: &str) -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == flag {
            return args.next();
        }
        if let Some(rest) = arg.strip_prefix(&format!("{flag}=")) {
            return Some(rest.to_string());
        }
    }
    None
}

/// Whether a bare flag is present (flags that take no value).
fn has_flag(flag: &str) -> bool {
    std::env::args().skip(1).any(|arg| arg == flag)
}
pub fn default_ui_dir() -> PathBuf {
    // <repo>/client/crates/ts6-bridge -> <repo>/client/apps/ui/dist
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    here.parent()
        .and_then(|p| p.parent())
        .map(|client| client.join("apps").join("ui").join("dist"))
        .unwrap_or_else(|| PathBuf::from("apps/ui/dist"))
}

pub async fn serve(port: u16, ui_dir: PathBuf) -> anyhow::Result<()> {
    serve_inner(port, Some(ui_dir)).await
}

/// How often the session loop wakes with nothing else to do.
const SESSION_TICK: std::time::Duration = std::time::Duration::from_millis(10);

async fn serve_inner(port: u16, ui_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let (commands, command_rx) = mpsc::channel::<Command>(32);
    let (events, _) = broadcast::channel::<String>(256);
    // The session loop also receives reports from its own transfer tasks.
    let internal = commands.clone();
    // Wakes the session loop regularly. Speaking rings expire on it, and so
    // does incoming network traffic: a packet that arrives while the loop
    // idles is only noticed at the next wake-up (measured on Windows: the
    // server answers a ping in 0.2 ms, yet the reply was handled up to one
    // tick later and the ping read 30-90 ms). A short tick bounds that wait
    // for pings and for the first packet of incoming voice.
    let ticker = commands.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(SESSION_TICK);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            // A full queue means the loop is awake anyway; skip this tick.
            if let Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) =
                ticker.try_send(Command::Tick)
            {
                break;
            }
        }
    });

    let serves_static_ui = ui_dir
        .as_ref()
        .is_some_and(|path| !path.as_os_str().is_empty());
    let app = build_router(port, commands, events.clone(), ui_dir);

    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port))).await?;
    println!("ts6-bridge: http://127.0.0.1:{port} (static UI: {serves_static_ui})");

    // The HTTP server runs as its own task; the protocol session stays on the thread
    // that runs this future (the connection is `!Send`). Keep the existing ownership
    // structure unless a continuously polled alternative is regression tested.
    // The earlier V-21 runtime diagnosis was superseded; see V-23 and V-25.
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    session_task(command_rx, events, internal).await;
    match server.await {
        Ok(result) => result?,
        Err(error) => tracing::warn!(%error, "http server task failed"),
    }
    Ok(())
}

fn build_router(
    port: u16,
    commands: mpsc::Sender<Command>,
    events: broadcast::Sender<String>,
    ui_dir: Option<PathBuf>,
) -> Router {
    let router = Router::new().route("/ws", get(ws_handler)).with_state(App {
        port,
        commands,
        events,
    });

    match ui_dir.filter(|path| !path.as_os_str().is_empty()) {
        Some(ui_dir) => {
            router.fallback_service(ServeDir::new(ui_dir).append_index_html_on_directories(true))
        }
        None => router,
    }
}

fn browser_allowed(headers: &HeaderMap, port: u16) -> bool {
    let hosts = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    if !headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|host| hosts.iter().any(|allowed| host == allowed))
    {
        return false;
    }
    // Native diagnostic clients have no Origin. Local processes remain trusted.
    match headers.get("origin") {
        None => true,
        Some(origin) => origin.to_str().is_ok_and(|origin| {
            hosts.iter().any(|host| origin == format!("http://{host}"))
                || origin == "http://127.0.0.1:5174"
                || origin == "http://localhost:5174"
        }),
    }
}

async fn ws_handler(ws: WebSocketUpgrade, State(app): State<App>, headers: HeaderMap) -> Response {
    if !browser_allowed(&headers, app.port) {
        return StatusCode::FORBIDDEN.into_response();
    }
    ws.max_message_size(16 * 1024)
        .max_frame_size(16 * 1024)
        .on_upgrade(move |socket| ws_session(socket, app))
}

async fn ws_session(socket: WebSocket, app: App) {
    let (mut sender, mut receiver) = socket.split();
    let mut events = app.events.subscribe();

    // Greeting: version plus the device lists the UI needs for its settings panel.
    let hello = Event::Hello {
        version: env!("CARGO_PKG_VERSION"),
        channel_chat: true,
        devices: audio::device_lists(),
    };
    if let Ok(json) = serde_json::to_string(&hello) {
        if sender.send(Message::Text(json.into())).await.is_err() {
            return;
        }
    }

    // A frontend may reload while the server session remains alive.
    // Request its current status and snapshot after subscribing to broadcasts.
    if app.commands.send(Command::Refresh).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(json) => {
                    if sender.send(Message::Text(json.into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "ui fell behind on events");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            message = receiver.next() => match message {
                Some(Ok(Message::Text(text))) => {
                    match serde_json::from_str::<Command>(text.as_str()) {
                        Ok(command) => {
                            tracing::debug!(?command, "ui command");
                            let _ = app.commands.send(command).await;
                        }
                        Err(error) => {
                            let log = Event::Log {
                                level: "error",
                                text: format!("invalid command: {error}"),
                                ts: protocol::timestamp(),
                            };
                            if let Ok(json) = serde_json::to_string(&log) {
                                let _ = sender.send(Message::Text(json.into())).await;
                            }
                        }
                    }
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {}
                Some(Err(error)) => {
                    tracing::debug!(%error, "websocket error");
                    break;
                }
            },
        }
    }
}

fn log(events: &broadcast::Sender<String>, level: &str, text: impl Into<String>) {
    protocol::publish(
        events,
        &Event::Log {
            level,
            text: text.into(),
            ts: protocol::timestamp(),
        },
    );
}

/// Compares our channel's members with the last snapshot: someone new means a
/// join sound, someone gone a leave sound. Moving channels ourselves sounds as
/// a join; the first snapshot of a session is silent.
fn membership_cue(
    members: &mut Option<(u64, std::collections::HashSet<u16>)>,
    snapshot: &ts6_core::Snapshot,
) -> Option<ts6_audio::cues::Cue> {
    use ts6_audio::cues::Cue;
    let own = snapshot.clients.iter().find(|c| c.is_self)?;
    let now: std::collections::HashSet<u16> = snapshot
        .clients
        .iter()
        .filter(|c| c.channel_id == own.channel_id && !c.is_self)
        .map(|c| c.id)
        .collect();
    let previous = members.replace((own.channel_id, now.clone()));
    // The first snapshot of a session: we just entered the call.
    let Some((channel, before)) = previous else {
        return Some(Cue::Join);
    };
    if channel != own.channel_id {
        return Some(Cue::Join);
    }
    if now.difference(&before).next().is_some() {
        Some(Cue::Join)
    } else if before.difference(&now).next().is_some() {
        Some(Cue::Leave)
    } else {
        None
    }
}

fn publish_state(session: &mut Session, events: &broadcast::Sender<String>) {
    match session.snapshot() {
        Ok(snapshot) => {
            protocol::publish(
                events,
                &Event::Status {
                    session_id: Some(session.session_id()),
                    connected: true,
                    address: Some(session.address()),
                    nickname: snapshot
                        .clients
                        .iter()
                        .find(|client| client.is_self)
                        .map(|client| client.name.as_str()),
                    error: None,
                },
            );
            protocol::publish(
                events,
                &Event::State {
                    snapshot: &snapshot,
                },
            );
        }
        Err(error) => log(events, "error", format!("state unavailable: {error:#}")),
    }
}

/// Publishes the snapshot, then fetches any avatar or icon it newly references.
fn refresh(
    session: &mut Session,
    events: &broadcast::Sender<String>,
    media: &mut media::Media,
    internal: &mpsc::Sender<Command>,
) {
    publish_state(session, events);
    if let Ok(snapshot) = session.snapshot() {
        media.sync(session, &snapshot, internal);
    }
    publish_media(media, events);
}

fn publish_connection_info(session: &Session, client: u16, events: &broadcast::Sender<String>) {
    let info = session.connection_info(client);
    protocol::publish(
        events,
        &Event::ConnectionInfo {
            client,
            info: info.as_ref(),
        },
    );
}

fn publish_media(media: &media::Media, events: &broadcast::Sender<String>) {
    protocol::publish(
        events,
        &Event::Media {
            avatars: media.avatars(),
            server_icon: media.server_icon(),
        },
    );
}

fn publish_audio(
    engine: &mut audio::AudioEngine,
    events: &broadcast::Sender<String>,
    tx_frames: u64,
    own: Option<ts6_core::OwnNetwork>,
) {
    let counters = engine.counters();
    protocol::publish(
        events,
        &Event::Audio {
            rx_packets: counters.rx_packets,
            tx_frames,
            ptt_down: counters.ptt_down,
            speaking: counters.speaking,
            transmitting: counters.transmitting,
            input_level_db: counters.input_level_db,
            capture: counters.capture,
            playback: counters.playback,
            tone: counters.tone,
            ptt_binding: counters.ptt_binding,
            ping_ms: own.map(|net| net.ping_ms),
            loss_out: own.map(|net| net.loss),
            lost_packets: own.map(|net| net.lost_packets),
            counted_packets: own.map(|net| net.counted_packets),
            levels: counters.levels,
        },
    );
}

async fn session_task(
    mut commands: mpsc::Receiver<Command>,
    events: broadcast::Sender<String>,
    internal: mpsc::Sender<Command>,
) {
    let mut session: Option<Session> = None;
    let mut media = media::Media::new();
    // Client whose connection statistics the UI is showing, and when they
    // were last requested.
    let mut watched: Option<u16> = None;
    let mut info_requested: Option<std::time::Instant> = None;
    // Our channel and who is in it, to sound joins and leaves.
    let mut members: Option<(u64, std::collections::HashSet<u16>)> = None;
    // Screen streams: signaling state, the viewer worker, and our channel
    // (leaving it ends the watched stream).
    let mut streams = streams::StreamRegistry::default();
    let mut viewer: Option<stream_media::StreamWorker> = None;
    let mut own_channel: Option<u64> = None;
    let mut engine = audio::AudioEngine::new();
    let mut config = audio::AudioConfig::default();
    // Diagnostic counter (docs/verification.md V-21): tells "packets never arrive"
    // apart from "packets arrive but are not surfaced".
    let mut audio_items: u64 = 0;
    // Heartbeat diagnostics: proves whether the loop is running while voice is
    // expected (docs/verification.md V-21).
    let mut polls: u64 = 0;
    let mut last_beat = std::time::Instant::now();

    loop {
        let mut pending: Option<Command> = None;
        let mut lost = false;

        match session.as_mut() {
            Some(active) => {
                let wake = futures::future::poll_fn(|cx| {
                    active.poll_with(Some(engine.mic_rx_mut()), Some(&mut commands), cx)
                })
                .await;
                polls += 1;
                if last_beat.elapsed() >= std::time::Duration::from_secs(2) {
                    tracing::debug!(
                        polls,
                        audio_items,
                        rx = engine.counters().rx_packets,
                        tx = active.audio_frames_sent(),
                        can_send = active.can_send_audio(),
                        can_recv = active.can_receive_audio(),
                        "session heartbeat"
                    );
                    last_beat = std::time::Instant::now();
                }

                match wake {
                    None => {
                        tracing::warn!(polls, "poll_with returned None: session over");
                        lost = true
                    }
                    Some(Wake::MicSent) => {} // frames already counted by the session
                    Some(Wake::Command(command)) => pending = Some(command),
                    Some(Wake::Item(item)) => match item {
                        Item::StateChanged => {
                            refresh(active, &events, &mut media, &internal);
                            if let Ok(snapshot) = active.snapshot() {
                                if let Some(cue) = membership_cue(&mut members, &snapshot) {
                                    engine.play_cue(cue);
                                }
                                let channel = snapshot
                                    .clients
                                    .iter()
                                    .find(|client| client.is_self)
                                    .map(|client| client.channel_id);
                                if own_channel.is_some() && channel != own_channel {
                                    let actions = streams.stop();
                                    stream_actions(
                                        actions,
                                        Some(&mut *active),
                                        &mut viewer,
                                        &events,
                                        &internal,
                                    );
                                    streams.reset();
                                }
                                own_channel = channel;
                            }
                            if let Some(client) = watched {
                                publish_connection_info(active, client, &events);
                            }
                        }
                        Item::FileDownload {
                            handle,
                            size,
                            stream,
                        } => media.on_download(handle, size, stream, &internal),
                        Item::FileUpload { handle, stream } => {
                            media.on_upload(handle, stream, &internal)
                        }
                        Item::FileFailed { handle, error } => match media.on_failed(handle, &error)
                        {
                            media::Failed::Avatar(text) => log(&events, "warn", text),
                            media::Failed::Attachment {
                                request_id,
                                refusal,
                            } => publish_chat_result(&events, request_id, Some(refusal)),
                            media::Failed::ChatFile { channel, path } => {
                                publish_chat_file(&events, channel, path, None)
                            }
                            media::Failed::Unknown => {}
                        },
                        Item::Audio(packet) => {
                            audio_items += 1;
                            if audio_items <= 3 || audio_items.is_multiple_of(100) {
                                tracing::info!(
                                    audio_items,
                                    "voice packet reached the bridge session task"
                                );
                            }
                            engine.on_audio(packet)
                        }
                        Item::TextMessage(message) => {
                            if matches!(message.target, ts6_core::MessageTarget::Channel) {
                                if let Some(channel) = message.channel_id.map(|id| id.to_string()) {
                                    let attachments =
                                        protocol::Attachment::from_text(&message.message);
                                    protocol::publish(
                                        &events,
                                        &Event::Chat {
                                            channel,
                                            sender: message
                                                .invoker_name
                                                .unwrap_or_else(|| "Unknown sender".into()),
                                            attachments,
                                            text: message.message,
                                            from_self: message.from_self,
                                            ts: format!("{} UTC", protocol::timestamp()),
                                        },
                                    );
                                }
                            } else {
                                log(
                                    &events,
                                    "text",
                                    format!(
                                        "{}: {}",
                                        message.invoker_name.as_deref().unwrap_or("?"),
                                        message.message
                                    ),
                                );
                            }
                        }
                        Item::UnknownCommand { name, content } => {
                            // Payloads may carry signalling or transfer credentials: log the
                            // command name and size only, never the raw content.
                            tracing::debug!(%name, bytes = content.len(), "unknown server command");
                            if let Some(event) = ts6_core::streams::StreamEvent::parse(&content) {
                                let actions = streams.on_event(&event, std::time::Instant::now());
                                stream_actions(
                                    actions,
                                    Some(&mut *active),
                                    &mut viewer,
                                    &events,
                                    &internal,
                                );
                            }
                        }
                        Item::Reconnecting => log(&events, "warn", "reconnecting..."),
                        Item::IdentityLevel(level) => {
                            tracing::debug!(level, "identity level increasing")
                        }
                    },
                }
            }
            None => match commands.recv().await {
                Some(command) => pending = Some(command),
                None => break,
            },
        }

        if lost {
            protocol::publish(
                &events,
                &Event::Status {
                    session_id: None,
                    connected: false,
                    address: None,
                    nickname: None,
                    error: Some("connection closed"),
                },
            );
            log(&events, "warn", "connection closed");
            session = None;
            members = None;
            own_channel = None;
            streams.reset();
            viewer = None;
            stream_media::frame::frames().close();
            engine.stop();
            media.reset();
            publish_media(&media, &events);
        }

        if let Some(Command::ConnectionInfo { client }) = &pending {
            watched = *client;
            info_requested = None;
            pending = None;
        }
        // Re-request the watched client's statistics once per second. The
        // request does not wait for its answer, so voice is never held up.
        if let (Some(active), Some(client)) = (session.as_mut(), watched) {
            if info_requested.is_none_or(|at| at.elapsed() >= std::time::Duration::from_secs(1)) {
                if let Err(error) = active.request_connection_info(client) {
                    tracing::debug!(%error, "connection info request failed");
                }
                info_requested = Some(std::time::Instant::now());
            }
        }

        // Stream commands need the registry and the viewer, which the
        // generic command handler does not own.
        let pending = match pending {
            Some(Command::WatchStream { client }) => {
                let actions = streams.watch(client, std::time::Instant::now());
                stream_actions(actions, session.as_mut(), &mut viewer, &events, &internal);
                None
            }
            Some(Command::StopWatching) => {
                let actions = streams.stop();
                stream_actions(actions, session.as_mut(), &mut viewer, &events, &internal);
                None
            }
            Some(Command::StreamWorker(report)) => {
                let actions = streams.on_worker(report);
                stream_actions(actions, session.as_mut(), &mut viewer, &events, &internal);
                None
            }
            other => other,
        };
        if let Some(command) = pending {
            // A new or ended session: the next snapshot is our own join.
            if matches!(command, Command::Connect { .. } | Command::Disconnect) {
                members = None;
            }
            apply_command(
                command,
                &mut session,
                &mut engine,
                &mut config,
                &mut media,
                &events,
                &internal,
            )
            .await;
            // A disconnect (or a new connection) ends any watched stream.
            if session.is_none() {
                streams.reset();
                viewer = None;
                own_channel = None;
                stream_media::frame::frames().close();
            }
        }
        // Signaling deadlines; the loop wakes at least every tick.
        let expired = streams.tick(std::time::Instant::now());
        if !expired.is_empty() {
            stream_actions(expired, session.as_mut(), &mut viewer, &events, &internal);
        }

        if engine.stats_due() {
            let tx_frames = session
                .as_ref()
                .map(|active| active.audio_frames_sent())
                .unwrap_or(0);
            let own = session.as_ref().and_then(|active| active.own_network());
            publish_audio(&mut engine, &events, tx_frames, own);
            engine.mark_published();
        }
    }
}

/// Carries out what the stream registry decided: server commands (fired
/// without waiting, so voice never waits on stream signaling), viewer
/// commands, and UI state.
fn stream_actions(
    actions: Vec<streams::Action>,
    mut session: Option<&mut Session>,
    viewer: &mut Option<stream_media::StreamWorker>,
    events: &broadcast::Sender<String>,
    internal: &mpsc::Sender<Command>,
) {
    use stream_media::WorkerCommand;
    for action in actions {
        match action {
            streams::Action::Send(name, args) => {
                if let Some(active) = session.as_deref_mut() {
                    if let Err(error) = active.send_command_no_wait(name, &args) {
                        tracing::debug!(%error, name, "stream command failed");
                    }
                }
            }
            streams::Action::Worker(command) => {
                if viewer.is_none() && matches!(command, WorkerCommand::Offer { .. }) {
                    *viewer = spawn_viewer(internal);
                }
                if let Some(worker) = viewer.as_ref() {
                    if worker.send(command).is_err() {
                        tracing::warn!("stream viewer queue full; its deadline ends the watch");
                    }
                }
            }
            streams::Action::Publish {
                client,
                state,
                reason,
            } => protocol::publish(
                events,
                &Event::StreamState {
                    client,
                    state,
                    reason,
                },
            ),
        }
    }
}

/// Starts the viewer worker; its reports come back as internal commands.
fn spawn_viewer(internal: &mpsc::Sender<Command>) -> Option<stream_media::StreamWorker> {
    let (reports, mut incoming) = mpsc::channel(64);
    let forward = internal.clone();
    tokio::spawn(async move {
        while let Some(report) = incoming.recv().await {
            if forward.send(Command::StreamWorker(report)).await.is_err() {
                break;
            }
        }
    });
    match stream_media::StreamWorker::spawn(reports) {
        Ok(worker) => Some(worker),
        Err(error) => {
            tracing::warn!(%error, "stream viewer could not start");
            None
        }
    }
}

async fn apply_command(
    command: Command,
    session: &mut Option<Session>,
    engine: &mut audio::AudioEngine,
    config: &mut audio::AudioConfig,
    media: &mut media::Media,
    events: &broadcast::Sender<String>,
    internal: &mpsc::Sender<Command>,
) {
    match command {
        Command::Connect {
            address,
            nickname,
            channel,
            server_password,
        } => {
            if let Some(old) = session.take() {
                let _ = old.disconnect().await;
            }
            engine.stop();
            engine.reset_counters();
            media.reset();
            publish_media(media, events);

            let nickname = nickname.unwrap_or_else(|| "ts6client".to_string());
            let mut client_config = ts6_core::ClientConfig::new(&address, &nickname);
            if let Some(channel) = channel.filter(|value| !value.trim().is_empty()) {
                client_config = client_config.channel(channel);
            }
            if let Some(password) = server_password.filter(|value| !value.is_empty()) {
                client_config = client_config.server_password(password);
            }

            log(events, "info", format!("connecting to {address}..."));
            match Session::connect(client_config).await {
                Ok(connected) => {
                    protocol::publish(
                        events,
                        &Event::Status {
                            session_id: Some(connected.session_id()),
                            connected: true,
                            address: Some(&address),
                            nickname: Some(&nickname),
                            error: None,
                        },
                    );
                    log(
                        events,
                        "ok",
                        format!("connected to {address} as {nickname}"),
                    );
                    let mut connected = connected;
                    refresh(&mut connected, events, media, internal);
                    *session = Some(connected);
                }
                Err(error) => {
                    protocol::publish(
                        events,
                        &Event::Status {
                            session_id: None,
                            connected: false,
                            address: None,
                            nickname: None,
                            error: Some(&format!("{error:#}")),
                        },
                    );
                    log(events, "error", format!("connection failed: {error:#}"));
                }
            }
        }

        Command::Disconnect => {
            if let Some(active) = session.take() {
                // Our own leave sound; playback stays up until it has played.
                engine.play_cue(ts6_audio::cues::Cue::Leave);
                let _ = active.disconnect().await;
                log(events, "info", "disconnected");
                tokio::time::sleep(std::time::Duration::from_millis(450)).await;
            }
            engine.stop();
            media.reset();
            publish_media(media, events);
            protocol::publish(
                events,
                &Event::Status {
                    session_id: None,
                    connected: false,
                    address: None,
                    nickname: None,
                    error: None,
                },
            );
        }

        Command::Join { channel } => match session.as_mut() {
            Some(active) => match active.join_channel_by_name(&channel).await {
                Ok(id) => {
                    log(events, "ok", format!("joined channel {channel} (#{id})"));
                    publish_state(active, events);
                }
                Err(error) => log(events, "error", format!("join failed: {error:#}")),
            },
            None => log(events, "warn", "not connected"),
        },

        Command::Chat {
            session_id,
            channel,
            text,
            request_id,
        } => {
            let text = ts6_core::attachments::expand_shortcodes(&text);
            let failure = match session.as_mut() {
                None => Some((
                    "not_connected",
                    "Not connected. Your draft has been preserved.",
                )),
                Some(active) => {
                    if session_id != active.session_id() {
                        Some((
                            "stale_session",
                            "The server session changed. Review your draft before sending again.",
                        ))
                    } else if let Err(failure) =
                        validate_channel_chat(&channel, &text, own_channel(active).as_deref())
                    {
                        Some(failure)
                    } else if active
                        .send_chat(ts6_core::ChatTarget::Channel, &text)
                        .await
                        .is_err()
                    {
                        // Never retried: the server may already have accepted it.
                        Some((
                            "not_confirmed",
                            "The server did not confirm the send. Check the chat before retrying.",
                        ))
                    } else {
                        None
                    }
                }
            };
            protocol::publish(
                events,
                &Event::ChatResult {
                    request_id,
                    error: failure.map(|(_, message)| message),
                    code: failure.map(|(code, _)| code),
                },
            );
        }

        Command::ChatAttach {
            session_id,
            channel,
            path,
            request_id,
        } => {
            let failure = match session.as_mut() {
                None => Some(("not_connected", "Not connected.")),
                Some(_) if !std::path::Path::new(&path).is_absolute() => {
                    Some(("invalid", "Attachments need an absolute file path."))
                }
                Some(active) if session_id != active.session_id() => Some((
                    "stale_session",
                    "The server session changed. Choose the file again.",
                )),
                Some(active) if own_channel(active).as_deref() != Some(channel.as_str()) => Some((
                    "stale_channel",
                    "The channel changed. Choose the file again.",
                )),
                Some(active) => match channel.parse::<u64>() {
                    Err(_) => Some(("invalid", "Unknown channel.")),
                    Ok(id) => match media.start_attachment(
                        active,
                        id,
                        std::path::Path::new(&path),
                        request_id.clone(),
                    ) {
                        // The result follows when the upload ends.
                        Ok(()) => return,
                        Err(error) => {
                            tracing::debug!(%error, "attachment rejected locally");
                            Some((
                                "invalid",
                                "The file could not be sent: it must exist, be readable and be at most 5 MB.",
                            ))
                        }
                    },
                },
            };
            publish_chat_result(events, request_id, failure);
        }

        Command::ChatFetch { channel, path } => {
            let file = channel
                .parse::<u64>()
                .ok()
                .and_then(|id| ts6_core::attachments::ChannelFile::from_path(id, &path));
            match (session.as_mut(), file) {
                (Some(active), Some(file)) => match media.fetch_chat_file(active, &file) {
                    Ok(Some(cached)) => {
                        publish_chat_file(events, file.channel_id, path, Some(cached))
                    }
                    Ok(None) => {} // `ChatFileDone` follows.
                    Err(error) => {
                        tracing::debug!(%error, "attachment fetch failed");
                        publish_chat_file(events, file.channel_id, path, None)
                    }
                },
                (_, file) => {
                    publish_chat_file(events, file.map_or(0, |f| f.channel_id), path, None)
                }
            }
        }

        Command::AttachmentSent {
            request_id,
            channel,
            message,
        } => {
            let failure = match (session.as_mut(), message) {
                (None, _) => Some(("not_connected", "Disconnected before the file was posted.")),
                (Some(_), None) => Some((
                    "upload_failed",
                    "The file upload did not complete. Try again.",
                )),
                (Some(active), Some(_))
                    if own_channel(active).as_deref() != Some(channel.to_string().as_str()) =>
                {
                    // The file is in the old channel; posting it elsewhere
                    // would point people at a channel they may not see.
                    Some((
                        "stale_channel",
                        "You changed channel while the file was uploading; it was not posted.",
                    ))
                }
                (Some(active), Some(message)) => {
                    match active
                        .send_chat(ts6_core::ChatTarget::Channel, &message)
                        .await
                    {
                        Ok(()) => None,
                        Err(_) => Some((
                            "not_confirmed",
                            "The server did not confirm the message. Check the chat before retrying.",
                        )),
                    }
                }
            };
            publish_chat_result(events, request_id, failure);
        }

        Command::ChatFileDone {
            channel,
            path,
            file,
        } => publish_chat_file(events, channel, path, file),

        Command::Nick { name } => match session.as_mut() {
            Some(active) => match active.set_nickname(&name).await {
                Ok(()) => {
                    log(events, "ok", format!("nickname changed to {name}"));
                    publish_state(active, events);
                }
                Err(error) => log(
                    events,
                    "error",
                    format!("nickname change failed: {error:#}"),
                ),
            },
            None => log(events, "warn", "not connected"),
        },

        Command::Mute { input, output } => match session.as_mut() {
            Some(active) => {
                let muted_input = input.unwrap_or(false);
                let muted_output = output.unwrap_or(false);
                match active.set_muted(muted_input, muted_output).await {
                    Ok(()) => log(
                        events,
                        "info",
                        format!("muted: input={muted_input} output={muted_output}"),
                    ),
                    Err(error) => log(events, "error", format!("mute change failed: {error:#}")),
                }
            }
            None => log(events, "warn", "not connected"),
        },

        Command::Audio {
            enabled,
            tone,
            ptt,
            input_device,
            output_device,
            voice_activation,
            vad_threshold,
            noise_suppression,
            monitor,
        } => {
            if let Some(monitor) = monitor {
                config.monitor = monitor;
            }
            if let Some(level) = noise_suppression {
                config.noise_suppression = ts6_audio::denoise::NoiseSuppression::from_level(level);
            }
            if let Some(voice_activation) = voice_activation {
                config.voice_activation = voice_activation;
            }
            if let Some(threshold) = vad_threshold.filter(|t| t.is_finite()) {
                config.threshold_db = threshold.clamp(-80.0, 0.0);
            }
            if let Some(enabled) = enabled {
                config.enabled = enabled;
            }
            if let Some(tone) = tone {
                config.tone = tone;
            }
            if let Some(ptt) = ptt {
                config.ptt = Some(ptt).filter(|key| !key.trim().is_empty());
            }
            if let Some(device) = input_device {
                config.input_device = Some(device).filter(|name| !name.is_empty());
            }
            if let Some(device) = output_device {
                config.output_device = Some(device).filter(|name| !name.is_empty());
            }
            engine.configure(config);
            log(
                events,
                "info",
                format!(
                    "audio: enabled={} tone={} ptt={}",
                    config.enabled,
                    engine.tone,
                    engine.ptt_binding.as_deref().unwrap_or("-")
                ),
            );
            let tx_frames = session
                .as_ref()
                .map(|active| active.audio_frames_sent())
                .unwrap_or(0);
            let own = session.as_ref().and_then(|active| active.own_network());
            publish_audio(engine, events, tx_frames, own);
        }

        Command::Devices => protocol::publish(
            events,
            &Event::Devices {
                devices: audio::device_lists(),
            },
        ),

        Command::Refresh => match session.as_mut() {
            Some(active) => refresh(active, events, media, internal),
            None => protocol::publish(
                events,
                &Event::Status {
                    session_id: None,
                    connected: false,
                    address: None,
                    nickname: None,
                    error: None,
                },
            ),
        },

        Command::SetAvatar { path } => match (session.as_mut(), path) {
            (Some(active), Some(path)) => {
                if let Err(error) = media.start_upload(active, std::path::Path::new(&path)) {
                    log(events, "warn", format!("avatar not sent: {error:#}"));
                }
            }
            (Some(active), None) => {
                if let Err(error) = active.set_avatar_flag("").await {
                    log(events, "warn", format!("avatar removal failed: {error:#}"));
                }
            }
            (None, _) => {}
        },

        Command::MediaDone { target, path } => {
            if media.on_done(target, path) {
                publish_media(media, events);
            }
        }

        // Nothing to do: waking the loop lets it publish fresh audio counters.
        Command::Tick => {}

        // Handled by the session loop before this function.
        Command::WatchStream { .. } | Command::StopWatching | Command::StreamWorker(_) => {}

        Command::Volume { client, volume } => engine.set_volume(client, volume),

        // Handled by the session loop before commands are applied.
        Command::ConnectionInfo { .. } => {}

        Command::AvatarUploaded { flag } => match (session.as_mut(), flag) {
            (Some(active), Some(flag)) => match active.set_avatar_flag(&flag).await {
                Ok(()) => log(events, "ok", "avatar published"),
                Err(error) => log(events, "warn", format!("avatar flag rejected: {error:#}")),
            },
            (Some(_), None) => log(events, "warn", "avatar upload did not complete"),
            (None, _) => {}
        },
    }
}

/// Runs the bridge on the calling thread until the server stops. Used by the
/// desktop client to host the bridge in-process, so one executable is enough.
/// The session is not `Send`, so this must own the thread it runs on.
pub fn serve_blocking(port: u16, ui_dir: PathBuf) -> anyhow::Result<()> {
    // Quiet by default: debug output can include server command metadata. Set
    // `RUST_LOG` (for example `ts6_bridge=debug`) to diagnose a problem.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .try_init();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    runtime.block_on(async move { serve(port, ui_dir).await })
}

/// Runs the WebSocket bridge without serving files from the process working directory.
/// Used by desktop clients that render their UI natively.
pub fn serve_blocking_without_ui(port: u16) -> anyhow::Result<()> {
    // Quiet by default: debug output can include server command metadata. Set
    // `RUST_LOG` (for example `ts6_bridge=debug`) to diagnose a problem.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .try_init();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    runtime.block_on(async move { serve_inner(port, None).await })
}

fn publish_chat_result(
    events: &broadcast::Sender<String>,
    request_id: String,
    failure: Option<(&'static str, &'static str)>,
) {
    protocol::publish(
        events,
        &Event::ChatResult {
            request_id,
            error: failure.map(|(_, message)| message),
            code: failure.map(|(code, _)| code),
        },
    );
}

/// Publishes a fetched attachment, typed by its content (never its name).
fn publish_chat_file(
    events: &broadcast::Sender<String>,
    channel: u64,
    path: String,
    file: Option<String>,
) {
    let kind = file.as_ref().and_then(|file| {
        use std::io::Read;
        let mut head = [0u8; 16];
        let read = std::fs::File::open(file)
            .and_then(|mut f| f.read(&mut head))
            .ok()?;
        Some(ts6_core::attachments::sniff(&head[..read]).0.as_str())
    });
    protocol::publish(
        events,
        &Event::ChatFile {
            channel: channel.to_string(),
            path,
            file,
            kind,
        },
    );
}

fn own_channel(session: &Session) -> Option<String> {
    let snapshot = session.snapshot().ok()?;
    snapshot
        .clients
        .iter()
        .find(|client| client.is_self)
        .map(|client| client.channel_id.to_string())
}

fn validate_channel_chat(
    channel: &str,
    text: &str,
    current: Option<&str>,
) -> Result<(), (&'static str, &'static str)> {
    if current != Some(channel) {
        return Err((
            "stale_channel",
            "The channel changed. Review your draft before sending again.",
        ));
    }
    if text.trim().is_empty() || text.len() > 1024 || text.contains('\0') {
        return Err((
            "invalid",
            "Messages must contain text, use at most 1024 UTF-8 bytes, and contain no NUL characters.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod access_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::HeaderMap;
    use tower::ServiceExt;

    #[test]
    fn channel_chat_rejects_stale_destinations_and_invalid_text() {
        assert!(validate_channel_chat("2", "hello", Some("1")).is_err());
        assert!(validate_channel_chat("1", "hello", None).is_err());
        assert!(validate_channel_chat("1", "  ", Some("1")).is_err());
        assert!(validate_channel_chat("1", "bad\0text", Some("1")).is_err());
        assert!(validate_channel_chat("1", &"a".repeat(1025), Some("1")).is_err());
        assert!(validate_channel_chat("1", &"\u{1f600}".repeat(257), Some("1")).is_err());
        assert!(validate_channel_chat("1", &"a".repeat(1024), Some("1")).is_ok());
    }

    #[tokio::test]
    async fn empty_ui_root_does_not_serve_files_from_the_working_directory() {
        let marker = format!(
            ".ts6-bridge-working-directory-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let marker_path = std::env::current_dir().unwrap().join(&marker);
        std::fs::write(&marker_path, "private sentinel").unwrap();
        let _cleanup = RemoveFile(marker_path);

        let (commands, _) = mpsc::channel(1);
        let (events, _) = broadcast::channel(1);
        let app = build_router(8787, commands, events, Some(PathBuf::new()));
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri(format!("/{marker}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    struct RemoveFile(PathBuf);

    impl Drop for RemoveFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn headers(host: &str, origin: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("host", host.parse().unwrap());
        if let Some(origin) = origin {
            headers.insert("origin", origin.parse().unwrap());
        }
        headers
    }

    #[test]
    fn websocket_access_rejects_cross_site_and_rebound_hosts() {
        for origin in [
            "https://evil.example",
            "null",
            "http://127.0.0.1:9999",
            "http://localhost:8787.evil.example",
        ] {
            assert!(!browser_allowed(
                &headers("127.0.0.1:8787", Some(origin)),
                8787
            ));
        }
        assert!(!browser_allowed(
            &headers("evil.example:8787", Some("http://evil.example:8787")),
            8787
        ));
        assert!(!browser_allowed(&HeaderMap::new(), 8787));
    }

    #[test]
    fn websocket_access_allows_local_ui_vite_and_native_probes() {
        for origin in [
            None,
            Some("http://127.0.0.1:8787"),
            Some("http://localhost:8787"),
            Some("http://localhost:5174"),
            Some("http://127.0.0.1:5174"),
        ] {
            assert!(browser_allowed(&headers("127.0.0.1:8787", origin), 8787));
        }
        assert!(browser_allowed(
            &headers("localhost:9000", Some("http://localhost:9000")),
            9000
        ));
    }
}

#[cfg(test)]
mod cue_tests {
    use super::membership_cue;
    use ts6_audio::cues::Cue;
    use ts6_core::{ClientInfo, ServerInfo, Snapshot};

    fn snapshot(clients: &[(u16, u64, bool)]) -> Snapshot {
        Snapshot {
            own_client_id: 1,
            own_uid: None,
            server: ServerInfo {
                name: String::new(),
                version: String::new(),
                platform: String::new(),
                max_clients: 0,
                welcome_message: String::new(),
                icon_id: 0,
            },
            channels: Vec::new(),
            clients: clients
                .iter()
                .map(|&(id, channel_id, is_self)| ClientInfo {
                    id,
                    channel_id,
                    name: String::new(),
                    uid: None,
                    input_muted: false,
                    output_muted: false,
                    is_self,
                    avatar_hash: None,
                    myteamspeak_avatar: None,
                    is_streaming: false,
                })
                .collect(),
        }
    }

    #[test]
    fn joins_and_leaves_in_our_channel_sound() {
        let mut members = None;
        // We entered the call ourselves.
        assert_eq!(
            membership_cue(&mut members, &snapshot(&[(1, 5, true)])),
            Some(Cue::Join)
        );
        // Someone in another channel does not count.
        assert_eq!(
            membership_cue(&mut members, &snapshot(&[(1, 5, true), (2, 6, false)])),
            None
        );
        assert_eq!(
            membership_cue(&mut members, &snapshot(&[(1, 5, true), (2, 5, false)])),
            Some(Cue::Join)
        );
        assert_eq!(
            membership_cue(&mut members, &snapshot(&[(1, 5, true)])),
            Some(Cue::Leave)
        );
        // We move channel ourselves.
        assert_eq!(
            membership_cue(&mut members, &snapshot(&[(1, 7, true)])),
            Some(Cue::Join)
        );
    }
}
