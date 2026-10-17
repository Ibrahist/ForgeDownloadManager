<#
.SYNOPSIS
    Builds distribution installers, standalone binaries, and extension packages for Forge Download Manager.
.PARAMETER SkipDesktop
    Skip building Tauri desktop installers.
.PARAMETER Toolchain
    Rust toolchain override (default: stable-x86_64-pc-windows-gnu on Windows if present).
#>
[CmdletBinding()]
param(
    [switch]$SkipDesktop = $false,
    [string]$Toolchain = ""
)

$ErrorActionPreference = 'Stop'

$RootDir = Resolve-Path (Join-Path $PSScriptRoot "..")
$DistDir = Join-Path $RootDir "dist\installers"

Write-Host "==========================================================" -ForegroundColor Cyan
Write-Host "       Forge Download Manager - Build Release Assets      " -ForegroundColor White
Write-Host "==========================================================" -ForegroundColor Cyan
Write-Host "Output Directory: $DistDir" -ForegroundColor Gray

# Ensure clean output directory
if (-not (Test-Path $DistDir)) {
    New-Item -ItemType Directory -Path $DistDir -Force | Out-Null
}

# Determine Rust toolchain
$CargoArgs = @("build", "--release", "-p", "forgedownloadmanager-cli")
$ToolchainPrefix = @()
if ($Toolchain) {
    $ToolchainPrefix = @("+$Toolchain")
} elseif (Get-Command "gcc" -ErrorAction SilentlyContinue) {
    # If GCC is on path on Windows, use GNU toolchain
    $ToolchainPrefix = @("+stable-x86_64-pc-windows-gnu")
}

# 1. Build CLI binary
Write-Host "`n[1/4] Building forge-dm CLI release binary..." -ForegroundColor Green
& cargo @ToolchainPrefix @CargoArgs
if ($LASTEXITCODE -ne 0) { throw "Cargo build failed" }

$CliSrc = Join-Path $RootDir "target\release\forge-dm.exe"
$CliDest = Join-Path $DistDir "forge-dm-windows-x86_64.exe"
Copy-Item -Path $CliSrc -Destination $CliDest -Force
Write-Host "      CLI binary staged: $CliDest" -ForegroundColor Gray

# 2. Package browser extension
Write-Host "`n[2/4] Packaging Browser Extension..." -ForegroundColor Green
& node (Join-Path $RootDir "scripts\package-extension.mjs")
$ExtZipSrc = Join-Path $RootDir "dist\forge-dm-extension.zip"
$ExtZipDest = Join-Path $DistDir "forge-dm-extension.zip"
if (Test-Path $ExtZipSrc) {
    Copy-Item -Path $ExtZipSrc -Destination $ExtZipDest -Force
    Write-Host "      Extension archive staged: $ExtZipDest" -ForegroundColor Gray
}

# 3. Build Desktop Application & Installers
if (-not $SkipDesktop) {
    Write-Host "`n[3/4] Building Desktop Web frontend..." -ForegroundColor Green
    npm --prefix (Join-Path $RootDir "apps\desktop") run build

    Write-Host "      Checking Tauri bundle..." -ForegroundColor Green
    # Check if we can run tauri build
    try {
        & npm --prefix (Join-Path $RootDir "apps\desktop") run tauri -- build
        $TauriBundleDir = Join-Path $RootDir "apps\desktop\src-tauri\target\release\bundle"
        if (Test-Path $TauriBundleDir) {
            Get-ChildItem -Path $TauriBundleDir -Recurse -Include *.exe,*.msi,*.zip | ForEach-Object {
                Copy-Item -Path $_.FullName -Destination $DistDir -Force
                Write-Host "      Installer staged: $($_.Name)" -ForegroundColor Gray
            }
        }
    } catch {
        Write-Warning "Tauri bundle build skipped or requires WiX/NSIS on system: $_"
    }
} else {
    Write-Host "`n[3/4] Skipping desktop bundle build (-SkipDesktop)." -ForegroundColor Gray
}

# 4. Generate SHA256 Checksums
Write-Host "`n[4/4] Generating SHA256SUMS.txt..." -ForegroundColor Green
$ChecksumFile = Join-Path $DistDir "SHA256SUMS.txt"
$ChecksumLines = @()

Get-ChildItem -Path $DistDir -File | Where-Object { $_.Name -ne "SHA256SUMS.txt" } | ForEach-Object {
    $hash = (Get-FileHash -Path $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
    $line = "$hash  $($_.Name)"
    $ChecksumLines += $line
    Write-Host "  $line" -ForegroundColor Gray
}

$ChecksumLines | Out-File -FilePath $ChecksumFile -Encoding utf8
Write-Host "`nSUCCESS: All release assets staged in: $DistDir" -ForegroundColor Green
