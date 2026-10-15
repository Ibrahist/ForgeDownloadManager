CREATE TABLE settings (
  key    TEXT PRIMARY KEY,
  value  TEXT NOT NULL
);

CREATE TABLE categories (
  id          TEXT PRIMARY KEY,
  name        TEXT NOT NULL,
  folder      TEXT,
  extensions  TEXT NOT NULL,
  sort_order  INTEGER NOT NULL
);

CREATE TABLE queues (
  id                  TEXT PRIMARY KEY,
  name                TEXT NOT NULL UNIQUE,
  concurrency         INTEGER NOT NULL DEFAULT 3,
  speed_limit_bps     INTEGER,
  schedule_start      TEXT,
  schedule_end        TEXT,
  days_mask           INTEGER NOT NULL DEFAULT 127,
  on_complete         TEXT NOT NULL DEFAULT 'none',
  on_complete_command TEXT,
  created_at          INTEGER NOT NULL
);

CREATE TABLE credentials (
  id           TEXT PRIMARY KEY,
  host         TEXT NOT NULL,
  username     TEXT NOT NULL,
  keychain_id  TEXT NOT NULL,
  auth_scheme  TEXT NOT NULL DEFAULT 'any',
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
  priority             INTEGER NOT NULL DEFAULT 0,
  status               TEXT NOT NULL,
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
  cookies_json         TEXT,
  proxy_json           TEXT,
  credential_id        TEXT REFERENCES credentials(id),
  insecure_tls         INTEGER NOT NULL DEFAULT 0,
  checksum_algo        TEXT,
  checksum_expected    TEXT,
  virus_scan_command   TEXT,
  error_code           TEXT,
  error_message        TEXT,
  retry_count          INTEGER NOT NULL DEFAULT 0,
  max_retries          INTEGER NOT NULL DEFAULT 5,
  speed_limit_bps      INTEGER,
  duplicate_policy     TEXT NOT NULL DEFAULT 'rename',
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
  start_byte    INTEGER NOT NULL,
  end_byte      INTEGER,
  cursor_byte   INTEGER NOT NULL,
  status        TEXT NOT NULL,
  updated_at    INTEGER NOT NULL
);

CREATE INDEX idx_segments_download ON segments(download_id, start_byte);

CREATE TABLE download_log (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  download_id  TEXT NOT NULL REFERENCES downloads(id) ON DELETE CASCADE,
  ts           INTEGER NOT NULL,
  level        TEXT NOT NULL,
  message      TEXT NOT NULL
);

CREATE TABLE site_rules (
  id          TEXT PRIMARY KEY,
  pattern     TEXT NOT NULL,
  action      TEXT NOT NULL,
  created_at  INTEGER NOT NULL
);
