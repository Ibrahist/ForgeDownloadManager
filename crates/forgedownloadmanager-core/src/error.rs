use uuid::Uuid;

/// Recoverable engine failure. `code()` is the stable string stored in SQLite.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("download {0} not found")]
    NotFound(Uuid),
    #[error("invalid id: {0}")]
    InvalidId(String),
    #[error("invalid url: {0}")]
    InvalidUrl(String),
    #[error("invalid filename")]
    InvalidFilename,
    #[error("path escapes the save directory")]
    PathEscape,
    #[error("link expired, refresh from browser")]
    LinkExpired,
    #[error("remote file changed; restart the download to get a consistent file")]
    RemoteChanged,
    #[error("not resumable; the partial download cannot be continued")]
    NotResumable,
    #[error("checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },
    #[error("authentication required")]
    AuthRequired,
    #[error("HTTP {status}: {message}")]
    Http { status: u16, message: String },
    #[error("incomplete response")]
    Incomplete,
    /// 429 or 503. `retry_after_ms` is set only for a delta-seconds Retry-After.
    #[error("rate limited")]
    RateLimited { retry_after_ms: Option<u64> },
    #[error("cancelled")]
    Cancelled,
    #[error("a filename already exists and the duplicate policy is ask")]
    DuplicateAsk,
    #[error("invalid state: {0}")]
    InvalidState(String),
    #[error("{0}")]
    Failed(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    HttpClient(#[from] reqwest::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Join(#[from] tokio::task::JoinError),
}

#[cfg(test)]
mod tests {
    use super::Error;

    #[test]
    fn duplicate_ask_has_its_own_code() {
        assert_eq!(Error::DuplicateAsk.code(), "duplicate_ask");
    }
}

impl Error {
    /// Stable code persisted on the download row.
    pub fn code(&self) -> &'static str {
        match self {
            Error::LinkExpired => "link_expired",
            Error::RemoteChanged => "remote_changed",
            Error::NotResumable => "not_resumable_interrupted",
            Error::ChecksumMismatch { .. } => "checksum_mismatch",
            Error::AuthRequired => "auth_required",
            Error::Io(_) | Error::PathEscape => "io",
            Error::Incomplete | Error::HttpClient(_) | Error::Http { .. } | Error::RateLimited { .. } => "network",
            Error::Cancelled => "cancelled",
            Error::DuplicateAsk => "duplicate_ask",
            _ => "failed",
        }
    }
}
