<#
.SYNOPSIS
    Forge Download Manager - Automated Windows Installer
.DESCRIPTION
    Installs forge-dm CLI, configures PATH, and registers Native Messaging Host
    for Google Chrome, Microsoft Edge, Brave, Vivaldi, and Mozilla Firefox.
.PARAMETER Version
    Specific version tag to install (defaults to 'latest').
.PARAMETER LocalBinary
    Path to a local forge-dm.exe binary (skips download).
#>
[CmdletBinding()]
param(
    [string]$Version = "latest",
    [string]$LocalBinary = ""
)

$ErrorActionPreference = 'Stop'

Write-Host "==========================================================" -ForegroundColor Cyan
Write-Host "         Forge Download Manager - Windows Installer       " -ForegroundColor White
Write-Host "==========================================================" -ForegroundColor Cyan
Write-Host ""

$AppName = "ForgeDownloadManager"
$BinName = "forge-dm.exe"
$InstallDir = Join-Path $env:LOCALAPPDATA "Programs\$AppName\bin"
$DestExe = Join-Path $InstallDir $BinName

# 1. Ensure install directory exists
if (-not (Test-Path $InstallDir)) {
    Write-Host "[1/5] Creating install directory: $InstallDir" -ForegroundColor Gray
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
} else {
    Write-Host "[1/5] Install directory ready: $InstallDir" -ForegroundColor Gray
}

# 2. Acquire binary (Local or GitHub Release)
if ($LocalBinary -and (Test-Path $LocalBinary)) {
    Write-Host "[2/5] Copying local binary from $LocalBinary..." -ForegroundColor Green
    Copy-Item -Path $LocalBinary -Destination $DestExe -Force
} else {
    $Repo = "forgedownloadmanager/forgedownloadmanager"
    if ($Version -eq "latest") {
        $DownloadUrl = "https://github.com/$Repo/releases/latest/download/forge-dm-windows-x86_64.exe"
    } else {
        $DownloadUrl = "https://github.com/$Repo/releases/download/$Version/forge-dm-windows-x86_64.exe"
    }

    # Check if building from repository workspace
    $WorkspaceBin = Join-Path $PSScriptRoot "..\target\release\$BinName"
    if (Test-Path $WorkspaceBin) {
        Write-Host "[2/5] Found local workspace binary: $WorkspaceBin" -ForegroundColor Green
        Copy-Item -Path $WorkspaceBin -Destination $DestExe -Force
    } else {
        Write-Host "[2/5] Downloading forge-dm ($Version) from GitHub..." -ForegroundColor Green
        Write-Host "      URL: $DownloadUrl" -ForegroundColor Gray
        try {
            [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12 -bor [Net.SecurityProtocolType]::Tls13
            Invoke-WebRequest -Uri $DownloadUrl -OutFile $DestExe -UseBasicParsing
        } catch {
            Write-Warning "Could not download from release asset. If building locally, run 'cargo build --release' first."
            throw $_
        }
    }
}

# 3. Add to user PATH
Write-Host "[3/5] Checking user PATH environment variable..." -ForegroundColor Gray
$UserPath = [Environment]::GetEnvironmentVariable("Path", [EnvironmentVariableTarget]::User)
$PathEntries = $UserPath -split ';' | Where-Object { $_ -ne '' }

if ($PathEntries -notcontains $InstallDir) {
    Write-Host "      Adding $InstallDir to User PATH..." -ForegroundColor Green
    $NewPath = ($PathEntries + $InstallDir) -join ';'
    [Environment]::SetEnvironmentVariable("Path", $NewPath, [EnvironmentVariableTarget]::User)
    $env:Path = "$env:Path;$InstallDir"
} else {
    Write-Host "      $InstallDir is already in User PATH." -ForegroundColor Gray
}

# 4. Register Native Messaging Host
Write-Host "[4/5] Registering Native Messaging Host for browsers..." -ForegroundColor Green
try {
    & $DestExe host --install
} catch {
    Write-Warning "Failed running 'forge-dm host --install': $_"
}

# 5. Verify installation
Write-Host "[5/5] Verifying installation..." -ForegroundColor Gray
try {
    $VerOutput = & $DestExe --version
    Write-Host ""
    Write-Host "SUCCESS: $VerOutput installed successfully!" -ForegroundColor Green
    Write-Host ""
    Write-Host "Getting Started:" -ForegroundColor White
    Write-Host "  1. CLI command:    forge-dm --help" -ForegroundColor Yellow
    Write-Host "  2. Media probe:    forge-dm media probe <url>" -ForegroundColor Yellow
    Write-Host "  3. Queue run:      forge-dm run" -ForegroundColor Yellow
    Write-Host "  4. Browser Ext:    Load apps/extension in Chrome/Edge/Firefox" -ForegroundColor Yellow
    Write-Host ""
} catch {
    Write-Warning "Verification returned an error: $_"
}
