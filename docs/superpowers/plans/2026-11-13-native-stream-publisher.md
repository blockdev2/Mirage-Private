# Native stream publisher, stage 1 implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans. Steps use checkbox syntax.

**Goal:** A native Rust WebRTC publisher and viewer that speak TeamSpeak 6's stream signaling shapes, proven on loopback with a synthetic H.264 source.

**Architecture:** New crate `ts6-stream`, independent of `ts6-core` (the `!Send` session owner is not touched in this stage). It exposes signaling-agnostic peers: they produce and consume SDP strings and `Signal` values; a caller relays those through `setupstream` / `respondjoinstreamrequest` / `streamsignaling`. Media enters as already-encoded Annex-B H.264 samples, so stage 2 can plug a Media Foundation encoder in without changing the peer API.

**Tech Stack:** Rust, `webrtc` 0.21.1 (runtime-tokio, crypto-ring), `tokio`, `serde_json`.

**Spec:** `docs/native-streaming-architecture.md`, `docs/protocol-ts6-astrum.md` section 2.

## Global Constraints

- First-party prose, comments and identifiers in English; MIT license.
- No command fields, SDP or ICE candidates in logs or `Debug` output (SDP carries addresses and ICE credentials).
- Loopback only in tests; no remote test server, no STUN/TURN servers contacted.
- Every channel and buffer is bounded; signaling payloads over 16 KiB are rejected (same bound as `ts6-core::streams`).
- `cargo clippy -D warnings`, `cargo fmt --check`, `cargo audit` stay clean.

## Review Focus

- A relayed signal with an oversized, malformed or unknown `cmd` must be rejected, not panic.
- An ICE candidate arriving before the remote description must not be lost or crash.
- Closing the publisher while the viewer is connected must end the viewer's track, not hang.
- A viewer must not be able to make the publisher send before the answer is applied.

## File structure

- `client/crates/ts6-stream/Cargo.toml`: crate manifest.
- `client/crates/ts6-stream/src/lib.rs`: module wiring and re-exports.
- `client/crates/ts6-stream/src/signal.rs`: `Signal` and the TS6 `streamsignaling` JSON codec.
- `client/crates/ts6-stream/src/peer.rs`: `Publisher` and `Viewer` over webrtc-rs.
- `client/crates/ts6-stream/tests/loopback.rs`: publisher to viewer over 127.0.0.1.
- `client/Cargo.toml`: add the member. `docs/CHANGELOG.md`/`CHANGELOG.md`: one line.

### Task 1: Signal codec

**Interfaces:**
- Produces: `enum Signal { Answer(String), IceCandidate { sdp: String, mid: Option<String>, m_line: Option<u16> } }`, `Signal::to_json(&self) -> String`, `Signal::parse(json: &str) -> Result<Signal, SignalError>`.

- [ ] Tests in `signal.rs`: round trip of both variants; parse of `{"cmd":"answer","args":{"answer":"v=0"}}` and `{"cmd":"iceCandidate","args":{"sdp":"candidate:1","mid":"0","mLine":0}}`; rejects unknown cmd, missing args, payload over 16 KiB; `Debug` never prints the SDP.
- [ ] Implement with `serde_json::Value` (shape is external, so no derive on the wire type).

### Task 2: Publisher and viewer

**Interfaces:**
- Produces: `Publisher::new(bind: IpAddr) -> Result<Publisher>`, `Publisher::offer(&self) -> Result<String>`, `Publisher::apply(&self, Signal) -> Result<()>`, `Publisher::signals(&mut self) -> &mut Receiver<Signal>`, `Publisher::send_sample(&self, annex_b: Vec<u8>, duration: Duration) -> Result<()>`, `Publisher::close(&self)`. `Viewer::new(bind)`, `Viewer::accept_offer(&self, sdp: &str) -> Result<String>` (returns the answer), `apply`, `signals`, `Viewer::packets(&self) -> u64`, `Viewer::close`.

- [ ] Write `tests/loopback.rs` first: publisher offers, viewer answers, signals relayed through `Signal::to_json`/`parse`, publisher sends 60 synthetic samples at 30 fps, viewer counts at least 30 RTP packets within 10 s. Run it and see it fail to compile.
- [ ] Implement `peer.rs` (H.264 `packetization-mode=1`, payload 102, send-only video; bounded signal channel of 64; sending refused until the answer is applied).
- [ ] Review-focus tests: candidate before remote description is queued; close ends the viewer's track.

### Task 3: Verify and ship

- [ ] `cargo fmt`, `cargo clippy --locked --workspace --all-targets --no-deps -- -D warnings`, `cargo test --locked -p ts6-stream`, `cargo audit`, `python scripts/check_repository.py`.
- [ ] Commit with the noreply identity, push `claude/screen-share`, open a PR; merge only on green CI and tell the user.

## Not in this stage (stated limits)

Real screen capture, hardware encoding, TS6 server relay, official-client interop, audio, and viewer-side decoding. Samples here are synthetic NAL bytes, so this proves transport and signaling only, not picture quality.
