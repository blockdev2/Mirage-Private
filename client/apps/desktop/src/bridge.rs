//! Background link to the local `ts6-bridge` WebSocket.
//!
//! A dedicated thread owns the socket. It polls for commands from the UI and
//! reads events from the bridge, so the egui thread never blocks on I/O.
//! The bridge is local and unauthenticated, so this client only ever connects to
//! `127.0.0.1`.

use std::io::ErrorKind;
use std::net::{SocketAddr, TcpStream};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use tungstenite::{Message, WebSocket};

use crate::protocol::{AudioEvent, BridgeEvent, Command};

const BRIDGE_PORT: u16 = 8787;
const BRIDGE_URL: &str = "ws://127.0.0.1:8787/ws";
const RETRY_DELAY: Duration = Duration::from_millis(1200);
/// Upper bound on how long a command can wait before it is written.
const POLL_INTERVAL: Duration = Duration::from_millis(40);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// Bridge events held for the UI. When a stalled UI lets this fill, new events
/// are dropped (the socket keeps being read) and a full refresh is requested
/// once there is room again, so the UI resynchronizes instead of growing memory.
const EVENT_CAPACITY: usize = 4096;
/// UI commands waiting to be written. A full queue rejects the command at once
/// instead of blocking the UI thread; nothing is retried automatically.
const COMMAND_CAPACITY: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketState {
    Connecting,
    Open,
    Closed,
}

pub enum Incoming {
    Socket(SocketState),
    Event(BridgeEvent),
}

pub struct Bridge {
    pub incoming: Receiver<Incoming>,
    pub outgoing: SyncSender<Command>,
    /// Latest audio meter. These arrive about ten times a second, so they
    /// replace each other here instead of filling the event queue while the
    /// window is minimized and not drained.
    pub audio: Arc<Mutex<Option<AudioEvent>>>,
}

impl Bridge {
    /// Starts the link thread. `ctx` is used only to wake the UI when data arrives.
    pub fn spawn(ctx: egui::Context) -> Self {
        start_embedded_bridge();
        let (event_tx, event_rx) = mpsc::sync_channel(EVENT_CAPACITY);
        let (command_tx, command_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        let audio = Arc::new(Mutex::new(None));
        let link_audio = Arc::clone(&audio);
        thread::Builder::new()
            .name("ts6-bridge-link".into())
            .spawn(move || link_loop(event_tx, command_rx, link_audio, ctx))
            .expect("failed to start the bridge link thread");
        Self {
            incoming: event_rx,
            outgoing: command_tx,
            audio,
        }
    }
}

/// Hosts the bridge inside this process unless one is already listening, so the
/// client runs from a single executable. An external `ts6-bridge` still works:
/// the link simply connects to it instead.
fn start_embedded_bridge() {
    let address = SocketAddr::from(([127, 0, 0, 1], BRIDGE_PORT));
    if TcpStream::connect_timeout(&address, Duration::from_millis(150)).is_ok() {
        return;
    }
    let started = thread::Builder::new()
        .name("ts6-bridge-host".into())
        .spawn(|| {
            if let Err(error) = ts6_bridge::serve_blocking_without_ui(BRIDGE_PORT) {
                eprintln!("embedded ts6-bridge stopped: {error:#}");
            }
        });
    if let Err(error) = started {
        eprintln!("could not start the embedded ts6-bridge: {error}");
    }
}

fn link_loop(
    events: SyncSender<Incoming>,
    commands: Receiver<Command>,
    audio: Arc<Mutex<Option<AudioEvent>>>,
    ctx: egui::Context,
) {
    loop {
        if !notify(&events, &ctx, SocketState::Connecting) {
            return;
        }

        if let Ok(mut socket) = connect() {
            if !notify(&events, &ctx, SocketState::Open) {
                return;
            }
            // `run_session` returns false only when the UI has gone away.
            if !run_session(&mut socket, &events, &commands, &audio, &ctx) {
                return;
            }
        }

        if !notify(&events, &ctx, SocketState::Closed) {
            return;
        }
        thread::sleep(RETRY_DELAY);
    }
}

/// Socket transitions are rare and must not be lost, so this one send may wait
/// for the UI; it happens outside a session, when there is no socket to read.
fn notify(events: &SyncSender<Incoming>, ctx: &egui::Context, state: SocketState) -> bool {
    let alive = events.send(Incoming::Socket(state)).is_ok();
    ctx.request_repaint();
    alive
}

fn connect() -> Result<WebSocket<TcpStream>, String> {
    let address = SocketAddr::from(([127, 0, 0, 1], BRIDGE_PORT));
    connect_to(address, BRIDGE_URL)
}

fn connect_to(address: SocketAddr, url: &str) -> Result<WebSocket<TcpStream>, String> {
    let stream =
        TcpStream::connect_timeout(&address, CONNECT_TIMEOUT).map_err(|error| error.to_string())?;
    // Bound stalled handshake reads and writes before exchanging any HTTP data.
    stream
        .set_read_timeout(Some(CONNECT_TIMEOUT))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(CONNECT_TIMEOUT))
        .map_err(|error| error.to_string())?;
    stream.set_nodelay(true).ok();
    let (socket, _response) =
        tungstenite::client(url, stream).map_err(|error| error.to_string())?;
    // Use a shorter read timeout after the handshake to poll UI commands.
    socket
        .get_ref()
        .set_read_timeout(Some(POLL_INTERVAL))
        .map_err(|error| error.to_string())?;
    Ok(socket)
}

/// Returns `true` when the connection ended and the link should retry, and
/// `false` when the UI has dropped its channels and the thread should exit.
fn run_session(
    socket: &mut WebSocket<TcpStream>,
    events: &SyncSender<Incoming>,
    commands: &Receiver<Command>,
    audio: &Mutex<Option<AudioEvent>>,
    ctx: &egui::Context,
) -> bool {
    let mut resync = false;
    loop {
        if resync && forward(events, Incoming::Socket(SocketState::Open)) == Forward::Sent {
            // Room again: ask for a full status and snapshot to replace what
            // was dropped.
            let Ok(json) = serde_json::to_string(&Command::Refresh) else {
                return true;
            };
            if socket.send(Message::Text(json.into())).is_err() {
                return true;
            }
            resync = false;
        }
        loop {
            match commands.try_recv() {
                Ok(command) => {
                    let Ok(json) = serde_json::to_string(&command) else {
                        continue;
                    };
                    if socket.send(Message::Text(json.into())).is_err() {
                        return true;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return false,
            }
        }

        match socket.read() {
            Ok(Message::Text(text)) => {
                if let Ok(event) = serde_json::from_str::<BridgeEvent>(text.as_str()) {
                    match route(events, audio, event) {
                        Forward::Sent => {}
                        Forward::Dropped => resync = true,
                        Forward::Gone => return false,
                    }
                    ctx.request_repaint();
                }
            }
            Ok(Message::Close(_)) => return true,
            Ok(_) => {}
            Err(tungstenite::Error::Io(error))
                if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => return true,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Forward {
    Sent,
    /// The UI queue was full; the event was discarded.
    Dropped,
    /// The UI has gone away.
    Gone,
}

/// Audio meters overwrite the latest slot; everything else is queued.
fn route(
    events: &SyncSender<Incoming>,
    audio: &Mutex<Option<AudioEvent>>,
    event: BridgeEvent,
) -> Forward {
    match event {
        BridgeEvent::Audio(meter) => {
            if let Ok(mut slot) = audio.lock() {
                *slot = Some(meter);
            }
            Forward::Sent
        }
        other => forward(events, Incoming::Event(other)),
    }
}

/// Never blocks: a full queue drops the event so the socket keeps being read.
fn forward(events: &SyncSender<Incoming>, item: Incoming) -> Forward {
    match events.try_send(item) {
        Ok(()) => Forward::Sent,
        Err(TrySendError::Full(_)) => Forward::Dropped,
        Err(TrySendError::Disconnected(_)) => Forward::Gone,
    }
}

#[cfg(test)]
mod queue_tests {
    use super::*;

    #[test]
    fn a_full_ui_queue_drops_events_without_blocking() {
        let (tx, rx) = mpsc::sync_channel(2);
        assert_eq!(
            forward(&tx, Incoming::Socket(SocketState::Open)),
            Forward::Sent
        );
        assert_eq!(
            forward(&tx, Incoming::Socket(SocketState::Open)),
            Forward::Sent
        );
        let started = std::time::Instant::now();
        assert_eq!(
            forward(&tx, Incoming::Socket(SocketState::Open)),
            Forward::Dropped
        );
        assert!(started.elapsed() < Duration::from_millis(100));
        assert_eq!(rx.try_iter().count(), 2);
        assert_eq!(
            forward(&tx, Incoming::Socket(SocketState::Open)),
            Forward::Sent
        );
        drop(rx);
        assert_eq!(
            forward(&tx, Incoming::Socket(SocketState::Open)),
            Forward::Gone
        );
    }

    #[test]
    fn audio_meters_never_fill_the_event_queue() {
        let (tx, rx) = mpsc::sync_channel(1);
        let slot = Mutex::new(None);
        for packets in 0..10_000 {
            let meter = AudioEvent {
                rx_packets: packets,
                ..AudioEvent::default()
            };
            assert_eq!(route(&tx, &slot, BridgeEvent::Audio(meter)), Forward::Sent);
        }
        assert_eq!(slot.lock().unwrap().as_ref().unwrap().rx_packets, 9_999);
        // The queue is still free for an event that must not be lost.
        let chat = BridgeEvent::ChatResult {
            request_id: "r1".into(),
            error: None,
            code: None,
        };
        assert_eq!(route(&tx, &slot, chat), Forward::Sent);
        assert_eq!(rx.try_iter().count(), 1);
    }

    #[test]
    fn a_full_command_queue_rejects_instead_of_blocking_the_ui() {
        let (tx, _rx) = mpsc::sync_channel::<Command>(1);
        assert!(tx.try_send(Command::Refresh).is_ok());
        assert!(matches!(
            tx.try_send(Command::Refresh),
            Err(TrySendError::Full(_))
        ));
    }
}

#[cfg(test)]
mod connection_tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn responsive_peer_connects_and_restores_command_polling_timeout() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let (release, wait) = mpsc::channel::<()>();
        let peer = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let _socket = tungstenite::accept(stream).unwrap();
            let _ = wait.recv_timeout(Duration::from_secs(4));
        });
        let socket = connect_to(address, &format!("ws://{address}/ws")).unwrap();
        let _ = release.send(());
        peer.join().unwrap();
        assert_eq!(
            socket.get_ref().read_timeout().unwrap(),
            Some(POLL_INTERVAL)
        );
        assert_eq!(
            socket.get_ref().write_timeout().unwrap(),
            Some(CONNECT_TIMEOUT)
        );
    }

    #[test]
    fn unresponsive_peer_does_not_hold_the_handshake_open() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let (release, wait) = mpsc::channel::<()>();
        let peer = thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            let _ = wait.recv_timeout(Duration::from_secs(4));
        });
        let started = std::time::Instant::now();
        let result = connect_to(address, &format!("ws://{address}/ws"));
        let _ = release.send(());
        peer.join().unwrap();
        assert!(
            result.is_err(),
            "an unresponsive peer must not complete the handshake"
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "handshake did not time out before the peer closed"
        );
    }
}
