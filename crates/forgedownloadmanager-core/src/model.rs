use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::Error;

/// Unix milliseconds. SQLite stores this as an integer.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

pub fn u64_to_i64(value: u64) -> Result<i64, Error> {
    i64::try_from(value).map_err(|_| Error::Failed("value does not fit in a sqlite integer".into()))
}

pub fn i64_to_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadStatus {
    Queued,
    Probing,
    Ready,
    Downloading,
    Paused,
    Completed,
    Failed,
    Stopped,
}

impl DownloadStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Probing => "probing",
            Self::Ready => "ready",
            Self::Downloading => "downloading",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
        }
    }

    pub fn parse(value: &str) -> Result<Self, Error> {
        Ok(match value {
            "queued" => Self::Queued,
            "probing" => Self::Probing,
            "ready" => Self::Ready,
            "downloading" => Self::Downloading,
            "paused" => Self::Paused,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "stopped" => Self::Stopped,
            other => {
                return Err(Error::Failed(format!("unknown download status {other}")));
            }
        })
    }

    pub fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Probing | Self::Ready | Self::Downloading)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentStatus {
    Pending,
    Active,
    Done,
    Failed,
}

impl SegmentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Result<Self, Error> {
        Ok(match value {
            "pending" => Self::Pending,
            "active" => Self::Active,
            "done" => Self::Done,
            "failed" => Self::Failed,
            other => return Err(Error::Failed(format!("unknown segment status {other}"))),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DuplicatePolicy {
    Rename,
    Overwrite,
    Ask,
}

impl DuplicatePolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rename => "rename",
            Self::Overwrite => "overwrite",
            Self::Ask => "ask",
        }
    }

    pub fn parse(value: &str) -> Result<Self, Error> {
        Ok(match value {
            "rename" => Self::Rename,
            "overwrite" => Self::Overwrite,
            "ask" => Self::Ask,
            other => {
                return Err(Error::Failed(format!("unknown duplicate policy {other}")));
            }
        })
    }
}

impl Default for DuplicatePolicy {
    fn default() -> Self {
        Self::Rename
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChecksumAlgo {
    Md5,
    Sha1,
    Sha256,
}

impl ChecksumAlgo {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Md5 => "md5",
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
        }
    }

    pub fn parse(value: &str) -> Result<Self, Error> {
        Ok(match value.to_ascii_lowercase().as_str() {
            "md5" => Self::Md5,
            "sha1" | "sha-1" => Self::Sha1,
            "sha256" | "sha-256" => Self::Sha256,
            other => return Err(Error::Failed(format!("unknown checksum algorithm {other}"))),
        })
    }
}

/// What the caller asked to add. The engine fills in probe results later.
/// `password` is handed to the keychain and is not stored on the download row.
#[derive(Clone)]
pub struct AddRequest {
    pub url: String,
    pub save_dir: PathBuf,
    pub filename: Option<String>,
    pub segments: Option<u32>,
    pub start: bool,
    pub referer: Option<String>,
    pub user_agent: Option<String>,
    pub headers: Vec<(String, String)>,
    pub cookies: Vec<(String, String)>,
    pub proxy: Option<String>,
    pub speed_limit_bps: Option<u64>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub max_retries: Option<u32>,
    pub insecure_tls: bool,
    pub duplicate_policy: DuplicatePolicy,
    pub checksum_algo: Option<ChecksumAlgo>,
    pub checksum_expected: Option<String>,
    pub queue_id: Option<Uuid>,
    pub priority: Option<i64>,
}

impl AddRequest {
    pub fn new(url: impl Into<String>, save_dir: impl Into<PathBuf>) -> Self {
        Self {
            url: url.into(),
            save_dir: save_dir.into(),
            filename: None,
            segments: None,
            start: true,
            referer: None,
            user_agent: None,
            headers: Vec::new(),
            cookies: Vec::new(),
            proxy: None,
            speed_limit_bps: None,
            username: None,
            password: None,
            max_retries: None,
            insecure_tls: false,
            duplicate_policy: DuplicatePolicy::Rename,
            checksum_algo: None,
            checksum_expected: None,
            queue_id: None,
            priority: None,
        }
    }
}

impl std::fmt::Debug for AddRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AddRequest")
            .field("url", &self.url)
            .field("save_dir", &self.save_dir)
            .field("filename", &self.filename)
            .field("segments", &self.segments)
            .field("start", &self.start)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "redacted"))
            .field("max_retries", &self.max_retries)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Download {
    pub id: Uuid,
    pub url: String,
    pub final_url: Option<String>,
    pub filename: String,
    pub save_dir: PathBuf,
    pub partial_path: PathBuf,
    pub queue_id: Uuid,
    pub priority: i64,
    pub status: DownloadStatus,
    pub resumable: bool,
    pub total_bytes: Option<u64>,
    pub downloaded_bytes: u64,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_type: Option<String>,
    pub segment_count: u32,
    pub user_agent: Option<String>,
    pub referer: Option<String>,
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub cookies: Vec<(String, String)>,
    #[serde(default)]
    pub proxy: Option<String>,
    #[serde(default)]
    pub credential_id: Option<Uuid>,
    #[serde(default)]
    pub speed_limit_bps: Option<u64>,
    pub insecure_tls: bool,
    pub checksum_algo: Option<ChecksumAlgo>,
    pub checksum_expected: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub retry_count: u32,
    pub max_retries: u32,
    pub duplicate_policy: DuplicatePolicy,
    pub created_at: i64,
    pub updated_at: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
}

impl Download {
    pub fn final_path(&self) -> PathBuf {
        self.save_dir.join(&self.filename)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    pub id: Uuid,
    pub download_id: Uuid,
    pub start_byte: u64,
    /// Exclusive end. `None` is an open-ended single stream.
    pub end_byte: Option<u64>,
    pub cursor_byte: u64,
    pub status: SegmentStatus,
    pub updated_at: i64,
}

impl Segment {
    pub fn remaining(&self) -> Option<u64> {
        self.end_byte.map(|end| end.saturating_sub(self.cursor_byte))
    }

    pub fn is_complete(&self) -> bool {
        match self.end_byte {
            Some(end) => self.cursor_byte >= end,
            None => false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentProgress {
    pub start: u64,
    pub end: Option<u64>,
    pub cursor: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SiteRuleAction {
    Intercept,
    Exclude,
}

impl SiteRuleAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Intercept => "intercept",
            Self::Exclude => "exclude",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "intercept" => Some(Self::Intercept),
            "exclude" => Some(Self::Exclude),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiteRule {
    pub id: Uuid,
    pub pattern: String,
    pub action: SiteRuleAction,
    pub created_at: i64,
}

impl SiteRule {
    pub fn new(pattern: impl Into<String>, action: SiteRuleAction) -> Self {
        Self {
            id: Uuid::new_v4(),
            pattern: pattern.into(),
            action,
            created_at: now_ms(),
        }
    }

    /// Tests if a hostname matches this rule's pattern (supports exact match, domain suffix, or wildcard).
    pub fn matches_host(&self, host: &str) -> bool {
        let pattern = self.pattern.trim().to_ascii_lowercase();
        let host = host.trim().to_ascii_lowercase();
        if pattern == host {
            return true;
        }
        if let Some(suffix) = pattern.strip_prefix("*.") {
            if host == suffix || host.ends_with(&format!(".{suffix}")) {
                return true;
            }
        }
        if let Some(suffix) = pattern.strip_prefix('.') {
            if host == suffix || host.ends_with(&format!(".{suffix}")) {
                return true;
            }
        }
        if pattern.starts_with('*') && pattern.ends_with('*') && pattern.len() > 2 {
            let inner = &pattern[1..pattern.len() - 1];
            return host.contains(inner);
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_rule_matching() {
        let rule_exact = SiteRule::new("example.com", SiteRuleAction::Intercept);
        assert!(rule_exact.matches_host("example.com"));
        assert!(rule_exact.matches_host("EXAMPLE.COM"));
        assert!(!rule_exact.matches_host("sub.example.com"));
        assert!(!rule_exact.matches_host("other.com"));

        let rule_wildcard = SiteRule::new("*.github.com", SiteRuleAction::Exclude);
        assert!(rule_wildcard.matches_host("github.com"));
        assert!(rule_wildcard.matches_host("raw.github.com"));
        assert!(rule_wildcard.matches_host("assets.cdn.github.com"));
        assert!(!rule_wildcard.matches_host("notgithub.com"));

        let rule_contains = SiteRule::new("*video*", SiteRuleAction::Intercept);
        assert!(rule_contains.matches_host("myvideosite.org"));
        assert!(rule_contains.matches_host("stream.video.cdn.net"));
        assert!(!rule_contains.matches_host("audio.cdn.net"));
    }

    #[test]
    fn site_rule_action_parsing() {
        assert_eq!(SiteRuleAction::parse("intercept"), Some(SiteRuleAction::Intercept));
        assert_eq!(SiteRuleAction::parse("INTERCEPT"), Some(SiteRuleAction::Intercept));
        assert_eq!(SiteRuleAction::parse("exclude"), Some(SiteRuleAction::Exclude));
        assert_eq!(SiteRuleAction::parse("  EXCLUDE  "), Some(SiteRuleAction::Exclude));
        assert_eq!(SiteRuleAction::parse("invalid"), None);
    }
}
