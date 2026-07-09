# Regenerates assets/icon.ico (the pppdf app icon: white page + red bookmark
# on a dark rounded tile, matching the app's chromeless dark look).
# Pure PowerShell + GDI+; no external tools. Run from anywhere.
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$S = 1024.0
$dark  = [System.Drawing.Color]::FromArgb(255, 26, 26, 26)    # #1a1a1a, app letterbox color
$white = [System.Drawing.Color]::FromArgb(255, 248, 248, 246)
$red   = [System.Drawing.Color]::FromArgb(255, 229, 57, 53)

function Pt($x, $y) { New-Object System.Drawing.PointF(($x*$S), ($y*$S)) }

function Draw-Master {
    $bmp = New-Object System.Drawing.Bitmap(1024, 1024, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = 'AntiAlias'
    $g.Clear([System.Drawing.Color]::Transparent)

    # Rounded dark tile
    $m = 0.04 * $S; $r = 0.20 * $S; $w = $S - 2*$m
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $path.AddArc($m, $m, 2*$r, 2*$r, 180, 90)
    $path.AddArc($m + $w - 2*$r, $m, 2*$r, 2*$r, 270, 90)
    $path.AddArc($m + $w - 2*$r, $m + $w - 2*$r, 2*$r, 2*$r, 0, 90)
    $path.AddArc($m, $m + $w - 2*$r, 2*$r, 2*$r, 90, 90)
    $path.CloseFigure()
    $g.FillPath((New-Object System.Drawing.SolidBrush($dark)), $path)

    # White page
    $l = 0.30; $t = 0.20; $rr = 0.70; $b = 0.80
    $g.FillRectangle((New-Object System.Drawing.SolidBrush($white)), ($l*$S), ($t*$S), (($rr-$l)*$S), (($b-$t)*$S))

    # Red bookmark ribbon with V notch
    $bl = 0.55; $br = 0.66; $bt = 0.20; $bb = 0.46; $v = 0.055
    $g.FillPolygon((New-Object System.Drawing.SolidBrush($red)), [System.Drawing.PointF[]]@(
        (Pt $bl $bt), (Pt $br $bt), (Pt $br $bb), (Pt (($bl+$br)/2) ($bb-$v)), (Pt $bl $bb)))

    $g.Dispose()
    $bmp
}

$master = Draw-Master
$sizes = @(16, 24, 32, 48, 64, 128, 256)
$blobs = @()
foreach ($sz in $sizes) {
    $small = New-Object System.Drawing.Bitmap($sz, $sz)
    $sg = [System.Drawing.Graphics]::FromImage($small)
    $sg.InterpolationMode = 'HighQualityBicubic'
    $sg.DrawImage($master, 0, 0, $sz, $sz)
    $ms = New-Object System.IO.MemoryStream
    $small.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $blobs += , @($sz, $ms.ToArray())
    $sg.Dispose(); $small.Dispose(); $ms.Dispose()
}
$master.Dispose()

# Assemble the .ico container (PNG-compressed entries; fine on Windows 10/11)
$out = New-Object System.IO.MemoryStream
$bw = New-Object System.IO.BinaryWriter($out)
$bw.Write([uint16]0); $bw.Write([uint16]1); $bw.Write([uint16]$blobs.Count)
$offset = 6 + 16 * $blobs.Count
foreach ($e in $blobs) {
    $sz = $e[0]; $bytes = $e[1]
    $bw.Write([byte]($sz -band 0xFF))   # 0 means 256
    $bw.Write([byte]($sz -band 0xFF))
    $bw.Write([byte]0); $bw.Write([byte]0)
    $bw.Write([uint16]1); $bw.Write([uint16]32)
    $bw.Write([uint32]$bytes.Length); $bw.Write([uint32]$offset)
    $offset += $bytes.Length
}
foreach ($e in $blobs) { $bw.Write($e[1]) }
$bw.Flush()

$icoPath = Join-Path $PSScriptRoot 'icon.ico'
[System.IO.File]::WriteAllBytes($icoPath, $out.ToArray())
$bw.Dispose(); $out.Dispose()
Write-Host "wrote $icoPath ($((Get-Item $icoPath).Length) bytes, $($blobs.Count) sizes)"
