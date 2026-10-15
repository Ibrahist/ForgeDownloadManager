use reqwest::header::{
    ACCEPT_ENCODING, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG,
    LAST_MODIFIED, RANGE, REFERER, USER_AGENT,
};
use reqwest::{Client, RequestBuilder, Response, StatusCode};

use crate::error::Error;
use crate::filename::filename_from_content_disposition;
use crate::Result;

/// Headers the engine adds to every transfer. User headers were checked at intake.
#[derive(Debug, Clone)]
pub struct RequestMeta {
    pub user_agent: String,
    pub referer: Option<String>,
    pub headers: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub final_url: String,
    pub total_bytes: Option<u64>,
    pub resumable: bool,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_type: Option<String>,
    pub filename: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentRange {
    pub start: Option<u64>,
    pub end_inclusive: Option<u64>,
    pub total: Option<u64>,
}

pub fn apply_request(builder: RequestBuilder, meta: &RequestMeta) -> RequestBuilder {
    // Identity keeps Content-Length and ranges equal to the bytes we store.
    let mut builder = builder
        .header(ACCEPT_ENCODING, "identity")
        .header(USER_AGENT, &meta.user_agent);
    if let Some(referer) = &meta.referer {
        builder = builder.header(REFERER, referer);
    }
    for (name, value) in &meta.headers {
        builder = builder.header(name, value);
    }
    builder
}

pub async fn probe_url(client: &Client, url: &str, meta: &RequestMeta) -> Result<Probe> {
    let head = match apply_request(client.head(url), meta).send().await {
        Ok(response) => Some(response),
        Err(err) => {
            tracing::debug!("HEAD failed, trying a ranged GET: {err}");
            None
        }
    };

    let head_snap = match head.as_ref() {
        Some(response) => {
            classify_probe_status(response.status())?;
            let status = response.status().as_u16();
            if status == 405 || status == 501 || status >= 400 {
                None
            } else {
                Some(snapshot(response))
            }
        }
        None => None,
    };
    drop(head);

    if head_snap.as_ref().and_then(|snap| snap.content_length) == Some(0) {
        let snap = head_snap.expect("checked");
        return Ok(finish_probe(snap, Some(0), true));
    }

    // A 200 here is the whole object. Read headers only and drop the body.
    let response = apply_request(client.get(url), meta)
        .header(RANGE, "bytes=0-0")
        .send()
        .await?;
    classify_probe_status(response.status())?;
    let status = response.status();
    let snap = snapshot(&response);
    drop(response);

    if status == StatusCode::PARTIAL_CONTENT {
        let total = snap.content_range.as_deref().and_then(parse_content_range).and_then(|item| item.total);
        let length = snap.content_length;
        return Ok(finish_probe(merge(head_snap, snap), total.or(length), total.is_some()));
    }

    if status == StatusCode::OK {
        let length = snap.content_length.or_else(|| head_snap.as_ref().and_then(|s| s.content_length));
        return Ok(finish_probe(merge(head_snap, snap), length, false));
    }

    if status == StatusCode::RANGE_NOT_SATISFIABLE {
        let total = snap
            .content_range
            .as_deref()
            .and_then(parse_content_range)
            .and_then(|item| item.total)
            .or(head_snap.as_ref().and_then(|s| s.content_length));
        if total == Some(0) {
            return Ok(finish_probe(merge(head_snap, snap), Some(0), true));
        }
        return Err(Error::Http {
            status: status.as_u16(),
            message: "range was not satisfiable".into(),
        });
    }

    Err(Error::Http {
        status: status.as_u16(),
        message: "probe failed".into(),
    })
}

fn finish_probe(snap: Snap, total_bytes: Option<u64>, resumable: bool) -> Probe {
    let filename = snap
        .disposition
        .as_deref()
        .and_then(filename_from_content_disposition);
    Probe {
        final_url: snap.final_url,
        total_bytes,
        resumable: resumable && total_bytes.is_some(),
        etag: snap.etag,
        last_modified: snap.last_modified,
        content_type: snap.content_type,
        filename,
    }
}

fn merge(head: Option<Snap>, get: Snap) -> Snap {
    let Some(head) = head else {
        return get;
    };
    Snap {
        final_url: if get.final_url.is_empty() { head.final_url } else { get.final_url },
        etag: get.etag.or(head.etag),
        last_modified: get.last_modified.or(head.last_modified),
        content_type: get.content_type.or(head.content_type),
        disposition: get.disposition.or(head.disposition),
        content_length: get.content_length.or(head.content_length),
        content_range: get.content_range.or(head.content_range),
    }
}

/// Continue only when the validators captured at the first probe still match.
pub fn ensure_same_remote(
    etag: Option<&str>,
    last_modified: Option<&str>,
    total_bytes: Option<u64>,
    probe: &Probe,
) -> Result<()> {
    if let Some(previous) = etag {
        if probe.etag.as_deref() != Some(previous) {
            return Err(Error::RemoteChanged);
        }
    } else if let Some(previous) = last_modified {
        if probe.last_modified.as_deref() != Some(previous) {
            return Err(Error::RemoteChanged);
        }
    }
    if let (Some(previous), Some(current)) = (total_bytes, probe.total_bytes) {
        if previous != current {
            return Err(Error::RemoteChanged);
        }
    }
    Ok(())
}

pub fn classify_probe_status(status: StatusCode) -> Result<()> {
    match status.as_u16() {
        401 => Err(Error::AuthRequired),
        403 | 410 => Err(Error::LinkExpired),
        404 => Err(Error::Http { status: 404, message: "not found".into() }),
        _ => Ok(()),
    }
}

pub fn classify_transfer_status(status: StatusCode) -> Result<()> {
    match status.as_u16() {
        200 | 206 => Ok(()),
        401 => Err(Error::AuthRequired),
        403 | 410 => Err(Error::LinkExpired),
        404 => Err(Error::Http { status: 404, message: "not found".into() }),
        416 => Err(Error::RemoteChanged),
        status => Err(Error::Http { status, message: "unexpected status".into() }),
    }
}

pub fn parse_content_range(value: &str) -> Option<ContentRange> {
    let rest = value.trim().strip_prefix("bytes ")?;
    let (range, total) = rest.split_once('/')?;
    let total = match total.trim() {
        "*" => None,
        text => Some(text.parse().ok()?),
    };
    if range.trim() == "*" {
        return Some(ContentRange { start: None, end_inclusive: None, total });
    }
    let (start, end) = range.split_once('-')?;
    Some(ContentRange {
        start: Some(start.trim().parse().ok()?),
        end_inclusive: Some(end.trim().parse().ok()?),
        total,
    })
}

struct Snap {
    final_url: String,
    etag: Option<String>,
    last_modified: Option<String>,
    content_type: Option<String>,
    disposition: Option<String>,
    content_length: Option<u64>,
    content_range: Option<String>,
}

fn snapshot(response: &Response) -> Snap {
    let headers = response.headers();
    let text = |name| {
        headers.get(name).and_then(|value| value.to_str().ok()).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
    };
    Snap {
        final_url: response.url().to_string(),
        etag: text(ETAG),
        last_modified: text(LAST_MODIFIED),
        content_type: text(CONTENT_TYPE),
        disposition: text(CONTENT_DISPOSITION),
        // Header only. response.content_length() is the body size hint, which is 0
        // for HEAD even when the resource is a chunked stream of unknown length.
        content_length: headers
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse().ok()),
        content_range: text(CONTENT_RANGE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_content_range() {
        let range = parse_content_range("bytes 0-0/12345").unwrap();
        assert_eq!(range.start, Some(0));
        assert_eq!(range.end_inclusive, Some(0));
        assert_eq!(range.total, Some(12345));
    }

    #[test]
    fn parses_an_unsatisfied_content_range() {
        let range = parse_content_range("bytes */0").unwrap();
        assert_eq!(range.total, Some(0));
        assert_eq!(range.start, None);
    }
}
