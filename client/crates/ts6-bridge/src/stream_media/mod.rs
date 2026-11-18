//! Watching TS6 screen streams: offer checks, WebRTC peer, H.264 decoding
//! and the newest-frame slot the desktop view reads.

pub mod decode;
pub mod frame;
pub mod offer;
/// Volume and mute of the watched stream's sound, for the window.
pub use ts6_audio::stream_audio::{stream_audio, MAX_STREAM_VOLUME};
mod peer;

#[cfg(all(test, windows))]
mod loopback_tests;

/// Requests from the session loop. Payloads are SDP / ICE JSON as relayed
/// by the TS6 server; they are never logged.
pub enum WorkerCommand {
    Offer {
        stream: String,
        sdp: String,
    },
    RemoteIce {
        stream: String,
        json: String,
    },
    /// Hidden: the connection stays, but nothing is decoded or played.
    Visible(bool),
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailReason {
    Unsupported,
    Network,
    NoDecoder,
    Timeout,
}

/// Reports to the session loop.
pub enum WorkerEvent {
    /// Our SDP answer, to send as `{"cmd":"answer","args":{"answer":…}}`.
    Answer {
        stream: String,
        sdp: String,
    },
    /// A local candidate as `{"cmd":"iceCandidate","args":{"sdp","mid","mLine"}}`.
    LocalIce {
        stream: String,
        json: String,
    },
    /// The first picture was decoded.
    Playing {
        stream: String,
    },
    Failed {
        stream: String,
        reason: FailReason,
    },
}

impl std::fmt::Debug for WorkerEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Answer { stream, .. } => write!(f, "Answer({stream})"),
            Self::LocalIce { stream, .. } => write!(f, "LocalIce({stream})"),
            Self::Playing { stream } => write!(f, "Playing({stream})"),
            Self::Failed { stream, reason } => write!(f, "Failed({stream}, {reason:?})"),
        }
    }
}

/// Commands the worker queues. The official client trickles its whole
/// candidate burst (30 observed) while the worker is still building the peer.
const COMMAND_QUEUE: usize = 1 + peer::MAX_REMOTE_CANDIDATES + 1;

/// One WebRTC viewer on its own thread with its own runtime, so the session
/// loop never awaits WebRTC work.
pub struct StreamWorker {
    commands: Option<tokio::sync::mpsc::Sender<WorkerCommand>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl StreamWorker {
    pub fn spawn(events: tokio::sync::mpsc::Sender<WorkerEvent>) -> std::io::Result<Self> {
        let (commands, queue) = tokio::sync::mpsc::channel(COMMAND_QUEUE);
        let thread = std::thread::Builder::new()
            .name("stream-viewer".into())
            .spawn(move || {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(peer::run(queue, events)),
                    Err(error) => tracing::warn!(%error, "stream viewer runtime failed"),
                }
            })?;
        Ok(Self {
            commands: Some(commands),
            thread: Some(thread),
        })
    }

    /// Queues a command; gives it back when the queue is full.
    pub fn send(&self, command: WorkerCommand) -> Result<(), WorkerCommand> {
        match &self.commands {
            Some(commands) => commands.try_send(command).map_err(|e| e.into_inner()),
            None => Err(command),
        }
    }
}

impl Drop for StreamWorker {
    fn drop(&mut self) {
        // Closing the queue ends the loop, which closes the peer.
        self.commands = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod queue_tests {
    #[test]
    fn queue_holds_a_full_candidate_burst() {
        // The offer, every remote candidate the worker accepts, and a close.
        const { assert!(super::COMMAND_QUEUE > 1 + super::peer::MAX_REMOTE_CANDIDATES) };
    }
}
