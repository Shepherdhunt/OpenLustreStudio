# Build the Windows downloads of OpenLustre Studio into dist\:
#
#   OpenLustreStudio-<version>-windows-x86_64-Setup.exe   the installer
#   openlustre-studio-<version>-windows-x86_64.zip        portable: unzip, run
#
#   .\packaging\windows\build-installer.ps1 [-Version 0.1.0]
#
# Requires: Rust (cargo), git, and Inno Setup 6 (ISCC.exe;
# `choco install innosetup` or https://jrsoftware.org/isinfo.php). Both
# downloads carry the program, the samples, and the Kind 2 wrappers for WSL
# and Docker (Kind 2 has no Windows build).

param(
    [string]$Version = "0.1.0"
)

$ErrorActionPreference = "Stop"
$repo = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$name = "openlustre-studio-$Version-windows-x86_64"
$dist = Join-Path $repo "dist"
$stage = Join-Path $dist "stage\$name"

Write-Host "==> cargo build --release"
Push-Location $repo
try {
    cargo build --release --locked -p ol_cli
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
} finally {
    Pop-Location
}

Write-Host "==> staging $stage"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force (Join-Path $stage "tools"), (Join-Path $stage "examples") | Out-Null
Copy-Item (Join-Path $repo "target\release\openlustre.exe") $stage
Copy-Item (Join-Path $repo "tools\kind2-wsl.cmd"), (Join-Path $repo "tools\kind2-docker.sh") (Join-Path $stage "tools")
foreach ($ex in @("pms", "release_logic")) {
    $src = Join-Path $repo "examples\$ex"
    # Tracked files only: no build output is ever shipped.
    $files = git -C $src ls-files -- . ":!:.github"
    if ($LASTEXITCODE -ne 0) { throw "git ls-files failed in $src" }
    foreach ($f in $files) {
        $dst = Join-Path (Join-Path $stage "examples\$ex") $f
        New-Item -ItemType Directory -Force (Split-Path $dst) | Out-Null
        Copy-Item (Join-Path $src $f) $dst
    }
}
Copy-Item (Join-Path $repo "README.md"), (Join-Path $repo "LICENSE"), (Join-Path $PSScriptRoot "README-windows.txt") $stage
Set-Content -Path (Join-Path $stage "VERSION") -Value $Version -NoNewline

Write-Host "==> $name.zip"
$zip = Join-Path $dist "$name.zip"
if (Test-Path $zip) { Remove-Item $zip }
Compress-Archive -Path $stage -DestinationPath $zip

$iscc = @(
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "${env:ProgramFiles}\Inno Setup 6\ISCC.exe",
    "${env:LOCALAPPDATA}\Programs\Inno Setup 6\ISCC.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) {
    throw "ISCC.exe not found - install Inno Setup 6 (choco install innosetup, or https://jrsoftware.org/isinfo.php)"
}
Write-Host "==> installer"
& $iscc "/DAppVersion=$Version" "/DStageDir=$stage" "/DOutputDir=$dist" (Join-Path $PSScriptRoot "openlustre.iss")
if ($LASTEXITCODE -ne 0) { throw "ISCC failed" }

Remove-Item -Recurse -Force (Join-Path $dist "stage")
Get-ChildItem $dist | Format-Table Name, Length
