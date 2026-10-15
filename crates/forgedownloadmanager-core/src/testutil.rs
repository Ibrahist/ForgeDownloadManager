//! In-process HTTP fixture for engine tests. Not a public API.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncWriteExt, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use crate::rawhttp;

#[derive(Clone)]
pub struct Behavior {
    pub body: Arc<Vec<u8>>,
    pub accept_ranges: bool,
    pub chunked: bool,
    pub disconnect_after: Option<usize>,
    pub disconnects_left: Arc<AtomicUsize>,
    pub force_status: Option<u16>,
    pub head_status: Option<u16>,
    pub delay: Duration,
    pub chunk_size: usize,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_type: Option<String>,
    pub content_disposition: Option<String>,
    /// While true, the handler writes no body bytes.
    pub stall: Arc<AtomicBool>,
    pub stop: CancellationToken,
}

impl Behavior {
    pub fn new(body: Vec<u8>) -> Self {
        Self {
            body: Arc::new(body),
            accept_ranges: true,
            chunked: false,
            disconnect_after: None,
            disconnects_left: Arc::new(AtomicUsize::new(0)),
            force_status: None,
            head_status: None,
            delay: Duration::ZERO,
            chunk_size: 64 * 1024,
            etag: Some("\"v1\"".into()),
            last_modified: None,
            content_type: Some("application/octet-stream".into()),
            content_disposition: None,
            stall: Arc::new(AtomicBool::new(false)),
            stop: CancellationToken::new(),
        }
    }
}

pub struct Mock {
    pub addr: std::net::SocketAddr,
    pub behavior: Arc<Mutex<Behavior>>,
    pub requests: Arc<Mutex<Vec<String>>>,
    shutdown: CancellationToken,
}

impl Mock {
    pub async fn spawn(mut behavior: Behavior) -> Self {
        let listener = TcpListener::bind(std::net::SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bind mock");
        let addr = listener.local_addr().expect("mock addr");
        let shutdown = CancellationToken::new();
        behavior.stop = shutdown.clone();
        let behavior = Arc::new(Mutex::new(behavior));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = shutdown.clone();
        let behavior_task = Arc::clone(&behavior);
        let requests_task = Arc::clone(&requests);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = stop.cancelled() => break,
                    accepted = listener.accept() => {
                        let sock = match accepted {
                            Ok((sock, _)) => sock,
                            Err(_) => {
                                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                                continue;
                            }
                        };
                        let behavior = Arc::clone(&behavior_task);
                        let requests = Arc::clone(&requests_task);
                        let port = addr.port();
                        tokio::spawn(async move {
                            let _ = handle(sock, port, behavior, requests).await;
                        });
                    }
                }
            }
        });
        Self { addr, behavior, requests, shutdown }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.addr.port())
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

async fn handle(
    mut sock: TcpStream,
    port: u16,
    behavior: Arc<Mutex<Behavior>>,
    requests: Arc<Mutex<Vec<String>>>,
) -> std::io::Result<()> {
    let request = rawhttp::read_request(&mut sock, 1024 * 1024).await?;
    {
        let mut log = requests.lock().unwrap_or_else(|err| err.into_inner());
        if log.len() < 200 {
            log.push(format!("{} {}", request.method, request.path));
        }
    }
    let behavior = behavior.lock().unwrap_or_else(|err| err.into_inner()).clone();
    if request.path == "/redirect" {
        let location = format!("http://127.0.0.1:{port}/file");
        return write_headers(&mut sock, 302, &[("Location", &location), ("Content-Length", "0")], &[]).await;
    }
    if request.path != "/file" {
        let body = b"not found";
        return write_headers(&mut sock, 404, &[("Content-Length", "9")], body).await;
    }
    if request.method.eq_ignore_ascii_case("HEAD") {
        return head(&mut sock, &behavior).await;
    }
    if let Some(status) = behavior.force_status {
        let message = status.to_string();
        return write_headers(
            &mut sock,
            status,
            &[("Content-Length", &message.len().to_string())],
            message.as_bytes(),
        )
        .await;
    }
    let range = rawhttp::header(&request.headers, "range").and_then(parse_range);
    if behavior.accept_ranges {
        if let Some((start, end_inclusive)) = range {
            return ranged(&mut sock, &behavior, start, end_inclusive).await;
        }
    }
    full(&mut sock, &behavior).await
}

async fn head(sock: &mut TcpStream, behavior: &Behavior) -> std::io::Result<()> {
    if let Some(status) = behavior.head_status.or(behavior.force_status) {
        return write_headers(sock, status, &[("Content-Length", "0")], &[]).await;
    }
    let mut headers = common_headers(behavior);
    if !behavior.chunked {
        headers.push(("Content-Length".into(), behavior.body.len().to_string()));
    }
    let view: Vec<(&str, String)> = headers.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
    let pairs: Vec<(&str, &str)> = view.iter().map(|(k, v)| (*k, v.as_str())).collect();
    write_headers(sock, 200, &pairs, &[]).await
}

async fn ranged(sock: &mut TcpStream, behavior: &Behavior, start: u64, end_inclusive: Option<u64>) -> std::io::Result<()> {
    let total = behavior.body.len() as u64;
    if start >= total {
        let range = format!("bytes */{total}");
        return write_headers(sock, 416, &[("Content-Range", &range), ("Content-Length", "0")], &[]).await;
    }
    let end = end_inclusive.unwrap_or(total.saturating_sub(1)).min(total.saturating_sub(1));
    if end < start {
        let range = format!("bytes */{total}");
        return write_headers(sock, 416, &[("Content-Range", &range), ("Content-Length", "0")], &[]).await;
    }
    let slice = &behavior.body[start as usize..=end as usize];
    let range = format!("bytes {start}-{end}/{total}");
    let length = slice.len().to_string();
    let mut headers = common_headers(behavior);
    headers.push(("Content-Range".into(), range));
    headers.push(("Content-Length".into(), length));
    write_stream(sock, 206, &headers, slice, behavior).await
}

async fn full(sock: &mut TcpStream, behavior: &Behavior) -> std::io::Result<()> {
    let mut headers = common_headers(behavior);
    if behavior.chunked {
        headers.push(("Transfer-Encoding".into(), "chunked".into()));
        write_chunked(sock, &headers, &behavior.body, behavior).await
    } else {
        headers.push(("Content-Length".into(), behavior.body.len().to_string()));
        write_stream(sock, 200, &headers, &behavior.body, behavior).await
    }
}

fn common_headers(behavior: &Behavior) -> Vec<(String, String)> {
    let mut headers = vec![("Content-Type".into(), behavior.content_type.clone().unwrap_or_else(|| "application/octet-stream".into()))];
    if behavior.accept_ranges {
        headers.push(("Accept-Ranges".into(), "bytes".into()));
    }
    if let Some(etag) = &behavior.etag {
        headers.push(("ETag".into(), etag.clone()));
    }
    if let Some(modified) = &behavior.last_modified {
        headers.push(("Last-Modified".into(), modified.clone()));
    }
    if let Some(disposition) = &behavior.content_disposition {
        headers.push(("Content-Disposition".into(), disposition.clone()));
    }
    headers
}

async fn write_stream(
    sock: &mut TcpStream,
    status: u16,
    headers: &[(String, String)],
    body: &[u8],
    behavior: &Behavior,
) -> std::io::Result<()> {
    let view: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    write_head(sock, status, &view).await?;
    let chunk = behavior.chunk_size.max(1);
    let mut sent = 0;
    while sent < body.len() {
        while behavior.stall.load(Ordering::Acquire) {
            if behavior.stop.is_cancelled() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        if cut(behavior, sent) {
            return Ok(());
        }
        let end = (sent + chunk).min(body.len());
        sock.write_all(&body[sent..end]).await?;
        sent = end;
        if !behavior.delay.is_zero() && sent < body.len() {
            tokio::time::sleep(behavior.delay).await;
        }
    }
    Ok(())
}

async fn write_chunked(
    sock: &mut TcpStream,
    headers: &[(String, String)],
    body: &[u8],
    behavior: &Behavior,
) -> std::io::Result<()> {
    let view: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    write_head(sock, 200, &view).await?;
    let chunk = behavior.chunk_size.max(1);
    let mut sent = 0;
    while sent < body.len() {
        while behavior.stall.load(Ordering::Acquire) {
            if behavior.stop.is_cancelled() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        if cut(behavior, sent) {
            return Ok(());
        }
        let end = (sent + chunk).min(body.len());
        let piece = &body[sent..end];
        let frame = format!("{:x}\r\n", piece.len());
        sock.write_all(frame.as_bytes()).await?;
        sock.write_all(piece).await?;
        sock.write_all(b"\r\n").await?;
        sent = end;
        if !behavior.delay.is_zero() && sent < body.len() {
            tokio::time::sleep(behavior.delay).await;
        }
    }
    sock.write_all(b"0\r\n\r\n").await
}

fn cut(behavior: &Behavior, sent: usize) -> bool {
    let Some(limit) = behavior.disconnect_after else {
        return false;
    };
    sent >= limit && behavior.disconnects_left.swap(0, Ordering::AcqRel) > 0
}

async fn write_headers(sock: &mut TcpStream, status: u16, headers: &[(&str, &str)], body: &[u8]) -> std::io::Result<()> {
    write_head(sock, status, headers).await?;
    sock.write_all(body).await
}

async fn write_head(sock: &mut (impl AsyncWrite + Unpin), status: u16, headers: &[(&str, &str)]) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        206 => "Partial Content",
        302 => "Found",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        410 => "Gone",
        416 => "Range Not Satisfiable",
        _ => "Error",
    };
    let mut text = format!("HTTP/1.1 {status} {reason}\r\n");
    for (name, value) in headers {
        text.push_str(name);
        text.push_str(": ");
        text.push_str(value);
        text.push_str("\r\n");
    }
    text.push_str("Connection: close\r\n\r\n");
    sock.write_all(text.as_bytes()).await
}

fn parse_range(value: &str) -> Option<(u64, Option<u64>)> {
    let value = value.trim().strip_prefix("bytes=")?;
    let value = value.split(',').next()?.trim();
    let (start, end) = value.split_once('-')?;
    let start = start.trim().parse().ok()?;
    let end = if end.trim().is_empty() { None } else { Some(end.trim().parse().ok()?) };
    Some((start, end))
}