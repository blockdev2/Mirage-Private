# Installer, updates and settings implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans. Steps use checkbox syntax.

**Goal:** One-click installer, in-app updates, and a roomy two-pane settings window.

**Architecture:** Velopack (crate and `vpk` both 1.3.39-g8d2844f) packs and updates; a worker thread in `ts6-desktop` drives it and the UI reads a state machine. See the design in `docs/superpowers/specs/2026-11-13-installer-updates-settings-design.md`.

**Tech Stack:** Rust, egui 0.37, `velopack` 1.3.39-g8d2844f, `vpk` 1.3.39-g8d2844f, GitHub Actions.

## Global Constraints

- English code and docs; pt-BR UI text; MIT.
- No token in the client; the update source is a public repository URL or a local folder (`MIRAGE_UPDATE_URL`).
- A failed update never touches the installed version.
- fmt, clippy `-D warnings`, workspace tests, audit and the repository policy check stay green.

## Review Focus

- A portable or debug copy (not installed by Velopack) must keep working and say so.
- A network failure, a missing release or a malformed index must show a readable error, not hang the window.
- The settings window must fit a 1000x640 window and a high-DPI screen without hiding content.
- Escape and keyboard focus keep working in the new window (name edit, photo crop).

## Tasks

### Task 1: Updater core
- [ ] `UpdateState` and `Updater` (`check`, `download`, `apply_and_restart`, `poll`) in `updater.rs`, state transitions unit-tested with a fake backend; `VelopackApp::build().run()` first in `main`.

### Task 2: Settings window
- [ ] Two-pane `settings.rs`: pages Perfil, Audio, Atualizacoes, Sobre; photo crop and name edit unchanged in behavior. Photographed at 1280x820 and 1000x640.

### Task 3: Updates page
- [ ] Version, status, check button, progress bar, restart button, release notes line, portable-copy message.

### Task 4: Packaging and CI
- [ ] `scripts/pack_installer.ps1` (vpk pack with icon and animated splash), `release.yml` on `v*` tags, installer artifact in `build.yml`.
- [ ] Local end-to-end: pack 0.1.0 and 0.1.1, install 0.1.0, update from a folder source to 0.1.1.

### Task 5: Docs, memory, ship
- [ ] README, CHANGELOG, DEVELOPMENT, architecture, release guide; memory notes; PR; merge on green CI.
