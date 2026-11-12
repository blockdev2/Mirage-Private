# Integration record: PR #2, PR #1 and main (2026-11-12)

Owner-authorized integration on branch `integration/pr2-chat-backend`.

## Candidates compared

| Candidate | Revision | Content |
| --- | --- | --- |
| main | `00de7d2` | Native redesign: chat, settings window, DPDFNet noise suppression, censored addresses |
| PR #2 `codex/protocol-reference-review` | `ec5658f` (base `66cf4a9`) | Correlated channel chat in core/bridge/native/browser, bridge safety fixes, tests, protocol research, handoff |
| PR #1 `dependabot/cargo/client/md-5-0.11.0` | `5b401a2` | md-5 0.10.6 -> 0.11.0 |

## Decisions

| Behavior | main | PR #2 | Selected and why |
| --- | --- | --- | --- |
| Receiving text | `MessageEvent(TextMessage)` projected to `TextScope` | `BookEvents` `Event::Message` projected with `MessageTarget` and channel id | PR #2. The vendored `ts-bookkeeping` marks `TextMessage` handled and emits it only as `BookEvents`, so main's path never fired. PR #2's path has unit tests and historical live evidence |
| Chat command | `{op:"chat", text}` | `session_id`, `channel`, `text`, `request_id` | PR #2. Rejects sends queued for a previous session or another channel |
| Chat results | `chat_sent`/`chat_failed`, matched FIFO | `chat_result` by `request_id` | PR #2, plus a new additive `code` field so localized frontends do not parse English text |
| Native chat UI | `ChatLog` with local pending copy and echo dedupe | Simpler received-only list | main's design and `ChatLog`, now resolved by `request_id`, gated on `channel_chat` and the live `session_id` |
| Sender echo | Assumed absent | Observed on beta13.1 | Neither is assumed. Native tolerates both; acceptance is labeled as acceptance, not delivery |
| Replay queue in `wait_result` | Unbounded audio/unknown/file pushes | Same | Fixed here: `trim_pending` applies the same 4096-item drop-oldest policy after every iteration; regression test added. No blocking backpressure |
| Unknown command logging | Raw content at debug | Same | Fixed here: name and byte length only; embedded bridge defaults to `warn` |
| md-5 | 0.10 | — | 0.11 applied on the integrated tree. PR #1's branch predates main's frontend work and would revert it; only the manifest/lock change was taken. Known-vector test passes |

Settings, DPDFNet, address censoring and the rest of main's frontend were kept
unchanged. PR #2's bridge safety (empty UI root, link timeouts), timer-resolution
guard, CI, scripts and documentation were kept.

## Checks on the integrated tree

Windows/MSVC, Rust 1.99.0, Node 22, CMake 3.31 (Visual Studio build tools) with
`CMAKE_POLICY_VERSION_MINIMUM=3.5`:

- `cargo test --locked --workspace`: 100 tests passed, 0 failed.
- `cargo build --locked --workspace`: passed.
- `cargo fmt ... -p ts6-desktop -- --check`: passed.
- `cargo clippy --locked --workspace --all-targets --no-deps -- -D warnings`: passed (vendored crates still emit compiler warnings).
- `python scripts/check_repository.py`: 0 findings. `git diff --check`: clean.
- UI: `npm ci --ignore-scripts`, `npm run build`, `npm audit`: 0 vulnerabilities.
- `scripts/smoke_bridge.py`: HTTP assets, six Host/Origin cases, refresh and oversized-input rejection passed.
- `cargo audit` and Gitleaks were not installed locally; they run in CI on the PR.

## Not verified

- No live TeamSpeak server test was run on this integrated revision
  (`check_channel_chat.py`, `check_chat_browser.py`, `check_chat_delivery.py`).
- Native chat was not exercised interactively against a server: failed sends,
  reconnects, channel changes and shutdown remain to be checked by hand.
- DPDFNet: model checksum against upstream, CPU/memory cost and audio quality
  were not measured. Loading failure falls back to RNNoise by design.
- Native link channels were unbounded at merge time; bounded in the follow-up
  below.

## Migration notes for contributors

- Native and browser senders must include `session_id`, `channel` (decimal
  string) and `request_id`; the text-only `chat` command no longer exists.
- `chat_sent`/`chat_failed` events are gone; use `chat_result.code`.
- `ts6_core::TextScope` is gone; use `TextMessage::target` and `channel_id`.
- Rollback: revert the integration merge commit on main with `git revert -m 1`.

## Follow-up verification (same day, after merge `dcf37b3`)

Tooling: Gitleaks 8.30.1 (release checksum verified), cargo-audit, Playwright
1.49.1 with its Chromium in an isolated virtual environment.

- Gitleaks with redaction: no leaks in all history (14 commits) or in the
  `git archive` of the published tree.
- `cargo audit --file client/Cargo.lock`: 0 vulnerabilities; six
  unmaintained-crate warnings (audiopus_sys, pin-utils, ttf-parser, zbus_names,
  zvariant, zvariant_derive), unchanged, no ignores added.
- DPDFNet: the committed `dpdfnet2_48khz_hr.onnx` SHA-256 equals the upstream
  Hugging Face LFS object for `Ceva-IP/DPDFNet` (`7f0575a5...91dc14b`), whose
  model card declares Apache-2.0. Release build, synthetic noise, one machine:
  136 ms load, 60 s of audio in 7.9 s (about 13% of one core), worst 20 ms frame
  6 ms, about 54 MB peak working set for the measuring process. Audio quality
  and debug-build cost were not assessed.
- Native link queues: events now use a 4096-item queue that drops on overflow
  without blocking the socket reader and then requests a refresh; UI commands
  use a 256-item queue with `try_send`. Two regression tests added.

### Live checks

Official TeamSpeak Server 6.0.0-beta13.1 (build 1790080330, win-x64 release
asset, SHA-256 verified), started for these tests on 127.0.0.1 voice port
19987 and file transfer port 30133. Query, metrics and S3 stayed at their
disabled defaults; WebRTC was disabled. Test identities lived under the ignored
`.audit-local`. No microphone was enabled. The server was stopped afterwards
and its data directory deleted.

1. `check_channel_chat.py`: passed (round trip, one sender echo, Unicode and
   escaping, stale/empty/large/offline and previous-session rejection).
2. `check_chat_delivery.py` immediately afterwards: failed with
   `ConnectFailedBanned`. The server's connection flood protection had banned
   127.0.0.1 after the bridge suite's connections. This explains the
   back-to-back failure PR #2 recorded as unexplained. After a server restart
   it passed: channel and direct reception, both sender echoes.
3. `check_chat_browser.py`: passed (rendered round trip, literal markup, Enter,
   oversize draft retained, reload resynchronization, 0 page errors).

Run suites against a freshly started server, or wait out the ban.

### teamspeak-go comparison (pinned `e7fd092`)

teamspeak-go treats `ConnectFailedBanned` as terminal and disconnects without
retrying. Mirage's vendored library reconnects only after timeouts, and the
desktop shows a ban message, so the behavior agrees. No code was copied.

### Still not verified

The native desktop chat was not driven automatically (no screen-control tool
was available in this session); it was built and opened for manual testing.
Real microphone/speaker calls and long-running load remain open.
