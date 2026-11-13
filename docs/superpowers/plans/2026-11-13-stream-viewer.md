# Stream Viewer (Stage 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Mirage desktop user sees "AO VIVO" on a streaming participant, clicks "Assistir", and watches that screen (from the official TS6 client) inside Mirage.

**Architecture:** The bridge session loop (sole owner of the `!Send` session) runs TS6 stream signaling and relays offer/answer/ICE to a stream worker on its own thread. The worker runs a webrtc-rs peer connection that accepts only H.264 (packetization-mode=1), depacketizes RTP and decodes with the Windows Media Foundation H.264 decoder into a process-wide newest-frame slot that the egui view reads.

**Tech Stack:** Rust 1.99, tokio, `webrtc` 0.21.1 (webrtc-rs), `windows` 0.62.2 (Media Foundation), eframe/egui 0.37.

**Spec:** `docs/superpowers/specs/2026-11-13-stream-viewer-design.md`

## Global Constraints

- Codec: H.264 with `packetization-mode=1` only; AV1/VP9/VP8 and the audio m-line are declined.
- Size bounds: offer ≤ 16 KiB; each ICE JSON ≤ 2 KiB; ≤ 64 remote candidates per stream; decoded frame scaled to ≤ 1920 px wide.
- Session loop ↔ worker channels: 8 commands, 64 events, `try_send` only; a full queue fails that stream, never voice.
- Deadlines: offer/answer path 15 s; first decoded frame 20 s after the answer is sent.
- Never log SDP, ICE candidates, fingerprints or payloads; only counts and states.
- English in code/comments; Brazilian Portuguese for desktop UI strings; error copy exactly as in the spec's Errors table.
- Commit only when the owner asks, as `git -c user.name="Mirage Developer" -c user.email="mirage-developer@users.noreply.github.com" commit`, with no other attribution.
- Every task ends with `cargo fmt` + `cargo clippy --locked -p <crate> --all-targets --no-deps -- -D warnings` clean for the touched crates.

## Review Focus

1. The publisher stops while we are negotiating (offer arrives after `notifystreamstopped`, or stop arrives mid-ICE): tear down cleanly, show "A transmissão terminou.", no panic, no stray frames. Test in Task 6.
2. The user clicks "Assistir" on a second person while watching the first: the first is closed (`is_remove=1`) before the second starts; only one stream is active. Test in Task 6.
3. Disconnect or channel change while watching: worker closes the peer, slot is cleared, a frame decoded after close never reaches the view. Tests in Tasks 4 and 6.
4. The publisher resizes the shared window mid-stream (new SPS/resolution): the decoder handles the output-type change and the view resizes. Test in Task 3.
5. We join a channel where a stream is already running: `is_streaming` shows the badge and "Assistir" first sends `requeststreaminfo` to learn the stream id. Test in Task 6.

---

## File Structure

| File | Responsibility |
|------|----------------|
| `client/crates/ts6-core/src/state.rs` (modify) | `ClientInfo.is_streaming` |
| `client/crates/ts6-bridge/Cargo.toml` (modify) | `webrtc`, `windows` deps |
| `client/crates/ts6-bridge/src/stream_media/mod.rs` (new) | `StreamWorker` thread, webrtc-rs peer, depacketizing |
| `client/crates/ts6-bridge/src/stream_media/offer.rs` (new) | Pure offer check |
| `client/crates/ts6-bridge/src/stream_media/decode.rs` (new) | `H264Decoder` (MF), NV12 → RGBA, scaling |
| `client/crates/ts6-bridge/src/stream_media/frame.rs` (new) | `FrameSlot`, global `frames()` |
| `client/crates/ts6-bridge/src/streams.rs` (new) | `StreamRegistry`: signaling state machine, pure |
| `client/crates/ts6-bridge/src/lib.rs`, `protocol.rs` (modify) | Wire registry/worker into the session loop; commands/events |
| `client/apps/desktop/src/protocol.rs`, `app.rs`, `strings.rs` (modify) | Badge, watch action, stream view, errors |

---

### Task 1: Dependencies and streaming flag

**Files:**
- Modify: `client/crates/ts6-bridge/Cargo.toml`, `client/Cargo.lock`
- Modify: `client/crates/ts6-core/src/state.rs:40-52,110-122`
- Modify: `client/apps/desktop/src/protocol.rs` (client struct gains `is_streaming`)
- Modify: `docs/compatibility-status.md` (dependency note)

**Interfaces:**
- Produces: `ts6_core::ClientInfo { …, pub is_streaming: bool }` (serialized as `is_streaming`); desktop `ClientInfo.is_streaming: bool` with `#[serde(default)]`.

- [ ] **Step 1: Write the failing test** in `state.rs` tests: a book client with `is_streaming: Some(true)` maps to `ClientInfo.is_streaming == true`; `None` maps to `false`.
- [ ] **Step 2: Run** `cargo test --locked -p ts6-core state` → FAIL (no field).
- [ ] **Step 3: Implement** `is_streaming: c.is_streaming.unwrap_or(false)` in the snapshot mapping; add the desktop field.
- [ ] **Step 4: Add deps** to ts6-bridge: `webrtc = "=0.21.1"`; `[target.'cfg(windows)'.dependencies] windows = { version = "=0.62.2", features = ["Win32_Media_MediaFoundation", "Win32_System_Com", "Win32_Foundation"] }`. Run `cargo build --locked -p ts6-bridge` (after `cargo update -p webrtc` writes the lock).
- [ ] **Step 5: Supply-chain check:** `cargo tree -p ts6-bridge -e normal --prefix none | sort -u | wc -l` before/after recorded in `docs/compatibility-status.md`; `cargo audit` exit 0 (existing warnings only); confirm every new crate is MIT/Apache/BSD/ISC via `cargo tree -f "{p} {l}"`. A copyleft license stops the task for an owner decision.
- [ ] **Step 6: Run** `cargo test --locked -p ts6-core -p ts6-desktop` → PASS.

### Task 2: Offer check

**Files:**
- Create: `client/crates/ts6-bridge/src/stream_media/offer.rs` (and `mod.rs` declaring `pub mod offer;`)

**Interfaces:**
- Produces: `pub enum OfferError { TooLarge, NoH264 }`; `pub fn check_offer(sdp: &str) -> Result<(), OfferError>`; `pub const MAX_OFFER_BYTES: usize = 16 * 1024;`

- [ ] **Step 1: Write failing tests** with a fixture `OFFICIAL_SHAPE` reproducing the spike's m-lines/rtpmap/fmtp lines (secrets replaced by `x`):
  - `accepts_official_offer`: `check_offer(OFFICIAL_SHAPE) == Ok(())`.
  - `rejects_without_h264`: the fixture with the two `H264` rtpmap lines removed → `Err(NoH264)`.
  - `rejects_only_packetization_mode_0`: only payload 104 (`packetization-mode=0`) kept → `Err(NoH264)`.
  - `rejects_oversized`: `"v=0\r\n" + "a=x\r\n".repeat(4000)` → `Err(TooLarge)`.
- [ ] **Step 2: Run** `cargo test --locked -p ts6-bridge offer` → FAIL.
- [ ] **Step 3: Implement** by scanning `a=rtpmap:<pt> H264/90000` and requiring an `a=fmtp:<pt>` with `packetization-mode=1` for the same pt.
- [ ] **Step 4: Run** the tests → PASS.

### Task 3: H.264 decoder

**Files:**
- Create: `client/crates/ts6-bridge/src/stream_media/decode.rs`

**Interfaces:**
- Produces: `pub struct Frame { pub width: u32, pub height: u32, pub rgba: Vec<u8> }`; `pub struct H264Decoder`; `impl H264Decoder { pub fn new() -> Result<Self, DecodeError>; pub fn decode(&mut self, access_unit: &[u8]) -> Result<Option<Frame>, DecodeError>; }`; `pub enum DecodeError { Unavailable, Failed }`; `pub const MAX_FRAME_WIDTH: u32 = 1920;`

- [ ] **Step 1: Write failing tests** (Windows only, `#[cfg(windows)]`), with a test helper `encode_fixture(width, height, bgr: [u8;3], frames: usize) -> Vec<Vec<u8>>` that uses the Media Foundation software H.264 encoder (Annex B access units):
  - `decodes_solid_green`: 640×360 BGR (80,180,20), 10 units → ≥1 frame, 640×360, center RGBA within 14 of (20,180,80).
  - `follows_resolution_change`: 5 units at 640×360 then 5 at 1280×720 → last frame is 1280×720 (Review Focus 4).
  - `scales_wide_frames`: 2560×1440 → frame width 1920, height 1080.
- [ ] **Step 2: Run** `cargo test --locked -p ts6-bridge decode` → FAIL.
- [ ] **Step 3: Implement** by porting `decode_frames` from `recon/probe-capture/src/mft.rs:610-683` into a persistent decoder: COM/MF startup once per thread, software decoder enumeration (`Ok(None)` there becomes `DecodeError::Unavailable`), NV12 output, `MF_E_TRANSFORM_STREAM_CHANGE` re-selects NV12 (the probe's `Pulled::Changed`), one input per call with non-blocking pull. NV12 → RGBA with BT.709 limited range, nearest-neighbor downscale when width > 1920. Keep `unsafe` blocks with `// SAFETY:` notes as the probe does.
- [ ] **Step 4: Run** the tests → PASS.

### Task 4: Frame slot

**Files:**
- Create: `client/crates/ts6-bridge/src/stream_media/frame.rs`

**Interfaces:**
- Produces: `pub struct FrameSlot`; `impl FrameSlot { pub fn publish(&self, generation: u64, frame: Frame); pub fn latest(&self, after: u64) -> Option<(u64, Frame)>; pub fn open(&self) -> u64; pub fn close(&self); pub fn set_waker(&self, waker: Box<dyn Fn() + Send + Sync>); }`; `pub fn frames() -> &'static FrameSlot` (`OnceLock`). `open` starts a new generation and returns it; `publish` with a stale generation is ignored; each accepted publish calls the waker; `latest(after)` returns the newest frame only when its sequence number is greater than `after`.

- [ ] **Step 1: Write failing tests:** `newest_frame_wins` (publish 3, latest returns the third); `stale_generation_is_dropped` (open → g1, open → g2, publish(g1) ignored — Review Focus 3); `close_clears` (after close, latest is None); `waker_called_once_per_publish`.
- [ ] **Step 2: Run** `cargo test --locked -p ts6-bridge frame` → FAIL.
- [ ] **Step 3: Implement** with one `Mutex` holding generation, sequence and `Option<Frame>`.
- [ ] **Step 4: Run** → PASS.

### Task 5: Stream worker (webrtc-rs)

**Files:**
- Modify: `client/crates/ts6-bridge/src/stream_media/mod.rs`
- Test: `client/crates/ts6-bridge/tests/stream_loopback.rs`

**Interfaces:**
- Consumes: `check_offer`, `H264Decoder`, `frames()`.
- Produces:
  - `pub enum WorkerCommand { Offer { stream: String, sdp: String }, RemoteIce { stream: String, json: String }, Close }`
  - `pub enum WorkerEvent { Answer { stream: String, sdp: String }, LocalIce { stream: String, json: String }, Playing { stream: String }, Failed { stream: String, reason: FailReason } }`
  - `pub enum FailReason { Unsupported, Network, NoDecoder, Timeout }`
  - `pub struct StreamWorker; impl StreamWorker { pub fn spawn(events: tokio::sync::mpsc::Sender<WorkerEvent>) -> std::io::Result<Self>; pub fn send(&self, command: WorkerCommand) -> Result<(), WorkerCommand>; }` — channel capacity 8; `Drop` closes the peer and joins the thread.
  - ICE JSON shape both ways: `{"cmd":"iceCandidate","args":{"sdp":…,"mid":…,"mLine":…}}`; answer is sent by the caller as `{"cmd":"answer","args":{"answer":…}}`.

- [ ] **Step 1: Write the failing loopback test** `watches_h264_from_a_webrtc_rs_publisher`: a test publisher (webrtc-rs `TrackLocalStaticSample`, H.264 pm=1, plus an audio Opus sendonly m-line to mirror the official offer) creates an offer; the worker answers; ICE relayed both ways in-test; publisher sends 60 fixture samples (Task 3 helper, 640×360 green) at 20 fps. Assert within 20 s: a `Playing` event, `frames().latest(0)` center pixel green, and the answer's audio m-line has port 0.
- [ ] **Step 2: Write** `refuses_offer_without_h264` (VP8-only offer → `Failed{Unsupported}`, no answer) and `close_stops_frames` (Close → after 500 ms `frames().latest(last_seen)` is None).
- [ ] **Step 3: Run** `cargo test --locked -p ts6-bridge --test stream_loopback` → FAIL.
- [ ] **Step 4: Implement** the worker: dedicated thread + current-thread runtime; `MediaEngine` registering only H.264 `packetization-mode=1;profile-level-id=42e01f` and `640c1f` with feedback `nack`, `nack pli`, `ccm fir`, `goog-remb`, `transport-cc`; default interceptors; no STUN server (host and server-reflexive candidates come from the publisher; adding STUN is the spec's open risk 2). On `on_track`: read RTP, `H264Packet` depacketizer + `SampleBuilder` (max late 64), feed the decoder, publish to the slot; on decoder error send PLI and drop until a keyframe. Timeout: `Failed{Timeout}` when no frame 20 s after the answer. Remote candidates beyond 64 are ignored.
- [ ] **Step 5: Run** → PASS. This test is the spec's risk-1 gate for webrtc-rs; if DTLS/ICE fails here, stop and report before Task 6.

### Task 6: Stream registry and session-loop wiring

**Files:**
- Create: `client/crates/ts6-bridge/src/streams.rs`
- Modify: `client/crates/ts6-bridge/src/lib.rs` (session loop at the `Item::UnknownCommand` arm near line 550; `apply_command`), `client/crates/ts6-bridge/src/protocol.rs`

**Interfaces:**
- Consumes: `ts6_core::streams::{StreamEvent, ControlKind}`, `WorkerCommand/WorkerEvent/FailReason`.
- Produces:
  - Protocol commands: `Command::WatchStream { client: u16 }`, `Command::StopWatching`, internal `Command::StreamWorker(WorkerEvent)` (`#[serde(skip)]`).
  - Protocol event: `Event::StreamState { client: u16, state: &'a str, reason: Option<&'a str> }` with `state` ∈ `"connecting" | "playing" | "ended" | "failed"` and `reason` ∈ `"refused" | "timeout" | "unsupported" | "network" | "no_decoder"`.
  - `pub struct StreamRegistry`; `pub enum Action { Send(&'static str, Vec<(&'static str, String)>), Worker(WorkerCommand), Publish { client: u16, state: &'static str, reason: Option<&'static str> } }`; `impl StreamRegistry { pub fn on_event(&mut self, event: &StreamEvent, now: Instant) -> Vec<Action>; pub fn watch(&mut self, client: u16, now: Instant) -> Vec<Action>; pub fn stop(&mut self) -> Vec<Action>; pub fn on_worker(&mut self, event: WorkerEvent) -> Vec<Action>; pub fn tick(&mut self, now: Instant) -> Vec<Action>; pub fn reset(&mut self); }`.

- [ ] **Step 1: Write failing registry tests** (pure, no network):
  - `watch_sends_join_and_relays_offer`: started(id "s", clid 302) → watch(302) yields `Send("joinstreamrequest", [id s, clid 302, msg "", is_remove 0])`; join response accepted with offer yields `Worker(Offer)`.
  - `answer_and_ice_become_streamsignaling`: `on_worker(Answer)` → `Send("streamsignaling", [id, clid, json {"cmd":"answer",…}])`; remote `iceCandidate` signaling → `Worker(RemoteIce)`; the 65th remote candidate is dropped.
  - `refusal_publishes_failed_refused` (decision 0).
  - `offer_deadline_15s`: `tick(now + 15 s)` without offer → `Publish failed/timeout` + `Worker(Close)`.
  - `stop_while_negotiating_ends_cleanly` (Review Focus 1): stopped notice after watch → `Worker(Close)` + `Publish ended`; a later offer for that id produces no action.
  - `second_watch_replaces_first` (Review Focus 2): watching 302 then `watch(303)` → leave 302 (`is_remove 1`) before joining 303.
  - `late_join_asks_for_info_first` (Review Focus 5): `watch(302)` with no known stream → `Send("requeststreaminfo", [clid 302])`; an `Info` notice then triggers the join.
  - `reset_drops_everything` (Review Focus 3): after `reset`, events for the old id do nothing.
- [ ] **Step 2: Run** `cargo test --locked -p ts6-bridge streams` → FAIL.
- [ ] **Step 3: Implement** `StreamRegistry` as an explicit state enum (`Idle`, `AwaitingInfo`, `AwaitingOffer`, `Negotiating`, `Playing`) for one active watch plus a `HashMap<u16, String>` of known stream ids by publisher; payloads over the Global Constraints bounds are dropped.
- [ ] **Step 4: Wire** into `lib.rs`: parse `Item::UnknownCommand` content with `StreamEvent::parse` and feed `on_event`; spawn `StreamWorker` lazily on first watch with events forwarded into the internal command channel as `Command::StreamWorker`; execute `Action::Send` with `send_command_no_wait` (never awaiting WebRTC work); call `tick` on each loop wake; `reset` + `frames().close()` on disconnect and on own channel change.
- [ ] **Step 5: Run** `cargo test --locked -p ts6-bridge` → PASS (all bridge tests).

### Task 7: Desktop UI

**Files:**
- Modify: `client/apps/desktop/src/protocol.rs`, `client/apps/desktop/src/app.rs`, `client/apps/desktop/src/strings.rs`

**Interfaces:**
- Consumes: `Command::WatchStream/StopWatching`, `Event::StreamState`, `ClientInfo.is_streaming`, `ts6_bridge::stream_media::frame::frames()`.
- Produces: `App.watching: Option<Watching { client: u16, state: WatchState, texture: Option<TextureHandle>, seen: u64 }>`.

- [ ] **Step 1: Write failing tests** in desktop `protocol.rs`: `stream_state_events_parse` for each state/reason; `watch_commands_serialize` (`{"cmd":"watch_stream","client":302}` shape matching the bridge's serde tags).
- [ ] **Step 2: Run** `cargo test --locked -p ts6-desktop protocol` → FAIL.
- [ ] **Step 3: Implement protocol** types and pt-BR strings: `live_badge: "AO VIVO"`, `watch_stream: "Assistir"`, `stop_watching: "Parar de assistir"`, `stream_connecting: "Conectando à transmissão…"`, and the six error strings verbatim from the spec's Errors table.
- [ ] **Step 4: Implement UI:** red "AO VIVO" pill on cards where `is_streaming`; card click or a hover "Assistir" button sends `WatchStream`; while watching, the participants area shows the stream (contain-fit, 16:9 letterbox, rounded 8) with the participants as a horizontal strip of small cards below and a close button top-right; frames pulled with `frames().latest(seen)` into one reused `TextureHandle` (`set` on size change); `frames().set_waker` registered once to `ctx.request_repaint()`; connecting spinner text until `playing`; failure text in the view with a close button; `ended` closes the view and shows "A transmissão terminou." in the chat notice line.
- [ ] **Step 5: Run** `cargo test --locked -p ts6-desktop` → PASS; build and launch `target/debug/ts6-desktop.exe` for the owner.

### Task 8: Live verification and records

**Files:**
- Modify: `docs/compatibility-status.md`, `docs/persistent-chat-and-streaming-research.md` (new section "Native viewer against the official client")

- [ ] **Step 1: Full checks:** `python scripts/check_repository.py`; from `client`: build, `test --locked --workspace`, fmt check, clippy `--workspace --all-targets --no-deps -D warnings`. All pass.
- [ ] **Step 2: Live, same network:** friend shares a window from the official TS6 in `Default Channel`; owner clicks "Assistir". Record: time to first frame, frames shown over 60 s (from the slot sequence), teardown on close and on publisher stop. Pass = picture visible and both teardowns clean.
- [ ] **Step 3: Live, different homes:** repeat with publisher and viewer on different networks. A failure here is recorded with the candidate-type counts (no addresses) and becomes the STUN follow-up, not a blocker for stage 1.
- [ ] **Step 4: Record** results (counts and booleans only) in the two docs; list what was not verified.
