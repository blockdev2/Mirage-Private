# Synthetic Stream Media Plan

> Execute inline with superpowers:executing-plans. No subagents.

**Goal:** Decode a synthetic video fixture after SDP/ICE negotiation relayed by
the authorized TeamSpeak server, without screen or microphone access.

**Architecture:** Reuse the two dedicated Rust session owners from stream-check.
A private subprocess controls installed Playwright/Chromium WebRTC peers.
Only server-received offers, answers and ICE reach the receiving browser peer.
Use bounded pipes/queues, hard deadlines, redacted Debug and explicit cleanup.

**Spec:** `docs/persistent-chat-and-streaming-research.md`, streaming gates 2-4.
The user authorized proceeding; official central groups remain deferred.

## Tasks

- [x] Test and implement bounded signaling payload parsing with redacted Debug.
- [x] Add a diagnostic-only Chromium controller and Rust media-check orchestration.
- [x] Run core/CLI tests, formatting/lint, privacy checks and remote experiment.
- [x] Record exact frames/pixel evidence, failures and same-host limitations.

## Constraints and review

No frontend redesign, user session extraction, capture permissions, proprietary
code copying, publishing or direct main update. No SDP/ICE in ordinary logs.
The Python/Chromium dependency is optional for this diagnostic; the product
still has no selected native media/capture stack. Missing browser, invalid pipe
input, rejected commands and timeout must fail visibly and stop owned peers.
No automatic retries that disguise server admission failures.

## Execution ledger

The signaling regression failed before implementation and passed afterward.
The first locked build refused the Tokio feature change; an offline Cargo test
updated only the existing signal-hook-registry dependency edge. Subsequent
locked tests passed: 22 core tests, one CLI timer test, zero doctests. Six
Python framing/privacy regressions passed. Strict core/CLI Clippy, formatting
and build passed, with existing vendored/profile warnings. Cargo audit exited
zero with six unmaintained-crate warnings; no ignores were added.

The remote experiment passed repeatedly, including a final received-video PNG:
five decoded 320x180 frames in the final run, matching fixture pixel, connected WebRTC state,
six relayed ICE candidates, explicit stop and exit zero. The result is scoped
to the same-host media setup documented in the research record.

Review tightened controller cancellation: mark the private pipe unusable before
an operation, clear that state only after a valid complete response, and kill
the subprocess after failure rather than issue another request on a cancelled
read. Cleanup failures are reported separately without raw payloads. The final
locked core/CLI test, strict Clippy and build runs passed after this edit.
The final remote check exited zero with no SDP/ICE markers in its public output.

No subagents, capture, account extraction, publication or main update occurred.
The owner then supplied the native GPU/capture/audio direction. It is recorded
in a separate architecture note so future work does not mistake this Chromium
diagnostic for native capture/encoding implementation.
