use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::error::Error;
use crate::secrets::SecretStore;
use crate::Result;

/// Runtime configuration. `None` means "use the value stored in settings".
#[derive(Clone)]
pub struct EngineConfig {
    pub db_path: PathBuf,
    pub max_concurrent_downloads: Option<usize>,
    pub per_host_connections: Option<usize>,
    pub default_segments: Option<u32>,
    pub min_segment_bytes: Option<u64>,
    pub user_agent: Option<String>,
    /// `Some` replaces the settings value, including `Some(0)` for unlimited.
    pub speed_limit_bps: Option<u64>,
    /// `None` uses the OS keychain. Tests pass [`crate::secrets::MemorySecrets`].
    pub secrets: Option<Arc<dyn SecretStore>>,
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
            speed_limit_bps: None,
            secrets: None,
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
    /// `0` is unlimited.
    pub speed_limit_bps: u64,
    pub flush_interval: Duration,
    pub progress_interval: Duration,
}

pub const SETTING_KEYS: &[&str] = &[
    "proxy_mode",
    "proxy_url",
    "speed_limit_bps",
    "max_concurrent_downloads",
    "per_host_connections",
    "default_segments",
    "min_segment_bytes",
    "user_agent",
];

pub fn validate_setting(key: &str, value: &serde_json::Value) -> Result<()> {
    match key {
        "proxy_mode" => {
            let mode = value.as_str().ok_or_else(|| Error::Failed("proxy_mode must be a string".into()))?;
            if !matches!(mode, "system" | "direct" | "http" | "https" | "socks5") {
                return Err(Error::Failed("proxy_mode must be system, direct, http, https, or socks5".into()));
            }
        }
        "user_agent" => {
            if !value.is_string() {
                return Err(Error::Failed("user_agent must be a string".into()));
            }
        }
        "proxy_url" | "speed_limit_bps" => {
            if !value.is_null() && value.as_u64().is_none() && value.as_str().is_none() && key == "proxy_url" {
                return Err(Error::Failed("proxy_url must be a string or null".into()));
            }
            if key == "speed_limit_bps" && !value.is_null() && value.as_u64().is_none() {
                return Err(Error::Failed("speed_limit_bps must be a number or null".into()));
            }
            if key == "proxy_url" && value.as_u64().is_some() {
                return Err(Error::Failed("proxy_url must be a string or null".into()));
            }
        }
        "max_concurrent_downloads" | "per_host_connections" | "default_segments" | "min_segment_bytes" => {
            let number = value.as_u64().ok_or_else(|| Error::Failed(format!("{key} must be a number")))?;
            if number == 0 {
                return Err(Error::Failed(format!("{key} must be at least 1")));
            }
        }
        _ => return Err(Error::Failed(format!("unknown setting {key}"))),
    }
    Ok(())
}

/// Parse a CLI value. JSON is accepted; a bare word is a string for text settings.
pub fn parse_setting_value(key: &str, raw: &str) -> Result<serde_json::Value> {
    if !SETTING_KEYS.contains(&key) {
        return Err(Error::Failed(format!("unknown setting {key}")));
    }
    let value = if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(raw) {
        parsed
    } else if matches!(key, "proxy_mode" | "proxy_url" | "user_agent") {
        serde_json::Value::String(raw.to_string())
    } else {
        return Err(Error::Failed(format!("{key} value is not JSON")));
    };
    validate_setting(key, &value)?;
    Ok(value)
}

pub const DEFAULT_USER_AGENT: &str = "ForgeDownloadManager/0.1";
pub const FILENAME_LOCK_MARK: &str = "filename-locked";
