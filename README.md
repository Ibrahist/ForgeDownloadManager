# Forge Download Manager

<<<<<<< HEAD
Forge Download Manager is an open-source, cross-platform download manager. The Rust library, the `forge-dm` command, and the desktop window do segmented HTTP(S) and FTP/FTPS downloads, pause and resume, retries, proxies, keychain auth, cookies, bandwidth limits, and SQLite crash recovery.
=======
**A modern download manager designed to make downloading, organizing, and managing files easier.**
>>>>>>> 80e3209e3e4ce664e55351cb25051be2bb91f10f

Forge Download Manager is a software project focused on providing a streamlined download management experience. The goal is to make file transfers easier to monitor and manage through an intuitive interface.

> **Project status:** Update this section to reflect whether the project is in development, ready for testing, or stable.

## Table of Contents

- [Overview](#overview)
- [Features](#features)
- [Technology Stack](#technology-stack)
- [Getting Started](#getting-started)
- [Installation](#installation)
- [Usage](#usage)
- [Project Structure](#project-structure)
- [Roadmap](#roadmap)
- [Contributing](#contributing)
- [License](#license)

## Overview

Forge Download Manager aims to provide a convenient way to manage file downloads in one place. The project focuses on usability, reliable transfer management, and a clean user experience.

Whether managing individual files or multiple transfers, the application is intended to provide a centralized workspace for monitoring download activity.

## Features

The following are intended features; retain only those implemented in the current version.

- **Download Management** — Manage downloads from a centralized interface.
- **Progress Tracking** — Monitor download progress and transfer status.
- **Pause and Resume** — Control transfers when supported by the download server.
- **Download Queue** — Organize multiple downloads.
- **File Organization** — Keep downloaded files organized.
- **Modern Interface** — Provide a clean and intuitive user experience.
- **Error Handling** — Communicate download failures and provide recovery options where supported.
- **Performance** — Aim for efficient use of system resources.

## Technology Stack

The technologies used by this project depend on the current implementation.

Update this section with the actual frameworks, programming languages, libraries, and tools used in the repository.

## Getting Started

### Prerequisites

Before installing the project, ensure you have the development tools required by its technology stack.

Examples include:

- Git
- The appropriate language runtime or SDK
- A package manager or build tool
- Any required system dependencies

### Installation

Clone the repository:

```bash
git clone https://github.com/Ibrahist/ForgeDownloadManager.git
```

<<<<<<< HEAD
On Windows, that command uses the MSVC linker and the Windows SDK. A machine without them can use the GNU toolchain instead: install a MinGW `gcc` on `PATH`, then run `cargo +stable-x86_64-pc-windows-gnu test --workspace`. GitHub Actions uses the Windows runner's MSVC tools.

## Desktop

The window lives in `apps/desktop` and is its own Cargo workspace, so the command above does not compile the WebView. That crate needs Rust 1.90 or newer because it links Tauri 2.12. The engine and CLI stay on Rust 1.89.

```bash
cd apps/desktop
npm ci
npm test
npm run dev
npm run tauri dev
```

`npm run dev` shows the interface only. Downloads run inside `npm run tauri dev`, which starts `forge-dm-desktop`. A release-style compile, after `npm run build` in `apps/desktop`, is:

```bash
cargo build --manifest-path apps/desktop/src-tauri/Cargo.toml
```

When this process wins `engine.lock`, it embeds the engine and serves the loopback HTTP API until it exits, including while the queue is idle. Closing the window leaves downloads running. The next launch requeues rows that were still downloading. If another process already holds the lock, the window forwards commands to it and refreshes the list about twice a second.

A Windows GNU link can print `multiple non-default manifests` while merging the resource section. The binary still links. CI uses the MSVC linker. GitHub Actions builds the shell on Ubuntu, Windows, and macOS. It does not produce installers.

The binary name is `forge-dm`. Data is stored in `%LOCALAPPDATA%\ForgeDownloadManager` on Windows, `~/Library/Application Support/ForgeDownloadManager` on macOS, or `$XDG_DATA_HOME/forgedownloadmanager` on Linux. `FORGE_DM_DATA` or `--data-dir` overrides that path.

## Commands

```bash
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

`--proxy` is `direct`, `system`, or a proxy URL (`http://`, `https://`, `socks5://`). `--password-stdin` reads one password line and stores it in the OS keychain. `auth set` does the same for `HOST` in `host:port` form. Passwords are not command-line arguments and are not written to SQLite. Settings are `proxy_mode`, `proxy_url`, `speed_limit_bps`, `max_concurrent_downloads`, `per_host_connections`, `default_segments`, `min_segment_bytes`, and `user_agent`.

`add` stays in the foreground until the download and any recovered queued work finish. Progress is printed to stderr. The download id is printed to stdout. Exit codes are `0` success, `2` download failed, `1` usage or another error, and `130` after Ctrl-C (active downloads are paused first).
=======
Navigate to the project directory:

```bash
cd ForgeDownloadManager
```

Install dependencies using the package manager or build tool appropriate for the project.
>>>>>>> 80e3209e3e4ce664e55351cb25051be2bb91f10f

Then follow the project's build and execution instructions.

> **Note:** The exact installation commands depend on the repository's actual technology stack. Add the verified commands here so developers can run the project without guesswork.

<<<<<<< HEAD
- `crates/forgedownloadmanager-core` — engine
- `crates/forgedownloadmanager-cli` — `forge-dm`
- `apps/desktop` — Tauri 2 + React window (`forge-dm-desktop`)
- `apps/extension` — later phase
- `ARCHITECTURE.md` — schema and phase plan
=======
## Usage
>>>>>>> 80e3209e3e4ce664e55351cb25051be2bb91f10f

Once the application is installed and running, use the interface and functionality available in the current version to manage your downloads.

<<<<<<< HEAD
The browser extension, queues and schedules beyond the seeded Default queue, the site grabber, HLS/DASH, installers, and SFTP. Linux builds do not use an OS keychain: the secret-service backend depends on dbus (AFL/GPL), which this MIT app does not link. See `ARCHITECTURE.md`.
=======
Document the supported workflows here, including how to add a download, choose a destination, monitor progress, and manage completed or failed transfers.
>>>>>>> 80e3209e3e4ce664e55351cb25051be2bb91f10f

## Project Structure

The repository structure depends on the implementation. A typical project may organize its code into:

```text
ForgeDownloadManager/
├── src/              # Application source code
├── public/            # Static assets, if applicable
├── tests/             # Automated tests, if available
├── docs/              # Documentation
├── README.md
└── LICENSE
```

Adapt this tree to match the actual repository.

## Roadmap

Potential future improvements include:

- [ ] Improve download management and monitoring.
- [ ] Add robust error handling and retry mechanisms.
- [ ] Improve download queue management.
- [ ] Add pause and resume support where possible.
- [ ] Improve the user interface and accessibility.
- [ ] Add automated tests.
- [ ] Expand documentation.
- [ ] Explore browser integration and cross-platform support.

Roadmap items should be updated as development progresses.

## Contributing

Contributions, suggestions, and bug reports are welcome.

1. Fork the repository.
2. Create a feature branch.
3. Make your changes.
4. Test your implementation.
5. Submit a pull request describing your changes.

For significant changes, open an issue first to discuss the proposed implementation.

## Issues and Support

If you encounter a bug or have a feature request, visit the repository's [Issues](https://github.com/Ibrahist/ForgeDownloadManager/issues) page.

When reporting a bug, include:

- A description of the problem.
- Steps to reproduce it.
- Expected and actual behavior.
- Relevant error messages.
- Your operating system and application version.

## License

Check the repository's `LICENSE` file for the applicable license and terms of use.

If the project does not yet have a license, choose and add one before distributing it as open-source software.

## Author

**Ibrahist**

- GitHub: [@Ibrahist](https://github.com/Ibrahist)
- Project: [Forge Download Manager](https://github.com/Ibrahist/ForgeDownloadManager)

---

*Forge Download Manager — a project focused on making file downloads easier to manage.*
