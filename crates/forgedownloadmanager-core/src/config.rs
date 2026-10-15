use std::path::PathBuf;
use std::time::Duration;

/// Runtime configuration. `None` means "use the value stored in settings".
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub db_path: PathBuf,
    pub max_concurrent_downloads: Option<usize>,
    pub per_host_connections: Option<usize>,
    pub default_segments: Option<u32>,
    pub min_segment_bytes: Option<u64>,
    pub user_agent: Option<String>,
    pub flush_interval: Duration,
    pub progress_interval: Duration,
}

impl EngineConfig {
    pub fn new(db_path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: db_path.into(),
            max_concurrent_downloads: None,
            per_host_connections: None,
            default_segments: None,
            min_segment_bytes: None,
            user_agent: None,
            flush_interval: Duration::from_secs(1),
            progress_interval: Duration::from_millis(125),
        }
    }
}

/// Values after settings and overrides are applied.
#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub max_concurrent_downloads: usize,
    pub per_host_connections: usize,
    pub default_segments: u32,
    pub min_segment_bytes: u64,
    pub user_agent: String,
    pub flush_interval: Duration,
    pub progress_interval: Duration,
}

pub const DEFAULT_USER_AGENT: &str = "ForgeDownloadManager/0.1";
pub const FILENAME_LOCK_MARK: &str = "filename-locked";
