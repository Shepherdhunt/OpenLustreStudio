# Generates packaging\windows\openlustre.ico — the app/shortcut/file-type icon.
# A white AND-gate D-shape (the SCADE silhouette the canvas draws) on the app's
# blue rounded square. Multi-resolution, PNG-compressed frames (Vista+ .ico).
#
#   powershell -ExecutionPolicy Bypass -File packaging\windows\make-icon.ps1
#
# Re-run whenever the brand mark changes; the .ico is committed so a normal
# `cargo build` + ISCC run needs no image tooling.

Add-Type -AssemblyName System.Drawing

function New-FramePng([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap($size, $size)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.Clear([System.Drawing.Color]::Transparent)

    # Rounded-square background in the app accent blue (#2B579A).
    $pad = [Math]::Max(1, [int]($size * 0.055))
    $x = $pad; $y = $pad; $w = $size - 2 * $pad; $h = $size - 2 * $pad
    $r = [int]($size * 0.20); $d = $r * 2
    $bgPath = New-Object System.Drawing.Drawing2D.GraphicsPath
    $bgPath.AddArc($x, $y, $d, $d, 180, 90)
    $bgPath.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $bgPath.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $bgPath.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $bgPath.CloseFigure()
    $bg = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(43, 87, 154))
    $g.FillPath($bg, $bgPath)

    # White AND-gate D-shape, centred. Flat left/top, semicircular right.
    $gw = $w * 0.46; $gh = $h * 0.42
    $gx = $x + $w * 0.30; $gy = $y + ($h - $gh) / 2
    $bw = $gw * 0.5            # straight portion before the bulge
    $gate = New-Object System.Drawing.Drawing2D.GraphicsPath
    $gate.AddLine($gx, $gy, $gx + $bw, $gy)
    # Semicircle bulging right: bounding box of the arc is (gx+bw .. gx+gw) wide.
    $arcX = $gx + $bw - ($gw - $bw)
    $gate.AddArc($arcX, $gy, ($gw - $bw) * 2, $gh, -90, 180)
    $gate.AddLine($gx + $bw, $gy + $gh, $gx, $gy + $gh)
    $gate.CloseFigure()
    $white = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::White)
    $g.FillPath($white, $gate)

    # Pin stubs (two in on the left, one out at the tip) — white lines.
    $penW = [Math]::Max(1.0, $size * 0.045)
    $pen = New-Object System.Drawing.Pen ([System.Drawing.Color]::White, $penW)
    $stub = $size * 0.10
    $iny1 = $gy + $gh * 0.30; $iny2 = $gy + $gh * 0.70
    $g.DrawLine($pen, [single]($gx - $stub), [single]$iny1, [single]$gx, [single]$iny1)
    $g.DrawLine($pen, [single]($gx - $stub), [single]$iny2, [single]$gx, [single]$iny2)
    $tipX = $gx + $gw; $tipY = $gy + $gh / 2
    $g.DrawLine($pen, [single]$tipX, [single]$tipY, [single]($tipX + $stub), [single]$tipY)

    $g.Dispose()
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    return , $ms.ToArray()
}

$sizes = @(16, 24, 32, 48, 64, 128, 256)
$frames = @()
foreach ($s in $sizes) { $frames += , (New-FramePng $s) }

$out = Join-Path $PSScriptRoot 'openlustre.ico'
$fs = [System.IO.File]::Create($out)
$bw = New-Object System.IO.BinaryWriter($fs)
# ICONDIR
$bw.Write([UInt16]0)            # reserved
$bw.Write([UInt16]1)            # type = icon
$bw.Write([UInt16]$frames.Count)
# ICONDIRENTRYs
$offset = 6 + 16 * $frames.Count
for ($i = 0; $i -lt $frames.Count; $i++) {
    $sz = $sizes[$i]; $len = $frames[$i].Length
    $bw.Write([Byte]($(if ($sz -ge 256) { 0 } else { $sz })))  # width
    $bw.Write([Byte]($(if ($sz -ge 256) { 0 } else { $sz })))  # height
    $bw.Write([Byte]0)          # color count
    $bw.Write([Byte]0)          # reserved
    $bw.Write([UInt16]1)        # planes
    $bw.Write([UInt16]32)       # bit count
    $bw.Write([UInt32]$len)     # bytes in resource
    $bw.Write([UInt32]$offset)  # image offset
    $offset += $len
}
foreach ($f in $frames) { $bw.Write($f) }
$bw.Flush(); $bw.Close(); $fs.Close()
Write-Output "Wrote $out ($((Get-Item $out).Length) bytes, $($frames.Count) frames)"
