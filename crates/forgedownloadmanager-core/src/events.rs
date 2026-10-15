use std::path::PathBuf;

use serde::Serialize;
use uuid::Uuid;

use crate::model::SegmentProgress;

/// Broadcast to the CLI and, later, the desktop UI.
/// `Progress` is coalesced to about 8 Hz per download.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EngineEvent {
    Added {
        id: Uuid,
    },
    Probed {
        id: Uuid,
        final_url: String,
        total_bytes: Option<u64>,
        resumable: bool,
        content_type: Option<String>,
        filename: String,
    },
    Started {
        id: Uuid,
    },
    Progress {
        id: Uuid,
        downloaded_bytes: u64,
        total_bytes: Option<u64>,
        speed_bps: u64,
        eta_ms: Option<u64>,
        segments: Vec<SegmentProgress>,
    },
    Paused {
        id: Uuid,
    },
    Resumed {
        id: Uuid,
    },
    Completed {
        id: Uuid,
        path: PathBuf,
        sha256: String,
    },
    Failed {
        id: Uuid,
        reason: String,
        message: String,
    },
    Removed {
        id: Uuid,
        deleted_file: bool,
    },
}

impl EngineEvent {
    pub fn id(&self) -> Uuid {
        match self {
            Self::Added { id }
            | Self::Probed { id, .. }
            | Self::Started { id }
            | Self::Progress { id, .. }
            | Self::Paused { id }
            | Self::Resumed { id }
            | Self::Completed { id, .. }
            | Self::Failed { id, .. }
            | Self::Removed { id, .. } => *id,
        }
    }
}
