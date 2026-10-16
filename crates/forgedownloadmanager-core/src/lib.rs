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
pub mod limiter;
pub mod lock;
pub mod model;
pub mod probe;
pub mod proxy;
pub mod retry;
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
    SegmentStatus,
};

#[doc(hidden)]
pub mod testutil;

pub type Result<T> = std::result::Result<T, Error>;
