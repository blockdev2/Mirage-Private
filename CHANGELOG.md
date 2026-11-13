# Changelog

No tagged releases have been established in this checkout. Entries describe repository changes, not a release announcement.

## Unreleased

- Native desktop: one sidebar for every screen. The connected server stays in a "Servidores" list with its channels folded under it, so the call no longer disappears on Inicio; the separate "Canais de voz" entry, the list/squares switch, the participant count and the three-dot menu are gone.
- Native desktop: the Convidar button copies a `ts3server://` link; the status bar is one button (connection, call time that resets per channel, ping graph, lost packets and percentage); chat rows align and no longer shift while hovered; channel chat is kept per server and channel on this computer and shown again after a restart (`profiles.json`, 200 messages per channel, removed with the server's history).
- Fix an inflated ping (80-100 ms instead of about 10 ms): replies were only read at the next 100 ms session wake-up, so the session now ticks every 10 ms. A vendored tsproto patch adds cumulative packet-loss counters (`client/patches/packet-loss-counts.patch`); `scripts/check_ping_latency.py` verifies the reported ping against a loopback server.
- Debug builds of the desktop client accept a scripted test plan (`MIRAGE_DEV_PLAN`, see `docs/DEVELOPMENT.md`) to drive and photograph the real window.
- CI: `build.yml` builds the Windows client (artifact on every run; a push to main refreshes the rolling `nightly` pre-release).
- Add channel chat attachments (photos, animated GIFs and files up to 5 MB) over classic channel file transfer with legacy BBCode references, `:shortcode:` emoji, cached `chat_fetch`, a distinct `upload_permission` refusal, and a two-bridge live check.
- CI: cache the pinned cargo-audit binary instead of compiling it every run; lint, test and audit the native capture probe; run harness unit tests.
- Let the native capture probe choose the encoder backend (hardware/software) and GPU (discrete, integrated, by index) with clean refusal of unsupported combinations.

- Add English onboarding, shared agent instructions, contribution and security policies, issue/PR templates, and CI/dependency monitoring.
- Translate UI messages and first-party fixtures; consolidate outdated architecture and historical verification notes.
- Restrict browser WebSocket access by Host/Origin and bound frame/message sizes.
- Remove credential fields from bridge command diagnostics; fail closed on invalid PTT bindings.
- Bound idle PTT gate shutdown and prevent overwriting an identity during creation.
- Replace personal script paths, sanitize upstream token-like examples, and repair the standalone probe's vendored dependency path.
- Record unresolved dependency advisories, historical privacy findings, and release prerequisites in `docs/AUDIT.md`.

- Keep the CLI tone sender polling network I/O during transmission; add a sender-correlated live progress regression to detect delivery deferred until transmission ends.

- Keep separate Opus decoder state per sender in CLI measurements, add a two-stream regression, and add live TS6 bridge control verification.

- Upgrade the vendored P-256/elliptic-curve stack to 0.14, resolve the crypto-bigint advisory in both lockfiles, and verify identity/crypto and live TS6 interoperability before the owner-authorized clean GitHub import.

- Coordinate Vite 8, React plugin 6, and TypeScript 7 updates with a regenerated compatible lockfile; upgrade tower-http to 0.7 and the SHA-pinned checkout/setup-node Actions to version 7.
