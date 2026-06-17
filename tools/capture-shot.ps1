# Capture a PNG screenshot of a running page in a specific JS-driven state,
# using headless Edge over the DevTools Protocol (CDP). Used to generate the
# README screenshots from the live Studio UI.
#
#   .\capture-shot.ps1 -Url http://127.0.0.1:7341/ -Out shot.png `
#       -Eval "(async()=>{ ... })()" -Width 1280 -Height 860 -SettleMs 600
param(
    [Parameter(Mandatory=$true)][string]$Url,
    [Parameter(Mandatory=$true)][string]$Out,
    [string]$Eval = "true",
    [int]$Width = 1280,
    [int]$Height = 860,
    [int]$SettleMs = 600
)
$ErrorActionPreference = "Stop"

$edge = "${env:ProgramFiles(x86)}\Microsoft\Edge\Application\msedge.exe"
if (-not (Test-Path $edge)) { $edge = "$env:ProgramFiles\Microsoft\Edge\Application\msedge.exe" }
if (-not (Test-Path $edge)) { throw "msedge.exe not found" }

$port = Get-Random -Minimum 9300 -Maximum 9600
$profile = Join-Path $env:TEMP ("edgeshot_" + $port)
New-Item -ItemType Directory -Force -Path $profile | Out-Null

$args = @(
    "--headless=new", "--disable-gpu", "--hide-scrollbars",
    "--no-first-run", "--no-default-browser-check",
    "--remote-debugging-port=$port",
    "--user-data-dir=`"$profile`"",
    "--window-size=$Width,$Height",
    $Url
)
$proc = Start-Process -FilePath $edge -ArgumentList $args -PassThru -WindowStyle Hidden

function Get-PageWs([int]$port) {
    for ($i = 0; $i -lt 50; $i++) {
        try {
            $list = Invoke-RestMethod -Uri "http://127.0.0.1:$port/json" -TimeoutSec 2
            $page = $list | Where-Object { $_.type -eq "page" -and $_.webSocketDebuggerUrl } | Select-Object -First 1
            if ($page) { return $page.webSocketDebuggerUrl }
        } catch {}
        Start-Sleep -Milliseconds 200
    }
    throw "Could not find a CDP page target on port $port"
}

$wsUrl = Get-PageWs $port
$ws = New-Object System.Net.WebSockets.ClientWebSocket
$ct = [System.Threading.CancellationToken]::None
$ws.ConnectAsync([Uri]$wsUrl, $ct).GetAwaiter().GetResult() | Out-Null

$script:msgId = 0
function Send-Cdp($method, $params) {
    $script:msgId++
    $id = $script:msgId
    $msg = @{ id = $id; method = $method }
    if ($params) { $msg.params = $params }
    $json = $msg | ConvertTo-Json -Depth 12 -Compress
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($json)
    $seg = New-Object System.ArraySegment[byte] (,$bytes)
    $ws.SendAsync($seg, [System.Net.WebSockets.WebSocketMessageType]::Text, $true, $ct).GetAwaiter().GetResult() | Out-Null
    # Read frames until we get the response matching this id.
    while ($true) {
        $sb = New-Object System.Text.StringBuilder
        do {
            $buf = New-Object byte[] 65536
            $seg2 = New-Object System.ArraySegment[byte] (,$buf)
            $res = $ws.ReceiveAsync($seg2, $ct).GetAwaiter().GetResult()
            [void]$sb.Append([System.Text.Encoding]::UTF8.GetString($buf, 0, $res.Count))
        } while (-not $res.EndOfMessage)
        $obj = $sb.ToString() | ConvertFrom-Json
        if ($obj.id -eq $id) { return $obj }
    }
}

function Eval-Js($expr, [bool]$awaitPromise = $true) {
    $p = @{ expression = $expr; awaitPromise = $awaitPromise; returnByValue = $true }
    return Send-Cdp "Runtime.evaluate" $p
}

# Wait for the SPA to finish its initial /api/inspect load.
for ($i = 0; $i -lt 60; $i++) {
    $r = Eval-Js "(typeof findNode==='function' && !!(state && state.inspect)) ? 'ready' : 'wait'"
    if ($r.result.result.value -eq "ready") { break }
    Start-Sleep -Milliseconds 250
}

# Drive to the desired state.
$null = Eval-Js $Eval $true
Start-Sleep -Milliseconds $SettleMs

# Capture.
$shot = Send-Cdp "Page.captureScreenshot" @{ format = "png"; fromSurface = $true; captureBeyondViewport = $false }
$b64 = $shot.result.data
if (-not $b64) { throw "captureScreenshot returned no data" }
[IO.File]::WriteAllBytes($Out, [Convert]::FromBase64String($b64))

try { $ws.CloseAsync([System.Net.WebSockets.WebSocketCloseStatus]::NormalClosure, "done", $ct).GetAwaiter().GetResult() | Out-Null } catch {}
try { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue } catch {}
Remove-Item -Recurse -Force $profile -ErrorAction SilentlyContinue

$f = Get-Item $Out
"WROTE $Out  ($([math]::Round($f.Length/1KB)) KB)"
