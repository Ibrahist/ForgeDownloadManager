//! Segmented download engine.
//!
//! Phase 1 covers HTTP(S) probe, equal segment splits, sparse writes,
//! pause/resume, and SQLite crash recovery. Dynamic splitting, retries,
//! proxies, and authentication are later phases.

pub mod checksum;
pub mod config;
pub mod control;
pub mod error;
pub mod events;
pub mod filename;
pub mod model;
pub mod probe;
pub mod segment;
pub mod store;

mod download;
mod engine;
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
