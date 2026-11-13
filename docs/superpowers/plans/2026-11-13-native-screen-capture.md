# Native screen capture and H.264 encoding, stage 2 implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans. Steps use checkbox syntax.

**Goal:** Capture the primary monitor, encode it to H.264 on the GPU, and feed the access units to the stage 1 `Publisher`, proven on loopback.

**Architecture:** New Windows-only crate `ts6-capture`. A Windows Graphics Capture callback copies each frame, a D3D11 video processor scales and converts it to NV12 into a small texture ring, and a direct Media Foundation H.264 encoder (hardware MFT on the capture adapter, zero CPU readback) emits Annex-B units into a bounded channel. Code is ported and narrowed from `recon/probe-capture` (hardware measured there: encoder driving, async events, low-latency settings).

**Tech Stack:** Rust, `windows` 0.62.2, `windows-capture` 2.0.1, `ts6-stream` (dev only).

**Spec:** `docs/native-streaming-architecture.md`, `recon/probe-capture/README.md`.

## Global Constraints

- First-party prose and identifiers in English; MIT.
- Capture only on explicit request; nothing is written to disk, no frame leaves the process; tests print counts, never pixels.
- Bounded everything: output channel of 8 units (a full channel drops the unit and forces the next keyframe), unit size 8 MiB, MFT event waits under a deadline.
- Hardware encoders only in this stage; a machine without one gets a clear error, not a silent CPU fallback.
- `cargo clippy -D warnings`, `cargo fmt --check` clean; tests that need a desktop and a GPU are `#[ignore]`.

## Review Focus

- Source smaller than the requested maximum is never upscaled; odd sizes become even (NV12).
- A unit without SPS/PPS before an IDR must get them prepended so a late viewer can start.
- A full output channel must not block the capture callback.
- A mid-stream monitor resolution change must rebuild the pipeline, not crash.
- A static screen produces no frames (WGC only delivers changes): documented limit, not hidden.

## Files

- `client/crates/ts6-capture/src/{lib,config,nal,convert,encoder,capture}.rs`: pure helpers (`config`, `nal`) are unit-tested; `convert`, `encoder`, `capture` need a GPU.
- `client/crates/ts6-capture/tests/screen_to_viewer.rs`: ignored end-to-end test through `ts6-stream`.

### Task 1: Pure helpers
- [ ] `fit_size(src: (u32,u32), max: (u32,u32)) -> (u32,u32)` never upscales, keeps aspect, even dims; `analyze(&[u8]) -> UnitInfo { keyframe, parameter_sets }` over Annex-B NAL types 5/7/8; `with_header(header, unit)`. Tests for each, including the review-focus cases.

### Task 2: Converter and encoder
- [ ] `Converter::new(device, context, src, dst, fps)` and `convert(&mut self, frame) -> &ID3D11Texture2D`; `H264Encoder::new(device, size, fps, kbps)`, `encode(&mut self, nv12, time_100ns) -> Result<Vec<EncodedUnit>>`, `request_keyframe()`.

### Task 3: Capture
- [ ] `ScreenCapture::start(CaptureConfig) -> Result<ScreenCapture>`, `units() -> &Receiver<EncodedUnit>`, `request_keyframe()`, `error()`, `stop()`.

### Task 4: End to end
- [ ] Ignored test: capture the primary monitor for a few seconds, first unit is a keyframe with parameter sets, units are pushed into `ts6_stream::Publisher` and a loopback `Viewer` counts RTP packets.

### Task 5: Verify and ship
- [ ] fmt, clippy, workspace tests, audit, policy check; PR; merge only on green CI.

## Not in this stage

TS6 session wiring, UI picker, window capture, audio, software/cross-adapter encoder fallback, idle-screen repeat frames, adaptive bitrate, official-client interop.
