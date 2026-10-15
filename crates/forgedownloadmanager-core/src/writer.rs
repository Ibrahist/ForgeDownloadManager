use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::Result;

/// One segment's handle. Workers do not share a handle, so seek + write stays ordered.
#[derive(Clone)]
pub struct SegmentFile {
    inner: Arc<Mutex<File>>,
}

impl SegmentFile {
    pub fn create(path: &Path, total_len: Option<u64>) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().read(true).write(true).create(true).open(path)?;
        if let Some(len) = total_len {
            if file.metadata()?.len() < len {
                // Sparse on NTFS, APFS, and ext4. Writes land at the final offset.
                file.set_len(len)?;
            }
        }
        Ok(Self { inner: Arc::new(Mutex::new(file)) })
    }

    pub async fn write_at(&self, offset: u64, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        let inner = Arc::clone(&self.inner);
        let data = data.to_vec();
        tokio::task::spawn_blocking(move || {
            let mut file = inner.lock().unwrap_or_else(|err| err.into_inner());
            file.seek(SeekFrom::Start(offset))?;
            file.write_all(&data)?;
            Ok(())
        })
        .await?
    }

    pub fn sync(&self) -> Result<()> {
        let file = self.inner.lock().unwrap_or_else(|err| err.into_inner());
        file.sync_data()?;
        Ok(())
    }
}
