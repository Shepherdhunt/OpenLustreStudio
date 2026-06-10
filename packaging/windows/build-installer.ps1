# Build the OpenLustre Studio Windows installer.
#
#   .\packaging\windows\build-installer.ps1 [-Version 0.1.0]
#
# Requires: Rust toolchain (cargo) and Inno Setup 6 (ISCC.exe). The GitHub
# Actions release workflow runs this on every version tag; the script also
# works on a developer machine.

param(
    [string]$Version = "0.1.0"
)

$ErrorActionPreference = "Stop"
$repo = Resolve-Path (Join-Path $PSScriptRoot "..\..")

Write-Host "==> cargo build --release"
Push-Location $repo
try {
    cargo build --release -p ol_cli
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
} finally {
    Pop-Location
}

$iscc = @(
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "${env:ProgramFiles}\Inno Setup 6\ISCC.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) {
    throw "ISCC.exe not found - install Inno Setup 6 (https://jrsoftware.org/isinfo.php)"
}

Write-Host "==> $iscc /DAppVersion=$Version openlustre.iss"
& $iscc "/DAppVersion=$Version" (Join-Path $PSScriptRoot "openlustre.iss")
if ($LASTEXITCODE -ne 0) { throw "ISCC failed" }

$setup = Join-Path $PSScriptRoot "dist\OpenLustreStudio-$Version-Setup.exe"
Write-Host "==> installer: $setup"
