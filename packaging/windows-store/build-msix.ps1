# Build Smithy and pack it as an MSIX for the Microsoft Store.
#
#   powershell -ExecutionPolicy Bypass -File packaging\windows-store\build-msix.ps1 `
#       -IdentityName "12345Publisher.Smithy" -Publisher "CN=XXXXXXXX-XXXX-..." `
#       -PublisherDisplayName "Your Name"
#
# The three identity values come from Partner Center (Product management >
# Product identity) once the name is reserved. Without them the package gets
# placeholder values: fine for installing on this PC to test, refused by the
# Store.
#
# Output: target\msix\Smithy_<version>_x64.msix (upload this to Partner Center)
# and target\msix\layout\ (the unpacked package, for a local test install).
param(
    [string]$IdentityName = "Smithy.Dev",
    [string]$Publisher = "CN=SmithyDev",
    [string]$PublisherDisplayName = "Smithy (development build)",
    [string]$DisplayName = "Smithy",
    # The Store needs four parts with the last one 0; default: the workspace
    # version from Cargo.toml plus ".0".
    [string]$Version = "",
    [switch]$SkipBuild
)
$ErrorActionPreference = "Stop"
$repo = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$here = $PSScriptRoot
$out = Join-Path $repo "target\msix"
$layout = Join-Path $out "layout"

if (-not $Version) {
    $line = Select-String -Path (Join-Path $repo "Cargo.toml") -Pattern '^version\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"' | Select-Object -First 1
    $Version = $line.Matches[0].Groups[1].Value + ".0"
}
if ($Version -notmatch '^\d+\.\d+\.\d+\.0$') { throw "Version must look like 1.2.3.0 (the Store reserves the last part): $Version" }

# makeappx.exe from the newest Windows 10/11 SDK.
$makeappx = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin" -Recurse -Filter makeappx.exe -ErrorAction SilentlyContinue |
    Where-Object { $_.FullName -like "*\x64\*" } | Sort-Object FullName | Select-Object -Last 1
if (-not $makeappx) { throw "makeappx.exe not found: install the Windows SDK (winget install Microsoft.WindowsSDK.10.0.26100)" }

if (-not $SkipBuild) {
    Push-Location $repo
    try {
        # .cargo\config.toml links the C runtime statically, so the package
        # needs no Visual C++ Redistributable.
        cargo build --release -p smithy
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
    } finally { Pop-Location }
}
$exe = Join-Path $repo "target\release\smithy.exe"
if (-not (Test-Path $exe)) { throw "no $exe" }

if (-not (Test-Path (Join-Path $here "Assets\Square150x150Logo.png"))) {
    & (Join-Path $here "make-icons.ps1") | Out-Null
}

# Stage the package.
if (Test-Path $layout) { Remove-Item -Recurse -Force $layout }
New-Item -ItemType Directory -Force (Join-Path $layout "Assets") | Out-Null
Copy-Item $exe $layout
Copy-Item (Join-Path $here "Assets\*.png") (Join-Path $layout "Assets")
Copy-Item (Join-Path $repo "LICENSE") $layout

$manifest = Get-Content (Join-Path $here "AppxManifest.xml") -Raw
$manifest = $manifest.Replace("{{IDENTITY_NAME}}", $IdentityName).
    Replace("{{PUBLISHER}}", [System.Security.SecurityElement]::Escape($Publisher)).
    Replace("{{PUBLISHER_DISPLAY_NAME}}", [System.Security.SecurityElement]::Escape($PublisherDisplayName)).
    Replace("{{DISPLAY_NAME}}", [System.Security.SecurityElement]::Escape($DisplayName)).
    Replace("{{VERSION}}", $Version)
if ($manifest -match '\{\{') { throw "a manifest placeholder was not filled" }
[System.IO.File]::WriteAllText((Join-Path $layout "AppxManifest.xml"), $manifest, (New-Object System.Text.UTF8Encoding $false))

$msix = Join-Path $out "Smithy_${Version}_x64.msix"
& $makeappx.FullName pack /o /d $layout /p $msix
if ($LASTEXITCODE -ne 0) { throw "makeappx failed" }

Write-Host ""
Write-Host "Package: $msix"
Write-Host "Identity: $IdentityName / $Publisher / $Version"
Write-Host ""
Write-Host "To try it on this PC (needs Settings > System > For developers > Developer Mode):"
Write-Host "  Add-AppxPackage -Register `"$layout\AppxManifest.xml`""
Write-Host "To remove it again:"
Write-Host "  Get-AppxPackage $IdentityName | Remove-AppxPackage"
