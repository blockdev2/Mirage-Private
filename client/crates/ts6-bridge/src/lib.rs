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
mod share;
pub mod share_media;
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

/// Installs the log subscriber. Output goes to stderr, or appends to the file
/// named by `MIRAGE_LOG_FILE` (the desktop exe has no console). Another
/// subscriber may already be installed when hosted inside the desktop.
fn init_logging(default_filter: &str) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_filter));
    let file = std::env::var_os("MIRAGE_LOG_FILE").and_then(|path| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
    });
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    let _ = match file {
        Some(file) => builder
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(file))
            .try_init(),
        None => builder.try_init(),
    };
}

pub fn main_entry() -> anyhow::Result<()> {
    init_logging("info,ts6_bridge=debug");

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
    // Our own screen share: signaling state and the capture/peer worker.
    let mut share = share::ShareRegistry::default();
    let mut sharer: Option<share_media::ShareWorker> = None;
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
                                    // Our stream belongs to the channel we left.
                                    let actions = share.stop(std::time::Instant::now());
                                    share_actions(
                                        actions,
                                        Some(&mut *active),
                                        &mut sharer,
                                        &events,
                                        &internal,
                                    );
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
                            // Channel messages need their channel; server-wide and
                            // private ones are routed by `scope` and `peer_id`.
                            let routed = match message.target {
                                ts6_core::MessageTarget::Channel => message
                                    .channel_id
                                    .map(|id| ("channel", id.to_string(), None)),
                                ts6_core::MessageTarget::Server => {
                                    Some(("server", String::new(), None))
                                }
                                ts6_core::MessageTarget::Client(target) => {
                                    let peer = if message.from_self {
                                        Some(target.0)
                                    } else {
                                        message.invoker_id
                                    };
                                    Some(("private", String::new(), peer))
                                }
                                ts6_core::MessageTarget::Poke(_) => None,
                            };
                            if routed.is_none() {
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
                            if let Some((scope, channel, peer_id)) = routed {
                                let attachments = protocol::Attachment::from_text(&message.message);
                                protocol::publish(
                                    &events,
                                    &Event::Chat {
                                        channel,
                                        scope,
                                        from_id: message.invoker_id,
                                        peer_id,
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
                                let actions = share.on_event(&event, std::time::Instant::now());
                                share_actions(
                                    actions,
                                    Some(&mut *active),
                                    &mut sharer,
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
            // Say why when the library knows (kick, ban, timeout, shutdown).
            let reason = session
                .as_ref()
                .and_then(|active| active.end_reason())
                .map(|why| format!("connection closed: {why}"))
                .unwrap_or_else(|| "connection closed".to_string());
            protocol::publish(
                &events,
                &Event::Status {
                    session_id: None,
                    connected: false,
                    address: None,
                    nickname: None,
                    error: Some(&reason),
                },
            );
            log(&events, "warn", &reason);
            session = None;
            members = None;
            own_channel = None;
            streams.reset();
            viewer = None;
            share.reset();
            sharer = None;
            publish_share(&events, "stopped", None, 0);
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
                let actions = streams.hide();
                stream_actions(actions, session.as_mut(), &mut viewer, &events, &internal);
                None
            }
            Some(Command::StreamWorker(report)) => {
                let actions = streams.on_worker(report);
                stream_actions(actions, session.as_mut(), &mut viewer, &events, &internal);
                None
            }
            Some(Command::ShareStart {
                monitor,
                window,
                quality,
                audio,
            }) => {
                let own = session.as_ref().and_then(own_client_id);
                let actions = match own {
                    Some(own) => {
                        // A window wins over a monitor when both are named.
                        let source = match window {
                            Some(handle) => share_media::CaptureSource::Window(handle),
                            None => share_media::CaptureSource::Monitor(monitor),
                        };
                        // Unknown names fall back to the default preset.
                        let quality = quality
                            .as_deref()
                            .and_then(share_media::Quality::from_name)
                            .unwrap_or_default();
                        // Stream sound is best effort: where the system cannot
                        // capture it the share goes ahead without sound.
                        let audio = audio
                            && match engine.set_stream_audio(true) {
                                Ok(()) => true,
                                Err(reason) => {
                                    log(
                                        &events,
                                        "warn",
                                        format!("stream sound unavailable: {reason}"),
                                    );
                                    false
                                }
                            };
                        share.start(own, source, quality, audio, std::time::Instant::now())
                    }
                    None => vec![share::Action::Publish {
                        state: "failed",
                        reason: Some("offline"),
                        viewers: 0,
                    }],
                };
                share_actions(actions, session.as_mut(), &mut sharer, &events, &internal);
                None
            }
            Some(Command::ShareQuality { quality }) => {
                if let (Some(worker), Some(quality)) =
                    (sharer.as_ref(), share_media::Quality::from_name(&quality))
                {
                    if worker
                        .send(share_media::ShareCommand::SetQuality(quality))
                        .is_err()
                    {
                        tracing::warn!("screen share queue full; quality unchanged");
                    }
                }
                None
            }
            Some(Command::ShareStop) => {
                let actions = share.stop(std::time::Instant::now());
                share_actions(actions, session.as_mut(), &mut sharer, &events, &internal);
                None
            }
            Some(Command::ShareWorker(report)) => {
                tracing::info!(target: SHARE_LOG, ?report, "screen share worker");
                let actions = share.on_worker(report, std::time::Instant::now());
                share_actions(actions, session.as_mut(), &mut sharer, &events, &internal);
                None
            }
            other => other,
        };
        // The computer's sound follows the share: off as soon as none is running.
        if share.is_idle() && engine.stream_audio_on() {
            let _ = engine.set_stream_audio(false);
        }
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
                share.reset();
                sharer = None;
                own_channel = None;
                stream_media::frame::frames().close();
            }
        }
        // Signaling deadlines; the loop wakes at least every tick.
        let expired = streams.tick(std::time::Instant::now());
        if !expired.is_empty() {
            stream_actions(expired, session.as_mut(), &mut viewer, &events, &internal);
        }
        let expired = share.tick(std::time::Instant::now());
        if !expired.is_empty() {
            share_actions(expired, session.as_mut(), &mut sharer, &events, &internal);
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

/// Our own client id, which identifies our `notifystreamstarted`.
fn own_client_id(session: &Session) -> Option<u16> {
    session
        .snapshot()
        .ok()?
        .clients
        .iter()
        .find(|client| client.is_self)
        .map(|client| client.id)
}

fn publish_share(
    events: &broadcast::Sender<String>,
    state: &str,
    reason: Option<&str>,
    viewers: usize,
) {
    protocol::publish(
        events,
        &Event::ShareState {
            state,
            reason,
            viewers,
        },
    );
}

/// Share stages logged from this file go under the share target, which the
/// desktop's default log filter keeps (it drops the rest of `ts6_bridge` info).
const SHARE_LOG: &str = "ts6_bridge::share_media";

/// Carries out what the share registry decided; like `stream_actions`, server
/// commands never wait, so voice never waits on stream signaling.
fn share_actions(
    actions: Vec<share::Action>,
    mut session: Option<&mut Session>,
    sharer: &mut Option<share_media::ShareWorker>,
    events: &broadcast::Sender<String>,
    internal: &mpsc::Sender<Command>,
) {
    for action in actions {
        match action {
            share::Action::Send(name, args) => {
                if let Some(active) = session.as_deref_mut() {
                    if let Err(error) = active.send_command_no_wait(name, &args) {
                        tracing::debug!(%error, name, "share command failed");
                    }
                }
            }
            share::Action::Worker(command) => {
                // Stages only: no addresses, SDP or candidates are logged.
                match &command {
                    share_media::ShareCommand::AddViewer { viewer } => {
                        tracing::info!(target: SHARE_LOG, viewer, "screen share: a viewer asked to watch")
                    }
                    share_media::ShareCommand::Signal { viewer, json } => tracing::info!(
                        target: SHARE_LOG,
                        viewer,
                        answer = json.contains("\"answer\""),
                        "screen share: signal from the viewer"
                    ),
                    share_media::ShareCommand::RemoveViewer { viewer } => {
                        tracing::info!(target: SHARE_LOG, viewer, "screen share: a viewer left")
                    }
                    share_media::ShareCommand::Start { .. }
                    | share_media::ShareCommand::SetQuality(_) => {}
                }
                if sharer.is_none() && matches!(command, share_media::ShareCommand::Start { .. }) {
                    *sharer = spawn_sharer(internal);
                }
                match sharer.as_ref() {
                    Some(worker) => {
                        if worker.send(command).is_err() {
                            tracing::warn!("screen share queue full; its deadline ends the share");
                        }
                    }
                    // The worker could not start: report it as a capture failure.
                    None => publish_share(events, "failed", Some("capture"), 0),
                }
            }
            // Dropping the worker closes its peers and stops the capture.
            share::Action::CloseWorker => *sharer = None,
            share::Action::Publish {
                state,
                reason,
                viewers,
            } => {
                tracing::info!(target: SHARE_LOG, state, ?reason, viewers, "screen share state");
                publish_share(events, state, reason, viewers)
            }
        }
    }
}

/// Starts the share worker; its reports come back as internal commands.
fn spawn_sharer(internal: &mpsc::Sender<Command>) -> Option<share_media::ShareWorker> {
    let (reports, mut incoming) = mpsc::channel(64);
    let forward = internal.clone();
    tokio::spawn(async move {
        while let Some(report) = incoming.recv().await {
            if forward.send(Command::ShareWorker(report)).await.is_err() {
                break;
            }
        }
    });
    match share_media::ShareWorker::spawn(reports) {
        Ok(worker) => Some(worker),
        Err(error) => {
            tracing::warn!(%error, "screen share could not start");
            None
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
            let nickname = match ts6_core::check_nickname(&nickname) {
                Ok(name) => name.to_owned(),
                Err(error) => {
                    protocol::publish(
                        events,
                        &Event::Status {
                            session_id: None,
                            connected: false,
                            address: None,
                            nickname: None,
                            error: Some(&error.to_string()),
                        },
                    );
                    log(
                        events,
                        "error",
                        format!("connection refused locally: {error}"),
                    );
                    return;
                }
            };
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

        Command::Admin {
            session_id,
            request_id,
            what,
        } => {
            let outcome: Result<Vec<ts6_core::admin::Row>, (&str, String)> = match session.as_mut()
            {
                None => Err(("not_connected", "Not connected.".into())),
                Some(_) if request_id.is_empty() || request_id.len() > 64 => Err((
                    "invalid",
                    "A request id of 1 to 64 characters is needed.".into(),
                )),
                Some(active) if !session_id.is_empty() && session_id != active.session_id() => {
                    Err(("stale_session", "The server session changed.".into()))
                }
                Some(active) => match what.request() {
                    Err(reason) => Err(("invalid", reason)),
                    Ok(_) => active
                        .admin(&what)
                        .await
                        .map_err(|error| ("refused", format!("{error:#}"))),
                },
            };
            let (ok, error, code, rows) = match outcome {
                Ok(rows) => (true, None, None, rows),
                Err((code, error)) => (false, Some(error), Some(code), Vec::new()),
            };
            protocol::publish(
                events,
                &Event::AdminResult {
                    request_id,
                    ok,
                    error,
                    code,
                    rows,
                },
            );
        }

        Command::Join { channel, password } => match session.as_mut() {
            Some(active) => match active
                .join_channel_by_name_with_password(&channel, password.as_deref())
                .await
            {
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
            scope,
            client,
        } => {
            let text = ts6_core::attachments::expand_shortcodes(&text);
            let failure = match session.as_mut() {
                None => Some((
                    "not_connected",
                    "Not connected. Your draft has been preserved.",
                )),
                Some(active) => {
                    let destination = chat_destination(active, scope, client);
                    if session_id != active.session_id() {
                        Some((
                            "stale_session",
                            "The server session changed. Review your draft before sending again.",
                        ))
                    } else if let Err(failure) = validate_chat(
                        scope,
                        &channel,
                        &text,
                        own_channel(active).as_deref(),
                        destination.is_some(),
                    ) {
                        Some(failure)
                    } else if active
                        .send_chat(destination.unwrap_or(ts6_core::ChatTarget::Channel), &text)
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
            Some(_) if ts6_core::check_nickname(&name).is_err() => log(
                events,
                "error",
                format!(
                    "nickname must have {} to {} characters",
                    ts6_core::NICKNAME_MIN_CHARS,
                    ts6_core::NICKNAME_MAX_CHARS
                ),
            ),
            Some(active) => match active.set_nickname(name.trim()).await {
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

        Command::Away { away, message } => match session.as_mut() {
            Some(_) if message.as_deref().is_some_and(|m| m.len() > 80) => {
                log(events, "error", "the away message is limited to 80 bytes")
            }
            Some(active) => match active
                .set_away(away.then(|| message.as_deref().unwrap_or("")))
                .await
            {
                Ok(()) => {
                    log(events, "info", format!("away: {away}"));
                    publish_state(active, events);
                }
                Err(error) => log(events, "error", format!("away change failed: {error:#}")),
            },
            None => log(events, "warn", "not connected"),
        },

        Command::Whisper { clients, channels } => match session.as_mut() {
            Some(active) => {
                let target = (!clients.is_empty() || !channels.is_empty())
                    .then_some(ts6_core::WhisperTarget { clients, channels });
                match active.set_whisper(target) {
                    Ok(()) => log(
                        events,
                        "info",
                        match active.whisper() {
                            Some(_) => "whispering",
                            None => "whisper off",
                        },
                    ),
                    Err(error) => log(events, "error", error),
                }
            }
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
        Command::WatchStream { .. }
        | Command::StopWatching
        | Command::StreamWorker(_)
        | Command::ShareStart { .. }
        | Command::ShareStop
        | Command::ShareQuality { .. }
        | Command::ShareWorker(_) => {}

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
    init_logging("warn");
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
    // `RUST_LOG` (for example `ts6_bridge=debug`) to diagnose a problem. The
    // desktop exe has no console, so stream connection and decoder lines (no
    // chat, no credentials) go to `mirage.log` in the config folder.
    if std::env::var_os("MIRAGE_LOG_FILE").is_none() && std::env::var_os("RUST_LOG").is_none() {
        let path = ts6_core::config::default_config_dir().join("mirage.log");
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > 2_000_000) {
            let _ = std::fs::remove_file(&path);
        }
        let _ = std::fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new(".")));
        std::env::set_var("MIRAGE_LOG_FILE", path);
    }
    if std::env::var_os("RUST_LOG").is_none() {
        std::env::set_var(
            "RUST_LOG",
            "warn,ts6_bridge::stream_media=info,ts6_bridge::share_media=info,ts6_stream=info",
        );
    }
    init_logging("warn");
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

/// Where a chat message goes, or `None` when its private recipient is not on
/// the server (or is us).
fn chat_destination(
    session: &Session,
    scope: protocol::ChatScope,
    client: Option<u16>,
) -> Option<ts6_core::ChatTarget> {
    match scope {
        protocol::ChatScope::Channel => Some(ts6_core::ChatTarget::Channel),
        protocol::ChatScope::Server => Some(ts6_core::ChatTarget::Server),
        protocol::ChatScope::Private => {
            let snapshot = session.snapshot().ok()?;
            let id = client?;
            snapshot
                .clients
                .iter()
                .any(|c| c.id == id && !c.is_self)
                .then_some(ts6_core::ChatTarget::Client(id))
        }
    }
}

fn validate_chat(
    scope: protocol::ChatScope,
    channel: &str,
    text: &str,
    current: Option<&str>,
    destination_present: bool,
) -> Result<(), (&'static str, &'static str)> {
    match scope {
        protocol::ChatScope::Channel => validate_channel_chat(channel, text, current),
        protocol::ChatScope::Server => validate_chat_text(text),
        protocol::ChatScope::Private if !destination_present => {
            Err(("stale_client", "That person is no longer on the server."))
        }
        protocol::ChatScope::Private => validate_chat_text(text),
    }
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
    validate_chat_text(text)
}

fn validate_chat_text(text: &str) -> Result<(), (&'static str, &'static str)> {
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

    #[test]
    fn server_and_private_chat_skip_the_channel_check_but_not_the_text_rules() {
        use protocol::ChatScope::{Private, Server};
        // The channel field is irrelevant outside the channel scope.
        assert!(validate_chat(Server, "", "hi", None, false).is_ok());
        assert!(validate_chat(Server, "", "  ", None, false).is_err());
        assert!(validate_chat(Private, "", "hi", None, true).is_ok());
        assert!(validate_chat(Private, "", "bad\0", None, true).is_err());
        // A recipient who left is a distinct, retry-safe refusal.
        assert_eq!(
            validate_chat(Private, "", "hi", None, false).unwrap_err().0,
            "stale_client"
        );
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
                    away: false,
                    away_message: None,
                    channel_group: 0,
                    db_id: 0,
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
