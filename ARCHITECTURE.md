# Forge Download Manager — Architecture

Status: **accepted**. Phase 1's cut line is section 10. Phase 2 adds dynamic split, retries, proxies, authentication, cookies, FTP/FTPS, and bandwidth limits. Phase 3 adds the desktop shell. That process embeds the engine, holds `engine.lock`, and serves the existing HTTP control API.

License target: **MIT**. Original UI and naming. No proprietary download-manager code, icons, or branding.

This document is the contract for Phases 1–7. Later phases add modules. They do not redesign the tables below.

## 1. Process model

One **engine owner** at a time. The desktop app and the CLI both link `forgedownloadmanager-core` in-process. They share one SQLite file under the user data directory.

```
%LOCALAPPDATA%\ForgeDownloadManager\        (Windows)
~/Library/Application Support/ForgeDownloadManager/   (macOS)
$XDG_DATA_HOME/forgedownloadmanager/        (Linux)
  forge.db          SQLite (WAL)
  engine.lock       PID + API port + auth token
  partials/         optional; default is the user-chosen save directory
```

Startup takes `engine.lock` with an exclusive create. The winner opens the database and listens on `127.0.0.1:<random>` with the HTTP control API. The token is 256 bits, stored only in the lock file (mode 0600). The Phase 3 desktop process is that owner and leaves the listener up until it exits, including while the queue is idle. The extension's loopback WebSocket, described below, is Phase 5.

If the lock is already held, the CLI does not open the database. It forwards commands to the loopback API. Two writers never download the same job.

The browser extension never talks to SQLite. It talks to the engine owner:

1. Native Messaging host (preferred), registered by the installer.
2. If that fails, loopback WebSocket using the token in the lock file.
3. If the app is not running, the host launches it, or the extension falls back to the browser's own download.

Web pages cannot call the API. The listener is loopback-only, requires the bearer token, and rejects browser `Origin` values. Native Messaging is already origin-locked to the extension id.

## 2. Crate and module map

```
crates/forgedownloadmanager-core/     library, no UI
crates/forgedownloadmanager-cli/      clap front end over the library
apps/desktop/                         Tauri 2 + React + TS + Tailwind
apps/extension/                       Manifest V3, TypeScript
```

Core modules:

| Module | Job |
| --- | --- |
| `engine` | State machine, concurrency caps, pause-all, recovery on open |
| `probe` | HEAD, then `Range: bytes=0-0` fallback |
| `segment` | Initial split and (Phase 2) dynamic split |
| `worker` | One HTTP range or one single stream |
| `writer` | Pre-allocated sparse file, positional writes |
| `persist` | SQLite repository, 1s flush |
| `events` | Broadcast bus consumed by CLI and Tauri |
| `retry` | Exponential backoff, jitter, `Retry-After` (Phase 2) |
| `limiter` | Token bucket, global and per download (Phase 2) |
| `filename` | Content-Disposition, sanitizer, duplicate policy |
| `schedule` | Queues, windows, completion actions (Phase 4) |
| `grabber` | Batch, pattern expansion, same-domain crawl (Phase 4) |
| `media` | HLS, DASH, ffmpeg remux, yt-dlp plugin (Phase 6) |
| `proxy` / `auth` | Proxy, cookie jar, keychain credentials (Phase 2) |

Dependency choice inside the spec: **rusqlite** (bundled SQLite) instead of sqlx. Migrations are plain SQL files applied at open. The engine stays free of compile-time query macros and builds faster as a long-lived desktop dependency. Public-domain SQLite, MIT rusqlite.

HTTP stack: **reqwest** on **rustls**, HTTP/1.1 and HTTP/2, redirect limit 10. FTP arrives in Phase 2 via a separate client behind the same `worker` trait. SFTP stays a later optional trait, not a Phase 1–7 promise.

## 3. Download state machine

```
                 probe ok
queued ─────────────────► ready ──start──► downloading ──► completed
  │                         │                  │    │
  │                         │                  │    ├──► failed
  │                         │                  │    └──► paused
  └─ add without start ─────┘                  │
                                               ▼
                                           resuming ──etag mismatch──► failed
                                               │                      (remote changed;
                                               └── match ──► downloading   user may restart)
```

`stopped` is reserved for a user cancel that keeps the partial. `failed.error_code` distinguishes `link_expired` (403/410), `auth_required`, `remote_changed`, `not_resumable_interrupted`, `checksum_mismatch`, `io`, `network`.

Global pause sets every `downloading` row to `paused` and aborts those workers. Global resume re-enters `resuming`.

## 4. Segment bytes

Ranges are inclusive. A file of `S` bytes is the interval `[0, S)`.

Initial plan, `N` segments (default 8, clamp 1–32):

- If `S` is unknown, or the server did not answer 206: **one** open-ended stream. `downloads.resumable = 0`. The UI copy is "not resumable".
- If `S < min_segment_bytes` (default 1 MiB): one segment covering `[0, S)`.
- Otherwise `chunk = ceil(S / N)`, segment `i` is `[i*chunk, min(S, (i+1)*chunk))`. The last segment ends at `S`. Fewer than `N` segments are used when the minimum size would be violated. The user's `N` is a maximum, not a reason to open 32 connections for a small file.

Dynamic split (Phase 2), under the segment-map lock:

- When a worker finishes and a connection slot is free, pick the active segment with the largest remainder `end - cursor`.
- If that remainder is at least `2 * min_segment_bytes`, shrink the active segment's `end` to the midpoint and insert a new segment for the tail.
- The active worker must observe the new `end` and stop. Segments never overlap and their union stays `[0, S)`.

## 5. Disk write protocol

This is the corruption rule. Tests lock it in.

1. `set_len(S)` once the size is known, so the file is sparse (NTFS, APFS, ext4) and every write has a final offset. No merge pass.
2. Each worker writes only with positional I/O (`seek_write` / `write_at`) inside its `[start, end)`.
3. Cursor advances only after those bytes are flushed. The cursor is the next byte to fetch.
4. SQLite is updated after the flush, at least once per second and on pause, resume, complete, and failure.
5. A crash between flush and commit re-downloads a suffix and overwrites it. A crash never leaves a hole that we believe is done.

Non-resumable streams append sequentially. Interrupt means the partial cannot be continued. Status becomes `failed` / `not_resumable_interrupted` unless the transfer already finished.

On resume, probe again. Continue only when `ETag` matches, or when `ETag` was absent and `Last-Modified` + `Content-Length` match. A mismatch emits `Failed { reason: remote_changed }` and does not delete the partial until the user confirms restart.

Checksum (MD5, SHA-1, SHA-256), when the user supplied one, is computed while writing and checked once at completion.

## 6. Events

`tokio::sync::broadcast`. Payload is serde-tagged JSON so Tauri can forward it unchanged.

```
Added        { id }
Probed       { id, final_url, total_bytes, resumable, content_type, filename }
Started      { id }
Progress     { id, downloaded_bytes, total_bytes, speed_bps, eta_ms, segments[] }
Paused       { id }
Resumed      { id }
Completed    { id, path, sha256 }
Failed       { id, reason, message }
Removed      { id, deleted_file }
```

`segments[]` is `{ start, end, cursor }` so the details panel can draw the connection map without a second query. The engine coalesces `Progress` to about 8 Hz per download. Tests can disable the coalesce.

## 7. Database

SQLite, `journal_mode=WAL`, `foreign_keys=ON`, `synchronous=FULL`. Timestamps are Unix milliseconds. Primary keys are UUID strings.

Phase 1 creates every table. Unused columns stay null until their phase. That avoids a migration in the middle of the desktop work.

```sql
CREATE TABLE schema_migrations (
  version     INTEGER PRIMARY KEY,
  applied_at  INTEGER NOT NULL
);

CREATE TABLE settings (
  key    TEXT PRIMARY KEY,
  value  TEXT NOT NULL              -- JSON scalar or object
);

CREATE TABLE categories (
  id          TEXT PRIMARY KEY,
  name        TEXT NOT NULL,
  folder      TEXT,                 -- null: engine default category dir
  extensions  TEXT NOT NULL,        -- JSON string array, lower-case, no dots
  sort_order  INTEGER NOT NULL
);

CREATE TABLE queues (
  id                  TEXT PRIMARY KEY,
  name                TEXT NOT NULL UNIQUE,
  concurrency         INTEGER NOT NULL DEFAULT 3,
  speed_limit_bps     INTEGER,      -- null: unlimited
  schedule_start      TEXT,         -- "HH:MM" local, null: always open
  schedule_end        TEXT,
  days_mask           INTEGER NOT NULL DEFAULT 127, -- bit 0 Sun .. bit 6 Sat
  on_complete         TEXT NOT NULL DEFAULT 'none',
      -- none | open_file | run_command | sleep | shutdown | quit
  on_complete_command TEXT,
  created_at          INTEGER NOT NULL
);

CREATE TABLE credentials (
  id           TEXT PRIMARY KEY,
  host         TEXT NOT NULL,
  username     TEXT NOT NULL,
  keychain_id  TEXT NOT NULL,       -- secret lives in the OS keychain
  auth_scheme  TEXT NOT NULL DEFAULT 'any', -- any | basic | digest
  created_at   INTEGER NOT NULL,
  UNIQUE (host, username)
);

CREATE TABLE downloads (
  id                   TEXT PRIMARY KEY,
  url                  TEXT NOT NULL,
  final_url            TEXT,
  filename             TEXT NOT NULL,
  save_dir             TEXT NOT NULL,
  partial_path         TEXT NOT NULL,
  category_id          TEXT REFERENCES categories(id),
  queue_id             TEXT REFERENCES queues(id),
  priority             INTEGER NOT NULL DEFAULT 0, -- lower runs first
  status               TEXT NOT NULL,
      -- queued | probing | ready | downloading | paused | completed | failed | stopped
  resumable            INTEGER NOT NULL DEFAULT 0,
  total_bytes          INTEGER,
  downloaded_bytes     INTEGER NOT NULL DEFAULT 0,
  etag                 TEXT,
  last_modified        TEXT,
  content_type         TEXT,
  segment_count        INTEGER NOT NULL DEFAULT 8,
  user_agent           TEXT,
  referer              TEXT,
  headers_json         TEXT,
  cookies_json         TEXT,        -- per-download jar; local DB only
  proxy_json           TEXT,        -- null: follow global proxy mode
  credential_id        TEXT REFERENCES credentials(id),
  insecure_tls         INTEGER NOT NULL DEFAULT 0,
  checksum_algo        TEXT,        -- md5 | sha1 | sha256
  checksum_expected    TEXT,
  virus_scan_command   TEXT,
  error_code           TEXT,
  error_message        TEXT,
  retry_count          INTEGER NOT NULL DEFAULT 0,
  max_retries          INTEGER NOT NULL DEFAULT 5,
  speed_limit_bps      INTEGER,
  duplicate_policy     TEXT NOT NULL DEFAULT 'rename', -- rename | overwrite | ask
  created_at           INTEGER NOT NULL,
  updated_at           INTEGER NOT NULL,
  started_at           INTEGER,
  completed_at         INTEGER
);

CREATE INDEX idx_downloads_status ON downloads(status);
CREATE INDEX idx_downloads_queue  ON downloads(queue_id, priority, created_at);

CREATE TABLE segments (
  id            TEXT PRIMARY KEY,
  download_id   TEXT NOT NULL REFERENCES downloads(id) ON DELETE CASCADE,
  start_byte    INTEGER NOT NULL,   -- inclusive
  end_byte      INTEGER,            -- exclusive; null only for an open-ended stream
  cursor_byte   INTEGER NOT NULL,   -- next byte to fetch
  status        TEXT NOT NULL,      -- pending | active | done | failed
  updated_at    INTEGER NOT NULL
);

CREATE INDEX idx_segments_download ON segments(download_id, start_byte);

CREATE TABLE download_log (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  download_id  TEXT NOT NULL REFERENCES downloads(id) ON DELETE CASCADE,
  ts           INTEGER NOT NULL,
  level        TEXT NOT NULL,       -- info | warn | error
  message      TEXT NOT NULL
);

CREATE TABLE site_rules (
  id          TEXT PRIMARY KEY,
  pattern     TEXT NOT NULL,        -- host glob, used by the extension
  action      TEXT NOT NULL,        -- intercept | exclude
  created_at  INTEGER NOT NULL
);
```

Seeded rows: queue `Default` (concurrency 3, always open) and categories Video, Music, Documents, Compressed, Programs, Other, with editable extension lists.

`history` is `downloads` where `status = 'completed'`. No second copy of the row.

Filename and path rules, applied before any create:

- Resolve `save_dir` and reject a `filename` that escapes it (`..`, absolute paths, drive-relative paths, NUL).
- Strip control characters and Windows-forbidden characters. Reject reserved device names (`CON`, `PRN`, `AUX`, `NUL`, `COM1`–`COM9`, `LPT1`–`LPT9`). Trim trailing dots and spaces.
- Duplicate policy default is `rename` (`name (1).ext`). `overwrite` replaces. `ask` is a desktop prompt; the CLI treats `ask` as an error so a headless run cannot block.

## 8. Networking behavior

Probe:

1. `HEAD` the URL. Store the final URL after redirects.
2. Read `Content-Length`, `Accept-Ranges`, `ETag`, `Last-Modified`, `Content-Type`, `Content-Disposition`.
3. If the status is 405 or 501, or there is no length, send `GET` with `Range: bytes=0-0`.
4. `206` plus `Content-Range: bytes 0-0/S` means resumable and size `S`.
5. `200` means the server ignored the range. Single stream. If `Content-Length` is missing (chunked), size stays null and the download is not resumable.

Workers send `Range: bytes=cursor-end-1` for a finite segment. They accept `206` only. A `200` mid-download aborts that plan and, if no byte has been committed past the start, falls back to a single stream.

Retry (Phase 2): full jittered exponential backoff, honor `Retry-After` on 429 and 503, cap at `max_retries`. 403 and 410 become `link_expired` with the message "link expired, refresh from browser".

Auth (Phase 2): reqwest basic and digest. Password material is written only through the `keyring` crate (Windows Credential Manager, macOS Keychain, libsecret). The database stores `keychain_id`.

Proxy (Phase 2): global mode `system | direct | http | https | socks5`, optional per-download `proxy_json` override.

Bandwidth (Phase 2): a process-wide token bucket plus an optional per-download bucket. Segment tasks acquire tokens before writing to the socket reader.

Limits: `max_concurrent_downloads` default 3. `per_host_connections` default 16, shared across jobs. Per-download segment count is additionally capped by the host budget.

TLS verification is on. `insecure_tls` is stored per download and the CLI requires an explicit flag. The desktop UI shows a warning before that flag is set.

## 9. Desktop, extension, media

Phase 3 landed the desktop shell in `apps/desktop`. It is a separate Cargo workspace (`forge-dm-desktop`, identifier `com.forgedownloadmanager.desktop`) so root `cargo test --workspace` does not compile the WebView. Tauri commands are thin wrappers (`add`, `pause`, `resume`, `remove`, `list`, settings, segments, and the default save directory). The React UI subscribes to the same event enum when this process owns the engine, and throttles progress again while the document is hidden. The main list is virtualized. The details panel reads `segments[]` for the map and keeps a 48-sample ring buffer for the speed graph. English strings live in `apps/desktop/src/locales/en.json`.

The owner serves the existing HTTP control API and leaves it up while the queue is idle, so `forge-dm` can forward. Closing the window leaves downloads running. The next open requeues rows that were still downloading, which is the recovery `Engine::open` already performs. If the lock is already held, the shell forwards over HTTP and polls `GET /v1/downloads` about every 500ms. This phase has no WebSocket.

`insecure_tls` stays off until the user confirms a warning. Duplicate policy `ask` opens a prompt and retries the add as rename or overwrite. The CLI still treats `ask` as an error. `npm run dev` outside the shell draws the interface and does not start downloads.

Tauri is 2.12.4 and tauri-build is 2.7.3. The desktop crate's rust-version is 1.90. The root workspace stays on 1.89. The Tauri `dbus` feature is off. The window uses system fonts and an original anvil mark.

Extension (Phase 5) permissions, each for one reason:

| Permission | Why |
| --- | --- |
| `downloads` | See `onCreated`, cancel intercepted types |
| `webRequest` | Observe the request URL when the downloads API omits it |
| `cookies` | Forward cookies for the target URL only |
| `contextMenus` | "Download with Forge Download Manager" and the link actions |
| `nativeMessaging` | Talk to the desktop host |
| `activeTab` / `scripting` | Media button and "download selected links" on the current tab |
| `storage` | Exclusion list and the Alt-bypass setting |
| host permission | Content script for the floating media button |

No `<all_urls>` unless interception is enabled. Host access is requested when the user turns interception on.

Media (Phase 6): HLS and DASH parsers produce a segment list and reuse the normal worker pool, then remux with ffmpeg. ffmpeg is detected on `PATH` first. A bundled GPL build is not linked into this binary (see open questions). `yt-dlp` is an optional plugin downloaded on first use after a consent prompt, never vendored, and never used to strip DRM.

## 10. Phase 1 cut line

In scope:

- Workspace, MIT license, this document.
- Core: add URL, probe, equal segment split, parallel range download, single-stream fallback, sparse positional writes, pause, resume, 1-second SQLite flush, recovery of `downloading` rows on open (reload cursors and continue).
- Events listed in section 6.
- CLI: `add`, `list`, `pause`, `resume`, `remove`, `run` (foreground until idle).
- Tests: segment math unit tests, proptest coverage invariants, filename sanitizer, mock HTTP server (ranges, no ranges, redirect, mid-stream disconnect, ETag change), crash-recovery (kill child process, restart, compare hash).

Out of scope until later phases: dynamic split, retry policy, proxy, auth, cookies, FTP, limiter, queues beyond the seeded Default queue, UI, extension, HLS, installers.

Phase 2 has since landed dynamic split, the retry policy, proxy, auth, cookies, an in-tree FTP/FTPS client, and the bandwidth limiter. Phase 3 has since landed the desktop shell in section 9. Still later: queues beyond Default, the extension, the site grabber, HLS, installers, SFTP, and a Linux OS keychain.

Definition of done: `cargo test --workspace` passes, and a manual `forge-dm add` of a range-capable file pauses, resumes, and matches the source hash.

## 11. Planned dependencies and licenses

Runtime (Phase 1): tokio, tokio-util, reqwest (rustls, stream), rusqlite (bundled), serde, serde_json, thiserror, tracing, tracing-subscriber, uuid, url, bytes, clap, sha2, hex. All MIT or Apache-2.0. Bundled SQLite is public domain.

Dev: proptest, tempfile. MIT or Apache-2.0.

Phase 2 also uses keyring 3 on Windows and macOS only (`windows-native` / `apple-native`), base64, rustls-platform-verifier, and tokio-rustls with default features off (`ring`, `tls12`). FTP/FTPS is in-tree and does not add a crate. Linux does not enable keyring's secret-service features.

Phase 3 links tauri 2.12.4 and tauri-build 2.7.3 (Apache-2.0 OR MIT). The Tauri `dbus` feature is off. The UI is React 19, Tailwind CSS 4, and i18next, English only. `webextension-polyfill` is MPL-2.0 and will not be used; the extension will call `chrome.*` directly with a tiny `browser` shim.

Tauri and Tailwind pull in MPL-2.0 libraries that this repo does not name as direct dependencies. Tauri's `dom_query` brings `cssparser` and `selectors`. Tauri's `dirs` brings `option-ext`. Tailwind 4 brings `lightningcss`. `docs/THIRD_PARTY.md` lists them. On Linux the shell also loads the system WebKitGTK library at runtime. That library is LGPL and is not vendored.

Anything outside MIT, Apache-2.0, BSD, ISC, or public domain is not added without asking.

## 12. Open questions

These do not block Phase 1 if the defaults below stand.

1. **ffmpeg.** Default: search `PATH`, and only with consent download a separate LGPL ffmpeg build. Do not link GPL ffmpeg into the MIT app. Confirm before Phase 6.
2. **yt-dlp.** Default: optional, consent-gated download on first use, no DRM bypass, user is responsible for copyright and site terms. Confirm before Phase 6.
3. **Cookies at rest.** Passwords go to the OS keychain. Per-download cookies sit in the local SQLite file, not the keychain. Confirm.
4. **CLI name.** `forge-dm`, display name "Forge Download Manager". Confirm.
5. **Minimum segment size.** 1 MiB, so `N` is a maximum and small files use one connection. Confirm.
