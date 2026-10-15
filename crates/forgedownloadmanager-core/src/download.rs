use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use futures_util::StreamExt;
use reqwest::header::RANGE;
use reqwest::{Client, StatusCode};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::error::Error;
use crate::probe::{apply_request, classify_transfer_status, parse_content_range, RequestMeta};
use crate::writer::SegmentFile;
use crate::Result;

pub struct SegmentRt {
    pub id: uuid::Uuid,
    pub start: u64,
    pub end: Option<u64>,
    pub cursor: AtomicU64,
    /// Set when an open-ended stream reaches EOF.
    pub eof: AtomicBool,
    pub file: SegmentFile,
}

/// Download one bounded region. `ranged` sends `Range` and requires 206.
pub async fn fetch_bounded(
    client: &Client,
    meta: &RequestMeta,
    url: &str,
    segment: &SegmentRt,
    cancel: &CancellationToken,
    host: &Semaphore,
    ranged: bool,
) -> Result<()> {
    let end = segment.end.ok_or_else(|| Error::Failed("missing segment end".into()))?;
    let start = segment.cursor.load(Ordering::Acquire);
    if start >= end {
        return Ok(());
    }
    let _permit = acquire_host(host, cancel).await?;
    let mut builder = apply_request(client.get(url), meta);
    if ranged {
        builder = builder.header(RANGE, format!("bytes={start}-{}", end - 1));
    }
    let response = builder.send().await?;
    let status = response.status();
    classify_transfer_status(status)?;
    if ranged && status != StatusCode::PARTIAL_CONTENT {
        return Err(Error::Failed("server ignored the range request".into()));
    }
    if ranged {
        if let Some(header) = response.headers().get("content-range").and_then(|v| v.to_str().ok()) {
            if let Some(range) = parse_content_range(header) {
                if range.start.is_some() && range.start != Some(start) {
                    return Err(Error::Failed(format!(
                        "content-range started at {:?}, expected {start}",
                        range.start
                    )));
                }
            }
        }
    }
    read_body(response, segment, Some(end), cancel).await
}

/// Single stream with no known length. EOF completes the segment.
pub async fn fetch_until_eof(
    client: &Client,
    meta: &RequestMeta,
    url: &str,
    segment: &SegmentRt,
    cancel: &CancellationToken,
    host: &Semaphore,
) -> Result<()> {
    let _permit = acquire_host(host, cancel).await?;
    let response = apply_request(client.get(url), meta).send().await?;
    classify_transfer_status(response.status())?;
    if response.status() == StatusCode::PARTIAL_CONTENT {
        return Err(Error::Failed("unexpected partial response for a single stream".into()));
    }
    read_body(response, segment, None, cancel).await?;
    segment.eof.store(true, Ordering::Release);
    Ok(())
}

pub(crate) async fn acquire_host<'a>(
    host: &'a Semaphore,
    cancel: &CancellationToken,
) -> Result<tokio::sync::SemaphorePermit<'a>> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(Error::Cancelled),
        permit = host.acquire() => {
            permit.map_err(|_| Error::Failed("connection limit closed".into()))
        }
    }
}

async fn read_body(
    response: reqwest::Response,
    segment: &SegmentRt,
    end: Option<u64>,
    cancel: &CancellationToken,
) -> Result<()> {
    let mut stream = response.bytes_stream();
    loop {
        let offset = segment.cursor.load(Ordering::Acquire);
        if end.is_some_and(|end| offset >= end) {
            return Ok(());
        }
        let next = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(Error::Cancelled),
            item = stream.next() => item,
        };
        let Some(item) = next else {
            break;
        };
        let chunk = item?;
        if chunk.is_empty() {
            continue;
        }
        let mut bytes = chunk.as_ref();
        if let Some(end) = end {
            let room = usize::try_from(end - offset).unwrap_or(usize::MAX);
            if bytes.len() > room {
                bytes = &bytes[..room];
            }
        }
        segment.file.write_at(offset, bytes).await?;
        segment.cursor.store(offset + bytes.len() as u64, Ordering::Release);
        if end.is_some_and(|end| segment.cursor.load(Ordering::Acquire) >= end) {
            return Ok(());
        }
    }
    if let Some(end) = end {
        if segment.cursor.load(Ordering::Acquire) < end {
            return Err(Error::Incomplete);
        }
    }
    Ok(())
}
