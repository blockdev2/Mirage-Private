# Development, configuration, and testing

Use the [root README](../README.md) for setup and [architecture](architecture.md) before changing session polling. Work from a clone with the committed vendored source and lockfiles. No external reference clones or proprietary artifacts are needed for offline builds.

## Offline checks

From the repository root in PowerShell:

```powershell
python scripts/check_repository.py
cd client
./scripts/build.ps1 build --locked --workspace
./scripts/build.ps1 test --locked --workspace
cargo fmt -p ts6-core -p ts6-audio -p ts6-input -p ts6-cli -p ts6-bridge -p ts6-desktop -- --check
cargo clippy --locked --workspace --all-targets --no-deps -- -D warnings
cd apps/ui
npm ci --ignore-scripts
npm run build
npm audit
```

The build helper sets `CMAKE_POLICY_VERSION_MINIMUM=3.5` for bundled libopus under CMake 4. Direct Cargo builds need the same environment setting. Select the six first-party packages explicitly with `cargo fmt -p ...`: `--all` also traverses local path dependencies and would reformat the vendored workspace. Clippy may emit upstream deprecation warnings; inspect the actual result rather than hiding warnings. The current first-party Clippy baseline is recorded in [AUDIT.md](AUDIT.md).

Rust tests cover codec/meter/resampling/mixing, state/configuration, key parsing/gating, bridge serialization, access policy, desktop components, and safety regressions. They do not prove real microphone quality, voice latency, server permissions, or cross-platform operation. After building the bridge, run `python scripts/smoke_bridge.py --binary client/target/debug/ts6-bridge.exe` from the repository root. This launches and stops only its own local bridge, checks HTTP and WebSocket boundaries, and sends no server-connect or audio commands.

The browser UI build runs TypeScript checking. There is no established automated end-to-end interaction suite for either frontend.

To run the native desktop application from `client`, use `cargo run --locked -p ts6-desktop`. It embeds the bridge and does not require the browser UI build. Its WebSocket link limits TCP connection attempts and individual handshake reads/writes to two seconds, then restores 40 ms read polling for UI commands. This handles an unresponsive service occupying the bridge port; it is not an overall deadline against a peer that continually sends partial data. To run the browser UI, build `apps/ui/dist` and start the standalone `ts6-bridge` as described below.

For dependency security, install the audited tool version separately and run:

```powershell
cargo install cargo-audit --version 0.23.0 --locked
cargo audit --file client/Cargo.lock
cargo audit --file recon/probe-ts6/Cargo.lock
```

The historical cryptography advisory was resolved through the migration recorded in [PUBLICATION.md](PUBLICATION.md). Never suppress new audit failures to obtain a passing result. Gitleaks 8.30.1 was used locally with `gitleaks git --redact --log-opts=--all`; run it before publication and scan a nonignored working-tree snapshot as well. Keep reports private; never print secret values. Older Git commits still require owner review.

## Bridge and UI configuration

No `.env` loader exists. Set environment variables in the launching shell. Do not create a credential-bearing `.env.example` for options the program does not read.

| Setting | Default / behavior |
| --- | --- |
| `TS6_BRIDGE_PORT` | 8787; valid environment value takes precedence over `--port` |
| `--port <u16>` | Alternative bridge port; invalid input currently falls back to a default |
| `TS6_UI_DIR` | Standalone bridge asset directory; overrides `--ui`, otherwise uses build-time source-tree `apps/ui/dist` |
| `--ui <directory>` | Standalone bridge static asset path; set it when relocating the binary |
| `ALLOW_PRIVATE_TARGETS=1` | Explicit private/loopback destination override in vendored resolver |
| `RUST_LOG` | Bridge tracing filter; default `info,ts6_bridge=debug` |
| `APPDATA` | First choice for identity directory, suffixed with `ts6client` |
| `XDG_CONFIG_HOME` | Used if APPDATA is absent; suffixed with `ts6client` |
| `HOME` | Final fallback: `.config/ts6client` below HOME; empty HOME falls back to a relative location |

The default private identity is `identity.json` inside that config directory. CLI `--identity <path>` selects a different file. Protect it with a user-only ACL on Windows. Do not share identities across simultaneous clients; the server can reject clones. Keep protected backups. On malformed JSON the client fails instead of replacing the account.

The native desktop embeds a bridge with no static-file fallback. Standalone bridge mode serves static files only from its resolved UI directory; an empty UI path does not fall back to the process working directory.

For UI development, run the bridge and then `npm run dev` under `client/apps/ui`. Vite uses port 5174 and proxies `/ws` to `ws://127.0.0.1:8787`. Use the exact local URL displayed; only HTTP origins on localhost/127.0.0.1 at bridge port or 5174 are allowed. The Vite proxy target is fixed: change it deliberately if testing a different bridge port. Never expose the development server or bridge publicly.

## Diagnostic CLI

From `client`, prefix these commands with `cargo run --locked -p ts6-cli --`:

```text
probe <address> [seconds]
chat <address> <server|channel|@client-id> <text...>
join <address> <channel>
nick <address> <nickname>
raw <address> <command> [key=value ...]
stream <address> [--type 2] [--bitrate 2500] [--seconds 6]
stream-check <address> <private-directory> --channel <test-channel>
stream-media-check <address> <private-directory> --channel <test-channel>
audio-send <address> [seconds]
audio-recv <address> [seconds]
audio-call <address> [seconds] [--tone] [--ptt F13]
devices
ptt-probe <key> [seconds]
```

Inspect `crates/ts6-cli/src/main.rs` for diagnostic-only modes including `duplex-probe`. Common connection options are `--name`, `--identity`, `--channel`, `--channel-password`, `--server-password`, `--local-ip`, and `--verbose`. Audio-call also accepts `--input` and `--output`. Passwords supplied on the command line can be visible to local process inspection; use only isolated test credentials. `--verbose` enables sensitive protocol logging. Never attach raw output to a public issue.

On Windows, the manual `audio-send` diagnostic temporarily requests 1 ms timer resolution so its 20 ms synthetic frames are paced accurately; an RAII guard pairs the request with restoration when generation ends. This does not change the real microphone path, which is driven by the audio device. Windows timer requests can increase wakeups and power use, and before Windows 10 version 2004 the setting is system-wide; run only this bounded diagnostic when needed. See Microsoft's [`timeBeginPeriod` documentation](https://learn.microsoft.com/en-us/windows/win32/api/timeapi/nf-timeapi-timebeginperiod).

Bridge session-only diagnostics: `cargo run --locked -p ts6-bridge -- --probe 127.0.0.1:9987 --seconds 20`. `--no-tone`, `--no-commands`, and `--current-thread` vary that diagnostic. These modes connect to a server and may transmit synthetic audio; ordinary offline verification does not run them.

`stream-check` starts two continuously polled Rust sessions with separate
identities under the supplied private directory. Protect that directory with a
user-only ACL on Windows and keep it ignored. An explicit test channel is
required and verified in each snapshot; verbose protocol logging is rejected.
The check announces a stream, independently observes it, requests its info,
requests viewer admission, observes the publisher's refusal, and explicitly
stops the announcement. It prints a sanitized JSON summary and never captures
the screen or transmits media. The directory argument selects its diagnostic
identities instead of the common `--identity` option. Run only against a server
you control or have permission to test. A failed check may reflect permissions
or connection admission, not a video-codec defect.

`stream-media-check` optionally uses installed Python Playwright/Chromium via
`scripts/stream_webrtc_controller.py`. No runtime is downloaded automatically.
Two private browser peers generate a 320x180 canvas fixture, relay SDP/ICE
through the two Rust TeamSpeak sessions, and require decoded video with a matching
fixture pixel. It uses no screen, microphone, camera, account profile or STUN
service. Both media peers run on this machine; it does not test cross-machine
NAT traversal or the official viewer. A synthetic received-video PNG is written
under `<private-directory>/browser-runtime/received-fixture.png`.

The browser controller's pipe carries SDP/ICE and must not be attached to a
terminal or ordinary logs. The Rust diagnostic prints only a sanitized summary.
Frames are limited to 32 KiB; the stream parser's local input limit is 16 KiB.
A failed or cancelled browser pipe is not reused. The optional script is
resolved relative to the source checkout at build time, so a relocated binary
alone is insufficient; this is not a packaged product feature.

## Manual server/hardware testing

The proprietary server is not distributed. Create an isolated local server using its own version-specific configuration, keep secrets and state under ignored `testbed/`, and restrict query interfaces to the test machine. Never wipe server state as a routine troubleshooting step. Back it up and obtain authorization before any destructive reset.

Use distinct protected identities for peers. Connect the listener before the speaker; historical beta13.1 tests observed zero received packets with the reverse ordering. Wait for the sender to confirm actual transmission before measuring deltas. Reset cumulative counters, exclude abandoned peers, and record the exact interval, server version, order, devices, and result. A tone proves transport/codec behavior, not real acoustic microphone quality.

The legacy `v21-*.ps1` and `bridge-first.ps1` scripts are research helpers, not CI tests. Their receive timeout handling and process cleanup need further work: do not treat a timed-out `ReceiveAsync` as cancelled or start a second concurrent receive. Use them only on an isolated testbed with explicit process supervision. Do not infer a successful test from zero baseline traffic or a script's final exit status alone.

## Troubleshooting

- Native Opus build fails under CMake 4: use `scripts/build.ps1`; confirm MSVC C++ tools and CMake are on PATH.
- Private destination rejected: explicitly enable `ALLOW_PRIVATE_TARGETS=1` only for a trusted local test.
- UI missing: build `apps/ui/dist` and confirm `TS6_UI_DIR`/`--ui` points there.
- WebSocket rejected: use a permitted local HTTP origin and exact bridge Host/port; remove public proxies. Native probes may omit Origin.
- No microphone or unsupported format: enumerate `devices`; inspect capture errors. A selected device does not prove its stream started successfully.
- Channel chat: use the two-peer regression below; command acceptance alone is not proof of receiver delivery. Persistent group-chat integration remains separate.
- PTT invalid/unavailable: transmission is disabled; select a valid Windows binding. Do not remove the guard.

## GitHub administration

Repository files do not configure hosted settings. Before public launch, the owner should:

1. Set the real repository URL and confirm maintainers and a private conduct-reporting channel. No CODEOWNERS is created because no owner identities are verified.
2. Enable and test private vulnerability reporting, secret scanning, push protection, and dependency alerts where available. Review historical token-like samples and personal paths privately; rotate real credentials before deciding whether authorized history remediation is needed.
3. Run the supplied workflows on GitHub and then require their actual check names in a branch ruleset. Restrict direct pushes and force pushes; choose review requirements that the real maintainer team can meet.
4. Require maintainer approval for untrusted workflow runs, keep token permissions read-only, and avoid privileged fork execution. Review dependency and action-pin updates manually.
5. Add practical labels (`bug`, `enhancement`, `documentation`, `maintenance`, `good first issue`); do not put private vulnerabilities in public labels/issues.
6. Resolve known advisories and review all resolved/native dependency licenses before distributing binaries. Define release tags, verified platforms/server versions, and release approval separately; no release workflow is supplied.

The CI includes Windows build/test/lint/format, native capture probe lint/tests, harness unit tests, UI build/audit, repository policy checks, and Rust dependency audits for all three lockfiles. It has no deployment credentials or publishing steps. Gitleaks is a local pre-publication requirement; hosted secret scanning requires administrator setup. Hosted CI has not been executed merely by adding workflow files.

## Live voice progress regression

`python scripts/check_voice_progress.py --binary .audit-local/target/debug/ts6-cli.exe`
checks sender-correlated voice progress against an existing trusted local server.
It transmits a synthetic tone and uses separate ignored test identities; it does
not capture the microphone. This is a manual integration test, not offline CI.
See [late-join investigation](late-join-investigation.md) for the diagnosed sender
polling defect, measurements, and remaining limitations.

## Build performance

Cargo normally chooses its parallel job count from the CPUs available to its
process. The native CMake driver receives Cargo's job limit. Avoid a hardcoded
machine-wide limit: memory pressure and other applications affect the best value.
For an explicit temporary limit, pass `--jobs N` to the Cargo subcommand, for
example `./scripts/build.ps1 build --locked --workspace --jobs 8`. This does not
change global configuration. Use `cargo build --locked --workspace --timings`
to generate a local timing report; reports can contain local paths and must not
be committed. Reuse a target directory for incremental builds and avoid running
multiple Cargo processes against the same directory, which makes them wait on a
lock. The release profile uses thin LTO and one codegen unit; this favors release
optimization over build throughput and remains unchanged.

The GitHub workflow runs for pull requests and pushes to main, avoiding duplicate
push and pull-request runs for the same feature branch. Its SHA-pinned Rust cache
restores dependency artifacts for the client workspace and both standalone
probes. Only successful pushes to main save these caches; pull requests can
restore but cannot populate them. Cargo binaries are excluded from the build
cache; the dependency-audit job caches only the pinned `cargo-audit` binary
(keyed by its version), so it is no longer compiled from source on every run.
npm downloads are cached by `setup-node` from the lockfile. CI uses GitHub-hosted runners and does not consume
the developer machine's CPU. Cache effectiveness depends on the runner and key;
measure hosted runs before claiming a speedup.

## Manual chat transport regression

With a trusted loopback TS6 server already running, use
`python scripts/check_chat_delivery.py --binary client/target/debug/ts6-cli.exe`
from the root. Set `--port` if the server uses a different voice port. The helper
connects independent ignored identities, verifies channel and direct reception
with sender echoes reported separately, and stops only its own diagnostic clients.
It does not start audio or enable frontend chat. Keep the test directory private.

For the frontend wire contract, with a trusted loopback server already running:
`python scripts/check_channel_chat.py --binary .audit-local/full-audit-target/debug/ts6-bridge.exe --server-port 19987`.
This starts two isolated bridges and does not enable audio. Build browser assets
first. It verifies independent delivery, sender echoes, and validation failures.
See [channel chat](channel-chat.md) for the current frontend scope.

For chat attachments and emoji, with a trusted server whose guest group may
upload files: `python scripts/check_chat_attachments.py --binary .audit-local/full-audit-target/debug/ts6-bridge.exe --server 127.0.0.1:19987`
(add `--channel <name>` for a dedicated test channel). Fixtures stay under
ignored `.audit-local/chat-attachments`. Exit code 2 means the server denied
uploads, so the success path was not verified. Unit tests for the harnesses
run with `python -m unittest discover -s scripts -p "test_*.py"`.

## Screen share against a server

`python scripts/check_share_session.py --binary <ts6-bridge.exe> --server-port PORT`
starts two isolated bridges against a trusted loopback server you run: one
shares its primary monitor, the other watches it. It passes when the sharer is
`live` and the viewer is `playing`. It needs a hardware H.264 encoder and a
monitor, and does not involve the official client.

## Driving the native window without a person

Debug builds of `ts6-desktop` run a scripted plan from `MIRAGE_DEV_PLAN` (steps
separated by `;`): `wait:SECS`, `connect:HOST:PORT`, `chat:TEXT`,
`attach:PATH`, `watch` (first other client in the room), `token:KEY` (redeem a privilege key), `admin` (open the administration window), `rclick:X,Y`, `type:TEXT`, `hover:X,Y`, `click:X,Y`, `key:Escape`, `shot:NAME`, `quit`.
Coordinates are egui points (screenshot pixels divided by the display scale,
e.g. 1.25 at 125%). To see a share, run a second bridge that shares (a small
script on `check_channel_chat.Peer` with `share_start`) and use `watch`.
`MIRAGE_DEV_OUT` is the folder for the screenshots. Use an isolated `APPDATA`
so the real profile is untouched, and only a loopback test server.
The code is compiled out of release builds.

## Server administration check

`python scripts/check_admin.py --binary <ts6-bridge.exe> --server-port PORT --token-file key.txt`
needs a throwaway TS6 server with a fresh database (start it from a copy of
the server archive in an ignored folder; its first `ServerAdmin` privilege key
is in its log) and drives two bridges: a plain client is refused, the key is
redeemed, groups and permissions are listed, a channel is created, edited and
deleted, a client is moved, poked and banned, and the ban is removed. Run it
once per fresh server: connecting the same identities again within about 30
seconds fails the handshake, and many quick attempts get the address banned
(`ConnectFailedBanned`).

## Installer and updates while developing

- A build run from `cargo run` is not an installed copy, so the updater reports
  "unavailable" (the page offers the downloads link). To see the other states
  without a release, run a debug build with
  `MIRAGE_DEV_UPDATE=available|uptodate|failed|checking|downloading`; it
  scripts a fake backend (compiled out of release builds).
- To exercise the real updater, pack two versions into a folder and install the
  older one; see "Local end-to-end test" in [releasing](RELEASING.md), which
  also lists the `vpk` environment variables that avoid Windows path-length and
  NuGet lock problems.
- `MIRAGE_UPDATE_URL` overrides where an installed app looks (a GitHub
  repository URL or a local feed folder).
- The `velopack` crate and `vpk` must be the same version.

`python scripts/check_ping_latency.py --binary <ts6-bridge.exe> --server 127.0.0.1:PORT`
connects one bridge to a loopback server you run and fails when the median
reported ping exceeds 5 ms; a regression of the session tick (see `SESSION_TICK` in `ts6-bridge`) shows up as
tens of milliseconds there.
