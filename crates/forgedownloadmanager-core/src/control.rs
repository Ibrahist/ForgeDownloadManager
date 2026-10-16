use std::net::SocketAddr;

use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use uuid::Uuid;

use crate::error::Error;
use crate::filename::validate_headers;
use crate::model::{AddRequest, ChecksumAlgo, DownloadStatus, DuplicatePolicy};
use crate::rawhttp::{self, RawRequest};
use crate::Engine;
use crate::Result;

const MAX_BODY: usize = 1024 * 1024;

/// Bind `127.0.0.1` and serve the Phase 1 control API until the engine shuts down.
pub async fn serve(engine: Engine, token: String) -> Result<SocketAddr> {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await?;
    let addr = listener.local_addr()?;
    let port = addr.port();
    tokio::spawn(async move {
        let stop = engine.shutdown_token();
        loop {
            tokio::select! {
                _ = stop.cancelled() => break,
                accepted = listener.accept() => {
                    // Windows reports WSAECONNRESET from accept when the peer
                    // closes before the handshake finishes. The listener is still good.
                    let (mut sock, peer) = match accepted {
                        Ok(pair) => pair,
                        Err(_) => {
                            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                            continue;
                        }
                    };
                    let engine = engine.clone();
                    let token = token.clone();
                    tokio::spawn(async move {
                        let response = match rawhttp::read_request(&mut sock, MAX_BODY).await {
                            Ok(request) => dispatch(&engine, &token, peer, port, request).await,
                            Err(err) => rawhttp::json_response(400, &json_err("failed", &err.to_string())),
                        };
                        let _ = sock.write_all(&response).await;
                    });
                }
            }
        }
    });
    Ok(addr)
}

/// One control call. `origin` and `host` override the headers a normal client sends.
pub async fn control_exchange(
    addr: SocketAddr,
    token: &str,
    method: &str,
    path: &str,
    body: &str,
    origin: Option<&str>,
    host: Option<&str>,
) -> Result<(u16, String)> {
    let auth = format!("Bearer {token}");
    let mut headers: Vec<(&str, &str)> = vec![("Authorization", auth.as_str())];
    if let Some(origin) = origin {
        headers.push(("Origin", origin));
    }
    if let Some(host) = host {
        headers.push(("Host", host));
    }
    // A loopback accept can reset once on Windows when the previous peer closes early.
    let mut last = None;
    for attempt in 0..3 {
        match rawhttp::exchange(addr, method, path, &headers, body.as_bytes()).await {
            Ok((status, text)) => return Ok((status, text)),
            Err(err) if attempt < 2 && transient_socket(&err) => {
                last = Some(err);
                tokio::time::sleep(std::time::Duration::from_millis(40)).await;
            }
            Err(err) => return Err(Error::Io(err)),
        }
    }
    Err(Error::Io(last.unwrap_or_else(|| {
        std::io::Error::other("control request failed")
    })))
}

fn transient_socket(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::UnexpectedEof
    )
}

async fn dispatch(engine: &Engine, token: &str, peer: SocketAddr, port: u16, request: RawRequest) -> Vec<u8> {
    if !peer.ip().is_loopback() {
        return rawhttp::json_response(403, &json_err("forbidden", "peer is not loopback"));
    }
    match rawhttp::header(&request.headers, "host") {
        Some(host) if host_allowed(host, port) => {}
        _ => return rawhttp::json_response(403, &json_err("forbidden", "host is not this loopback listener")),
    }
    if rawhttp::header(&request.headers, "origin").is_some() {
        return rawhttp::json_response(403, &json_err("forbidden", "origin is not allowed"));
    }
    let presented = rawhttp::header(&request.headers, "authorization")
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or("");
    if !constant_time_eq(presented.as_bytes(), token.as_bytes()) {
        return rawhttp::json_response(401, &json_err("unauthorized", "bearer token rejected"));
    }
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/v1/health") => rawhttp::json_response(200, r#"{"ok":true}"#),
        ("GET", "/v1/downloads") => match engine.list() {
            Ok(rows) => match serde_json::to_string(&rows) {
                Ok(body) => rawhttp::json_response(200, &body),
                Err(err) => error_response(&Error::Json(err)),
            },
            Err(err) => error_response(&err),
        },
        ("GET", path) if setting_key(path).is_some() => match setting_key(path) {
            Some(key) => match engine.setting_value(key) {
                Ok(value) => match serde_json::to_string(&value) {
                    Ok(body) => rawhttp::json_response(200, &body),
                    Err(err) => error_response(&Error::Json(err)),
                },
                Err(err) => error_response(&err),
            },
            None => rawhttp::json_response(404, &json_err("not_found", "unknown route")),
        },
        ("GET", path) if segments_path(path).is_some() => match segments_path(path) {
            Some(id) => match load_segments(engine, id) {
                Ok(body) => rawhttp::json_response(200, &body),
                Err(err) => error_response(&err),
            },
            None => rawhttp::json_response(404, &json_err("not_found", "unknown route")),
        },
        ("POST", "/v1/downloads") => match add_download(engine, &request.body).await {
            Ok(id) => rawhttp::json_response(200, &format!(r#"{{"id":"{id}"}}"#)),
            Err(err) => error_response(&err),
        },
        ("POST", "/v1/settings") => match set_setting(engine, &request.body) {
            Ok(()) => rawhttp::json_response(200, r#"{"ok":true}"#),
            Err(err) => error_response(&err),
        },
        ("POST", "/v1/credentials") => match save_credential(engine, &request.body) {
            Ok(id) => rawhttp::json_response(200, &format!(r#"{{"id":"{id}"}}"#)),
            Err(err) => error_response(&err),
        },
        ("POST", path) => match action_path(path) {
            Some((id, "pause")) => match act(engine, id, Action::Pause, &request.body).await {
                Ok(()) => rawhttp::json_response(200, r#"{"ok":true}"#),
                Err(err) => error_response(&err),
            },
            Some((id, "resume")) => match act(engine, id, Action::Resume, &request.body).await {
                Ok(()) => rawhttp::json_response(200, r#"{"ok":true}"#),
                Err(err) => error_response(&err),
            },
            Some((id, "remove")) => match act(engine, id, Action::Remove, &request.body).await {
                Ok(()) => rawhttp::json_response(200, r#"{"ok":true}"#),
                Err(err) => error_response(&err),
            },
            _ => rawhttp::json_response(404, &json_err("not_found", "unknown route")),
        },
        _ => rawhttp::json_response(404, &json_err("not_found", "unknown route")),
    }
}

enum Action {
    Pause,
    Resume,
    Remove,
}

async fn act(engine: &Engine, id: &str, action: Action, body: &[u8]) -> Result<()> {
    let id = engine.resolve(id)?;
    match action {
        Action::Pause => engine.pause(id).await,
        Action::Resume => engine.resume(id).await,
        Action::Remove => {
            let delete_file = if body.is_empty() {
                false
            } else {
                serde_json::from_slice::<RemoveBody>(body)?.delete_file
            };
            engine.remove(id, delete_file).await
        }
    }
}

fn action_path(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix("/v1/downloads/")?;
    let (id, action) = rest.split_once('/')?;
    if id.is_empty() || action.contains('/') {
        return None;
    }
    Some((id, action))
}

async fn add_download(engine: &Engine, body: &[u8]) -> Result<Uuid> {
    let parsed: AddBody = serde_json::from_slice(body)?;
    let save_dir = parsed.save_dir.ok_or_else(|| Error::Failed("save_dir is required".into()))?;
    validate_headers(&parsed.headers)?;
    let mut request = AddRequest::new(parsed.url, save_dir);
    request.filename = parsed.filename;
    request.segments = parsed.segments;
    if let Some(start) = parsed.start {
        request.start = start;
    }
    request.referer = parsed.referer;
    request.user_agent = parsed.user_agent;
    request.headers = parsed.headers;
    request.insecure_tls = parsed.insecure_tls;
    if let Some(policy) = parsed.duplicate_policy {
        request.duplicate_policy = policy;
    }
    request.checksum_algo = parsed.checksum_algo;
    request.checksum_expected = parsed.checksum_expected;
    request.cookies = parsed.cookies;
    request.proxy = parsed.proxy;
    request.speed_limit_bps = parsed.speed_limit_bps;
    request.username = parsed.username;
    request.password = parsed.password;
    request.max_retries = parsed.max_retries;
    engine.add(request).await
}

fn set_setting(engine: &Engine, body: &[u8]) -> Result<()> {
    let parsed: SettingBody = serde_json::from_slice(body)?;
    engine.set_setting(&parsed.key, &parsed.value)
}

fn save_credential(engine: &Engine, body: &[u8]) -> Result<Uuid> {
    let parsed: CredentialBody = serde_json::from_slice(body)?;
    engine.save_credential(&parsed.host, &parsed.username, &parsed.password)
}

fn segments_path(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/v1/downloads/")?;
    let (id, action) = rest.split_once('/')?;
    if action == "segments" && !id.is_empty() && !id.contains('/') {
        Some(id)
    } else {
        None
    }
}

fn load_segments(engine: &Engine, id: &str) -> Result<String> {
    let id = engine.resolve(id)?;
    Ok(serde_json::to_string(&engine.segments(id)?)?)
}

fn setting_key(path: &str) -> Option<&str> {
    let key = path.strip_prefix("/v1/settings/")?;
    if key.is_empty() || key.contains('/') {
        None
    } else {
        Some(key)
    }
}

#[derive(Deserialize)]
struct AddBody {
    url: String,
    save_dir: Option<std::path::PathBuf>,
    filename: Option<String>,
    segments: Option<u32>,
    start: Option<bool>,
    referer: Option<String>,
    user_agent: Option<String>,
    #[serde(default)]
    headers: Vec<(String, String)>,
    #[serde(default)]
    insecure_tls: bool,
    duplicate_policy: Option<DuplicatePolicy>,
    checksum_algo: Option<ChecksumAlgo>,
    checksum_expected: Option<String>,
    #[serde(default)]
    cookies: Vec<(String, String)>,
    proxy: Option<String>,
    speed_limit_bps: Option<u64>,
    username: Option<String>,
    password: Option<String>,
    max_retries: Option<u32>,
}

#[derive(Deserialize)]
struct SettingBody {
    key: String,
    value: serde_json::Value,
}

#[derive(Deserialize)]
struct CredentialBody {
    host: String,
    username: String,
    password: String,
}

#[derive(Deserialize)]
struct RemoveBody {
    #[serde(default)]
    delete_file: bool,
}

fn host_allowed(host: &str, port: u16) -> bool {
    let host = host.trim();
    host.eq_ignore_ascii_case(&format!("127.0.0.1:{port}"))
        || host.eq_ignore_ascii_case(&format!("localhost:{port}"))
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut diff = left.len() ^ right.len();
    let len = left.len().max(right.len());
    for index in 0..len {
        let a = left.get(index).copied().unwrap_or(0);
        let b = right.get(index).copied().unwrap_or(0);
        diff |= usize::from(a ^ b);
    }
    diff == 0
}

fn error_response(err: &Error) -> Vec<u8> {
    let status = match err {
        Error::NotFound(_) => 404,
        Error::RemoteChanged => 409,
        Error::Sqlite(_) | Error::Io(_) => 500,
        _ => 400,
    };
    rawhttp::json_response(status, &json_err(err.code(), &err.to_string()))
}

fn json_err(code: &str, message: &str) -> String {
    serde_json::json!({ "error": message, "code": code }).to_string()
}

/// Used by the human list when no engine is running. Not a stored status.
pub fn interrupted_label(status: DownloadStatus, supervisor: bool) -> String {
    if !supervisor && matches!(status, DownloadStatus::Downloading | DownloadStatus::Probing | DownloadStatus::Ready)
    {
        "interrupted".to_string()
    } else {
        status.as_str().to_string()
    }
}