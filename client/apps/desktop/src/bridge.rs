//! Background link to the local `ts6-bridge` WebSocket.
//!
//! A dedicated thread owns the socket. It polls for commands from the UI and
//! reads events from the bridge, so the egui thread never blocks on I/O.
//! The bridge is local and unauthenticated, so this client only ever connects to
//! `127.0.0.1`.

use std::io::ErrorKind;
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

use tungstenite::{Message, WebSocket};

use crate::protocol::{BridgeEvent, Command};

const BRIDGE_PORT: u16 = 8787;
const BRIDGE_URL: &str = "ws://127.0.0.1:8787/ws";
const RETRY_DELAY: Duration = Duration::from_millis(1200);
/// Upper bound on how long a command can wait before it is written.
const POLL_INTERVAL: Duration = Duration::from_millis(40);

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
    pub outgoing: Sender<Command>,
}

impl Bridge {
    /// Starts the link thread. `ctx` is used only to wake the UI when data arrives.
    pub fn spawn(ctx: egui::Context) -> Self {
        start_embedded_bridge();
        let (event_tx, event_rx) = mpsc::channel();
        let (command_tx, command_rx) = mpsc::channel();
        thread::Builder::new()
            .name("ts6-bridge-link".into())
            .spawn(move || link_loop(event_tx, command_rx, ctx))
            .expect("failed to start the bridge link thread");
        Self {
            incoming: event_rx,
            outgoing: command_tx,
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
            if let Err(error) = ts6_bridge::serve_blocking(BRIDGE_PORT, PathBuf::new()) {
                eprintln!("embedded ts6-bridge stopped: {error:#}");
            }
        });
    if let Err(error) = started {
        eprintln!("could not start the embedded ts6-bridge: {error}");
    }
}

fn link_loop(events: Sender<Incoming>, commands: Receiver<Command>, ctx: egui::Context) {
    loop {
        if !notify(&events, &ctx, SocketState::Connecting) {
            return;
        }

        if let Ok(mut socket) = connect() {
            if !notify(&events, &ctx, SocketState::Open) {
                return;
            }
            // `run_session` returns false only when the UI has gone away.
            if !run_session(&mut socket, &events, &commands, &ctx) {
                return;
            }
        }

        if !notify(&events, &ctx, SocketState::Closed) {
            return;
        }
        thread::sleep(RETRY_DELAY);
    }
}

fn notify(events: &Sender<Incoming>, ctx: &egui::Context, state: SocketState) -> bool {
    let alive = events.send(Incoming::Socket(state)).is_ok();
    ctx.request_repaint();
    alive
}

fn connect() -> Result<WebSocket<TcpStream>, String> {
    let address = SocketAddr::from(([127, 0, 0, 1], BRIDGE_PORT));
    let stream = TcpStream::connect(address).map_err(|error| error.to_string())?;
    stream.set_nodelay(true).ok();
    let (socket, _response) =
        tungstenite::client(BRIDGE_URL, stream).map_err(|error| error.to_string())?;
    // Set the timeout only after the handshake, which needs blocking reads.
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
    events: &Sender<Incoming>,
    commands: &Receiver<Command>,
    ctx: &egui::Context,
) -> bool {
    loop {
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
                    if events.send(Incoming::Event(event)).is_err() {
                        return false;
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
