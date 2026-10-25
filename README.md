# Forge Download Manager

**An open-source, cross-platform download manager built in Rust.**

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
![Rust](https://img.shields.io/badge/Rust-1.89%2B-orange)
![Platforms](https://img.shields.io/badge/platforms-Windows%20%7C%20macOS%20%7C%20Linux-lightgrey)

Forge Download Manager is made up of a high-performance Rust engine library, a command-line tool (`forge-dm`), a browser extension (Manifest V3), and a modern desktop application built with Tauri 2 and React. It handles multi-connection segmented HTTP(S) and FTP/FTPS downloads, pause and resume, dynamic segment splitting, proxy routing, keychain-backed credentials, bandwidth limiting, multi-queue scheduling, website link crawling, media streaming (HLS/DASH), and SQLite crash recovery.

> **Status:** Production-ready core (v0.1.0). All Phases 1 through 7 are complete, featuring cross-platform desktop installers, standalone CLI binaries, and automated installation scripts. See [Roadmap](#roadmap).


## Table of Contents

- [Features](#features)
- [Project Structure](#project-structure)
- [Installers & Downloads](#installers--downloads)
- [Getting Started](#getting-started)
- [Desktop App](#desktop-app)
- [CLI Usage](#cli-usage)
- [Data Storage](#data-storage)
- [How It Works](#how-it-works)
- [Roadmap](#roadmap)
- [Contributing](#contributing)
- [License](#license)

## Features

- **Segmented downloads**: files are split into parallel HTTP range requests (default 8 segments, up to 32), with dynamic re-splitting when a connection finishes early.
- **Pause and resume**: resumes only after re-probing the server and confirming the file has not changed (`ETag`, or `Last-Modified` + `Content-Length`).
- **Crash recovery**: progress is flushed to SQLite at least once per second; downloads interrupted by a crash continue on next launch.
- **Safe disk writes**: pre-allocated sparse files with positional writes, so segments never overlap and no merge pass is needed.
- **Retries**: jittered exponential backoff, honoring `Retry-After` on 429/503.
- **FTP / FTPS**: in-tree client behind the same worker interface as HTTP.
- **Proxy support**: `system`, `direct`, `http`, `https`, or `socks5`, globally or per download.
- **Authentication**: Basic and Digest; passwords are stored in the OS keychain, never in the database.
- **Cookies**: per-download cookies for authenticated downloads.
- **Bandwidth limits**: global and per-download token-bucket limiter.
- **Integrity checks**: optional MD5, SHA-1, or SHA-256 verification on completion.
- **Safe filenames**: sanitization, reserved-name rejection, path-escape protection, and a duplicate policy (`rename`, `overwrite`, `ask`).
- **Desktop UI**: virtualized download list, per-connection segment map, and live speed graph.
- **Multi-Queue Scheduling**: priority queues, time-based windows, recurring cron triggers, and concurrency limits.
- **Site Grabber**: recursive link crawler with depth limits, regex filters, and batch URL expansion.
- **Browser Extension**: Manifest V3 extension with Native Messaging host for Chrome, Edge, Brave, and Firefox.
- **Media Streaming Engine**: HLS/DASH manifest parsing, segmented stream downloads, and FFmpeg remuxing with optional yt-dlp plugin support.
- **Single-owner engine**: the desktop app and CLI share one database; whichever process holds `engine.lock` runs the engine, and the other forwards commands over a token-protected loopback HTTP API.

## Project Structure

```text
ForgeDownloadManager/
├── crates/
│   ├── forgedownloadmanager-core/   # Engine library (no UI)
│   └── forgedownloadmanager-cli/    # `forge-dm` command-line front end & host
├── apps/
│   ├── desktop/                     # Tauri 2 + React + TypeScript + Tailwind window
│   └── extension/                   # Manifest V3 browser extension
├── export/                          # Exported standalone installers & browser extension
│   ├── installers/                  # Desktop GUI .exe, CLI .exe, install.ps1, install.sh
│   ├── browser-extension/           # Packaged .zip & unpacked extension folder
│   ├── SHA256SUMS.txt               # Verification checksums
│   └── README.md                    # Export usage instructions
├── dist/
│   └── installers/                  # Staged installer bundles & release binaries
├── docs/                            # Guides (INSTALL.md, THIRD_PARTY.md)
├── scripts/                         # Build automation & installation scripts
├── .github/workflows/               # GitHub Actions CI/CD release workflow
├── ARCHITECTURE.md                  # Schema, state machine, and phase plan
├── Cargo.toml                       # Rust workspace (engine + CLI)
└── LICENSE
```

## Installers & Downloads

Official releases, installer bundles, and release checksums are published on the [GitHub Releases](https://github.com/Ibrahist/ForgeDownloadManager/releases) page.

### Exported Installers & Browser Extension (Project Root)

All standalone executables, automated installers, and browser extension packages are exported to the **[`export/`](export/)** folder located directly in the root of the project (also aliased as **[`exports/`](exports/)**):

```text
export/
├── installers/
│   ├── forge-dm-desktop-windows-x86_64.exe   # Desktop GUI Application (Tauri 2 + React 19)
│   ├── forge-dm-windows-x86_64.exe           # Headless CLI binary & Native Messaging host
│   ├── install.ps1                           # Automated Windows PowerShell installer
│   └── install.sh                            # Automated Linux/macOS Bash installer
├── browser-extension/
│   ├── forge-dm-extension.zip                # Packaged MV3 extension archive (.zip)
│   └── unpacked/                             # Unpacked extension directory for Developer Mode
├── forge-dm-desktop-windows-x86_64.exe       # Direct shortcut to Desktop GUI
├── forge-dm-windows-x86_64.exe               # Direct shortcut to CLI binary
├── forge-dm-extension.zip                   # Direct shortcut to extension archive
├── install.ps1                              # Direct shortcut to Windows installer
├── install.sh                               # Direct shortcut to Linux/macOS installer
├── SHA256SUMS.txt                           # Cryptographic SHA-256 verification hashes
└── README.md                                # Dedicated export documentation
```

#### How to Use Exported Installers & Extension:

- **Desktop GUI Application:**
  - Launch `export/installers/forge-dm-desktop-windows-x86_64.exe` (or the root shortcut `export/forge-dm-desktop-windows-x86_64.exe`). It starts the visual download manager immediately without installation.
- **Standalone CLI:**
  - Run `export/installers/forge-dm-windows-x86_64.exe` directly from the terminal (e.g. `.\export\forge-dm-windows-x86_64.exe --help`).
- **Automated Windows Installation (CLI & Native Host):**
  - Run PowerShell:
    ```powershell
    powershell -ExecutionPolicy Bypass -File .\export\installers\install.ps1 -LocalBinary .\export\installers\forge-dm-windows-x86_64.exe
    ```
  - Automatically copies `forge-dm.exe` into `%LOCALAPPDATA%\Programs\ForgeDownloadManager\bin`, configures your `PATH`, and registers native host manifests for Chrome, Edge, Brave, Vivaldi, and Firefox.
- **Browser Extension:**
  - **Chrome, Edge, Brave, Vivaldi:** Open `chrome://extensions` (or `edge://extensions`), turn on **Developer mode**, and click **Load unpacked**, selecting the [`export/browser-extension/unpacked`](export/browser-extension/unpacked) folder (or drag and drop `export/browser-extension/forge-dm-extension.zip`).
  - **Firefox:** Open `about:debugging#/runtime/this-firefox`, click **Load Temporary Add-on...**, and choose `export/browser-extension/unpacked/manifest.json`.
  - Complete the native host registration with `.\export\forge-dm-windows-x86_64.exe host --install`.
- **Integrity Check:**
  - Hashes for all exported artifacts are recorded in [`export/SHA256SUMS.txt`](export/SHA256SUMS.txt).

### Pre-Built Packages

| Platform | Format / Type | Description / Filename |
| :--- | :--- | :--- |
| **Windows** | Setup Wizard (`.exe`) | `ForgeDownloadManager_0.1.0_x64-setup.exe` (NSIS installer with shortcuts & uninstaller) |
| **Windows** | Enterprise MSI (`.msi`) | `ForgeDownloadManager_0.1.0_x64_en-US.msi` (WiX installer for silent/enterprise deployment) |
| **Windows** | Standalone CLI (`.exe`) | `forge-dm-windows-x86_64.exe` (Headless CLI binary & Native Messaging host) |
| **macOS** | Disk Image (`.dmg`) | `ForgeDownloadManager_0.1.0_universal.dmg` (Universal binary for Apple Silicon & Intel) |
| **macOS** | Standalone CLI | `forge-dm-macos-universal` (Headless CLI binary & Native Messaging host) |
| **Linux** | AppImage (`.AppImage`) | `ForgeDownloadManager_0.1.0_amd64.AppImage` (Portable bundle with all dependencies) |
| **Linux** | Debian / Ubuntu (`.deb`) | `forge-download-manager_0.1.0_amd64.deb` (Debian package) |
| **Linux** | Standalone CLI | `forge-dm-linux-x86_64` (Headless CLI binary & Native Messaging host) |
| **Browser** | Extension (`.zip`) | `forge-dm-extension.zip` (Manifest V3 extension for Chrome, Edge, Brave, Firefox) |

All official release artifacts are signed and published with cryptographic SHA-256 checksums in `SHA256SUMS.txt`.

### Quick Install (Standalone CLI)

**Windows (PowerShell):**
```powershell
irm https://raw.githubusercontent.com/Ibrahist/ForgeDownloadManager/main/scripts/install.ps1 | iex
```

**macOS & Linux (Bash):**
```bash
curl -fsSL https://raw.githubusercontent.com/Ibrahist/ForgeDownloadManager/main/scripts/install.sh | bash
```

### Local Build Artifacts Location

When building installers from source using `./scripts/build-installers.sh` or `.\scripts\build-installers.ps1`, the resulting packages are staged in:

- **Release Staging Directory:** `dist/installers/`
  - `dist/installers/forge-dm-<platform>-<arch>` (Compiled CLI binary)
  - `dist/installers/forge-dm-extension.zip` (Packaged MV3 browser extension)
  - `dist/installers/SHA256SUMS.txt` (Generated SHA-256 verification hashes)
- **Tauri Bundle Output Directory:** `apps/desktop/src-tauri/target/release/bundle/`
  - **Windows:** `bundle/nsis/*.exe`, `bundle/msi/*.msi`
  - **macOS:** `bundle/dmg/*.dmg`, `bundle/macos/*.app`
  - **Linux:** `bundle/appimage/*.AppImage`, `bundle/deb/*.deb`

For silent enterprise installations, package managers (`winget`, `scoop`, `brew`), and browser extension sideloading, see the complete [Installation Guide (docs/INSTALL.md)](docs/INSTALL.md).


## Getting Started

### Prerequisites

- [Git](https://git-scm.com/)
- [Rust](https://rustup.rs/) 1.89 or newer (1.90+ to build the desktop app)
- [Node.js](https://nodejs.org/) and npm (desktop app and extension packaging)
- Linux only: system WebKitGTK libraries for the desktop window

### Build and test the engine and CLI

```bash
git clone https://github.com/Ibrahist/ForgeDownloadManager.git
cd ForgeDownloadManager
cargo build --workspace
cargo test --workspace
```

The CLI binary is named `forge-dm`.

**Windows note:** the default build uses the MSVC linker and the Windows SDK. Without them, install a MinGW `gcc` on your `PATH` and run:

```bash
cargo +stable-x86_64-pc-windows-gnu test --workspace
```

CI uses the MSVC toolchain on Windows runners.

## Desktop App

The desktop window lives in `apps/desktop` and is a **separate Cargo workspace**, so `cargo test --workspace` at the repository root does not compile the WebView. It requires Rust 1.90+ (Tauri 2.12); the engine and CLI stay on 1.89.

```bash
cd apps/desktop
npm ci
npm test
npm run dev          # UI only; downloads do NOT run in this mode
npm run tauri dev    # full app; downloads run via forge-dm-desktop
```

For a release-style compile, run `npm run build` in `apps/desktop`, then:

```bash
cargo build --manifest-path apps/desktop/src-tauri/Cargo.toml
```

Behavior to know about:

- When the desktop process wins `engine.lock`, it embeds the engine and serves the loopback HTTP API until it exits, even while the queue is idle.
- Closing the window leaves downloads running. The next launch requeues rows that were still downloading.
- If another process already holds the lock, the window forwards commands to it and refreshes the list about twice a second.
- On Windows with the GNU toolchain, you may see a `multiple non-default manifests` warning; the binary still links.
- CI builds and packages installers across Windows, macOS, and Linux runners automatically upon release tags.

## CLI Usage

```text
forge-dm add URL [--dir PATH] [--segments N] [--name FILE] [--no-start]
                 [--cookie Name=Value] [--speed-limit BPS] [--proxy MODE]
                 [--user USER] [--password-stdin]
forge-dm list [--json]
forge-dm pause ID
forge-dm resume ID
forge-dm remove ID [--delete-file]
forge-dm run
forge-dm auth set HOST USER
forge-dm config get SETTING
forge-dm config set SETTING VALUE
forge-dm host --install | --uninstall | --run
forge-dm media probe URL
forge-dm media tools
forge-dm media download URL [--output PATH] [--remux FORMAT]
```

**Notes**

- `--proxy` accepts `direct`, `system`, or a proxy URL (`http://`, `https://`, `socks5://`).
- `--password-stdin` reads one password line and stores it in the OS keychain. `auth set` does the same for `HOST` in `host:port` form. Passwords are never passed as command-line arguments and never written to SQLite.
- Available settings: `proxy_mode`, `proxy_url`, `speed_limit_bps`, `max_concurrent_downloads`, `per_host_connections`, `default_segments`, `min_segment_bytes`, `user_agent`.
- `add` stays in the foreground until the download (and any recovered queued work) finishes. Progress goes to stderr; the download ID goes to stdout.
- Exit codes: `0` success, `1` usage or other error, `2` download failed, `130` after Ctrl-C (active downloads are paused first).

**Example**

```bash
forge-dm add https://example.com/big-file.iso --dir ~/Downloads --segments 8
forge-dm list --json
```

## Data Storage

| Platform | Location                                             |
| -------- | ---------------------------------------------------- |
| Windows  | `%LOCALAPPDATA%\ForgeDownloadManager`                |
| macOS    | `~/Library/Application Support/ForgeDownloadManager` |
| Linux    | `$XDG_DATA_HOME/forgedownloadmanager`                |

Override with the `FORGE_DM_DATA` environment variable or the `--data-dir` flag. The directory contains `forge.db` (SQLite, WAL mode) and `engine.lock` (PID, API port, and auth token).

## How It Works

- **Probe:** `HEAD` the URL (falling back to `GET` with `Range: bytes=0-0`) to learn size, range support, and validators.
- **Segment:** split the file into equal ranges, no smaller than the minimum segment size (default 1 MiB), so small files use a single connection.
- **Write:** each worker writes only inside its own byte range using positional I/O; the cursor advances only after a flush.
- **Persist:** state is committed to SQLite at least once per second, so a crash re-downloads at most a short suffix and never leaves a believed-complete hole.
- **Security:** the control API listens on loopback only, requires a 256-bit bearer token, and rejects browser `Origin` headers. TLS verification is on by default.

For installation options, automated setup scripts, and platform installers, see [docs/INSTALL.md](docs/INSTALL.md).
For the full design, database schema, state machine, and phase plan, see [ARCHITECTURE.md](ARCHITECTURE.md).

## Roadmap

- [x] **Phase 1:** core engine, segmented downloads, pause/resume, SQLite recovery, CLI
- [x] **Phase 2:** dynamic split, retries, proxies, authentication, cookies, FTP/FTPS, bandwidth limiter
- [x] **Phase 3:** desktop shell (Tauri 2 + React)
- [x] **Phase 4:** queues and schedules beyond the default queue, site grabber
- [x] **Phase 5:** browser extension (Manifest V3, Native Messaging)
- [x] **Phase 6:** HLS/DASH media downloads, ffmpeg remux, optional yt-dlp plugin
- [x] **Phase 7:** installers
- [ ] SFTP and a Linux OS keychain backend (the secret-service backend depends on dbus, whose license is incompatible with this MIT app)

## Contributing

Contributions, suggestions, and bug reports are welcome.

1. Fork the repository.
2. Create a feature branch.
3. Make your changes and add tests.
4. Run `cargo test --workspace` (and `npm test` in `apps/desktop` for UI changes).
5. Open a pull request describing your changes.

For significant changes, please open an issue first. New dependencies must be MIT, Apache-2.0, BSD, ISC, or public domain; anything else needs discussion first. Third-party license notes are in `docs/THIRD_PARTY.md`.

Found a bug? Open an [issue](https://github.com/Ibrahist/ForgeDownloadManager/issues) with steps to reproduce, expected vs. actual behavior, error messages, and your OS and version.

## License

Released under the [MIT License](LICENSE).

## Author

**Ibrahist**: [@Ibrahist](https://github.com/Ibrahist)
