# Native Capture Probe Plan

> Execute inline with superpowers:executing-plans. No subagents.

**Goal:** Capture only an owned synthetic Win32 window through WGC, copy frames
into an owned D3D11 texture, and validate one final readback after stopping.

**Architecture:** A standalone Windows-only `recon/probe-capture` crate has its
own lockfile and pinned capture dependency. The fixture owns its HWND; no window
enumeration, user display selection, network, audio or account is involved.
The callback keeps at most one owned texture, recreating it on fixture resize.
The process controller enforces an outer deadline and stops only its own probe.

**Spec:** `docs/native-streaming-architecture.md`, capture gate. The owner
authorized this stage by asking to continue after the native-pipeline proposal.

## Tasks

- [x] Write failing layout/pixel validation tests and implement checked offsets.
- [x] Create the owned fixture, WGC callback and bounded GPU texture slot.
- [x] Run the probe, verify resize and final pixels, and audit its lockfile.
- [x] Review unsafe lifetimes, cleanup and evidence limits; document results.

## Review constraints

No arbitrary HWND or monitor input, desktop capture, screen-image export or
microphone. Keep capture-pool frames inside the callback; do not retain them as
owned frames. Distinguish GPU copy submission cost from GPU completion time.
One final staging readback is validation, not proof of zero-copy streaming.
No native H.264/WebRTC integration is claimed by a passing capture-only probe.
Preserve the existing dirty worktree and team PR policy; no main update.

## Execution ledger

The bounds/color tests failed before implementation, then passed. An interactive
window-eligibility test failed with the initial tool-window fixture and passed
after correction. The manual integration wrapper also failed before the native
creation issue was resolved. See the probe README for the full failed/successful
sequence; queue pumping alone did not resolve `0x80070057`.

A successful run received forty frames and allocated two owned textures across
the controlled resize. The final owned texture survived capture shutdown and
matched the fixture color in one staging readback. The measured mean callback
copy-submission cost was about 10.7 microseconds in that run; it includes host
work and is not GPU completion time, encoder latency or a general benchmark.

Probe unit tests, explicit interactive test, build, strict Clippy and lockfile
audit passed. Its forty-three dependency audit reported no warnings. The final
format/test/lint/manual checks after diagnostic error-detail edits also passed:
four tests including the explicit interactive test, locked build, strict
Clippy, formatting, and the thirty-second manual wrapper. The final run captured
forty frames and validated one final readback, with a mean copy-submission cost
of about 14.8 microseconds. This difference from the first run reinforces that
these are observations, not a GPU latency guarantee. Repository policy scanned
272 publication candidates with zero findings; no new secret scan or full
application workspace test was performed in this capture-only stage. The main
application audit retained six pre-existing unmaintained-crate warnings.

No native encoder, WebRTC replacement, user-window capture or publication
occurred. The final process inventory found no remaining native probe.
