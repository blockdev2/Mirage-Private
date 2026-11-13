# Mirage

An independent, experimental TeamSpeak 6 client with a native Rust desktop frontend and a React browser frontend. Both use the Rust session and audio core through a local WebSocket bridge. First-party code is MIT licensed. This project is not affiliated with TeamSpeak and does not redistribute the proprietary client or server.

**First-party comments, documentation, logs, tests, and contribution text must be in English.** The native desktop client's end-user UI is in Brazilian Portuguese. Protocol identifiers, upstream legal notices, and externally supplied server/device names retain their original values.

## Status

Implemented: TS3-compatible UDP session establishment, channel/client snapshots, correlated commands, Opus voice, capture/playback, Windows global push-to-talk, and a local WebSocket UI bridge.

The native frontend is an eframe/egui desktop application. The React/Vite browser frontend remains available for browser-based development with the standalone bridge. Neither frontend is distributed through a packaged installer or release. Previous local experiments used TeamSpeak Server **6.0.0-beta13.1**; their results are summarized in [verification](docs/verification.md) and do not establish compatibility with every TS6 server release.

Both frontends implement current-channel text chat through the bridge. Browser send/receive and reload recovery were verified against a local beta13.1 server; native rendering and interaction remain to be manually validated. Direct-client messages are verified through the CLI but have no dedicated frontend conversation view. The native frontend also sends photos, animated GIFs and files (up to 5 MB) through the channel's classic file transfer, with `:shortcode:` emoji; the bridge path was verified end to end on a loopback server, while the rendered native window was not exercised and default servers deny guest uploads. See [channel chat](docs/channel-chat.md). Matrix chat integration, WebRTC video/screensharing, S3 file integration, and acoustic echo cancellation are not implemented. RNNoise noise suppression is present in the capture pipeline, but real microphone quality, long-running stability, and non-Windows operation remain unverified. The synthetic CLI tone-rate defect was corrected in a loopback retest; sustained voice and late-join behavior still need broader validation.

**Experimental software:** the RustCrypto advisory identified in the historical audit has been addressed by a dependency migration. Publication uses a clean source import; unsafe local historical commits are excluded. Private security reporting and production readiness still require follow-up. See [publication verification](docs/PUBLICATION.md) and the [historical audit](docs/AUDIT.md).

Local-server results and remaining feature gaps are tracked in [compatibility status](docs/compatibility-status.md). Later tests of the integrated backend against an owner-provided remote server, including chat and synthetic voice in both connection orders, are recorded in [integrated live validation](docs/live-validation-integrated.md).

Manual stream-control and synthetic WebRTC diagnostics now exercise signaling
and decoded video fixtures. These are not a native screen-sharing feature;
end-user capture, hardware codecs and official-viewer video remain unverified. A
standalone [owned-window capture probe](recon/probe-capture/README.md) validates
WGC, resizing and a GPU texture copy with one final readback. It also drives
Media Foundation H.264, HEVC and AV1 encoders directly (no FFmpeg), lets the user
choose the encoder backend and GPU (discrete, integrated or CPU), and streams
live RTP over loopback UDP with loss recovery; those checks passed on one
development machine. End-user screen sharing, TS6 stream transport and
official-viewer video remain unimplemented or unverified. See
[streaming research](docs/persistent-chat-and-streaming-research.md) and the
[native pipeline direction](docs/native-streaming-architecture.md).

## Requirements

Locally checked on Windows with Rust **1.99.0** (MSVC), Node **22.23.2**, npm, CMake **4.4.4**, and the Visual Studio C++ build tools. These are tested versions, not an established minimum compatibility range. The UI's Vite dependency requires Node 20.19+ or 22.12+; Node 22 is the documented setup. PowerShell 7 is used by the helper scripts. Python 3.10+ runs repository policy checks. No non-Windows support claim is established; the global key binding implementation is Windows-only.

## Build and run

From the repository root in PowerShell, build and run the native desktop client:

```powershell
cd client
./scripts/build.ps1 build --locked --workspace
cargo run --locked -p ts6-desktop
```

To run the browser frontend with the standalone bridge:

```powershell
cd client/apps/ui
npm ci --ignore-scripts
npm run build
cd ../..
cargo run --locked -p ts6-bridge
```

The standalone bridge serves `client/apps/ui/dist` by default, owns one session, and accepts commands from permitted local browser origins and native local probes. `TS6_UI_DIR` or `--ui <directory>` selects a different asset directory. The native desktop client embeds the bridge and uses only its WebSocket endpoint; it does not serve static files from the working directory. Keep the unauthenticated bridge on a trusted machine.

For a server you control on loopback or a private network, explicitly enable private destinations before starting the bridge or CLI:

```powershell
$env:ALLOW_PRIVATE_TARGETS = '1'
cd client
cargo run --locked -p ts6-cli -- probe 127.0.0.1:9987 5 --name tester
```

`ALLOW_PRIVATE_TARGETS` disables the vendored resolver's private-address guard for that process. Do not enable it for untrusted remote callers. A server is not included. Download and configure a server separately under the ignored `testbed/` directory if needed; keep administrative query interfaces disabled unless your isolated test requires them.

## Download a build

The `Build client` workflow compiles the native Windows client. Every run
uploads `Mirage-windows-x64.zip` (with a `.sha256` file) as an artifact, and
each push to `main` replaces the `nightly` pre-release with the newest build.
The binary is unsigned, so Windows SmartScreen may warn; compare the hash.

Mirage opens one window per computer: launching it again brings the running
window forward. `Mirage.exe --new-instance` opens another, fully separate
window (own bridge port, profiles and identity, stored under
`%APPDATA%\Mirage\slots`), for example to join as a second user.

## Develop and verify

```powershell
python scripts/check_repository.py
cd client
./scripts/build.ps1 test --locked --workspace
cargo fmt -p ts6-core -p ts6-audio -p ts6-input -p ts6-cli -p ts6-bridge -p ts6-desktop -- --check
cargo clippy --locked --workspace --all-targets --no-deps -- -D warnings
```

See [development and configuration](docs/DEVELOPMENT.md), [architecture](docs/architecture.md), and [historical verification](docs/verification.md). The CLI prints its available subcommands when run without arguments.

## Repository map

| Path | Responsibility |
| --- | --- |
| `client/crates/ts6-core` | Session, identity, state projection, commands, continuous duplex polling |
| `client/crates/ts6-audio` | Opus, resampling, metering, mixer, capture/playback threads |
| `client/crates/ts6-input` | Key parsing, Windows key polling, transmit gate |
| `client/crates/ts6-cli` | Diagnostics and manual measurement commands |
| `client/crates/ts6-bridge` | Loopback HTTP/WebSocket service and local audio orchestration |
| `client/crates/ts6-stream` | Native WebRTC publisher and viewer for TS6 streams (loopback-proven; not wired to the session yet) |
| `client/apps/desktop` | Native eframe/egui desktop frontend |
| `client/apps/ui` | React 19, TypeScript 7, Vite 8, CSS |
| `client/vendor/tsclientlib` | Tracked protocol source, separate workspace and licenses |
| `recon/probe-ts6` | Standalone early handshake probe |
| `docs` | Architecture, research references, verification, audit |

## Contribute and report security issues

Read [CONTRIBUTING.md](CONTRIBUTING.md), [community expectations](CODE_OF_CONDUCT.md), and [SECURITY.md](SECURITY.md). Coding agents start with [AGENTS.md](AGENTS.md); Claude-specific orientation is in [CLAUDE.md](CLAUDE.md). Technical claims require source or executed verification evidence.

The [MIT license](LICENSE) already applies to first-party code. Retain all [third-party notices](client/THIRD_PARTY.md) and follow [vendoring rules](client/UPSTREAM.md). No releases or release-support commitments are established by this repository.

## Protocol research and team integration

Read the [version-pinned TeamSpeak Go reference review](docs/protocol-reference-teamspeak-go.md) for backend research leads, evidence limits and proposed experiments. Follow the [PR-only team integration workflow](docs/TEAM_INTEGRATION.md) when contributing or consolidating branches.
