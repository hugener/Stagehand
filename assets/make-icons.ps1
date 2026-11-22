<#
.SYNOPSIS
    Makes every icon file Stagehand uses from the two drawings in this folder.

.DESCRIPTION
    logo.svg is the logo; logo-small.svg is the same hand drawn bolder for the smallest
    sizes. This script renders both with Microsoft Edge, which every Windows machine
    has, and writes:

        packaging\Assets\*.png              the logos the Store package names
        crates\stagehand\assets\stagehand.ico   the icon of stagehand.exe
        crates\stagehand\assets\tray.rgba       the notification area icon, 32 x 32

    The .rgba file is bare pixels, four bytes each, which the app can use without
    carrying an image decoder. The results are checked in, so this only has to be run
    again when a drawing changes.
#>
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$root = Split-Path $PSScriptRoot -Parent
$packageAssets = Join-Path $root 'packaging\Assets'
$appAssets = Join-Path $root 'crates\stagehand\assets'
New-Item $packageAssets, $appAssets -ItemType Directory -Force | Out-Null

$edge = @("${env:ProgramFiles(x86)}\Microsoft\Edge\Application\msedge.exe", "$env:ProgramFiles\Microsoft\Edge\Application\msedge.exe") |
    Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $edge) { throw 'Microsoft Edge was not found; it is used to render the drawings.' }

$work = Join-Path ([IO.Path]::GetTempPath()) "stagehand-icons-$PID"
New-Item $work -ItemType Directory -Force | Out-Null

# Renders a drawing once, large, to scale down from.
function Render([string]$name) {
    $master = 1024
    $page = Join-Path $work "$name.html"
    $picture = Join-Path $work "$name.png"
    $svg = Get-Content (Join-Path $PSScriptRoot "$name.svg") -Raw
    # A page of exactly the drawing, so nothing but the drawing is in the picture.
    "<!doctype html><html><body style='margin:0;background:transparent;overflow:hidden'><div style='width:${master}px;height:${master}px'>$svg</div></body></html>" |
        Set-Content $page -Encoding utf8
    $arguments = @(
        '--headless=new', '--disable-gpu', '--hide-scrollbars', '--force-device-scale-factor=1',
        '--default-background-color=00000000', "--window-size=$master,$master",
        "--user-data-dir=$(Join-Path $work 'profile')", "--screenshot=$picture",
        ([Uri]$page).AbsoluteUri
    )
    Start-Process $edge -ArgumentList $arguments -Wait -WindowStyle Hidden
    if (-not (Test-Path $picture)) { throw "Edge did not render $name.svg." }
    $bitmap = [System.Drawing.Bitmap]::FromFile($picture)
    if ($bitmap.Width -ne $master -or $bitmap.Height -ne $master) {
        throw "Edge rendered $name.svg at $($bitmap.Width) x $($bitmap.Height) instead of $master x $master."
    }
    $bitmap
}

function Scale([System.Drawing.Bitmap]$source, [int]$size) {
    $bitmap = New-Object System.Drawing.Bitmap $size, $size, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.InterpolationMode = 'HighQualityBicubic'
    $graphics.PixelOffsetMode = 'HighQuality'
    $graphics.CompositingQuality = 'HighQuality'
    $attributes = New-Object System.Drawing.Imaging.ImageAttributes
    # Without this the edge pixels are blended with a repeat of the opposite edge.
    $attributes.SetWrapMode([System.Drawing.Drawing2D.WrapMode]::TileFlipXY)
    $whole = New-Object System.Drawing.Rectangle 0, 0, $size, $size
    $graphics.DrawImage($source, $whole, 0, 0, $source.Width, $source.Height, [System.Drawing.GraphicsUnit]::Pixel, $attributes)
    $graphics.Dispose(); $attributes.Dispose()
    $bitmap
}

# The drawing to use at a size: the bold one where the fine one would blur.
function Sized([int]$size) {
    $drawing = if ($size -le 32) { $small } else { $logo }
    Scale $drawing $size
}

function PngBytes([System.Drawing.Bitmap]$bitmap) {
    $stream = New-Object IO.MemoryStream
    $bitmap.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
    , $stream.ToArray()
}

function SavePixels([int]$size, [string]$path) {
    $bitmap = Sized $size
    $bytes = New-Object byte[] ($size * $size * 4)
    for ($y = 0; $y -lt $size; $y++) {
        for ($x = 0; $x -lt $size; $x++) {
            $pixel = $bitmap.GetPixel($x, $y)
            $at = ($y * $size + $x) * 4
            $bytes[$at] = $pixel.R; $bytes[$at + 1] = $pixel.G; $bytes[$at + 2] = $pixel.B; $bytes[$at + 3] = $pixel.A
        }
    }
    $bitmap.Dispose()
    [IO.File]::WriteAllBytes($path, $bytes)
}

try {
    $logo = Render 'logo'
    $small = Render 'logo-small'

    # The logos the package manifest names.
    foreach ($entry in @{ 'StoreLogo.png' = 50; 'Square44x44Logo.png' = 44; 'Square150x150Logo.png' = 150 }.GetEnumerator()) {
        $bitmap = Sized $entry.Value
        $bitmap.Save((Join-Path $packageAssets $entry.Key), [System.Drawing.Imaging.ImageFormat]::Png)
        $bitmap.Dispose()
    }

    # The icon of the program: one file holding a picture for each size Windows asks for.
    $sizes = 16, 20, 24, 32, 40, 48, 64, 128, 256
    $pictures = foreach ($size in $sizes) { $bitmap = Sized $size; , (PngBytes $bitmap); $bitmap.Dispose() }
    $stream = New-Object IO.MemoryStream
    $writer = New-Object IO.BinaryWriter $stream
    $writer.Write([uint16]0); $writer.Write([uint16]1); $writer.Write([uint16]$sizes.Count)
    $offset = 6 + 16 * $sizes.Count
    for ($index = 0; $index -lt $sizes.Count; $index++) {
        # A size of 256 is written as 0.
        $side = [byte]($sizes[$index] % 256)
        $writer.Write($side); $writer.Write($side); $writer.Write([byte]0); $writer.Write([byte]0)
        $writer.Write([uint16]1); $writer.Write([uint16]32)
        $writer.Write([uint32]$pictures[$index].Length); $writer.Write([uint32]$offset)
        $offset += $pictures[$index].Length
    }
    foreach ($picture in $pictures) { $writer.Write($picture) }
    $writer.Flush()
    [IO.File]::WriteAllBytes((Join-Path $appAssets 'stagehand.ico'), $stream.ToArray())

    SavePixels 32 (Join-Path $appAssets 'tray.rgba')

    $logo.Dispose(); $small.Dispose()
    Write-Host "Made the icons in $packageAssets and $appAssets"
}
finally {
    Remove-Item $work -Recurse -Force -ErrorAction Ignore
}
