//! Segmented download engine.
//!
//! Phase 2 adds dynamic splitting, retries, proxies, keychain authentication,
//! cookies, a bandwidth limiter, and FTP/FTPS. Phase 3's desktop shell links
//! this crate in-process and shares [`lock`] with the CLI.

pub mod auth;
pub mod checksum;
pub mod config;
pub mod control;
pub mod error;
pub mod events;
pub mod filename;
pub mod grabber;
pub mod limiter;
pub mod lock;
pub mod media;
pub mod model;
pub mod probe;
pub mod proxy;
pub mod retry;
pub mod schedule;
pub mod secrets;
pub mod segment;
pub mod store;

mod download;
mod engine;
mod ftp;
mod rawhttp;
mod writer;

pub use config::EngineConfig;
pub use engine::{insert_request, Engine};
pub use error::Error;
pub use events::EngineEvent;
pub use model::{
    AddRequest, ChecksumAlgo, Download, DownloadStatus, DuplicatePolicy, Segment, SegmentProgress,
    SegmentStatus, SiteRule, SiteRuleAction,
};
pub use media::{
    check_media_tools, create_media_client, download_and_remux_stream, download_media_url,
    parse_dash, parse_hls, probe_media_stream, resolve_media_segments, DashManifest,
    DashRepresentation, FfmpegRemuxer, HlsPlaylist, HlsSegment, HlsVariant, MediaProbeResult,
    MediaToolsStatus, YtDlpPlugin,
};
pub use schedule::{OnCompleteAction, Queue};
pub use tokio_util::sync::CancellationToken;

#[doc(hidden)]
pub mod testutil;

pub type Result<T> = std::result::Result<T, Error>;
