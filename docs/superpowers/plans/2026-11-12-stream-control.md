# Stream Control Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans inline. No subagents.

**Goal:** Reproduce stream announcement, viewer request/refusal and explicit
termination between two Rust clients on an authorized server, without media.

**Architecture:** Each diagnostic session owns a dedicated thread/runtime and
continuously polls the existing `!Send` session with a bounded command queue.
Parse only bounded control metadata in the core; never log SDP or ICE.

**Tech Stack:** Existing Rust, Tokio, tsproto command parser; no new dependency.

**Spec:** [persistent chat and streaming research](../../persistent-chat-and-streaming-research.md),
streaming proposal. The owner approved proceeding with server streaming and
deferred official central groups.

## Global constraints

- Preserve existing pending changes and use the already isolated topic worktree.
- No direct main update, merge, capture, microphone or proprietary code copying.
- Keep endpoints, identities and raw notifications private.
- Hard deadlines, bounded queues and one event owner per session.
- This is control-plane validation, not a working screen-sharing feature.

## Review focus

- Missing, duplicate, invalid or oversized fields must not identify a wrong peer.
- Viewer requests must match the current stream and expected diagnostic peer.
- Refusal must include an empty offer; success must not imply media delivery.
- Disconnect and deadline failures must stop both owned diagnostic sessions.
- Unknown stream notifications must not expose their contents through logging.

## Tasks

- [x] Add parser-backed failing tests in `client/crates/ts6-core/src/streams.rs`
  for control notices, malformed/batched input, limits and escaped identifiers.
  Implement `ControlNotice::parse` using the installed TS command parser.
- [x] Add `stream-check` to the CLI with bounded owner actors. Exercise setup,
  independent stream discovery, viewer request, refusal and explicit stop.
- [x] Run core/CLI tests, formatting, strict Clippy and the manual remote check.
  Inspect the final diff and document exact evidence and remaining media gaps.

## Execution ledger

The worktree already contains reviewed live-test documentation and a Python
harness correction from earlier work. Preserve those changes. No publication
or consolidation of competing team branches is part of this implementation.

Ruling: use a refusal rather than an accepted placeholder SDP offer. A fake
offer would not prove a WebRTC connection and could mislead an official viewer.

Ruling: keep one notice per record and reject batches in this initial parser.
This avoids ambiguous peer association; multi-record support remains future
work and is not advertised as a general stream registry.

Verification completed on the final source:

- `cargo test --locked --package ts6-core --package ts6-cli`: 20 core tests and
  one CLI timer test passed; zero doctests. Four stream parser tests are included.
- `cargo fmt --package ts6-core --package ts6-cli -- --check`: passed before the
  final cleanup-only edit; formatter rerun after that edit.
- `cargo build --locked --package ts6-cli`: passed.
- `cargo clippy --locked --package ts6-core --package ts6-cli --all-targets --no-deps -- -D warnings`: passed.
- `python -m unittest discover -s scripts -p 'test_*.py'`: three tests passed.
- `python scripts/check_repository.py`: 258 UTF-8 publication candidates, zero
  policy findings at that check. `git diff --check` passed.
- Final remote `stream-check` binary: exit zero; announcement, info, viewer
  request, refusal and explicit stop independently observed. No media sent.
- Missing explicit channel: exit one as expected. Nonexistent remote channel:
  controlled startup failure. Neither produced a success summary.

The success summary is emitted only after both owner threads complete cleanup.
Manual diff review checked bounded queues, deadlines even with ready events,
request/stream/peer matching, startup cleanup and log privacy. No fresh reviewer
agent was used because the owner prohibited subagents. Existing vendored and
unmatched profile warnings remain. Full workspace/UI checks and a new secret
scan were not run; no dependency changed and nothing was committed or pushed.

The owner also confirmed a separate channel-chat fixture was visible in the
official TS6 client. See the live-validation record. That observation does not
validate persistent central group chat, which remains deferred.
