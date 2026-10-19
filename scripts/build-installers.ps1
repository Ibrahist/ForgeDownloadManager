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

# 3. Stage Desktop Application & Installers
Write-Host "`n[3/5] Staging Desktop Application & Installers..." -ForegroundColor Green
if (-not $SkipDesktop) {
    $DesktopSrc = Join-Path $RootDir "apps\desktop\src-tauri\target\release\forge-dm-desktop.exe"
    $DesktopDest = Join-Path $DistDir "forge-dm-desktop-windows-x86_64.exe"
    if (Test-Path $DesktopSrc) {
        Copy-Item -Path $DesktopSrc -Destination $DesktopDest -Force
        Write-Host "      Desktop binary staged: $DesktopDest" -ForegroundColor Gray
    }

    $DllSrc = Join-Path $RootDir "apps\desktop\src-tauri\target\release\WebView2Loader.dll"
    if (-not (Test-Path $DllSrc)) { $DllSrc = Join-Path $RootDir "export\WebView2Loader.dll" }
    $DllDest = Join-Path $DistDir "WebView2Loader.dll"
    if (Test-Path $DllSrc) {
        Copy-Item -Path $DllSrc -Destination $DllDest -Force
        Write-Host "      WebView2 runtime loader staged: $DllDest" -ForegroundColor Gray
    }

    $TauriBundleDir = Join-Path $RootDir "apps\desktop\src-tauri\target\release\bundle"
    if (Test-Path $TauriBundleDir) {
        Get-ChildItem -Path $TauriBundleDir -Recurse -Include *.exe,*.msi,*.zip | ForEach-Object {
            Copy-Item -Path $_.FullName -Destination $DistDir -Force
            Write-Host "      Bundle staged: $($_.Name)" -ForegroundColor Gray
        }
    }

    # Create standalone portable desktop zip (contains .exe + WebView2Loader.dll)
    if ((Test-Path $DesktopDest) -and (Test-Path $DllDest)) {
        $PortableStageDir = Join-Path $RootDir "target\portable-desktop-staging"
        if (Test-Path $PortableStageDir) { Remove-Item -Path $PortableStageDir -Recurse -Force }
        New-Item -ItemType Directory -Path $PortableStageDir -Force | Out-Null
        Copy-Item -Path $DesktopDest -Destination (Join-Path $PortableStageDir "forge-dm-desktop.exe") -Force
        Copy-Item -Path $DllDest -Destination (Join-Path $PortableStageDir "WebView2Loader.dll") -Force
        "Launch 'forge-dm-desktop.exe' to run Forge Download Manager without installation." | Out-File (Join-Path $PortableStageDir "README.txt") -Encoding utf8
        
        $PortableZipDest = Join-Path $DistDir "forge-dm-desktop-windows-x86_64-portable.zip"
        if (Test-Path $PortableZipDest) { Remove-Item -Path $PortableZipDest -Force }
        Compress-Archive -Path "$PortableStageDir\*" -DestinationPath $PortableZipDest -Force
        Remove-Item -Path $PortableStageDir -Recurse -Force
        Write-Host "      Portable Desktop ZIP created: $PortableZipDest" -ForegroundColor Gray
    }
} else {
    Write-Host "      Skipping desktop bundle build (-SkipDesktop)." -ForegroundColor Gray
}

# Stage installer scripts
Copy-Item -Path (Join-Path $RootDir "scripts\install.ps1") -Destination (Join-Path $DistDir "install.ps1") -Force
Copy-Item -Path (Join-Path $RootDir "scripts\install.sh") -Destination (Join-Path $DistDir "install.sh") -Force
Write-Host "      Installer scripts staged: install.ps1, install.sh" -ForegroundColor Gray

# 4. Package complete standalone bundle
Write-Host "`n[4/5] Packaging All-in-One Installer Bundle..." -ForegroundColor Green
$BundleStageDir = Join-Path $RootDir "target\bundle-staging"
if (Test-Path $BundleStageDir) { Remove-Item -Path $BundleStageDir -Recurse -Force }
New-Item -ItemType Directory -Path $BundleStageDir -Force | Out-Null

$BundleInstallersDir = Join-Path $BundleStageDir "installers"
New-Item -ItemType Directory -Path $BundleInstallersDir -Force | Out-Null
Copy-Item -Path $CliDest -Destination $BundleInstallersDir -Force
if (Test-Path (Join-Path $DistDir "forge-dm-desktop-windows-x86_64.exe")) {
    Copy-Item -Path (Join-Path $DistDir "forge-dm-desktop-windows-x86_64.exe") -Destination $BundleInstallersDir -Force
}
if (Test-Path (Join-Path $DistDir "WebView2Loader.dll")) {
    Copy-Item -Path (Join-Path $DistDir "WebView2Loader.dll") -Destination $BundleInstallersDir -Force
}
Copy-Item -Path (Join-Path $DistDir "install.ps1") -Destination $BundleInstallersDir -Force
Copy-Item -Path (Join-Path $DistDir "install.sh") -Destination $BundleInstallersDir -Force

$BundleExtDir = Join-Path $BundleStageDir "browser-extension"
New-Item -ItemType Directory -Path $BundleExtDir -Force | Out-Null
Copy-Item -Path $ExtZipDest -Destination $BundleExtDir -Force

$UnpackedExtDir = Join-Path $RootDir "export\browser-extension\unpacked"
if (Test-Path $UnpackedExtDir) {
    Copy-Item -Path $UnpackedExtDir -Destination (Join-Path $BundleExtDir "unpacked") -Recurse -Force
}

Copy-Item -Path (Join-Path $RootDir "export\README.md") -Destination (Join-Path $BundleStageDir "README.md") -Force
Copy-Item -Path (Join-Path $RootDir "LICENSE") -Destination (Join-Path $BundleStageDir "LICENSE") -Force

$BundleZipDest = Join-Path $DistDir "forge-dm-v0.1.1-windows-x86_64-installer-bundle.zip"
if (Test-Path $BundleZipDest) { Remove-Item -Path $BundleZipDest -Force }
Compress-Archive -Path "$BundleStageDir\*" -DestinationPath $BundleZipDest -Force
Remove-Item -Path $BundleStageDir -Recurse -Force
Write-Host "      Installer bundle created: $BundleZipDest" -ForegroundColor Gray

# 5. Generate SHA256 Checksums
Write-Host "`n[5/5] Generating SHA256SUMS.txt..." -ForegroundColor Green
$ChecksumFile = Join-Path $DistDir "SHA256SUMS.txt"
$ChecksumLines = @()

Get-ChildItem -Path $DistDir -File | Where-Object { $_.Name -ne "SHA256SUMS.txt" } | Sort-Object Name | ForEach-Object {
    $hash = (Get-FileHash -Path $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
    $line = "$hash  $($_.Name)"
    $ChecksumLines += $line
    Write-Host "  $line" -ForegroundColor Gray
}

$ChecksumLines | Out-File -FilePath $ChecksumFile -Encoding utf8
Write-Host "`nSUCCESS: All release assets staged in: $DistDir" -ForegroundColor Green
