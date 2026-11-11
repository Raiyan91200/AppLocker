# Generates res/app.ico (blue rounded square with a white padlock) as a multi-size PNG icon.
# Run: powershell -ExecutionPolicy Bypass -File res\make-icon.ps1
Add-Type -AssemblyName System.Drawing

function New-RoundRect([float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = $r * 2
    $p.AddArc($x, $y, $d, $d, 180, 90)
    $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $p.CloseFigure()
    return $p
}

$sizes = 16, 20, 24, 32, 40, 48, 64, 256
$images = @()
$blueTop = [System.Drawing.Color]::FromArgb(255, 79, 141, 247)
$blueBottom = [System.Drawing.Color]::FromArgb(255, 30, 79, 216)

foreach ($s in $sizes) {
    $k = $s / 256.0
    $bmp = New-Object System.Drawing.Bitmap $s, $s, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.Clear([System.Drawing.Color]::Transparent)

    # Background
    $bg = New-RoundRect (6 * $k) (6 * $k) (244 * $k) (244 * $k) (56 * $k)
    $grad = New-Object System.Drawing.Drawing2D.LinearGradientBrush (New-Object System.Drawing.PointF 0, 0), (New-Object System.Drawing.PointF 0, $s), $blueTop, $blueBottom
    $g.FillPath($grad, $bg)

    # Shackle
    $pen = New-Object System.Drawing.Pen ([System.Drawing.Color]::White), ([Math]::Max(1.6, 24 * $k))
    $pen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
    $pen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
    $sh = New-Object System.Drawing.Drawing2D.GraphicsPath
    $sh.AddLine((84 * $k), (124 * $k), (84 * $k), (100 * $k))
    $sh.AddArc((84 * $k), (52 * $k), (88 * $k), (96 * $k), 180, 180)
    $sh.AddLine((172 * $k), (100 * $k), (172 * $k), (124 * $k))
    $g.DrawPath($pen, $sh)

    # Body
    $body = New-RoundRect (58 * $k) (112 * $k) (140 * $k) (100 * $k) (20 * $k)
    $g.FillPath([System.Drawing.Brushes]::White, $body)

    # Keyhole (skipped at tiny sizes where it turns to mush)
    if ($s -ge 24) {
        $kb = New-Object System.Drawing.SolidBrush $blueBottom
        $g.FillEllipse($kb, (114 * $k), (140 * $k), (28 * $k), (28 * $k))
        $g.FillRectangle($kb, (121 * $k), (156 * $k), (14 * $k), (30 * $k))
    }

    $g.Dispose()
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    $images += , @($s, $ms.ToArray())
}

$out = Join-Path $PSScriptRoot "app.ico"
$fs = [System.IO.File]::Create($out)
$w = New-Object System.IO.BinaryWriter $fs
$w.Write([UInt16]0); $w.Write([UInt16]1); $w.Write([UInt16]$images.Count)
$offset = 6 + 16 * $images.Count
foreach ($img in $images) {
    $sz = $img[0]; $data = $img[1]
    $dim = if ($sz -ge 256) { 0 } else { $sz }
    $w.Write([Byte]$dim); $w.Write([Byte]$dim); $w.Write([Byte]0); $w.Write([Byte]0)
    $w.Write([UInt16]1); $w.Write([UInt16]32)
    $w.Write([UInt32]$data.Length); $w.Write([UInt32]$offset)
    $offset += $data.Length
}
foreach ($img in $images) { $w.Write([byte[]]$img[1]) }
$w.Close()
Write-Output "Wrote $out"
