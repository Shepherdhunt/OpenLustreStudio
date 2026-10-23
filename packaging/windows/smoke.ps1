# Install the Windows downloads the way a user would and check the installed
# copy works: silent per-user install with the PATH task, the program runs,
# the shortcuts exist, the PMS sample opens and checks, its scenarios run on
# the model (and the compiled C when a C compiler is present), the Studio
# serves; then uninstall and check it is all gone. Also the portable zip.
#
#   .\packaging\windows\smoke.ps1 [-Dist dist]      (CI)

param([string]$Dist = "dist")
$ErrorActionPreference = "Stop"
$dist = (Resolve-Path $Dist).Path
$work = Join-Path ([IO.Path]::GetTempPath()) ("olsmoke-" + [Guid]::NewGuid())
New-Item -ItemType Directory $work | Out-Null

function Check([string]$what) { if ($LASTEXITCODE -ne 0) { throw "$what failed ($LASTEXITCODE)" } }

function Check-Installed([string]$ol, [string]$label) {
    Write-Host "== $label"
    & $ol --version; Check "--version"
    $home2 = Join-Path $work "home"
    New-Item -ItemType Directory -Force $home2 | Out-Null
    $saved = $env:USERPROFILE
    $env:USERPROFILE = $home2
    $token = "openlustre-smoke-token"
    $env:OPENLUSTRE_STUDIO_TOKEN = $token
    try {
        $server = Start-Process $ol -ArgumentList "studio", "launch", "--sample", "pms", "--no-open", "--port", "8471" `
            -PassThru -NoNewWindow -RedirectStandardOutput (Join-Path $work "serve.log") -RedirectStandardError (Join-Path $work "serve.err")
        $up = $false
        for ($i = 0; $i -lt 50 -and -not $up; $i++) {
            try { Invoke-WebRequest -UseBasicParsing "http://127.0.0.1:8471/api/health" | Out-Null; $up = $true } catch { Start-Sleep -Milliseconds 200 }
        }
        # The Studio serves its launch token, and nothing to a request without it.
        $auth = @{ "X-OpenLustre-Token" = $token }
        $page = (Invoke-WebRequest -UseBasicParsing -Headers $auth "http://127.0.0.1:8471/").Content
        Invoke-WebRequest -UseBasicParsing -Headers $auth "http://127.0.0.1:8471/api/inspect" | Out-Null
        $refused = (Invoke-WebRequest -UseBasicParsing -SkipHttpErrorCheck "http://127.0.0.1:8471/api/inspect").StatusCode
        Stop-Process $server
        if (-not $up -or $page -notmatch 'id="diagram-status"') { Get-Content (Join-Path $work "serve.log"); throw "the Studio did not serve" }
        if ($refused -ne 403) { throw "the Studio answered a request without its token ($refused)" }
    } finally {
        $env:USERPROFILE = $saved
        Remove-Item Env:OPENLUSTRE_STUDIO_TOKEN
    }
    $pms = Join-Path $home2 "OpenLustre\samples\pms"
    & $ol check (Join-Path $pms "pms.wksc"); Check "check"
    & $ol test run (Join-Path $pms "pms.wksc") --scenarios (Join-Path $pms "scenarios"); Check "test run"
    Remove-Item -Recurse -Force $home2
}

# 1. The installer: silent, for this user, with the PATH task.
$setup = (Get-ChildItem (Join-Path $dist "OpenLustreStudio-*-Setup.exe") | Select-Object -First 1).FullName
$log = Join-Path $work "setup.log"
$p = Start-Process $setup -ArgumentList "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/CURRENTUSER", "/TASKS=addtopath", "/LOG=$log" -Wait -PassThru
if ($p.ExitCode -ne 0) { Get-Content $log; throw "setup exited with $($p.ExitCode)" }
$app = Join-Path $env:LOCALAPPDATA "Programs\OpenLustre Studio"
$ol = Join-Path $app "openlustre.exe"
if (-not (Test-Path $ol)) { Get-Content $log; throw "not installed at $app" }
if ([Environment]::GetEnvironmentVariable("Path", "User") -notlike "*$app*") { throw "the PATH task did not add $app" }
$menu = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\OpenLustre Studio"
foreach ($lnk in @("OpenLustre Studio.lnk", "OpenLustre Studio - PMS sample.lnk", "Uninstall OpenLustre Studio.lnk")) {
    if (-not (Test-Path (Join-Path $menu $lnk))) { throw "missing Start Menu shortcut $lnk" }
}
if (-not (Test-Path (Join-Path $app "examples\pms\pms.wksc"))) { throw "the PMS sample is not installed" }
if (Test-Path (Join-Path $app "examples\pms\.github")) { throw ".github shipped with the sample" }
Check-Installed $ol "installer (per-user)"

Start-Process (Join-Path $app "unins000.exe") -ArgumentList "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART" -Wait
for ($i = 0; $i -lt 60 -and (Test-Path $ol); $i++) { Start-Sleep -Seconds 1 }
if (Test-Path $ol) { throw "the uninstaller left $ol" }
if ([Environment]::GetEnvironmentVariable("Path", "User") -like "*$app*") { throw "the uninstaller left $app on PATH" }
if (Test-Path $menu) { throw "the uninstaller left the Start Menu folder" }

# 2. The portable zip.
$zip = (Get-ChildItem (Join-Path $dist "openlustre-studio-*-windows-x86_64.zip") | Select-Object -First 1).FullName
Expand-Archive $zip -DestinationPath (Join-Path $work "zip")
$portable = (Get-ChildItem (Join-Path $work "zip") -Directory | Select-Object -First 1).FullName
Check-Installed (Join-Path $portable "openlustre.exe") "portable zip"

Remove-Item -Recurse -Force $work
Write-Host "smoke: Windows downloads install, run and uninstall"
