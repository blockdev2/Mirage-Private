//! Screen sharing media: the screen capture and one WebRTC publisher per
//! viewer, on a thread of their own so the session loop never awaits them.
//!
//! TS6 streams are peer to peer, so each viewer gets its own `ts6-stream`
//! publisher; the picture is captured and encoded once and the same access
//! units go to every connected viewer. SDP and ICE payloads are relayed to the
//! session loop and never logged.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
pub use ts6_capture::CaptureSource;
use ts6_capture::EncodedUnit;
#[cfg(windows)]
pub use ts6_capture::{
    list_monitors as monitors, list_windows as windows, MonitorInfo, WindowInfo,
};
use ts6_stream::{Publisher, Signal};

/// Viewers served at once. Every viewer is a separate peer connection, so the
/// uplink needed is this many times the stream bitrate.
pub const MAX_VIEWERS: usize = 4;
/// Remote candidates applied per viewer.
pub const MAX_REMOTE_CANDIDATES: usize = 64;
/// A viewer that is not connected after this long is dropped.
const VIEWER_DEADLINE: Duration = Duration::from_secs(20);
/// Encoded units waiting between the capture thread and the worker.
const UNIT_QUEUE: usize = 8;
/// Room for every viewer's offer, answer, candidates and removal at once.
const COMMAND_QUEUE: usize = MAX_VIEWERS * (MAX_REMOTE_CANDIDATES + 3) + 4;
/// Fallback sample length before two units give a real one.
const DEFAULT_SAMPLE: Duration = Duration::from_millis(33);

/// Requests from the session loop. Payloads come from the TS6 server.
pub enum ShareCommand {
    /// Starts the capture; answered with `CaptureReady` or `Failed`.
    Start {
        source: CaptureSource,
    },
    AddViewer {
        viewer: u16,
    },
    /// A `streamsignaling` JSON from this viewer (answer or candidate).
    Signal {
        viewer: u16,
        json: String,
    },
    RemoveViewer {
        viewer: u16,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareFailure {
    /// No usable hardware H.264 encoder, or not Windows.
    Unsupported,
    /// The capture could not start or stopped on its own.
    Capture,
}

/// Reports to the session loop.
pub enum ShareEvent {
    CaptureReady,
    /// The offer for `respondjoinstreamrequest`.
    Offer {
        viewer: u16,
        sdp: String,
    },
    /// A local candidate as `{"cmd":"iceCandidate","args":{…}}`.
    LocalIce {
        viewer: u16,
        json: String,
    },
    ViewerConnected {
        viewer: u16,
    },
    /// The viewer never connected, or its connection ended.
    ViewerFailed {
        viewer: u16,
    },
    Failed(ShareFailure),
}

impl std::fmt::Debug for ShareEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CaptureReady => write!(f, "CaptureReady"),
            Self::Offer { viewer, .. } => write!(f, "Offer({viewer})"),
            Self::LocalIce { viewer, .. } => write!(f, "LocalIce({viewer})"),
            Self::ViewerConnected { viewer } => write!(f, "ViewerConnected({viewer})"),
            Self::ViewerFailed { viewer } => write!(f, "ViewerFailed({viewer})"),
            Self::Failed(reason) => write!(f, "Failed({reason:?})"),
        }
    }
}

/// What the capture side hands the worker.
pub enum Pumped {
    Unit(EncodedUnit),
    /// The capture stopped on its own.
    Ended,
}

/// A running capture as the worker sees it. Field order is drop order: the
/// queue closes before the capture thread is joined.
pub struct Source {
    pub units: mpsc::Receiver<Pumped>,
    /// Set to ask the encoder for a keyframe.
    pub keyframe: Arc<AtomicBool>,
    pub guard: Option<Box<dyn Send>>,
}

/// Starts a capture of the given monitor or window.
pub type Starter = Box<dyn FnMut(CaptureSource) -> Result<Source, ShareFailure> + Send>;

/// The share worker. Dropping it closes every viewer and stops the capture.
pub struct ShareWorker {
    commands: Option<mpsc::Sender<ShareCommand>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ShareWorker {
    /// The real thing: monitor capture, peers bound to every interface.
    pub fn spawn(events: mpsc::Sender<ShareEvent>) -> std::io::Result<Self> {
        Self::spawn_with(
            events,
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            Box::new(capture_source),
        )
    }

    pub fn spawn_with(
        events: mpsc::Sender<ShareEvent>,
        bind: IpAddr,
        starter: Starter,
    ) -> std::io::Result<Self> {
        let (commands, queue) = mpsc::channel(COMMAND_QUEUE);
        let thread = std::thread::Builder::new()
            .name("screen-share".into())
            .spawn(move || {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(run(queue, events, bind, starter)),
                    Err(error) => tracing::warn!(%error, "screen share runtime failed"),
                }
            })?;
        Ok(Self {
            commands: Some(commands),
            thread: Some(thread),
        })
    }

    /// Queues a command; gives it back when the queue is full.
    pub fn send(&self, command: ShareCommand) -> Result<(), ShareCommand> {
        match &self.commands {
            Some(commands) => commands.try_send(command).map_err(|e| e.into_inner()),
            None => Err(command),
        }
    }
}

impl Drop for ShareWorker {
    fn drop(&mut self) {
        // Closing the queue ends the loop, which closes the peers.
        self.commands = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Viewer {
    publisher: Publisher,
    connected: bool,
    /// Last moment the viewer was known well; a viewer that stays away longer
    /// than `VIEWER_DEADLINE` is dropped.
    since: Instant,
    /// Deltas are held back until a keyframe starts the viewer's picture.
    need_key: bool,
}

async fn run(
    mut queue: mpsc::Receiver<ShareCommand>,
    events: mpsc::Sender<ShareEvent>,
    bind: IpAddr,
    mut starter: Starter,
) {
    let mut source: Option<Source> = None;
    let mut viewers: HashMap<u16, Viewer> = HashMap::new();
    // The newest keyframe, so a viewer that joins a still screen (which
    // produces no new units) has a picture at once.
    let mut last_keyframe: Option<Vec<u8>> = None;
    let mut last_time: Option<i64> = None;
    let mut tick = tokio::time::interval(Duration::from_millis(25));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    'outer: loop {
        tokio::select! {
            command = queue.recv() => {
                let Some(command) = command else { break };
                match command {
                    ShareCommand::Start { source: chosen } => {
                        if source.is_some() {
                            continue;
                        }
                        let report = match starter(chosen) {
                            Ok(started) => {
                                source = Some(started);
                                ShareEvent::CaptureReady
                            }
                            Err(reason) => ShareEvent::Failed(reason),
                        };
                        if events.send(report).await.is_err() {
                            break;
                        }
                    }
                    ShareCommand::AddViewer { viewer } => {
                        if let Some(old) = viewers.remove(&viewer) {
                            old.publisher.close().await;
                        }
                        match add_viewer(bind, viewer).await {
                            Ok((publisher, sdp)) => {
                                viewers.insert(viewer, Viewer {
                                    publisher,
                                    connected: false,
                                    since: Instant::now(),
                                    need_key: true,
                                });
                                if events.send(ShareEvent::Offer { viewer, sdp }).await.is_err() {
                                    break;
                                }
                            }
                            Err(()) => {
                                if events.send(ShareEvent::ViewerFailed { viewer }).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    ShareCommand::Signal { viewer, json } => {
                        let Some(entry) = viewers.get_mut(&viewer) else { continue };
                        let Ok(signal) = Signal::parse(&json) else { continue };
                        let is_answer = matches!(signal, Signal::Answer(_));
                        // A bad candidate is harmless; a bad answer ends the viewer.
                        if entry.publisher.apply(signal).await.is_err() && is_answer {
                            if let Some(old) = viewers.remove(&viewer) {
                                old.publisher.close().await;
                            }
                            if events.send(ShareEvent::ViewerFailed { viewer }).await.is_err() {
                                break;
                            }
                        }
                    }
                    ShareCommand::RemoveViewer { viewer } => {
                        if let Some(old) = viewers.remove(&viewer) {
                            old.publisher.close().await;
                        }
                    }
                }
            }
            pumped = next_unit(&mut source) => {
                match pumped {
                    Some(Pumped::Unit(unit)) => {
                        let duration = sample_duration(&mut last_time, unit.time_100ns);
                        if unit.keyframe {
                            last_keyframe = Some(unit.data.clone());
                        }
                        for entry in viewers.values_mut().filter(|v| v.connected) {
                            if entry.need_key {
                                if !unit.keyframe {
                                    continue;
                                }
                                entry.need_key = false;
                            }
                            // A failed send is a lost packet, not a reason to stop.
                            let _ = entry.publisher.send_sample(unit.data.clone(), duration).await;
                        }
                    }
                    Some(Pumped::Ended) | None => {
                        source = None;
                        if events.send(ShareEvent::Failed(ShareFailure::Capture)).await.is_err() {
                            break;
                        }
                    }
                }
            }
            _ = tick.tick() => {
                let mut failed = Vec::new();
                for (&id, entry) in viewers.iter_mut() {
                    while let Ok(signal) = entry.publisher.signals().try_recv() {
                        let event = ShareEvent::LocalIce { viewer: id, json: signal.to_json() };
                        if events.send(event).await.is_err() {
                            break 'outer;
                        }
                    }
                    if entry.publisher.is_connected() {
                        entry.since = Instant::now();
                        if !entry.connected {
                            entry.connected = true;
                            entry.need_key = true;
                            if let Some(source) = &source {
                                source.keyframe.store(true, Ordering::Release);
                            }
                            // The newest picture now; live units follow from the next keyframe.
                            if let Some(frame) = &last_keyframe {
                                let _ = entry.publisher.send_sample(frame.clone(), DEFAULT_SAMPLE).await;
                            }
                            if events.send(ShareEvent::ViewerConnected { viewer: id }).await.is_err() {
                                break 'outer;
                            }
                        }
                    } else {
                        entry.connected = false;
                        if entry.since.elapsed() >= VIEWER_DEADLINE {
                            failed.push(id);
                        }
                    }
                }
                for id in failed {
                    if let Some(old) = viewers.remove(&id) {
                        old.publisher.close().await;
                    }
                    if events.send(ShareEvent::ViewerFailed { viewer: id }).await.is_err() {
                        break 'outer;
                    }
                }
            }
        }
    }

    for (_, entry) in viewers {
        let _ = tokio::time::timeout(Duration::from_secs(2), entry.publisher.close()).await;
    }
}

/// Waits for the next unit; never completes without a capture.
async fn next_unit(source: &mut Option<Source>) -> Option<Pumped> {
    match source {
        Some(source) => source.units.recv().await,
        None => std::future::pending().await,
    }
}

async fn add_viewer(bind: IpAddr, viewer: u16) -> Result<(Publisher, String), ()> {
    let publisher = Publisher::new(bind).await.map_err(|error| {
        tracing::warn!(viewer, %error, "screen share peer could not start");
    })?;
    match publisher.offer().await {
        Ok(sdp) => Ok((publisher, sdp)),
        Err(error) => {
            tracing::warn!(viewer, %error, "screen share offer failed");
            publisher.close().await;
            Err(())
        }
    }
}

/// Length of the sample that ends at `time_100ns`, from the previous unit.
fn sample_duration(last: &mut Option<i64>, time_100ns: i64) -> Duration {
    let duration = match *last {
        Some(previous) if time_100ns > previous => {
            let nanos = u64::try_from(time_100ns - previous).unwrap_or(0) * 100;
            Duration::from_nanos(nanos).clamp(Duration::from_millis(1), Duration::from_secs(1))
        }
        _ => DEFAULT_SAMPLE,
    };
    *last = Some(time_100ns);
    duration
}

/// Monitor capture with a hardware H.264 encoder; the capture lives on its
/// own thread and its units are forwarded through a bounded queue.
#[cfg(windows)]
fn capture_source(chosen: CaptureSource) -> Result<Source, ShareFailure> {
    use std::sync::mpsc::RecvTimeoutError;
    use ts6_capture::{CaptureConfig, ScreenCapture};

    struct Guard {
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    let (sender, units) = mpsc::channel(UNIT_QUEUE);
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<bool>(1);
    let keyframe = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let (wanted, halt) = (Arc::clone(&keyframe), Arc::clone(&stop));
    let thread = std::thread::Builder::new()
        .name("screen-capture".into())
        .spawn(move || {
            let config = CaptureConfig {
                source: chosen,
                ..CaptureConfig::default()
            };
            let capture = match ScreenCapture::start(config) {
                Ok(capture) => capture,
                Err(error) => {
                    tracing::warn!(%error, "screen capture could not start");
                    let _ = ready_tx.send(false);
                    return;
                }
            };
            let _ = ready_tx.send(true);
            while !halt.load(Ordering::Acquire) {
                if wanted.swap(false, Ordering::AcqRel) {
                    capture.request_keyframe();
                }
                match capture.units().recv_timeout(Duration::from_millis(100)) {
                    Ok(unit) => match sender.try_send(Pumped::Unit(unit)) {
                        Ok(()) => {}
                        // The worker is behind: drop this unit and restart on a keyframe.
                        Err(mpsc::error::TrySendError::Full(_)) => capture.request_keyframe(),
                        Err(mpsc::error::TrySendError::Closed(_)) => return,
                    },
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
                if !capture.is_running() {
                    break;
                }
            }
            if !halt.load(Ordering::Acquire) {
                if let Some(error) = capture.error() {
                    tracing::warn!(%error, "screen capture ended");
                }
                // Closed receiver (worker gone) unblocks this.
                let _ = sender.blocking_send(Pumped::Ended);
            }
        })
        .map_err(|_| ShareFailure::Capture)?;
    let guard = Guard {
        stop,
        thread: Some(thread),
    };
    match ready_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(true) => Ok(Source {
            units,
            keyframe,
            guard: Some(Box::new(guard)),
        }),
        _ => Err(ShareFailure::Capture),
    }
}

#[cfg(not(windows))]
fn capture_source(_wanted: CaptureSource) -> Result<Source, ShareFailure> {
    Err(ShareFailure::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::timeout;
    use ts6_stream::Viewer as RemoteViewer;

    const LOCALHOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

    /// Annex-B access unit with parameter sets and an IDR slice of dummy bytes:
    /// enough for the RTP packetizer, not a decodable picture.
    fn unit(keyframe: bool, time_100ns: i64) -> EncodedUnit {
        let mut data = Vec::new();
        if keyframe {
            data.extend_from_slice(&[0, 0, 0, 1, 0x67, 0x42, 0xe0, 0x1f, 0xda, 0x01]);
            data.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xce, 0x3c, 0x80]);
            data.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x88, 0x84]);
        } else {
            data.extend_from_slice(&[0, 0, 0, 1, 0x41, 0x9a, 0x24]);
        }
        data.extend(std::iter::repeat_n(0x5a, 2000));
        EncodedUnit {
            data,
            keyframe,
            time_100ns,
        }
    }

    struct Fixture {
        worker: ShareWorker,
        events: mpsc::Receiver<ShareEvent>,
        units: mpsc::Sender<Pumped>,
        keyframe: Arc<AtomicBool>,
    }

    /// A worker whose capture is a channel the test feeds.
    fn fixture() -> Fixture {
        let (events_tx, events) = mpsc::channel(256);
        let (units_tx, units_rx) = mpsc::channel(UNIT_QUEUE);
        let keyframe = Arc::new(AtomicBool::new(false));
        let mut source = Some(Source {
            units: units_rx,
            keyframe: Arc::clone(&keyframe),
            guard: None,
        });
        let worker = ShareWorker::spawn_with(
            events_tx,
            LOCALHOST,
            Box::new(move |_| source.take().ok_or(ShareFailure::Capture)),
        )
        .expect("worker");
        Fixture {
            worker,
            events,
            units: units_tx,
            keyframe,
        }
    }

    async fn next_event(events: &mut mpsc::Receiver<ShareEvent>) -> ShareEvent {
        timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("event in time")
            .expect("worker alive")
    }

    #[test]
    fn sample_durations_follow_the_capture_clock_within_bounds() {
        let mut last = None;
        assert_eq!(sample_duration(&mut last, 0), DEFAULT_SAMPLE);
        assert_eq!(
            sample_duration(&mut last, 400_000),
            Duration::from_millis(40)
        );
        // A repeated or backwards timestamp falls back; a long stall is capped.
        assert_eq!(sample_duration(&mut last, 400_000), DEFAULT_SAMPLE);
        assert_eq!(
            sample_duration(&mut last, 400_000 + 600_000_000),
            Duration::from_secs(1)
        );
    }

    #[test]
    fn events_never_print_payloads() {
        let text = format!(
            "{:?}",
            ShareEvent::Offer {
                viewer: 7,
                sdp: "v=0 private-fixture".into()
            }
        );
        assert!(!text.contains("private-fixture"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_start_is_reported_and_the_worker_stays_usable() {
        let (events_tx, mut events) = mpsc::channel(8);
        let worker = ShareWorker::spawn_with(
            events_tx,
            LOCALHOST,
            Box::new(|_| Err(ShareFailure::Unsupported)),
        )
        .unwrap();
        assert!(worker
            .send(ShareCommand::Start {
                source: CaptureSource::Monitor(None)
            })
            .is_ok());
        assert!(matches!(
            next_event(&mut events).await,
            ShareEvent::Failed(ShareFailure::Unsupported)
        ));
        drop(worker);
    }

    /// Adds viewer 9 and connects a ts6-stream viewer to it on loopback: offer,
    /// answer and candidates all pass through the same JSON the TS6 relay
    /// carries. Returns once the worker reports the connection.
    async fn connect_viewer(
        worker: &ShareWorker,
        events: &mut mpsc::Receiver<ShareEvent>,
    ) -> RemoteViewer {
        worker
            .send(ShareCommand::AddViewer { viewer: 9 })
            .ok()
            .unwrap();
        let ShareEvent::Offer { viewer: 9, sdp } = next_event(events).await else {
            panic!("expected the offer");
        };
        let mut remote = RemoteViewer::new(LOCALHOST).await.expect("remote viewer");
        let answer = remote.accept_offer(&sdp).await.expect("answer");
        worker
            .send(ShareCommand::Signal {
                viewer: 9,
                json: Signal::Answer(answer).to_json(),
            })
            .ok()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut connected = false;
        while !connected {
            assert!(Instant::now() < deadline, "peers did not connect");
            while let Ok(Some(signal)) =
                timeout(Duration::from_millis(5), remote.signals().recv()).await
            {
                worker
                    .send(ShareCommand::Signal {
                        viewer: 9,
                        json: signal.to_json(),
                    })
                    .ok()
                    .unwrap();
            }
            while let Ok(Some(event)) = timeout(Duration::from_millis(5), events.recv()).await {
                match event {
                    ShareEvent::LocalIce { viewer: 9, json } => {
                        remote.apply(Signal::parse(&json).unwrap()).await.unwrap();
                    }
                    ShareEvent::ViewerConnected { viewer: 9 } => connected = true,
                    other => panic!("unexpected {other:?}"),
                }
            }
        }
        remote
    }

    /// The whole publisher path against a ts6-stream viewer on loopback, with a
    /// synthetic capture. A joining viewer asks for a keyframe and gets no
    /// delta before it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_viewer_receives_the_shared_screen_over_loopback() {
        let mut fixture = fixture();
        fixture
            .worker
            .send(ShareCommand::Start {
                source: CaptureSource::Monitor(None),
            })
            .ok()
            .unwrap();
        assert!(matches!(
            next_event(&mut fixture.events).await,
            ShareEvent::CaptureReady
        ));
        let remote = connect_viewer(&fixture.worker, &mut fixture.events).await;
        assert!(
            fixture.keyframe.load(Ordering::Acquire),
            "a joining viewer asks the encoder for a keyframe"
        );

        // A delta alone is held back: the viewer could not decode it.
        fixture
            .units
            .send(Pumped::Unit(unit(false, 0)))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(remote.packets(), 0, "a delta must wait for a keyframe");

        // The keyframe starts the picture and the deltas after it follow.
        let mut time = 330_000;
        for key in [true, false, false] {
            fixture
                .units
                .send(Pumped::Unit(unit(key, time)))
                .await
                .unwrap();
            time += 330_000;
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while remote.packets() < 3 {
            assert!(Instant::now() < deadline, "no media arrived");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        // A viewer leaving is closed without disturbing the worker.
        fixture
            .worker
            .send(ShareCommand::RemoveViewer { viewer: 9 })
            .ok()
            .unwrap();
        remote.close().await;
    }

    /// The real monitor capture and hardware encoder, through the worker, to a
    /// loopback viewer. Needs an interactive desktop, a GPU H.264 encoder and
    /// something moving on screen (the cursor counts). Counts only; no picture
    /// is kept.
    ///
    /// `cargo test -p ts6-bridge share_media -- --ignored --nocapture`
    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "needs an interactive desktop, a hardware H.264 encoder and screen motion"]
    async fn the_primary_monitor_reaches_a_loopback_viewer_through_the_worker() {
        let (events_tx, mut events) = mpsc::channel(256);
        let worker =
            ShareWorker::spawn_with(events_tx, LOCALHOST, Box::new(capture_source)).unwrap();
        worker
            .send(ShareCommand::Start {
                source: CaptureSource::Monitor(None),
            })
            .ok()
            .unwrap();
        assert!(matches!(
            next_event(&mut events).await,
            ShareEvent::CaptureReady
        ));
        let remote = connect_viewer(&worker, &mut events).await;
        let deadline = Instant::now() + Duration::from_secs(20);
        while remote.packets() < 30 {
            assert!(
                Instant::now() < deadline,
                "viewer saw {} packets: move something on screen",
                remote.packets()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        eprintln!("viewer_packets={}", remote.packets());
        remote.close().await;
        drop(worker);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_capture_that_ends_by_itself_is_a_failure() {
        let mut fixture = fixture();
        fixture
            .worker
            .send(ShareCommand::Start {
                source: CaptureSource::Monitor(None),
            })
            .ok()
            .unwrap();
        assert!(matches!(
            next_event(&mut fixture.events).await,
            ShareEvent::CaptureReady
        ));
        fixture.units.send(Pumped::Ended).await.unwrap();
        assert!(matches!(
            next_event(&mut fixture.events).await,
            ShareEvent::Failed(ShareFailure::Capture)
        ));
    }
}
