#!/usr/bin/env pwsh
# Packs the desktop client into the Windows installer and the update packages.
#
# Output (in -Out): Mirage-win-Setup.exe (the installer), the .nupkg update
# packages (full, plus a delta when an earlier release is in -Out) and the
# releases.win.json feed the in-app updater reads.
#
# Needs the `vpk` tool at the SAME version as the `velopack` crate in
# client/apps/desktop/Cargo.toml (see docs/RELEASING.md). Nothing is built
# here: pass the already compiled client with -Exe.
#
# Usage: pwsh scripts/pack_installer.ps1 -Version 0.2.0 [-Exe path] [-Out dir]
#        [-PackId Mirage] [-Vpk path] [-Notes file] [-SignParams "..."]

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)] [string] $Version,
    [string] $Exe = 'client/target/release/ts6-desktop.exe',
    [string] $Out = 'dist/installer',
    [string] $PackId = 'Mirage',
    [string] $Vpk = 'vpk',
    [string] $Notes = '',
    [string] $SignParams = ''
)

$ErrorActionPreference = 'Stop'
$root = Resolve-Path (Join-Path $PSScriptRoot '..')
Set-Location $root

if ($Version -notmatch '^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$') {
    throw "Version '$Version' is not semver (for example 0.2.0)."
}
if (-not (Test-Path $Exe)) { throw "The client executable '$Exe' does not exist; build it first." }

# One clean folder holds exactly what ships: the program under its final name.
$stage = Join-Path ([System.IO.Path]::GetTempPath()) "mirage-stage-$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Force $stage | Out-Null
try {
    Copy-Item $Exe (Join-Path $stage 'Mirage.exe')
    Copy-Item LICENSE $stage
    New-Item -ItemType Directory -Force $Out | Out-Null

    $arguments = @(
        'pack',
        '--packId', $PackId,
        '--packVersion', $Version,
        '--packDir', $stage,
        '--mainExe', 'Mirage.exe',
        '--packTitle', 'Mirage',
        '--packAuthors', 'Mirage',
        '--icon', 'installer/mirage.ico',
        '--splashImage', 'installer/splash.gif',
        '--splashProgressColor', '#ECEFF4',
        '--outputDir', $Out
    )
    if ($Notes -and (Test-Path $Notes)) { $arguments += @('--releaseNotes', $Notes) }
    if ($SignParams) { $arguments += @('--signParams', $SignParams) }

    Write-Host "vpk $($arguments -join ' ')" -ForegroundColor Cyan
    & $Vpk @arguments
    if ($LASTEXITCODE -ne 0) { throw "vpk pack failed with exit code $LASTEXITCODE" }
}
finally {
    Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
}

# A checksum file for people who verify downloads by hand.
Get-ChildItem $Out -Filter '*Setup.exe' | ForEach-Object {
    $hash = (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower()
    "$hash  $($_.Name)" | Set-Content (Join-Path $Out "$($_.Name).sha256")
}
Get-ChildItem $Out | Select-Object Name, Length | Format-Table -AutoSize
