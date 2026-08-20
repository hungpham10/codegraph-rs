# codegraph install script for Windows
#
# Usage (latest release, one-liner):
#   irm https://raw.githubusercontent.com/hungpham10/codegraph-rs/main/scripts/install.ps1 | iex
#
# Usage (pin a version / download the script first):
#   irm https://raw.githubusercontent.com/hungpham10/codegraph-rs/main/scripts/install.ps1 -OutFile install.ps1
#   .\install.ps1 -Version 2.0.2

[CmdletBinding()]
param(
    # Pin a specific version, e.g. "2.0.2". Empty = latest release.
    [string]$Version
)

$ErrorActionPreference = 'Stop'

$Repo       = 'hungpham10/codegraph-rs'
$BinName    = 'codegraph.exe'
$Target     = 'x86_64-pc-windows-msvc'
$AssetName  = "codegraph-$Target.zip"

# Install dir: $CODEGRAPH_INSTALL_DIR or %LOCALAPPDATA%\codegraph\bin
$InstallDir = if ($env:CODEGRAPH_INSTALL_DIR) {
    $env:CODEGRAPH_INSTALL_DIR
} else {
    Join-Path $env:LOCALAPPDATA 'codegraph\bin'
}

# Detect architecture (only x86_64 is supported on Windows for now).
$arch = (Get-CimInstance Win32_Processor).AddressWidth
if ($arch -ne 64) {
    Write-Error "Only x86_64 is supported on Windows."
    exit 1
}

# Resolve the release tag.
if ($Version) {
    $Tag = if ($Version.StartsWith('v')) { $Version } else { "v$Version" }
    # Verify the release exists before downloading.
    $release = Invoke-RestMethod "https://api.github.com/repos/$Repo/releases/tags/$Tag"
    if (-not $release) {
        Write-Error "Release $Tag not found."
        exit 1
    }
} else {
    Write-Host "Fetching latest release..."
    $release = Invoke-RestMethod "https://api.github.com/repos/$Repo/releases/latest"
    $Tag = $release.tag_name
    if (-not $Tag) {
        Write-Error "Could not detect latest release tag."
        exit 1
    }
}

$Url = "https://github.com/$Repo/releases/download/$Tag/$AssetName"

# Download into a temp dir.
$TmpDir = Join-Path $env:TEMP "codegraph-install-$(Get-Random)"
New-Item -ItemType Directory -Path $TmpDir | Out-Null
$ZipPath = Join-Path $TmpDir $AssetName

Write-Host "Downloading $Url"
Invoke-WebRequest -Uri $Url -OutFile $ZipPath -UseBasicParsing

if (-not (Test-Path $ZipPath) -or ((Get-Item $ZipPath).Length -eq 0)) {
    Remove-Item -Recurse -Force $TmpDir
    Write-Error "Download failed: $AssetName is missing or empty. Check that release $Tag ships a Windows build."
    exit 1
}

# Extract.
Expand-Archive -Path $ZipPath -DestinationPath $TmpDir -Force

$BinSrc = Join-Path $TmpDir $BinName
if (-not (Test-Path $BinSrc)) {
    Remove-Item -Recurse -Force $TmpDir
    Write-Error "Asset $AssetName did not contain $BinName."
    exit 1
}

# Install.
if (-not (Test-Path $InstallDir)) {
    New-Item -ItemType Directory -Path $InstallDir | Out-Null
}
Copy-Item -Path $BinSrc -Destination (Join-Path $InstallDir $BinName) -Force

# Remove the Mark-of-the-Web so SmartScreen / Defender don't block the binary
# (we don't code-sign the prebuilt binary, so a downloaded file is flagged).
try { Unblock-File -Path (Join-Path $InstallDir $BinName) -ErrorAction SilentlyContinue } catch { }

# Cleanup.
Remove-Item -Recurse -Force $TmpDir

# Verify the binary runs.
$Installed = Join-Path $InstallDir $BinName
if (-not (Test-Path $Installed)) {
    Write-Error "Installation failed: $Installed not found."
    exit 1
}

Write-Host "Installed codegraph $Tag to $InstallDir"

# Add to user PATH if not already present.
$UserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($UserPath -notlike "*$InstallDir*") {
    [Environment]::SetEnvironmentVariable('Path', "$UserPath;$InstallDir", 'User')
    Write-Host "Added $InstallDir to user PATH. Restart your terminal to apply."
} else {
    Write-Host "$InstallDir is already in PATH."
}
