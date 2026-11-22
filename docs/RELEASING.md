# Releasing Mirage (installer and updates)

Mirage ships as a per-user Windows installer built with
[Velopack](https://velopack.io). The installed app checks a public GitHub
releases page for a newer version and updates itself from the settings window
("Atualizações"). This page is the maintainer's reference; the design and the
alternatives that were rejected are in
[the design note](superpowers/specs/2026-11-13-installer-updates-settings-design.md).

## What a release contains

`scripts/pack_installer.ps1` (wrapping `vpk pack`) produces, in `dist/installer`:

| File | Purpose |
| --- | --- |
| `Mirage-win-Setup.exe` | One-click installer (per user, `%LocalAppData%\Mirage`, no administrator prompt). Shows the animated splash from `installer/splash.gif`. |
| `Mirage-win-Portable.zip` | Portable copy. It cannot update itself; its settings page links to the installer. |
| `Mirage-<version>-full.nupkg` | Full update package. |
| `Mirage-<version>-delta.nupkg` | Binary diff from the previous release (only when the previous packages are in the output folder); usually a small fraction of the full size. |
| `releases.win.json`, `RELEASES` | The feed the app reads. |
| `*.sha256` | Checksum of the installer for manual verification. |

The installed layout is `%LocalAppData%\Mirage\current\Mirage.exe` plus the
`Update.exe` that applies updates. The updater downloads the full or delta
package, verifies it, and on "Reiniciar e atualizar" closes the app, swaps the
folder and starts the new version. User data stays in `%APPDATA%\Mirage` and is
never touched by an update or an uninstall of the program files.

## Where releases live (this matters)

The app reads releases **without any token**, so they must be in a repository
that is readable anonymously. A release in a private repository is invisible to
users and the updater would report an error. The default feed is
`https://github.com/blockdev2/Mirage-Private` (constant
`DEFAULT_SOURCE` in `client/apps/desktop/src/updater.rs`); change that constant
if releases move. Never put a token in the client.

Two ways to get a release there:

1. **Tag in the public repository.** Push the source and a `v*` tag to that
   repository; its `Release` workflow publishes to itself with the built-in
   token. Nothing else to configure.
2. **Tag in the private repository, publish to the public one.** Set the
   repository variable `RELEASES_REPO` (`owner/name`) and the secret
   `RELEASES_TOKEN` (a fine-grained token with *Contents: read and write* on that
   repository only) in the private repository. The workflow then publishes
   there. Only the release assets (the built program and feed) are exposed, not
   the source.

Test builds can point at another feed without rebuilding: set
`MIRAGE_UPDATE_URL` to a GitHub repository URL or to a local folder holding a
feed (used for the end-to-end test below).

## Cutting a release

1. Bump `version` under `[workspace.package]` in `client/Cargo.toml` (the
   installer, the update feed and the app's "Mirage x.y.z" label all read it)
   and refresh `client/Cargo.lock`.
2. Make sure the `## Unreleased` section of `CHANGELOG.md` reads well for
   users: the workflow copies exactly that section into the release notes the
   update page shows (first 1200 characters). Rename it to the version heading
   in the next change after the tag.
3. Merge to `main`, then tag the merge commit and push the tag:
   `git tag v0.2.0 && git push <remote> v0.2.0`.
4. The `Release` workflow checks that the tag equals the workspace version,
   builds `ts6-desktop` in release mode, downloads the previous release (for the
   delta), packs, and publishes. It fails loudly instead of publishing a
   mismatched version.

The `Build client` workflow also packs the installer on every run (artifact
`Mirage-installer`), so a broken packaging step shows up long before a tag.

## Releasing by hand (no Actions)

When the Actions workflow cannot run (for example the account's billing is
blocked), build and pack locally and hand out `Mirage-win-Setup.exe`
(`Mirage-win-Setup.exe.sha256` lets people verify it). Nothing needs a release
feed for that: an installed copy whose feed has no release simply finds no
update. Steps: `cargo build --release --locked -p ts6-desktop` in `client`,
then `./scripts/pack_installer.ps1 -Version X.Y.Z -Vpk <vpk.exe> -Notes <file>`
with the environment from "Local end-to-end test". Keep `## Unreleased` as the
heading in `CHANGELOG.md` while the workflow is the thing that reads it.

## Version pairing

The `velopack` crate (`client/apps/desktop/Cargo.toml`) and the `vpk` tool must
be the **same version** (both `1.3.39-g8d2844f` today, also pinned in both workflows). Do not go back to 1.2.x: its `zip 8` dependency carries four open RustSec advisories (RUSTSEC-2026-0367, -0368, -0369, -0429) that fail `cargo audit`.
Update them together, then run the local end-to-end test.

## Local end-to-end test

Uses a throw-away package id so a real install is never touched. Short paths and
a private NuGet cache avoid Windows path-length problems:

```powershell
$env:TEMP = $env:TMP = 'C:\mt\tmp'; $env:DOTNET_CLI_HOME = 'C:\mt\dotnethome'; $env:NUGET_PACKAGES = 'C:\mt\nuget'
dotnet tool install vpk --version 1.3.39-g8d2844f --tool-path C:\mt\vpk
./scripts/pack_installer.ps1 -Version 0.1.0 -PackId MirageDev -Vpk C:\mt\vpk\vpk.exe -Out C:\mt\pack\rel
Copy-Item C:\mt\pack\rel\MirageDev-win-Setup.exe C:\mt\pack\Setup-0.1.0.exe
./scripts/pack_installer.ps1 -Version 0.1.1 -PackId MirageDev -Vpk C:\mt\vpk\vpk.exe -Out C:\mt\pack\rel   # adds the delta
C:\mt\pack\Setup-0.1.0.exe --silent --installto C:\mt\install
$env:MIRAGE_UPDATE_URL = 'C:\mt\pack\rel'
C:\mt\install\current\Mirage.exe     # Ajustes -> Atualizações: download, then restart
C:\mt\install\Update.exe --uninstall --silent
```

This was run (with `vpk` and the crate at the version above): 0.1.0 found 0.1.1, downloaded
it, closed, applied it and relaunched as 0.1.1; the uninstall removed the
folder and both shortcuts. It used a debug build and a local folder feed, not
GitHub, and not a signed binary.

## Not done yet

- **Code signing.** Installers and updates are unsigned, so Windows SmartScreen
  warns on first run (the hash file lets people verify downloads). Options:
  SignPath Foundation (free for open-source projects; requires the public
  repository and vouches that the binary is built from it) or Azure Artifact
  Signing (about US$10/month). `pack_installer.ps1 -SignParams` forwards the
  signing command to `vpk`. Signing improves the warning over time; it does not
  remove it on day one.
- **A first published release.** Nothing has been published; the updater is
  idle until a `v*` release exists in the feed repository.
- **Update channel for pre-releases** (the nightly build is deliberately not
  an update source).
