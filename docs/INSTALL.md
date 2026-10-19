# Installing Forge Download Manager

Forge Download Manager is available in two editions:
1. **Desktop Shell (GUI)** – Built with Tauri 2 and React 19, provides visual queues, segmented speed graphs, category management, and desktop notifications.
2. **Command Line Interface (`forge-dm`)** – Lightweight headless binary for servers, scripts, terminal power users, and browser native messaging host integration.

---

## 1. Desktop GUI Installers

Pre-built binaries and official installers are available on the [Releases Page](https://github.com/Ibrahist/ForgeDownloadManager/releases).

### Windows
- **NSIS Installer (`.exe`)**: Standard setup wizard with desktop and Start Menu shortcuts, automatic uninstall support, and optional per-machine or current-user installation.
- **WiX Installer (`.msi`)**: Enterprise-ready Windows Installer package for silent or policy-driven deployments.
  ```powershell
  # Silent install via MSI
  msiexec /i ForgeDownloadManager_0.1.0_x64_en-US.msi /qn
  ```

### macOS
- **Apple Disk Image (`.dmg`)**: Universal binary supporting both Apple Silicon (M1/M2/M3/M4) and Intel Macs.
- Drag `Forge Download Manager.app` into your `/Applications` directory.

### Linux
- **AppImage**: Standalone executable bundle with all required libraries.
  ```bash
  chmod +x ForgeDownloadManager_0.1.0_amd64.AppImage
  ./ForgeDownloadManager_0.1.0_amd64.AppImage
  ```
- **Debian / Ubuntu (`.deb`)**:
  ```bash
  sudo apt update
  sudo apt install ./forge-download-manager_0.1.0_amd64.deb
  ```

---

## 2. Standalone CLI (`forge-dm`) Quick Install

### Windows (PowerShell)
Run the automated installation script in PowerShell:
```powershell
irm https://raw.githubusercontent.com/Ibrahist/ForgeDownloadManager/main/scripts/install.ps1 | iex
```
This script:
- Places `forge-dm.exe` into `%LOCALAPPDATA%\Programs\ForgeDownloadManager\bin`.
- Adds the directory to your user `PATH`.
- Registers the browser Native Messaging host for Chrome, Edge, Brave, Vivaldi, and Firefox.

### Linux & macOS (Bash)
Run the automated installation script in your terminal:
```bash
curl -fsSL https://raw.githubusercontent.com/Ibrahist/ForgeDownloadManager/main/scripts/install.sh | bash
```
This script:
- Installs `forge-dm` into `~/.local/bin` (or `/usr/local/bin` if run with `sudo`).
- Registers Native Messaging manifests into browser host directories.
- Verifies PATH configuration.

---

## 3. Package Managers

### Windows (Winget & Scoop)
```powershell
# Windows Package Manager (Winget)
winget install ForgeDownloadManager

# Scoop
scoop bucket add extras
scoop install forge-dm
```

### macOS (Homebrew)
```bash
# GUI Desktop App
brew install --cask forge-download-manager

# Standalone CLI
brew install forge-dm
```

---

## 4. Browser Extension Installation

The extension connects directly to Forge Download Manager through the Native Messaging protocol to capture downloads and floating media streams.

### Chrome, Edge, Brave, and Vivaldi
1. Download or package `forge-dm-extension.zip` (or use `apps/extension` directly).
2. Open your browser's extensions page (`chrome://extensions` or `edge://extensions`).
3. Enable **Developer mode** (toggle in the top-right corner).
4. Click **Load unpacked** and select the `apps/extension` folder (or drag and drop the `.zip`).
5. Ensure the Native Messaging host is registered:
   ```bash
   forge-dm host --install
   ```

### Mozilla Firefox
1. Navigate to `about:debugging#/runtime/this-firefox`.
2. Click **Load Temporary Add-on...**
3. Select `apps/extension/manifest.json`.
4. Ensure the host is registered:
   ```bash
   forge-dm host --install
   ```

---

## 5. Building from Source

### Prerequisites
- [Rust](https://rustup.rs/) 1.89+ (with `stable-x86_64-pc-windows-gnu` or `msvc` on Windows).
- [Node.js](https://nodejs.org/) 22+ and `npm`.
- Linux only: `libwebkit2gtk-4.1-dev`, `libxdo-dev`, `libssl-dev`, `libayatana-appindicator3-dev`.

### Local Build Scripts
```powershell
# Windows PowerShell
.\scripts\build-installers.ps1
```

```bash
# Linux / macOS Bash
./scripts/build-installers.sh
```

All built artifacts (`forge-dm` executable, `forge-dm-extension.zip`, and desktop installer bundles) will be staged in `dist/installers/` and exported to the project root **[`export/`](../export/)** folder (also aliased as **`exports/`**) alongside `SHA256SUMS.txt`.

---

## 6. Uninstallation

### Windows
- To uninstall the Desktop app: Use **Windows Settings > Installed apps > Forge Download Manager > Uninstall**.
- To unregister the browser host and remove CLI:
  ```powershell
  forge-dm host --uninstall
  Remove-Item -Recurse -Force "$env:LOCALAPPDATA\Programs\ForgeDownloadManager"
  ```

### Linux & macOS
```bash
forge-dm host --uninstall
rm -f ~/.local/bin/forge-dm
```
