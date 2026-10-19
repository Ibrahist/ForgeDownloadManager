<#
.SYNOPSIS
    Forge Download Manager - Automated Windows Installer
.DESCRIPTION
    Installs Forge Download Manager (Desktop GUI and CLI), bundles WebView2Loader.dll,
    configures user PATH, creates Desktop & Start Menu shortcuts, and registers Native Messaging Host
    for Google Chrome, Microsoft Edge, Brave, Vivaldi, and Mozilla Firefox.
.PARAMETER Version
    Specific version tag to install (defaults to 'latest').
.PARAMETER LocalBinary
    Path to a local forge-dm.exe binary (skips CLI download).
.PARAMETER SkipDesktop
    Skip installing the Desktop GUI application.
#>
[CmdletBinding()]
param(
    [string]$Version = "latest",
    [string]$LocalBinary = "",
    [switch]$SkipDesktop = $false
)

$ErrorActionPreference = 'Stop'

Write-Host "==========================================================" -ForegroundColor Cyan
Write-Host "         Forge Download Manager - Windows Installer       " -ForegroundColor White
Write-Host "==========================================================" -ForegroundColor Cyan
Write-Host ""

$AppName = "ForgeDownloadManager"
$InstallDir = Join-Path $env:LOCALAPPDATA "Programs\$AppName\bin"
$DestCliExe = Join-Path $InstallDir "forge-dm.exe"
$DestDesktopExe = Join-Path $InstallDir "forge-dm-desktop.exe"
$DestLoaderDll = Join-Path $InstallDir "WebView2Loader.dll"

# 1. Ensure install directory exists
if (-not (Test-Path $InstallDir)) {
    Write-Host "[1/6] Creating install directory: $InstallDir" -ForegroundColor Gray
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
} else {
    Write-Host "[1/6] Install directory ready: $InstallDir" -ForegroundColor Gray
}

$Repo = "Ibrahist/ForgeDownloadManager"
$BaseReleaseUrl = if ($Version -eq "latest") {
    "https://github.com/$Repo/releases/latest/download"
} else {
    "https://github.com/$Repo/releases/download/$Version"
}

# 2. Acquire CLI binary
Write-Host "[2/6] Staging CLI binary (forge-dm.exe)..." -ForegroundColor Green
if ($LocalBinary -and (Test-Path $LocalBinary)) {
    Write-Host "      Copying local CLI from $LocalBinary..." -ForegroundColor Gray
    Copy-Item -Path $LocalBinary -Destination $DestCliExe -Force
} else {
    $WorkspaceBin = Join-Path $PSScriptRoot "..\target\release\forge-dm.exe"
    if (Test-Path $WorkspaceBin) {
        Write-Host "      Using local workspace binary: $WorkspaceBin" -ForegroundColor Gray
        Copy-Item -Path $WorkspaceBin -Destination $DestCliExe -Force
    } else {
        $CliDownloadUrl = "$BaseReleaseUrl/forge-dm-windows-x86_64.exe"
        Write-Host "      Downloading forge-dm from GitHub: $CliDownloadUrl" -ForegroundColor Gray
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12 -bor [Net.SecurityProtocolType]::Tls13
        Invoke-WebRequest -Uri $CliDownloadUrl -OutFile $DestCliExe -UseBasicParsing
    }
}

# 3. Acquire Desktop GUI & WebView2Loader.dll
if (-not $SkipDesktop) {
    Write-Host "[3/6] Staging Desktop GUI & WebView2 dependencies..." -ForegroundColor Green
    
    # 3a. WebView2Loader.dll
    $LocalDll1 = Join-Path $PSScriptRoot "..\apps\desktop\src-tauri\target\release\WebView2Loader.dll"
    $LocalDll2 = Join-Path $PSScriptRoot "..\export\WebView2Loader.dll"
    $LocalDll3 = Join-Path $PSScriptRoot "..\dist\installers\WebView2Loader.dll"
    $DllFound = $false
    foreach ($candidate in @($LocalDll1, $LocalDll2, $LocalDll3)) {
        if (Test-Path $candidate) {
            Copy-Item -Path $candidate -Destination $DestLoaderDll -Force
            $DllFound = $true
            Write-Host "      Installed WebView2Loader.dll from local source" -ForegroundColor Gray
            break
        }
    }
    if (-not $DllFound) {
        $DllUrl = "$BaseReleaseUrl/WebView2Loader.dll"
        Write-Host "      Downloading WebView2Loader.dll from $DllUrl..." -ForegroundColor Gray
        try {
            Invoke-WebRequest -Uri $DllUrl -OutFile $DestLoaderDll -UseBasicParsing
        } catch {
            Write-Warning "Could not download WebView2Loader.dll: $_"
        }
    }

    # 3b. Desktop Executable
    $LocalDesktop1 = Join-Path $PSScriptRoot "..\apps\desktop\src-tauri\target\release\forge-dm-desktop.exe"
    $LocalDesktop2 = Join-Path $PSScriptRoot "..\dist\installers\forge-dm-desktop-windows-x86_64.exe"
    $LocalDesktop3 = Join-Path $PSScriptRoot "..\export\forge-dm-desktop-windows-x86_64.exe"
    $DesktopFound = $false
    foreach ($candidate in @($LocalDesktop1, $LocalDesktop2, $LocalDesktop3)) {
        if (Test-Path $candidate) {
            Copy-Item -Path $candidate -Destination $DestDesktopExe -Force
            $DesktopFound = $true
            Write-Host "      Installed forge-dm-desktop.exe from local source" -ForegroundColor Gray
            break
        }
    }
    if (-not $DesktopFound) {
        $DesktopUrl = "$BaseReleaseUrl/forge-dm-desktop-windows-x86_64.exe"
        Write-Host "      Downloading forge-dm-desktop.exe from $DesktopUrl..." -ForegroundColor Gray
        try {
            Invoke-WebRequest -Uri $DesktopUrl -OutFile $DestDesktopExe -UseBasicParsing
        } catch {
            Write-Warning "Could not download forge-dm-desktop.exe: $_"
        }
    }

    # 3c. Create Start Menu and Desktop shortcuts
    try {
        $WshShell = New-Object -ComObject WScript.Shell
        
        # Start Menu
        $StartMenuDir = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Forge Download Manager"
        if (-not (Test-Path $StartMenuDir)) { New-Item -ItemType Directory -Path $StartMenuDir -Force | Out-Null }
        $LnkStart = $WshShell.CreateShortcut((Join-Path $StartMenuDir "Forge Download Manager.lnk"))
        $LnkStart.TargetPath = $DestDesktopExe
        $LnkStart.WorkingDirectory = $InstallDir
        $LnkStart.Description = "Forge Download Manager Desktop Application"
        $LnkStart.Save()

        # Desktop
        $DesktopDir = [Environment]::GetFolderPath('Desktop')
        $LnkDesktop = $WshShell.CreateShortcut((Join-Path $DesktopDir "Forge Download Manager.lnk"))
        $LnkDesktop.TargetPath = $DestDesktopExe
        $LnkDesktop.WorkingDirectory = $InstallDir
        $LnkDesktop.Description = "Forge Download Manager Desktop Application"
        $LnkDesktop.Save()
        Write-Host "      Created Desktop & Start Menu shortcuts" -ForegroundColor Gray
    } catch {
        Write-Warning "Failed to create shortcuts: $_"
    }
} else {
    Write-Host "[3/6] Skipping Desktop GUI (-SkipDesktop)." -ForegroundColor Gray
}

# 4. Add to user PATH
Write-Host "[4/6] Checking user PATH environment variable..." -ForegroundColor Gray
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

# 5. Register Native Messaging Host
Write-Host "[5/6] Registering Native Messaging Host for browsers..." -ForegroundColor Green
try {
    & $DestCliExe host --install
} catch {
    Write-Warning "Failed running 'forge-dm host --install': $_"
}

# 6. Verify installation
Write-Host "[6/6] Verifying installation..." -ForegroundColor Gray
try {
    $VerOutput = & $DestCliExe --version
    Write-Host ""
    Write-Host "SUCCESS: Forge Download Manager ($VerOutput) installed successfully!" -ForegroundColor Green
    Write-Host ""
    Write-Host "Installed Components in: $InstallDir" -ForegroundColor White
    Write-Host "  - forge-dm.exe              (CLI & Native Messaging Host)" -ForegroundColor Cyan
    if (Test-Path $DestDesktopExe) {
        Write-Host "  - forge-dm-desktop.exe      (Desktop GUI Application)" -ForegroundColor Cyan
        Write-Host "  - WebView2Loader.dll        (Bundled runtime loader)" -ForegroundColor Cyan
    }
    Write-Host ""
    Write-Host "Getting Started:" -ForegroundColor White
    Write-Host "  1. Desktop App:    Launch from Desktop shortcut or run 'forge-dm-desktop'" -ForegroundColor Yellow
    Write-Host "  2. CLI command:    forge-dm --help" -ForegroundColor Yellow
    Write-Host "  3. Media probe:    forge-dm media probe <url>" -ForegroundColor Yellow
    Write-Host "  4. Browser Ext:    Load apps/extension in Chrome/Edge/Firefox" -ForegroundColor Yellow
    Write-Host ""
} catch {
    Write-Warning "Verification returned an error: $_"
}
