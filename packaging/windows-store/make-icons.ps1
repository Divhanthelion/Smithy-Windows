# Draws the package's tile and Store icons into Assets\.
#
# A placeholder in the app's own colours (the gold serif of the SMITHY title bar
# on its dark navy), drawn at 1024 px and scaled down for each size the manifest
# names. Replace Assets\*.png with a designed icon at the same sizes before
# launch; nothing else needs to change.
#
#   powershell -ExecutionPolicy Bypass -File packaging\windows-store\make-icons.ps1
param([string]$Out = (Join-Path $PSScriptRoot "Assets"))

Add-Type -AssemblyName System.Drawing
New-Item -ItemType Directory -Force $Out | Out-Null

$navy = [System.Drawing.Color]::FromArgb(255, 17, 17, 27)
$gold = [System.Drawing.Color]::FromArgb(255, 214, 175, 96)
$goldDark = [System.Drawing.Color]::FromArgb(255, 150, 116, 52)

function New-Mark([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap $size, $size
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = 'AntiAlias'
    $g.TextRenderingHint = 'AntiAliasGridFit'
    $g.Clear([System.Drawing.Color]::Transparent)
    # Rounded square with a thin gold rim.
    $r = [int]($size * 0.18); $pad = [int]($size * 0.04)
    $rect = New-Object System.Drawing.Rectangle $pad, $pad, ($size - 2 * $pad), ($size - 2 * $pad)
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $path.AddArc($rect.X, $rect.Y, 2 * $r, 2 * $r, 180, 90)
    $path.AddArc($rect.Right - 2 * $r, $rect.Y, 2 * $r, 2 * $r, 270, 90)
    $path.AddArc($rect.Right - 2 * $r, $rect.Bottom - 2 * $r, 2 * $r, 2 * $r, 0, 90)
    $path.AddArc($rect.X, $rect.Bottom - 2 * $r, 2 * $r, 2 * $r, 90, 90)
    $path.CloseFigure()
    $g.FillPath((New-Object System.Drawing.SolidBrush $navy), $path)
    $g.DrawPath((New-Object System.Drawing.Pen $goldDark, ([float]($size * 0.025))), $path)
    # The letter.
    $font = New-Object System.Drawing.Font "Georgia", ([float]($size * 0.62)), ([System.Drawing.FontStyle]::Bold), ([System.Drawing.GraphicsUnit]::Pixel)
    $fmt = New-Object System.Drawing.StringFormat
    $fmt.Alignment = 'Center'; $fmt.LineAlignment = 'Center'
    $box = New-Object System.Drawing.RectangleF 0, ([float]($size * 0.02)), $size, $size
    $g.DrawString("S", $font, (New-Object System.Drawing.SolidBrush $gold), $box, $fmt)
    $g.Dispose()
    return $bmp
}

function Save-Scaled($src, [int]$w, [int]$h, [string]$name) {
    $dst = New-Object System.Drawing.Bitmap $w, $h
    $g = [System.Drawing.Graphics]::FromImage($dst)
    $g.InterpolationMode = 'HighQualityBicubic'
    $g.SmoothingMode = 'AntiAlias'
    $g.Clear([System.Drawing.Color]::Transparent)
    $side = [Math]::Min($w, $h)
    $g.DrawImage($src, [int](($w - $side) / 2), [int](($h - $side) / 2), $side, $side)
    $g.Dispose()
    $dst.Save((Join-Path $Out $name), [System.Drawing.Imaging.ImageFormat]::Png)
    $dst.Dispose()
}

$mark = New-Mark 1024
Save-Scaled $mark 44 44 "Square44x44Logo.png"
Save-Scaled $mark 88 88 "Square44x44Logo.scale-200.png"
Save-Scaled $mark 256 256 "Square44x44Logo.targetsize-256.png"
Save-Scaled $mark 71 71 "Square71x71Logo.png"
Save-Scaled $mark 150 150 "Square150x150Logo.png"
Save-Scaled $mark 300 300 "Square150x150Logo.scale-200.png"
Save-Scaled $mark 310 150 "Wide310x150Logo.png"
Save-Scaled $mark 310 310 "Square310x310Logo.png"
Save-Scaled $mark 50 50 "StoreLogo.png"
Save-Scaled $mark 100 100 "StoreLogo.scale-200.png"
# For the Store listing itself (Partner Center asks for a 1:1 logo of at least 300 px).
Save-Scaled $mark 1080 1080 "..\listing-logo-1080.png"
$mark.Dispose()
Get-ChildItem $Out | Select-Object Name, Length
