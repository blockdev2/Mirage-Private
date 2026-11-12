# Backend contribution handoff

Date: 2026-11-12. Proposed in PR #2 for owner and frontend review; not merged
into main. This records our team's backend contribution and the interface work
needed to exercise it. Local assignment notes and private test artifacts are
excluded from the proposal.

## Included work

| Area | Changes | Review focus |
| --- | --- | --- |
| Chat core | Project bookkeeping message events into text items in all four read paths; preserve scope, sender and channel context; bound replay batches | `client/crates/ts6-core/src/session.rs`; event ordering and queue limits |
| Chat bridge | Capability advertisement, structured chat events/results, stale session/channel rejection, input limits and refresh after reconnect | `client/crates/ts6-bridge/src/{lib,protocol}.rs`; wire compatibility and session ownership |
| Chat adapters | Mirror the wire model, render received events only, cap history, correlate results and handle drafts/errors | Native `app.rs`, `protocol.rs`, `strings.rs`; browser `App.tsx`, `ChannelView.tsx`, `types.ts`, `useBridge.ts`, chat CSS |
| Local bridge safety | Embedded native bridge has no static-file fallback; empty asset root cannot expose the working directory | Embedded bridge regression and standalone HTTP smoke test |
| Native link | Bound TCP and stalled handshake I/O; restore short command-polling read timeout | Does not claim a total handshake deadline against a continuously trickling peer |
| Synthetic diagnostic | Scoped Windows timer-resolution guard restores its request after tone generation | CLI diagnostic only; real microphone timing remains device-driven |
| Build and CI | Native crate formatting, pinned dependency cache, main-push/PR trigger policy, private repository metadata | Cache changes are not a measured hosted-CI speedup |
| Documentation | Current architecture, compatibility evidence, build guidance, channel contract, protocol research and PR-only integration | Distinguish historical observations from current checks |

The native frontend framework and existing design were already present in the
private base. This contribution extends its chat adapter and link safety. The
frontend contributor should review the listed adapter files for overlap with
unpublished work; the owner can select or reconcile the best implementation.
Do not interpret this PR as authority to replace another contributor's design.

## Fresh checks on the publication worktree

The commands below passed. Cargo used a reusable ignored target directory;
paths and raw output are intentionally omitted from this shared record.

- `python scripts/check_repository.py`.
- From `client`: `./scripts/build.ps1 test --locked --workspace`: 77 tests.
- From `client`: `./scripts/build.ps1 build --locked --workspace`.
- From `client`: `cargo fmt -p ts6-core -p ts6-audio -p ts6-input -p ts6-cli -p ts6-bridge -p ts6-desktop -- --check`.
- From `client`: `cargo clippy --locked --workspace --all-targets --no-deps -- -D warnings`, with the documented CMake policy environment.
- From `client/apps/ui`: `npm ci --ignore-scripts`, `npm run build`, `npm audit`: zero reported npm vulnerabilities.
- `cargo audit --file client/Cargo.lock`: zero reported vulnerabilities; six unmaintained-dependency warnings remain. No new advisory ignores were added.
- `git diff --check`; staged source and new-commit Gitleaks checks are part of the publication procedure.

Three live checks each used a separately started/stopped isolated official
TeamSpeak Server 6.0.0-beta13.1 instance on IPv4 loopback port 19987. Query,
metrics and S3 interfaces stayed disabled. Each test used its own ignored profile
or identity; no microphone was enabled. The order was bridge wire, rendered
browser, then CLI, with a server restart between suites:

1. `scripts/check_channel_chat.py`: both directions, one sender echo, Unicode
   and escaping, stale channel/session, empty/large content and offline rejection.
2. `scripts/check_chat_browser.py`: two Chromium pages, button/Enter submission,
   literal markup, oversized draft retention, and sending after page reload.
3. `scripts/check_chat_delivery.py`: independent channel/direct reception and
   sender echoes; the snapshot reported beta13.1 build 1790080330.

Use each helper's `--binary` argument and `--server-port 19987` (or `--port`
for the CLI helper). Their docstrings describe runtime requirements. Test
processes were supervised and stopped; private profiles/captures are not part
of this PR. These are interoperability samples, not universal TS6 certification.

## Failure found while preparing publication

The HTTP/WebSocket smoke test assumed every frame immediately following a
refresh was a status response. A metadata-only trace reproduced `status` then
`audio`: periodic counters can interleave with the reply. The test now waits
for the required status under a three-second deadline while validating other
permitted event types. Oversized-input rejection must still close the connection
or send a close frame under its own deadline; already queued events do not
count as rejection. Two consecutive reruns passed, including all six Host/Origin
cases and the static-asset check. No production access limits were weakened.

The older combined back-to-back CLI failure remains unexplained. The isolated
recheck passes, but reconnect/load/flood behavior still needs investigation.
Native chat rendering/interaction, real acoustic calls, long-duration load and
other official deployments were not freshly verified by this publication pass.

## Exploratory build measurements retained for future work

Earlier local runs on 32 logical CPUs with about 31 GiB usable memory observed
approximately 90 seconds at 8 jobs, 61 at 16, 55 and 69 at 24, and 58 and 62 at
32. Cache warmth and background load were not standardized; these are exploratory
samples, not a reproducible throughput benchmark or proof of an optimal job
count. They do not justify hardcoding 24 or 32 jobs for every developer. Cargo's
normal available-CPU selection remains in use; temporary `--jobs N` overrides
and local `--timings` reports are documented in DEVELOPMENT.md. Do not publish
raw timing captures containing workstation paths.

## Integration boundaries

The research commit remains independently reviewable in the same PR history.
Existing dependency PR #1 is not merged here. Runtime code and frontend adapter
changes should be reviewed together because their command/event schema is
shared. Resolve overlaps on an owner-authorized integration branch and rerun
checks on that combined result. Never update main directly.

## Concurrent main advancement observed during publication

While the contribution branch was being verified, remote main advanced from
`66cf4a9fa875a8d41e893e7382fb0bdd01e100dd` to
`79469dfce780366f4953ca10b90a4d79db8acc3d` (chat sending, redesigned settings,
and DPDFNet noise suppression). Our publication used only a normal push to its
topic branch; it did not update main. The tests above apply to our contribution
based on 66cf4a9, not to an integration with the newer main revision.

A read-only comparison identified concrete semantic overlaps:

- Core message projection: both revisions change session chat events; the newer
  main introduces TextScope while this branch preserves MessageTarget and
  captured channel context. Reconcile the model and all consumers together.
- Wire commands: newer main accepts chat with text alone; this branch requires
  session/channel/request correlation. Dropping those checks could route queued
  text to a replacement session or a channel the user did not select.
- Wire events: newer main uses scope/from_id/from_name and chat_sent/chat_failed;
  this branch uses channel/sender/timestamp plus correlated chat_result. Native
  and browser mirrors must use the selected contract consistently.
- Confirmation semantics: comments on the newer branch differ about sender
  echoes. Our isolated beta13.1 tests observed sender echoes and independent
  receiver delivery; command acceptance alone must not be called delivery.
- Native chat/settings, strings, manifests/lockfile, audio dependencies and
  compatibility documentation overlap. This review did not execute the new
  main's code, validate its model binary, or assess its license/dependencies.

These observations identify review points, not a decision that one contribution
should replace the other. No automatic merge/rebase or cherry-pick was done.
The owner should compare both pinned revisions, preserve the frontend work and
security properties, and test a separately authorized combined implementation.

## Follow-up review gaps (2026-11-12)

A subsequent read-only review confirmed that all 31 original pending paths are
present in this PR. The original checkout differs only in AGENTS.md, README.md
and the smoke test, where this PR intentionally preserves shared policy/research
and the tested asynchronous-frame correction. The publication worktree was clean
at the start of this review.
GitHub reports this PR as draft and mergeable=false/dirty against the newer main;
no check runs were returned for head bfabae4 at review time. Earlier local
verification is not equivalent to green hosted checks on a combined result.
Dependency PR #1 moved to head 5b401a261adf87f9fa0b3c4b5a5a1f20c1aad94f;
its older check results must not be applied to that new head.

Two backend hardening gaps remain visible in source:

- `Session::wait_result` bounds bookkeeping batches, but queues audio, unknown
  commands and the raw-message fallback using direct push_back calls. Its
  15-second deadline bounds waiting time, not queue length. The general buffer
  path's 4096-item limit does not cover every insertion in this method. Apply
  one explicit overflow policy across these paths and test a silent command
  reply with continuing voice/notifications. Memory growth under that scenario
  was not measured in this review; a finite queue bound is not established.
- The bridge logs both name and raw content for UnknownCommand at debug level.
  A future TS6 extension could carry signalling details or credential-bearing
  transfer data there. Prefer command names, sizes and an allowlisted sanitized
  schema; keep full protocol dumps an explicit private diagnostic. No exposed
  credential was discovered or printed by this review.

Native link channels also use unbounded std::sync::mpsc queues. Stress a stalled
UI and a busy server before claiming bounded end-to-end memory use. Address
these as focused regression-backed changes, preserving fair duplex polling and
session ownership. These are source findings and proposed tests, not newly
executed load experiments or permission to weaken existing checks.

Functional verification still needed includes native rendered chat interaction,
real microphone/speaker/PTT calls, long-running/reconnect/flood behavior,
authorized hosted/official-client interoperability, and the planned Go-peer
matrix. Persistent group chat, direct-message UI, WebRTC media, S3 integration
and release packaging remain outside the implemented feature set of this branch.
Do not infer their state on a future combined branch from this record alone.
