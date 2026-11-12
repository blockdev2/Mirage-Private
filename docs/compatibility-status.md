# TeamSpeak 6 compatibility status

Date: 2026-11-12. This client is intended to connect directly to official TeamSpeak 6 servers. It includes a native eframe/egui frontend and a separate React browser frontend. It is not a new server implementation or a complete replacement for the official client.

## Fresh verification

For later checks of the integrated backend against local and owner-authorized
remote beta13.1 servers, see [integrated live validation](live-validation-integrated.md).
That record includes independently received chat, synthetic voice in both
connection orders, and unresolved local connection refusals. The historical
measurements below retain their original scope.

The local server identified itself through the client snapshot as **6.0.0-beta13.1**. A fresh CLI handshake succeeded, returned one channel and included the connecting client. Version reporting establishes the responding software's self-identification, not an independent binary authenticity proof.

The rebuilt bridge was launched on a separate local port using its own ignored profile. The actual UI JSON protocol successfully exercised connect, snapshot, nickname change, input/output mute and unmute, refresh, and disconnect against that server. No microphone or playback source was enabled. `scripts/check_ts6_bridge.py` reproduces this integration and stops only its own bridge process. The ordinary smoke test also passed HTTP assets, permitted/hostile WebSocket access, and oversized message rejection. These are service/wire tests, not a rendered-browser interaction or accessibility audit.

The sender-correlated live voice test passed with a first positive receiver report at **0.171 seconds** after sender readiness. This uses synthetic Opus audio and is not an acoustic microphone or latency certification. Only the tested local server release/environment is covered; arbitrary TS6 servers, permissions, network conditions, and future releases remain unverified.

An offline test interleaving two independent encoded voices reproduced decoder-state contamination when a single decoder was shared. `PeerDecoders` now keeps independent Opus state per sender in the CLI receiver. The fixed test compares each interleaved output against a dedicated reference decoder; **34 Rust tests passed**. The playback mixer already uses the upstream per-peer audio handler; no claim is made that this CLI fix changed production playback mixing.

UI TypeScript/production build and npm audit passed (zero reported UI vulnerabilities). Repository policy and HTTP/WebSocket smoke checks passed. The Rust cryptography advisory was subsequently resolved through the migration and verification recorded in [PUBLICATION.md](PUBLICATION.md).

## Feature matrix

| Feature | Current evidence / remaining gap |
| --- | --- |
| Official TS6 server login and identity | Fresh local beta13.1 handshake/snapshot verified |
| Channels and client state | Snapshot and UI-wire refresh verified; broad permission scenarios not repeated |
| Nickname and mute controls | Fresh bridge-to-server integration verified |
| Voice transport | Fresh synthetic sender-correlated reception verified |
| Multiple voice senders | Independent decoder state regression fixed; sustained multi-user acoustic calls not verified |
| Microphone, speakers and PTT | Implemented, partly covered by unit/historical tests; fresh acoustic end-to-end validation remains open |
| Native desktop frontend | Workspace build and 24 desktop unit tests pass; rendered/interactive UI and live-server flows were not freshly verified in this continuation |
| Browser UI | Builds; HTTP/wire integration passes; full rendered interaction/accessibility validation remains open |
| Embedded bridge static-file boundary | Regression test returns 404 for a file under an empty UI root; standalone static asset smoke test passes |
| Text chat | Correlated channel chat (`session_id`/`channel`/`request_id` -> `chat_result` with `code`) in core, bridge, native and browser frontends. Historical evidence only: PR #2 verified CLI channel/direct delivery, sender echoes and the rendered browser flow on beta13.1; separately, a message sent by the native client was manually observed in the official client on another TS6 server that did not echo it back. Echo behavior therefore varies; the native UI shows its own copy and deduplicates an echo. Command acceptance is not proof of delivery. Not re-verified live on the integrated revision |
| Video/screensharing and files | Not implemented |
| Installer/release packaging | Not implemented |
| Production security readiness | Experimental; current dependency/history remediation and remaining reporting/maintenance work are documented in PUBLICATION.md |

## Research and chat uncertainty

Primary references checked during this continuation:

- Official [server releases](https://github.com/teamspeak/teamspeak6-server/releases) list beta13.1 and version-specific changes. Future compatibility must be verified rather than assumed.
- Official [server configuration](https://github.com/teamspeak/teamspeak6-server/blob/main/CONFIG.md) documents UDP voice and query controls. No administrative interfaces or credentials are required by the new bridge regression.
- A [community issue in the official repository](https://github.com/teamspeak/teamspeak6-server/issues/110) distinguishes voice-channel chat from persistent Matrix-backed group chats. Its description is user-reported, not an authoritative protocol specification. It is a reason to investigate the distinction, not proof of a new API.
- The [Matrix interoperability discussion](https://github.com/teamspeak/teamspeak6-server/issues/31) concerns a separate service integration. The historical failure of `sendtextmessage` in this checkout does not establish that every TS6 text workflow is Matrix-only.

At that stage, the UI described chat as unimplemented rather than asserting a universal external-Matrix requirement. The later receive-path correction and channel UI integration are recorded below. Before implementing chat, reproduce voice-channel text delivery with controlled peers and compare the official client's signaling using authorized observation. Separately establish the supported authentication/service requirements for persistent group chats. Do not invent endpoints, tokens, homeserver support, or delivery guarantees.

## Repeatable checks

From the repository root, after building the binaries and UI:

```powershell
python scripts/smoke_bridge.py --binary .audit-local/target/debug/ts6-bridge.exe
python scripts/check_ts6_bridge.py --binary .audit-local/target/debug/ts6-bridge.exe
python scripts/check_voice_progress.py --binary .audit-local/target/debug/ts6-cli.exe
python scripts/check_repository.py
```

The last two server integration helpers require the existing trusted local server on port 9987 and create ignored test identities. They are manual tests, not offline CI. Keep .audit-local private and out of releases. Test reports are application-level evidence, not packet captures or official-client equivalence.

## Fresh voice-channel text experiment

With a continuously polling independent listener, three `sendtextmessage` variants
were tested: channel mode with target omitted, target set to the current channel
ID (1 in this test snapshot), and legacy target 0. All command processes exited
successfully; the independent receiver observed none of the three unique fixture
messages during its 20-second observation interval and exited successfully.
This reproduces acceptance without observed delivery for the tested setup. It
does not identify the underlying TS6 text protocol or establish that every server
requires Matrix. No UI chat was enabled on the strength of command acceptance.
Private raw evidence is under the ignored `.audit-local/chat-compatibility/`.

## Isolated beta13.1 loopback retest (2026-11-12)

The official [server releases](https://github.com/teamspeak/teamspeak6-server/releases)
listed 6.0.0-beta13.1 as the latest release. The Windows x64 release archive's
SHA-256 matched the digest published by the GitHub Releases API. The server ran
from a fresh ignored directory with its voice and file-transfer listeners bound
to `127.0.0.1`; HTTP/SSH/HTTPS query, metrics, and S3 transfer were disabled.
The clients used distinct ignored identities and a process-only
`ALLOW_PRIVATE_TARGETS=1` override for loopback.

In order, `ts6-cli probe` connected and reported version
`6.0.0-beta13.1 [Build: 1790080330]`, the default channel, and the local client;
two-client channel and direct-client `ts6-cli chat` commands then produced no
sender echo within three seconds and no matching message event at the receiver.
No UI chat was involved. This is another result of command acceptance without
observed delivery; it does not establish a server-wide protocol limitation.

For synthetic voice, a separate receiver ran `ts6-cli audio-recv` while a second
identity ran `ts6-cli audio-send` for three seconds. Both server audio
capabilities were reported as enabled. The first run queued and decoded 98
frames; a separate Tokio timer probe reproduced 98 ticks in three seconds at
the default Windows timer resolution. Requesting a temporary 1 ms resolution
produced 151 ticks. The CLI now scopes a matched Windows timer request to this
manual diagnostic, and a regression test checks a minimum of 135 frames in
three seconds. The beta13.1 retest queued and decoded 151 packets with nonzero
RMS (0.3546) and peak (0.5546). This verifies synthetic transport and the test
cadence in that run; it does not validate a microphone, speakers, acoustic
quality, a rendered frontend, or a long-running call. The timer setting can
increase wakeups and power use; its platform scope is documented in
[development notes](DEVELOPMENT.md).

The isolated server process was stopped and its test ports were released. The
temporary server, generated identities, and private output files remain under
the ignored `.audit-local` tree. Its ACL grants access only to the current user,
SYSTEM, and local Administrators; these files are not tracked and must not be
published.

## Desktop link recovery verification (2026-11-12)

A loopback TCP peer that accepted connections without answering the WebSocket handshake reproduced an indefinitely stalled desktop link. The regression test failed before the fix, returning only when the test peer closed after four seconds. The desktop link now uses two-second TCP connect and per-operation handshake read/write timeouts. A second loopback test verifies a successful handshake and restoration of 40 ms read polling. Both pass as part of the 72-test workspace suite. These are local transport tests, not rendered UI or TeamSpeak server integration tests. A continuously trickling peer can still exceed two seconds overall; no absolute handshake deadline is claimed.

## Chat receive-path correction (2026-11-12)

The earlier absence of observed messages was not evidence of absent server delivery.
The vendored bookkeeping library consumes `notifytextmessage` and emits
`BookEvents::Message`; the core discarded those events as state changes while
waiting for the already-consumed raw `MessageEvent::TextMessage` variant.
The core now projects every bookkeeping message into a text item in the sync,
command-result wait, next-item, and duplex polling paths, preserving sender,
content, order, and self-echo detection without changing vendored code.
A regression with two bookkeeping messages failed with zero text items before
the correction and passes afterward, including Unicode and escaped characters.

The isolated loopback beta13.1 server's persisted voice port was 19987, not the
manual harness default 9987. An initial integration attempt failed at the
handshake against the wrong port; inspecting only listener metadata identified
that setup error. With the correct port, two separate identities verified
channel and direct-client reception and sender echoes for both messages.
The server and diagnostic children were stopped afterward. No microphone,
query administration, account login, or official-client capture was used.

Reproduce against a separately started trusted loopback server:

```powershell
python scripts/check_chat_delivery.py --binary .audit-local/full-audit-target/debug/ts6-cli.exe --port 19987
```

The helper uses ignored identities and reports only sanitized test results.
It does not start a server or reset its database. These results establish CLI
channel/direct transport for the tested release; server-wide broadcasts,
permissions, offline history, persistent group chat, and frontend integration
remain separate work. Frontend sending remained disabled at that milestone; the subsequent channel UI integration is documented below.

## Channel frontend integration (2026-11-12)

Both frontends now implement current-channel sending and bounded in-memory
history. A new two-bridge regression verified independent receiver delivery,
a single sender echo, Unicode/escaping, and rejection of stale channel, empty,
oversized, disconnected, and previous-session sends against beta13.1.

Rendered Chromium testing with two independent bridges verified button and
Enter submission, literal markup rendering, draft clearing on acceptance,
oversized-draft retention, and page-reload resynchronization. The initial reload
test failed because a new WebSocket received no active-session snapshot; the
bridge now requests a refresh after subscription and publishes authoritative
status with its snapshot. The repeated browser test passed without page errors.
Screenshots were inspected at 1440x1000 and 900x700. Browser plugin was unavailable;
testing used the installed Python Playwright runtime and Chromium, without new
dependencies, through `scripts/check_chat_browser.py`. Audio stayed disabled. Native protocol tests/build pass, but the
native chat surface was not driven in a rendered live-server interaction.

See [channel chat](channel-chat.md) for the protocol and remaining limitations.
