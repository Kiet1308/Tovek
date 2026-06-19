#!/usr/bin/env pwsh
# run-server.ps1 — build (if needed) and launch the Medal Luau decompiler web server.
#
#   POST http://127.0.0.1:3000/decompile
#     Body    : base64-encoded Luau bytecode
#     Headers : X-Script-Name = <full script path>   (optional, used for naming)
#     Returns : decompiled Luau source (text/plain)
#
# Usage:
#   .\run-server.ps1            # run the release binary (builds it first if missing)
#   .\run-server.ps1 -Build     # force a fresh release rebuild before running

param(
    [switch]$Build
)

$ErrorActionPreference = 'Stop'
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$Manifest  = Join-Path $ScriptDir 'Cargo.toml'
$Toolchain = 'nightly-2024-12-15'   # required: stable/bootstrap fail on feature gates
$Exe       = Join-Path $ScriptDir 'target\release\web-server.exe'

if ($Build -or -not (Test-Path $Exe)) {
    Write-Host "==> Building web-server (release, +$Toolchain) ..." -ForegroundColor Cyan
    cargo "+$Toolchain" build --release -p web-server --manifest-path $Manifest
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit $LASTEXITCODE)" }
}

Write-Host "==> Starting decompiler server on http://127.0.0.1:3000/decompile" -ForegroundColor Green
Write-Host "    (Ctrl+C to stop)" -ForegroundColor DarkGray
& $Exe
