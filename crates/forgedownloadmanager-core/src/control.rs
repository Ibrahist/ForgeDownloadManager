use std::net::SocketAddr;

use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use uuid::Uuid;

use crate::error::Error;
use crate::filename::validate_headers;
use crate::grabber;
use crate::model::{
    AddRequest, ChecksumAlgo, DownloadStatus, DuplicatePolicy, SiteRule, SiteRuleAction,
};
use crate::rawhttp::{self, RawRequest};
use crate::schedule::{OnCompleteAction, Queue};
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
        ("GET", "/v1/queues") => match engine.list_queues() {
            Ok(queues) => match serde_json::to_string(&queues) {
                Ok(body) => rawhttp::json_response(200, &body),
                Err(err) => error_response(&Error::Json(err)),
            },
            Err(err) => error_response(&err),
        },
        ("GET", path) if queue_path(path).is_some() => match queue_path(path) {
            Some(id_or_name) => match get_queue_handler(engine, id_or_name) {
                Ok(Some(queue)) => match serde_json::to_string(&queue) {
                    Ok(body) => rawhttp::json_response(200, &body),
                    Err(err) => error_response(&Error::Json(err)),
                },
                Ok(None) => rawhttp::json_response(404, &json_err("not_found", "queue not found")),
                Err(err) => error_response(&err),
            },
            None => rawhttp::json_response(404, &json_err("not_found", "unknown route")),
        },
        ("PUT", path) if queue_path(path).is_some() => match queue_path(path) {
            Some(id_str) => match update_queue_handler(engine, id_str, &request.body) {
                Ok(queue) => match serde_json::to_string(&queue) {
                    Ok(body) => rawhttp::json_response(200, &body),
                    Err(err) => error_response(&Error::Json(err)),
                },
                Err(err) => error_response(&err),
            },
            None => rawhttp::json_response(404, &json_err("not_found", "unknown route")),
        },
        ("DELETE", path) if queue_path(path).is_some() => match queue_path(path) {
            Some(id_str) => match delete_queue_handler(engine, id_str) {
                Ok(()) => rawhttp::json_response(200, r#"{"ok":true}"#),
                Err(err) => error_response(&err),
            },
            None => rawhttp::json_response(404, &json_err("not_found", "unknown route")),
        },
        ("POST", "/v1/queues") => match create_queue_handler(engine, &request.body) {
            Ok(queue) => match serde_json::to_string(&queue) {
                Ok(body) => rawhttp::json_response(200, &body),
                Err(err) => error_response(&Error::Json(err)),
            },
            Err(err) => error_response(&err),
        },
        ("GET", "/v1/site-rules") => match engine.list_site_rules() {
            Ok(rules) => match serde_json::to_string(&rules) {
                Ok(body) => rawhttp::json_response(200, &body),
                Err(err) => error_response(&Error::Json(err)),
            },
            Err(err) => error_response(&err),
        },
        ("POST", "/v1/site-rules") => match create_site_rule_handler(engine, &request.body) {
            Ok(rule) => match serde_json::to_string(&rule) {
                Ok(body) => rawhttp::json_response(200, &body),
                Err(err) => error_response(&Error::Json(err)),
            },
            Err(err) => error_response(&err),
        },
        ("DELETE", path) if site_rule_path(path).is_some() => match site_rule_path(path) {
            Some(id) => match engine.delete_site_rule(id) {
                Ok(()) => rawhttp::json_response(200, r#"{"ok":true}"#),
                Err(err) => error_response(&err),
            },
            None => rawhttp::json_response(404, &json_err("not_found", "unknown route")),
        },
        ("POST", "/v1/grabber/expand") => match expand_pattern_handler(&request.body) {
            Ok(urls) => match serde_json::to_string(&urls) {
                Ok(body) => rawhttp::json_response(200, &body),
                Err(err) => error_response(&Error::Json(err)),
            },
            Err(err) => error_response(&err),
        },
        ("POST", "/v1/grabber/crawl") => match crawl_handler(&request.body).await {
            Ok(urls) => match serde_json::to_string(&urls) {
                Ok(body) => rawhttp::json_response(200, &body),
                Err(err) => error_response(&Error::Json(err)),
            },
            Err(err) => error_response(&err),
        },
        ("POST", "/v1/media/probe") => match media_probe_handler(engine, &request.body).await {
            Ok(res) => match serde_json::to_string(&res) {
                Ok(body) => rawhttp::json_response(200, &body),
                Err(err) => error_response(&Error::Json(err)),
            },
            Err(err) => error_response(&err),
        },
        ("GET", "/v1/media/tools") => match serde_json::to_string(&engine.media_tools_status()) {
            Ok(body) => rawhttp::json_response(200, &body),
            Err(err) => error_response(&Error::Json(err)),
        },
        ("POST", "/v1/media/download") => match media_download_handler(engine, &request.body).await {
            Ok(bytes) => rawhttp::json_response(200, &format!(r#"{{"ok":true,"bytes":{bytes}}}"#)),
            Err(err) => error_response(&err),
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
    request.queue_id = parsed.queue_id;
    request.priority = parsed.priority;
    engine.add(request).await
}

fn queue_path(path: &str) -> Option<&str> {
    let id = path.strip_prefix("/v1/queues/")?;
    if id.is_empty() || id.contains('/') {
        None
    } else {
        Some(id)
    }
}

fn get_queue_handler(engine: &Engine, id_or_name: &str) -> Result<Option<Queue>> {
    if let Ok(id) = Uuid::parse_str(id_or_name) {
        if let Some(q) = engine.get_queue(id)? {
            return Ok(Some(q));
        }
    }
    engine.get_queue_by_name(id_or_name)
}

fn create_queue_handler(engine: &Engine, body: &[u8]) -> Result<Queue> {
    let parsed: CreateQueueBody = serde_json::from_slice(body)?;
    let on_complete = match parsed.on_complete.as_deref() {
        Some(action) => OnCompleteAction::parse(action)?,
        None => OnCompleteAction::None,
    };
    let queue = Queue {
        id: Uuid::new_v4(),
        name: parsed.name,
        concurrency: parsed.concurrency.unwrap_or(3).max(1),
        speed_limit_bps: parsed.speed_limit_bps,
        schedule_start: parsed.schedule_start,
        schedule_end: parsed.schedule_end,
        days_mask: parsed.days_mask.unwrap_or(127),
        on_complete,
        on_complete_command: parsed.on_complete_command,
        created_at: crate::model::now_ms(),
    };
    engine.create_queue(queue.clone())?;
    Ok(queue)
}

fn update_queue_handler(engine: &Engine, id_str: &str, body: &[u8]) -> Result<Queue> {
    let id = if let Ok(parsed_id) = Uuid::parse_str(id_str) {
        parsed_id
    } else if let Some(q) = engine.get_queue_by_name(id_str)? {
        q.id
    } else {
        return Err(Error::NotFound(Uuid::nil()));
    };
    let mut queue = engine.get_queue(id)?.ok_or(Error::NotFound(id))?;
    let parsed: UpdateQueueBody = serde_json::from_slice(body)?;
    if let Some(name) = parsed.name {
        queue.name = name;
    }
    if let Some(concurrency) = parsed.concurrency {
        queue.concurrency = concurrency.max(1);
    }
    if let Some(speed_limit) = parsed.speed_limit_bps {
        queue.speed_limit_bps = if speed_limit > 0 { Some(speed_limit) } else { None };
    }
    if parsed.schedule_start.is_some() {
        queue.schedule_start = parsed.schedule_start;
    }
    if parsed.schedule_end.is_some() {
        queue.schedule_end = parsed.schedule_end;
    }
    if let Some(mask) = parsed.days_mask {
        queue.days_mask = mask;
    }
    if let Some(on_complete_str) = parsed.on_complete {
        queue.on_complete = OnCompleteAction::parse(&on_complete_str)?;
    }
    if parsed.on_complete_command.is_some() {
        queue.on_complete_command = parsed.on_complete_command;
    }
    engine.update_queue(queue.clone())?;
    Ok(queue)
}

fn delete_queue_handler(engine: &Engine, id_str: &str) -> Result<()> {
    let id = if let Ok(parsed_id) = Uuid::parse_str(id_str) {
        parsed_id
    } else if let Some(q) = engine.get_queue_by_name(id_str)? {
        q.id
    } else {
        return Err(Error::NotFound(Uuid::nil()));
    };
    engine.delete_queue(id)
}

fn expand_pattern_handler(body: &[u8]) -> Result<Vec<String>> {
    let parsed: ExpandBody = serde_json::from_slice(body)?;
    grabber::expand_pattern(&parsed.pattern)
}

async fn crawl_handler(body: &[u8]) -> Result<Vec<String>> {
    let parsed: CrawlBody = serde_json::from_slice(body)?;
    let client = reqwest::Client::builder()
        .user_agent(crate::config::DEFAULT_USER_AGENT)
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let cancel = tokio_util::sync::CancellationToken::new();
    grabber::crawl_same_domain(
        &client,
        &parsed.start_url,
        parsed.max_depth.unwrap_or(1),
        parsed.max_links.unwrap_or(50),
        &parsed.allowed_extensions,
        &cancel,
    )
    .await
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
struct CreateQueueBody {
    name: String,
    concurrency: Option<usize>,
    speed_limit_bps: Option<u64>,
    schedule_start: Option<String>,
    schedule_end: Option<String>,
    days_mask: Option<u8>,
    on_complete: Option<String>,
    on_complete_command: Option<String>,
}

#[derive(Deserialize)]
struct UpdateQueueBody {
    name: Option<String>,
    concurrency: Option<usize>,
    speed_limit_bps: Option<u64>,
    schedule_start: Option<String>,
    schedule_end: Option<String>,
    days_mask: Option<u8>,
    on_complete: Option<String>,
    on_complete_command: Option<String>,
}

#[derive(Deserialize)]
struct ExpandBody {
    pattern: String,
}

#[derive(Deserialize)]
struct CrawlBody {
    start_url: String,
    max_depth: Option<usize>,
    max_links: Option<usize>,
    #[serde(default)]
    allowed_extensions: Vec<String>,
}

#[derive(Deserialize)]
struct MediaProbeBody {
    url: String,
}

async fn media_probe_handler(engine: &Engine, body: &[u8]) -> Result<crate::media::MediaProbeResult> {
    let req = serde_json::from_slice::<MediaProbeBody>(body)?;
    engine.probe_media(&req.url).await
}

#[derive(Deserialize)]
struct MediaDownloadBody {
    url: Option<String>,
    segment_urls: Option<Vec<String>>,
    output_path: String,
}

async fn media_download_handler(engine: &Engine, body: &[u8]) -> Result<u64> {
    let req = serde_json::from_slice::<MediaDownloadBody>(body)?;
    let p = std::path::PathBuf::from(req.output_path);
    if let Some(url) = req.url {
        engine.download_media_url(&url, &p).await
    } else if let Some(urls) = req.segment_urls {
        engine.download_media_stream(&urls, &p).await
    } else {
        Err(Error::Failed("either url or segment_urls must be provided".into()))
    }
}

fn deserialize_cookies<'de, D>(deserializer: D) -> std::result::Result<Vec<(String, String)>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let val: serde_json::Value = serde::Deserialize::deserialize(deserializer)?;
    let mut cookies = Vec::new();
    match val {
        serde_json::Value::Array(arr) => {
            for item in arr {
                match item {
                    serde_json::Value::Array(pair) => {
                        if pair.len() == 2 {
                            if let (Some(serde_json::Value::String(k)), Some(serde_json::Value::String(v))) = (pair.get(0), pair.get(1)) {
                                cookies.push((k.clone(), v.clone()));
                            }
                        }
                    }
                    serde_json::Value::Object(obj) => {
                        if let (Some(serde_json::Value::String(k)), Some(serde_json::Value::String(v))) = (obj.get("name"), obj.get("value")) {
                            cookies.push((k.clone(), v.clone()));
                        }
                    }
                    _ => {}
                }
            }
        }
        serde_json::Value::String(s) => {
            for part in s.split(';') {
                let part = part.trim();
                if let Some((k, v)) = part.split_once('=') {
                    cookies.push((k.trim().to_string(), v.trim().to_string()));
                }
            }
        }
        _ => {}
    }
    Ok(cookies)
}

fn deserialize_headers<'de, D>(deserializer: D) -> std::result::Result<Vec<(String, String)>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let val: serde_json::Value = serde::Deserialize::deserialize(deserializer)?;
    let mut headers = Vec::new();
    match val {
        serde_json::Value::Array(arr) => {
            for item in arr {
                if let serde_json::Value::Array(pair) = item {
                    if pair.len() == 2 {
                        if let (Some(serde_json::Value::String(k)), Some(serde_json::Value::String(v))) = (pair.get(0), pair.get(1)) {
                            headers.push((k.clone(), v.clone()));
                        }
                    }
                }
            }
        }
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                if let serde_json::Value::String(s) = v {
                    headers.push((k, s));
                }
            }
        }
        _ => {}
    }
    Ok(headers)
}

#[derive(Deserialize)]
struct AddBody {
    url: String,
    #[serde(alias = "dir")]
    save_dir: Option<std::path::PathBuf>,
    filename: Option<String>,
    segments: Option<u32>,
    start: Option<bool>,
    referer: Option<String>,
    user_agent: Option<String>,
    #[serde(default, deserialize_with = "deserialize_headers")]
    headers: Vec<(String, String)>,
    #[serde(default)]
    insecure_tls: bool,
    duplicate_policy: Option<DuplicatePolicy>,
    checksum_algo: Option<ChecksumAlgo>,
    checksum_expected: Option<String>,
    #[serde(default, deserialize_with = "deserialize_cookies")]
    cookies: Vec<(String, String)>,
    proxy: Option<String>,
    speed_limit_bps: Option<u64>,
    username: Option<String>,
    password: Option<String>,
    max_retries: Option<u32>,
    queue_id: Option<Uuid>,
    priority: Option<i64>,
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

#[derive(Deserialize)]
struct CreateSiteRuleBody {
    pattern: String,
    #[serde(default = "default_site_rule_action")]
    action: String,
}

fn default_site_rule_action() -> String {
    "intercept".to_string()
}

fn create_site_rule_handler(engine: &Engine, body: &[u8]) -> Result<SiteRule> {
    let req = serde_json::from_slice::<CreateSiteRuleBody>(body)?;
    let action = SiteRuleAction::parse(&req.action)
        .ok_or_else(|| Error::Failed(format!("invalid site rule action '{}', must be 'intercept' or 'exclude'", req.action)))?;
    let rule = SiteRule::new(req.pattern, action);
    engine.create_site_rule(rule)
}

fn site_rule_path(path: &str) -> Option<Uuid> {
    let rest = path.strip_prefix("/v1/site-rules/")?;
    Uuid::parse_str(rest).ok()
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