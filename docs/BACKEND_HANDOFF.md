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
