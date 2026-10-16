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
    /// Byte offset at which `slow_delay` replaces `delay`.
    pub slow_from: Option<u64>,
    pub slow_delay: Duration,
    /// Remaining non-probe transfers that fail with `fail_status`.
    pub fail_transfers: Arc<AtomicUsize>,
    pub fail_status: u16,
    /// When set, every request must send this Basic pair before a body is served.
    pub require_basic: Option<(String, String)>,
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
            slow_from: None,
            slow_delay: Duration::ZERO,
            fail_transfers: Arc::new(AtomicUsize::new(0)),
            fail_status: 503,
            require_basic: None,
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
        if log.len() < 400 {
            log.push(format!("{} {}", request.method, request.path));
            if let Some(cookie) = rawhttp::header(&request.headers, "cookie") {
                log.push(format!("cookie:{cookie}"));
            }
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
    if let Some((user, pass)) = &behavior.require_basic {
        let expected = crate::auth::basic_header(user, pass);
        let presented = rawhttp::header(&request.headers, "authorization").unwrap_or("");
        if presented != expected {
            return write_headers(
                &mut sock,
                401,
                &[("WWW-Authenticate", "Basic realm=\"forge\""), ("Content-Length", "0")],
                &[],
            )
            .await;
        }
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
    let probe_range = matches!(range, Some((0, Some(0))));
    if !probe_range && take_failure(&behavior.fail_transfers) {
        let status = behavior.fail_status;
        return write_headers(&mut sock, status, &[("Content-Length", "0"), ("Retry-After", "0")], &[]).await;
    }
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
    write_stream(sock, 206, &headers, slice, behavior, start).await
}

async fn full(sock: &mut TcpStream, behavior: &Behavior) -> std::io::Result<()> {
    let mut headers = common_headers(behavior);
    if behavior.chunked {
        headers.push(("Transfer-Encoding".into(), "chunked".into()));
        write_chunked(sock, &headers, &behavior.body, behavior).await
    } else {
        headers.push(("Content-Length".into(), behavior.body.len().to_string()));
        write_stream(sock, 200, &headers, &behavior.body, behavior, 0).await
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
    file_offset: u64,
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
        let delay = chunk_delay(behavior, file_offset + sent as u64);
        if !delay.is_zero() && sent < body.len() {
            tokio::time::sleep(delay).await;
        }
    }
    Ok(())
}

fn chunk_delay(behavior: &Behavior, next_offset: u64) -> Duration {
    if behavior.slow_from.is_some_and(|from| next_offset > from) && !behavior.slow_delay.is_zero() {
        behavior.slow_delay
    } else {
        behavior.delay
    }
}

fn take_failure(left: &AtomicUsize) -> bool {
    loop {
        let current = left.load(Ordering::Acquire);
        if current == 0 {
            return false;
        }
        if left.compare_exchange(current, current - 1, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            return true;
        }
    }
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

/// HTTP proxy that forwards an absolute-form request to the origin.
pub struct HttpProxy {
    pub addr: std::net::SocketAddr,
    pub hits: Arc<AtomicUsize>,
    shutdown: CancellationToken,
}

impl HttpProxy {
    pub async fn spawn() -> Self {
        let listener = TcpListener::bind(std::net::SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bind proxy");
        let addr = listener.local_addr().expect("proxy addr");
        let hits = Arc::new(AtomicUsize::new(0));
        let shutdown = CancellationToken::new();
        let stop = shutdown.clone();
        let hits_task = Arc::clone(&hits);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = stop.cancelled() => break,
                    accepted = listener.accept() => {
                        let sock = match accepted {
                            Ok((sock, _)) => sock,
                            Err(_) => continue,
                        };
                        let hits = Arc::clone(&hits_task);
                        tokio::spawn(async move {
                            let _ = relay_proxy(sock, hits).await;
                        });
                    }
                }
            }
        });
        Self { addr, hits, shutdown }
    }
}

impl Drop for HttpProxy {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

async fn relay_proxy(mut client: TcpStream, hits: Arc<AtomicUsize>) -> std::io::Result<()> {
    let request = rawhttp::read_request(&mut client, 1024 * 1024).await?;
    let url: url::Url = request
        .path
        .parse()
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("proxy target: {err}")))?;
    let host = url
        .host_str()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "proxy target has no host"))?;
    let port = url.port_or_known_default().unwrap_or(80);
    hits.fetch_add(1, Ordering::AcqRel);
    let mut origin = TcpStream::connect((host, port)).await?;
    let path = if url.path().is_empty() { "/" } else { url.path() };
    let mut head = format!("{} {path} HTTP/1.1\r\n", request.method);
    let mut saw_host = false;
    for (name, value) in &request.headers {
        if name.eq_ignore_ascii_case("proxy-connection") {
            continue;
        }
        if name.eq_ignore_ascii_case("host") {
            saw_host = true;
        }
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    if !saw_host {
        head.push_str(&format!("Host: {host}:{port}\r\n"));
    }
    head.push_str("\r\n");
    origin.write_all(head.as_bytes()).await?;
    origin.write_all(&request.body).await?;
    tokio::io::copy(&mut origin, &mut client).await?;
    Ok(())
}

/// One-stream FTP fixture. `cuts` is how many RETR replies end after 4096 bytes.
pub struct FtpMock {
    pub addr: std::net::SocketAddr,
    shutdown: CancellationToken,
}

impl FtpMock {
    pub async fn spawn(body: Vec<u8>, cuts: usize) -> Self {
        let listener = TcpListener::bind(std::net::SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bind ftp");
        let addr = listener.local_addr().expect("ftp addr");
        let shutdown = CancellationToken::new();
        let stop = shutdown.clone();
        let body = Arc::new(body);
        let cuts = Arc::new(AtomicUsize::new(cuts));
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = stop.cancelled() => break,
                    accepted = listener.accept() => {
                        let sock = match accepted {
                            Ok((sock, _)) => sock,
                            Err(_) => continue,
                        };
                        let body = Arc::clone(&body);
                        let cuts = Arc::clone(&cuts);
                        tokio::spawn(async move {
                            let _ = serve_ftp(sock, body, cuts).await;
                        });
                    }
                }
            }
        });
        Self { addr, shutdown }
    }

    pub fn url(&self) -> String {
        format!("ftp://127.0.0.1:{}/file.bin", self.addr.port())
    }
}

impl Drop for FtpMock {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

async fn serve_ftp(sock: TcpStream, body: Arc<Vec<u8>>, cuts: Arc<AtomicUsize>) -> std::io::Result<()> {
    sock.set_nodelay(true)?;
    let mut ctrl = tokio::io::BufReader::new(sock);
    ftp_write(&mut ctrl, b"220 forge ready\r\n").await?;
    let mut rest = 0u64;
    let mut data_wait: Option<tokio::sync::oneshot::Receiver<std::io::Result<(TcpStream, std::net::SocketAddr)>>> = None;
    loop {
        let Some(line) = ftp_line(&mut ctrl).await? else {
            break;
        };
        let (verb, arg) = split_ftp(&line);
        match verb.as_str() {
            "USER" => ftp_write(&mut ctrl, b"331 password required\r\n").await?,
            "PASS" => ftp_write(&mut ctrl, b"230 logged in\r\n").await?,
            "TYPE" => ftp_write(&mut ctrl, b"200 type set\r\n").await?,
            "SIZE" => ftp_write(&mut ctrl, format!("213 {}\r\n", body.len()).as_bytes()).await?,
            "REST" => {
                rest = arg.parse().unwrap_or(0);
                ftp_write(&mut ctrl, b"350 restarting\r\n").await?;
            }
            "PASV" => {
                let listener = TcpListener::bind(std::net::SocketAddr::from(([127, 0, 0, 1], 0))).await?;
                let port = listener.local_addr()?.port();
                let p1 = port / 256;
                let p2 = port % 256;
                ftp_write(&mut ctrl, format!("227 Entering Passive Mode (127,0,0,1,{p1},{p2}).\r\n").as_bytes()).await?;
                let (tx, rx) = tokio::sync::oneshot::channel();
                tokio::spawn(async move {
                    let _ = tx.send(listener.accept().await);
                });
                data_wait = Some(rx);
            }
            "RETR" => {
                let Some(rx) = data_wait.take() else {
                    ftp_write(&mut ctrl, b"425 no data connection\r\n").await?;
                    continue;
                };
                ftp_write(&mut ctrl, b"150 opening data\r\n").await?;
                let (mut data, _) = rx.await.map_err(|_| std::io::Error::other("data accept dropped"))??;
                let start = (rest as usize).min(body.len());
                let mut end = body.len();
                if take_failure(&cuts) {
                    end = (start + 4096).min(end);
                }
                data.write_all(&body[start..end]).await?;
                data.shutdown().await?;
                rest = 0;
                ftp_write(&mut ctrl, b"226 transfer complete\r\n").await?;
            }
            "QUIT" => {
                ftp_write(&mut ctrl, b"221 bye\r\n").await?;
                break;
            }
            _ => ftp_write(&mut ctrl, b"502 command not implemented\r\n").await?,
        }
    }
    Ok(())
}

async fn ftp_write(ctrl: &mut tokio::io::BufReader<TcpStream>, bytes: &[u8]) -> std::io::Result<()> {
    ctrl.write_all(bytes).await?;
    ctrl.flush().await
}

async fn ftp_line(ctrl: &mut tokio::io::BufReader<TcpStream>) -> std::io::Result<Option<String>> {
    let mut line = String::new();
    let n = tokio::io::AsyncBufReadExt::read_line(ctrl, &mut line).await?;
    if n == 0 {
        Ok(None)
    } else {
        Ok(Some(line.trim().to_string()))
    }
}

fn split_ftp(line: &str) -> (String, String) {
    match line.split_once(' ') {
        Some((verb, arg)) => (verb.to_ascii_uppercase(), arg.trim().to_string()),
        None => (line.to_ascii_uppercase(), String::new()),
    }
}