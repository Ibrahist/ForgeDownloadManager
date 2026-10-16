use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use reqwest::header::{RANGE, RETRY_AFTER};
use reqwest::{Client, StatusCode};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::auth;
use crate::error::Error;
use crate::limiter::Pace;
use crate::probe::{classify_transfer_status, parse_content_range, send_with_auth, RequestMeta};
use crate::retry;
use crate::writer::SegmentFile;
use crate::Result;

pub struct SegmentRt {
    pub id: uuid::Uuid,
    pub start: u64,
    /// `false` is an open-ended stream. `end` is ignored until the stream finishes.
    pub bounded: bool,
    pub end: AtomicU64,
    pub cursor: AtomicU64,
    /// Set when an open-ended stream reaches EOF.
    pub eof: AtomicBool,
    pub file: SegmentFile,
    /// Held across the end check and the positional write so a split cannot overlap that write.
    pub gate: Mutex<()>,
}

impl SegmentRt {
    pub fn live_end(&self) -> Option<u64> {
        if self.bounded {
            Some(self.end.load(Ordering::Acquire))
        } else {
            None
        }
    }

    pub fn finished(&self) -> bool {
        let cursor = self.cursor.load(Ordering::Acquire);
        self.eof.load(Ordering::Acquire) || self.live_end().is_some_and(|end| cursor >= end)
    }
}

pub struct Fetch<'a> {
    pub client: &'a Client,
    pub meta: &'a RequestMeta,
    pub url: &'a str,
    pub cancel: &'a CancellationToken,
    pub host: &'a Semaphore,
    pub pace: &'a Pace,
    pub max_retries: u32,
    pub on_retry: &'a (dyn Fn(u32) + Send + Sync),
}

/// Download one bounded region. `ranged` sends `Range` and requires 206.
pub async fn fetch_bounded(fetch: &Fetch<'_>, segment: &Arc<SegmentRt>, ranged: bool) -> Result<()> {
    let on_retry = fetch.on_retry;
    let max_retries = fetch.max_retries;
    let cancel = fetch.cancel;
    retry::retry_loop(max_retries, cancel, on_retry, || attempt_bounded(fetch, segment, ranged)).await
}

async fn attempt_bounded(fetch: &Fetch<'_>, segment: &Arc<SegmentRt>, ranged: bool) -> Result<()> {
    let end = segment.live_end().ok_or_else(|| Error::Failed("missing segment end".into()))?;
    let start = segment.cursor.load(Ordering::Acquire);
    if start >= end {
        return Ok(());
    }
    let _permit = acquire_host(fetch.host, fetch.cancel).await?;
    let target = auth::request_target(fetch.url);
    let response = send_with_auth(fetch.meta, "GET", &target, || {
        let mut builder = fetch.client.get(fetch.url);
        if ranged {
            let end_now = segment.live_end().unwrap_or(end);
            let start_now = segment.cursor.load(Ordering::Acquire);
            builder = builder.header(RANGE, format!("bytes={start_now}-{}", end_now.saturating_sub(1)));
        }
        builder
    })
    .await?;
    let status = response.status();
    classify_transfer_status(status, response.headers().get(RETRY_AFTER).and_then(|value| value.to_str().ok()))?;
    if ranged && status != StatusCode::PARTIAL_CONTENT {
        return Err(Error::Failed("server ignored the range request".into()));
    }
    if ranged {
        let start_now = segment.cursor.load(Ordering::Acquire);
        if let Some(header) = response.headers().get("content-range").and_then(|value| value.to_str().ok()) {
            if let Some(range) = parse_content_range(header) {
                if range.start.is_some() && range.start != Some(start_now) {
                    return Err(Error::Failed(format!(
                        "content-range started at {:?}, expected {start_now}",
                        range.start
                    )));
                }
            }
        }
    }
    read_body(response, segment, fetch.pace, fetch.cancel).await?;
    if segment.finished() {
        Ok(())
    } else {
        // The live end moved back, or the body stopped early. Either way the next try continues.
        Err(Error::Incomplete)
    }
}

/// Single stream with no known length. EOF completes the segment.
pub async fn fetch_until_eof(fetch: &Fetch<'_>, segment: &Arc<SegmentRt>) -> Result<()> {
    let on_retry = fetch.on_retry;
    retry::retry_loop(fetch.max_retries, fetch.cancel, on_retry, || attempt_eof(fetch, segment)).await
}

async fn attempt_eof(fetch: &Fetch<'_>, segment: &Arc<SegmentRt>) -> Result<()> {
    if !segment.bounded && segment.cursor.load(Ordering::Acquire) > segment.start {
        return Err(Error::NotResumable);
    }
    let _permit = acquire_host(fetch.host, fetch.cancel).await?;
    let target = auth::request_target(fetch.url);
    let response = send_with_auth(fetch.meta, "GET", &target, || fetch.client.get(fetch.url)).await?;
    classify_transfer_status(
        response.status(),
        response.headers().get(RETRY_AFTER).and_then(|value| value.to_str().ok()),
    )?;
    if response.status() == StatusCode::PARTIAL_CONTENT {
        return Err(Error::Failed("unexpected partial response for a single stream".into()));
    }
    read_body(response, segment, fetch.pace, fetch.cancel).await?;
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

async fn read_body(response: reqwest::Response, segment: &Arc<SegmentRt>, pace: &Pace, cancel: &CancellationToken) -> Result<()> {
    let mut stream = response.bytes_stream();
    loop {
        let offset = segment.cursor.load(Ordering::Acquire);
        if segment.live_end().is_some_and(|end| offset >= end) {
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
        let end_now = segment.live_end();
        if end_now.is_some_and(|end| offset >= end) {
            return Ok(());
        }
        let mut bytes = chunk.as_ref();
        if let Some(end) = end_now {
            let room = usize::try_from(end - offset).unwrap_or(usize::MAX);
            if bytes.len() > room {
                bytes = &bytes[..room];
            }
        }
        if bytes.is_empty() {
            return Ok(());
        }
        pace.acquire(bytes.len() as u64, cancel).await?;
        if commit_chunk(segment, bytes).await? {
            return Ok(());
        }
    }
    Ok(())
}

/// Write `bytes` at the current cursor, trimmed to the live end. Returns true when the segment is done.
///
/// The gate, the end, and the cursor move together so a split cannot publish an end the write already passed.
pub async fn commit_chunk(segment: &Arc<SegmentRt>, bytes: &[u8]) -> Result<bool> {
    let segment = Arc::clone(segment);
    let data = bytes.to_vec();
    tokio::task::spawn_blocking(move || {
        let _gate = segment.gate.lock().unwrap_or_else(|err| err.into_inner());
        let offset = segment.cursor.load(Ordering::Acquire);
        let live = segment.live_end();
        if live.is_some_and(|end| offset >= end) {
            return Ok(true);
        }
        let mut slice = data.as_slice();
        if let Some(end) = live {
            let room = usize::try_from(end - offset).unwrap_or(usize::MAX);
            if slice.len() > room {
                slice = &slice[..room];
            }
        }
        if slice.is_empty() {
            return Ok(true);
        }
        segment.file.write_at_blocking(offset, slice)?;
        let next = offset + slice.len() as u64;
        segment.cursor.store(next, Ordering::Release);
        Ok(segment.live_end().is_some_and(|end| next >= end))
    })
    .await?
}
