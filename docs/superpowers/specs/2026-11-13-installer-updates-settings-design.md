# Installer, in-app updates and a new settings window: design

Status: approved by the maintainer in chat on 2026-11-13 ("sim, faca tudo"). Written before the code.

## Intent

Mirage is distributed today as a zip and loose `.exe` files. People should be able to install it with one click, get updates from inside the app, and find every setting in a window that is not cramped. The maintainer also wants the installer to feel like Mirage, not like a generic wizard.

## Goals

- A separate installer (`Mirage-Setup.exe`) that installs per user, with no administrator prompt, a Start menu and desktop shortcut and an uninstall entry.
- Updates from the settings window ("Atualizacoes"): check, download with progress, restart into the new version. Small downloads (delta packages).
- A settings window sized for the app: navigation on the left, one page at a time on the right, no hidden content.
- The installer never compiles anything on the user's machine; it only unpacks a build that CI already produced and verified.
- Documentation and project memory stay current.

## Non-goals

- Code signing in this change (documented as the next step, see Constraints).
- Auto-update for portable zip users (the page says the copy is portable and links to the installer).
- Update channels beyond stable and the rolling nightly, a background updater service, silent forced updates.

## Anti-goals

- Shipping any credential or token inside the client.
- An update that can leave the app broken: failed downloads must leave the installed version untouched.
- Publishing source or binaries to the public repository without the maintainer doing it on purpose.

## Constraints found by research

- `blockdev2/Mirage-Private` is private: its release assets cannot be downloaded without a token. The updater therefore reads the public repository's releases (the existing `origin`, which already mirrors the source and has no releases yet). It was 51 commits behind the private `main` when checked.
- A workflow in the private repository cannot publish into the public one without a personal access token stored as a secret, which only the maintainer can create. So the release workflow runs in whichever repository the tag is pushed to, and can optionally publish to another repository when `RELEASES_REPO` and a `RELEASES_TOKEN` secret are set.
- `vpk` (Velopack's packer) is a .NET tool; CI runners ship a .NET SDK. The Rust crate and `vpk` must be the same version (both 1.3.39-g8d2844f here).
- SmartScreen warns on unsigned installers and Microsoft states that signing does not remove the warning instantly either. SignPath Foundation is free for qualifying open-source projects but verifies the binary comes from the open repository, which is another reason to release from the public repo.

## Approaches considered

1. **Velopack (chosen).** One-click per-user `Setup.exe`, `Update.exe`, delta packages, a `GithubSource` and a Rust crate. Trade-off: the installer window is fixed (icon and a splash image, which can be an animated GIF), so the creative part goes into that splash and into the in-app update page.
2. **Inno Setup plus `self_update`.** A classic wizard, but two separate mechanisms to keep in step and more code for replacing a running executable on Windows.
3. **An installer of our own in egui.** The most on-brand, but we would own download verification, atomic replacement and rollback, which is where updaters usually get hurt. Possible later as a front end that drives Velopack's silent install; not verified.

## Design

- `client/apps/desktop/src/updater.rs` wraps Velopack on a worker thread and exposes a small state machine (`Unavailable`, `Idle`, `Checking`, `UpToDate`, `Available`, `Downloading`, `Ready`, `Failed`). The UI only reads the state and sends commands. A copy that was not installed by Velopack reports `Unavailable` and keeps working.
- The update source is the public repository's releases; `MIRAGE_UPDATE_URL` overrides it (a folder path works, which is how it is tested without the network).
- `VelopackApp::build().run()` is the first thing `main` does.
- The settings window is rebuilt as a two-pane layout: Perfil, Audio, Atualizacoes, Sobre e diagnostico.
- CI: `release.yml` builds, packs with `vpk` and publishes a GitHub release on a `v*` tag; the nightly workflow also uploads the installer as an artifact.

## Left to the builder

Visual details of the settings pages, the splash animation, exact wording (pt-BR), and the choice of the update check cadence (checked when the page opens and on a button, never in a loop).
