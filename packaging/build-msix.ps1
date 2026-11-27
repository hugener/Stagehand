<#
.SYNOPSIS
    Builds the Stagehand package for the Microsoft Store.

.DESCRIPTION
    Builds stagehand.exe, lays it out next to the package manifest and the logos, and
    packs that into an .msix file under target\msix.

    The package is not signed. The Store signs what is submitted to it, so nothing more
    is needed for a Store release. Windows will not install an unsigned package, so to
    try the packaged app on this machine use -Register, which registers the layout
    folder directly. That needs Developer Mode (Settings > System > For developers).

.PARAMETER Architecture
    x64 or arm64. Defaults to the architecture of this machine.

.PARAMETER IdentityName
    The package identity name. For a Store release, use the value shown in Partner
    Center under Product identity.

.PARAMETER Publisher
    The package publisher. For a Store release, use the "CN=..." value shown in Partner
    Center under Product identity.

.PARAMETER PublisherDisplayName
    The publisher name shown to users. For a Store release, use the value shown in
    Partner Center under Product identity.

.PARAMETER Version
    The package version, as four numbers. Defaults to the version in Cargo.toml with a
    fourth number of 0, which the Store requires.

.PARAMETER SkipBuild
    Package the stagehand.exe that is already built instead of building it.

.PARAMETER Register
    Register the layout folder as an installed app for the current user, replacing a
    previous registration. Remove it again with:
        Get-AppxPackage -Name <IdentityName> | Remove-AppxPackage

.EXAMPLE
    packaging\build-msix.ps1 -Register

.EXAMPLE
    packaging\build-msix.ps1 -IdentityName 12345Publisher.Stagehand -Publisher 'CN=0A1B2C3D-...' -PublisherDisplayName 'Publisher'
#>
[CmdletBinding()]
param(
    [ValidateSet('x64', 'arm64')]
    [string]$Architecture,
    [string]$IdentityName = 'Stagehand.Development',
    [string]$Publisher = 'CN=Stagehand Development',
    [string]$PublisherDisplayName = 'Stagehand Development',
    [ValidatePattern('^\d+\.\d+\.\d+\.\d+$')]
    [string]$Version,
    [switch]$SkipBuild,
    [switch]$Register
)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent

$hostArchitecture = switch ([Runtime.InteropServices.RuntimeInformation]::OSArchitecture) {
    'X64' { 'x64' }
    'Arm64' { 'arm64' }
    default { throw "Stagehand cannot be packaged on a $_ machine." }
}
if (-not $Architecture) { $Architecture = $hostArchitecture }

if (-not $Version) {
    $cargo = Get-Content (Join-Path $root 'Cargo.toml') -Raw
    if ($cargo -notmatch '(?ms)^\[workspace\.package\].*?^version\s*=\s*"(\d+\.\d+\.\d+)"') {
        throw 'Could not find the version in Cargo.toml.'
    }
    $Version = "$($Matches[1]).0"
}

# Building for the architecture of this machine uses the ordinary target folder, so the
# build is shared with everyday development. Any other architecture gets its own.
$target = @{ x64 = 'x86_64-pc-windows-msvc'; arm64 = 'aarch64-pc-windows-msvc' }[$Architecture]
$crossBuild = $Architecture -ne $hostArchitecture
$releaseFolder = if ($crossBuild) { "target\$target\release" } else { 'target\release' }
$executable = Join-Path $root "$releaseFolder\stagehand.exe"

if (-not $SkipBuild) {
    $arguments = @('build', '--release', '--locked', '-p', 'stagehand')
    if ($crossBuild) { $arguments += @('--target', $target) }
    Push-Location $root
    try {
        & cargo @arguments
        if ($LASTEXITCODE -ne 0) { throw 'The build failed.' }
    }
    finally { Pop-Location }
}
if (-not (Test-Path $executable)) {
    throw "There is no $executable. Run without -SkipBuild to build it."
}

$output = Join-Path $root 'target\msix'
$layout = Join-Path $output "layout-$Architecture"
if (Test-Path $layout) { Remove-Item $layout -Recurse -Force }
New-Item (Join-Path $layout 'Assets') -ItemType Directory -Force | Out-Null

Copy-Item $executable $layout
Copy-Item (Join-Path $PSScriptRoot 'Assets\*.png') (Join-Path $layout 'Assets')

$manifest = Get-Content (Join-Path $PSScriptRoot 'AppxManifest.xml') -Raw
$values = @{
    IdentityName         = $IdentityName
    Publisher            = $Publisher
    PublisherDisplayName = $PublisherDisplayName
    Version              = $Version
    Architecture         = $Architecture
}
foreach ($name in $values.Keys) {
    $manifest = $manifest.Replace("{{$name}}", [Security.SecurityElement]::Escape($values[$name]))
}
if ($manifest -match '\{\{\w+\}\}') { throw "The manifest still has a placeholder: $($Matches[0])" }
# Without a byte order mark, which the packaging tool reads as part of the document.
[IO.File]::WriteAllText((Join-Path $layout 'AppxManifest.xml'), $manifest, [Text.UTF8Encoding]::new($false))

# The packaging tool comes with the Windows SDK; the newest one installed is used.
$kits = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
# Not every SDK has the tool built for ARM, where the x64 one runs just as well.
$makeAppx = Get-ChildItem $kits -Directory -Filter '10.*' -ErrorAction Ignore |
    Sort-Object { [version]$_.Name } -Descending |
    ForEach-Object { foreach ($tools in $hostArchitecture, 'x64', 'x86') { Join-Path $_.FullName "$tools\makeappx.exe" } } |
    Where-Object { Test-Path $_ } |
    Select-Object -First 1
if (-not $makeAppx) { throw "makeappx.exe was not found under $kits. Install the Windows SDK." }

$package = Join-Path $output "Stagehand_${Version}_$Architecture.msix"
& $makeAppx pack /d $layout /p $package /o | Out-Null
if ($LASTEXITCODE -ne 0) {
    # Run again with its output shown, which says what is wrong with the package.
    & $makeAppx pack /d $layout /p $package /o
    throw 'Packing failed.'
}
Write-Host "Packaged $package"

if ($Register) {
    if ($crossBuild) { throw "A $Architecture package cannot be registered on this $hostArchitecture machine." }
    # Windows refuses to register a changed manifest over one of the same version, so an
    # earlier registration made this way is removed first. An installed copy from the
    # Store is not a development registration and is left alone.
    Get-AppxPackage -Name $IdentityName |
        Where-Object IsDevelopmentMode |
        Remove-AppxPackage
    Add-AppxPackage -Register (Join-Path $layout 'AppxManifest.xml') -ForceApplicationShutdown
    Write-Host "Registered $IdentityName for the current user. Start it from the Start menu."
}
