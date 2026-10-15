# Forge Download Manager

Forge Download Manager is an open-source, cross-platform download manager. Phase 1 is a Rust library and the `forge-dm` command: segmented HTTP(S) downloads, pause and resume, and SQLite crash recovery.

The license is MIT. This tree does not copy proprietary download-manager code, icons, or branding. There is no telemetry.

You are responsible for the copyright and the site terms of anything you download.

## Build

Install Rust 1.89 or newer, then:

```bash
cargo test --workspace
cargo run -p forgedownloadmanager-cli -- add https://example.com/file.bin
```

On Windows, that command uses the MSVC linker and the Windows SDK. A machine without them can use the GNU toolchain instead: install a MinGW `gcc` on `PATH`, then run `cargo +stable-x86_64-pc-windows-gnu test --workspace`. GitHub Actions uses the Windows runner's MSVC tools.

The binary name is `forge-dm`. Data is stored in `%LOCALAPPDATA%\ForgeDownloadManager` on Windows, `~/Library/Application Support/ForgeDownloadManager` on macOS, or `$XDG_DATA_HOME/forgedownloadmanager` on Linux. `FORGE_DM_DATA` or `--data-dir` overrides that path.

## Phase 1 commands

```bash
forge-dm add URL [--dir PATH] [--segments N] [--name FILE] [--no-start]
forge-dm list [--json]
forge-dm pause ID
forge-dm resume ID
forge-dm remove ID [--delete-file]
forge-dm run
```

`add` stays in the foreground until the download and any recovered queued work finish. Progress is printed to stderr. The download id is printed to stdout. Exit codes are `0` success, `2` download failed, `1` usage or another error, and `130` after Ctrl-C (active downloads are paused first).

A second `forge-dm` process does not open the database. It forwards to the loopback API of the process that holds the lock. That API listens on `127.0.0.1` only, requires a bearer token, and rejects any `Origin` header.

## Layout

- `crates/forgedownloadmanager-core` — engine
- `crates/forgedownloadmanager-cli` — `forge-dm`
- `apps/desktop` and `apps/extension` — later phases
- `ARCHITECTURE.md` — schema and phase plan

## Not in this phase

Dynamic segment splits, retries, proxies, authentication, cookies, FTP, bandwidth limits, the desktop UI, the browser extension, HLS/DASH, and installers. See `ARCHITECTURE.md`.

Third-party licenses are listed in `docs/THIRD_PARTY.md`.
