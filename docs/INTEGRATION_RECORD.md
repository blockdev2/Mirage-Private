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
- Native link channels (`std::sync::mpsc`) remain unbounded; see
  BACKEND_HANDOFF.md. Not changed here to avoid UI/network deadlock risk.

## Migration notes for contributors

- Native and browser senders must include `session_id`, `channel` (decimal
  string) and `request_id`; the text-only `chat` command no longer exists.
- `chat_sent`/`chat_failed` events are gone; use `chat_result.code`.
- `ts6_core::TextScope` is gone; use `TextMessage::target` and `channel_id`.
- Rollback: revert the integration merge commit on main with `git revert -m 1`.
