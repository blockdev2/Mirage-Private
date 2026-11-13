# Screen share session wiring (stage 3)

**Goal:** A Mirage user clicks the share button, picks a monitor, and viewers in
the channel can request the stream; capture, encoder and peers run off the
session thread.

**Architecture:** `ts6-bridge/src/share.rs` is a pure state machine like
`streams.rs` (events in, actions out). `share_media.rs` is a worker thread with
its own runtime that owns the `ts6-capture` capture (on a pump thread) and one
`ts6-stream` `Publisher` per viewer. The session loop only translates actions
into non-waiting server commands, so voice never waits on stream work.

## Decisions

- Capture first, then `setupstream`: no stream is announced that cannot run.
- Capture and encode once; fan the same units out to up to 4 viewers (P2P
  uplink is viewers x bitrate). Further requests get `decision=0`.
- A viewer that connects asks for a keyframe, receives the cached newest one at
  once (a still screen produces no new units) and no delta before a fresh
  keyframe, which keeps the decoder consistent.
- Deadlines: 15 s for capture start and the server's confirmation; 20 s for a
  viewer to connect. A share stopped before the stream id is known is cancelled
  with `stopstream` when `notifystreamstarted` arrives.
- The share ends on channel change, disconnect, a server stop and capture loss.
- UI: the existing disabled button in the call controls became the share
  button; a popup lists monitors (listed again on each open); a running share
  shows the button as active with the viewer count in its tooltip. Failures
  show in the chat notice line (pt-BR).

## Verification

- Unit: `share.rs` (13 state-machine tests), `ts6-core` parser additions,
  protocol round trips in bridge and desktop.
- Worker: loopback against a `ts6-stream` viewer with synthetic units
  (keyframe gating, removal, capture loss, failed start).
- Real hardware (ignored): `cargo test -p ts6-bridge share_media -- --ignored
  --nocapture` with something moving on screen.

## Not done

Official-client interoperability, a live server run, cross-machine/NAT/STUN,
audio, per-viewer bitrate caps, idle-screen repeat frames, window capture.
