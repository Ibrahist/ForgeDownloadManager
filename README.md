# Forge Download Manager

<<<<<<< HEAD
**An open-source, cross-platform download manager built in Rust.**
=======
**[An open-source, cross-platform download manager built in Rust]**
>>>>>>> 546169a505787fd9c04bd0e69f002ae6c6cdfff7

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
![Rust](https://img.shields.io/badge/Rust-1.89%2B-orange)
![Platforms](https://img.shields.io/badge/platforms-Windows%20%7C%20macOS%20%7C%20Linux-lightgrey)

Forge Download Manager is made up of a Rust engine library, a command-line tool (`forge-dm`), and a desktop window built with Tauri 2 and React. It handles segmented HTTP(S) and FTP/FTPS downloads, pause and resume, retries, proxies, keychain-backed authentication, cookies, bandwidth limits, and SQLite crash recovery.

> **Status:** Early development (v0.1.0). The engine, CLI, and desktop shell are implemented. The browser extension, advanced queues/scheduling, site grabber, and media (HLS/DASH) support are planned. See [Roadmap](#roadmap).

## Table of Contents

- [Features](#features)
- [Project Structure](#project-structure)
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
- **Single-owner engine**: the desktop app and CLI share one database; whichever process holds `engine.lock` runs the engine, and the other forwards commands over a token-protected loopback HTTP API.

## Project Structure

```
ForgeDownloadManager/
├── crates/
│   ├── forgedownloadmanager-core/   # Engine library (no UI)
│   └── forgedownloadmanager-cli/    # `forge-dm` command-line front end
├── apps/
│   ├── desktop/                     # Tauri 2 + React + TypeScript + Tailwind window
│   └── extension/                   # Browser extension (planned)
├── docs/                            # Additional documentation (e.g. THIRD_PARTY.md)
├── .github/workflows/               # CI
├── ARCHITECTURE.md                  # Schema, state machine, and phase plan
├── Cargo.toml                       # Rust workspace (engine + CLI)
└── LICENSE
```

## Getting Started

### Prerequisites

- [Git](https://git-scm.com/)
- [Rust](https://rustup.rs/) 1.89 or newer (1.90+ to build the desktop app)
- [Node.js](https://nodejs.org/) and npm (desktop app only)
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
- CI builds the shell on Ubuntu, Windows, and macOS but does not produce installers.

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

<<<<<<< HEAD
| Platform | Location                                             |
| -------- | ---------------------------------------------------- |
| Windows  | `%LOCALAPPDATA%\ForgeDownloadManager`                |
| macOS    | `~/Library/Application Support/ForgeDownloadManager` |
| Linux    | `$XDG_DATA_HOME/forgedownloadmanager`                |
=======
| Platform | Location |
| -------- | -------- |
| Windows  | `%LOCALAPPDATA%\ForgeDownloadManager` |
| macOS    | `~/Library/Application Support/ForgeDownloadManager` |
| Linux    | `$XDG_DATA_HOME/forgedownloadmanager` |
>>>>>>> 546169a505787fd9c04bd0e69f002ae6c6cdfff7

Override with the `FORGE_DM_DATA` environment variable or the `--data-dir` flag. The directory contains `forge.db` (SQLite, WAL mode) and `engine.lock` (PID, API port, and auth token).

## How It Works

- **Probe:** `HEAD` the URL (falling back to `GET` with `Range: bytes=0-0`) to learn size, range support, and validators.
- **Segment:** split the file into equal ranges, no smaller than the minimum segment size (default 1 MiB), so small files use a single connection.
- **Write:** each worker writes only inside its own byte range using positional I/O; the cursor advances only after a flush.
- **Persist:** state is committed to SQLite at least once per second, so a crash re-downloads at most a short suffix and never leaves a believed-complete hole.
- **Security:** the control API listens on loopback only, requires a 256-bit bearer token, and rejects browser `Origin` headers. TLS verification is on by default.

<<<<<<< HEAD
For installation options, automated setup scripts, and platform installers, see [docs/INSTALL.md](docs/INSTALL.md).
=======
>>>>>>> 546169a505787fd9c04bd0e69f002ae6c6cdfff7
For the full design, database schema, state machine, and phase plan, see [ARCHITECTURE.md](ARCHITECTURE.md).

## Roadmap

- [x] **Phase 1:** core engine, segmented downloads, pause/resume, SQLite recovery, CLI
- [x] **Phase 2:** dynamic split, retries, proxies, authentication, cookies, FTP/FTPS, bandwidth limiter
- [x] **Phase 3:** desktop shell (Tauri 2 + React)
<<<<<<< HEAD
- [x] **Phase 4:** queues and schedules beyond the default queue, site grabber
- [x] **Phase 5:** browser extension (Manifest V3, Native Messaging)
- [x] **Phase 6:** HLS/DASH media downloads, ffmpeg remux, optional yt-dlp plugin
- [x] **Phase 7:** installers
=======
- [ ] **Phase 4:** queues and schedules beyond the default queue, site grabber
- [ ] **Phase 5:** browser extension (Manifest V3, Native Messaging)
- [ ] **Phase 6:** HLS/DASH media downloads, ffmpeg remux, optional yt-dlp plugin
- [ ] **Phase 7:** installers
>>>>>>> 546169a505787fd9c04bd0e69f002ae6c6cdfff7
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
