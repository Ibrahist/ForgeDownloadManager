//! Desktop shell. This process embeds the engine when it wins `engine.lock`.
//! It keeps the loopback HTTP API up until the process exits, including while
//! the queue is idle, so `forge-dm` can forward. Closing the window does not
//! pause downloads; the next open requeues rows that were still downloading.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use forgedownloadmanager_core::checksum::random_token;
use forgedownloadmanager_core::config::{EngineConfig, SETTING_KEYS};
use forgedownloadmanager_core::control::{self, control_exchange};
use forgedownloadmanager_core::filename::validate_headers;
use forgedownloadmanager_core::lock::{self, Attach, Owner};
use forgedownloadmanager_core::model::{AddRequest, ChecksumAlgo, Download, DuplicatePolicy, Segment};
use forgedownloadmanager_core::Engine;
use serde::Deserialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::watch;

struct Forward {
    addr: std::net::SocketAddr,
    token: String,
}

struct Session {
    mode: String,
    engine: Option<Engine>,
    forward: Option<Forward>,
    /// Held so the OS lock stays taken until the process exits.
    _owner: Option<Owner>,
}

enum Boot {
    Pending,
    Ready(Arc<Session>),
    Failed(String),
}

struct AppState {
    ready: watch::Sender<Boot>,
    _hold: watch::Receiver<Boot>,
}

#[derive(serde::Serialize)]
struct UiError {
    code: String,
    message: String,
}

fn ui_err(err: forgedownloadmanager_core::Error) -> UiError {
    UiError { code: err.code().to_string(), message: err.to_string() }
}

fn text_err(code: &str, message: impl Into<String>) -> UiError {
    UiError { code: code.to_string(), message: message.into() }
}

async fn session(state: &AppState) -> Result<Arc<Session>, UiError> {
    let mut rx = state.ready.subscribe();
    loop {
        {
            let boot = rx.borrow();
            match &*boot {
                Boot::Ready(ready) => return Ok(Arc::clone(ready)),
                Boot::Failed(message) => return Err(text_err("boot", message.clone())),
                Boot::Pending => {}
            }
        }
        if rx.changed().await.is_err() {
            return Err(text_err("boot", "engine boot stopped"));
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddInput {
    url: String,
    save_dir: String,
    filename: Option<String>,
    segments: Option<u32>,
    referer: Option<String>,
    user_agent: Option<String>,
    headers: Option<String>,
    cookies: Option<String>,
    proxy: Option<String>,
    speed_limit_bps: Option<u64>,
    username: Option<String>,
    password: Option<String>,
    #[serde(default)]
    insecure_tls: bool,
    duplicate_policy: Option<String>,
    checksum_algo: Option<String>,
    checksum_expected: Option<String>,
}

fn blank(value: Option<String>) -> Option<String> {
    value.and_then(|text| {
        let trimmed = text.trim();
        if trimmed.is_empty() { None } else { Some(trimmed.to_string()) }
    })
}

fn parse_pairs(raw: &str, separator: char, label: &str) -> Result<Vec<(String, String)>, UiError> {
    let mut out = Vec::new();
    let lines: Vec<&str> = if separator == ':' { raw.lines().collect() } else { raw.split(';').collect() };
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(separator) else {
            return Err(text_err("failed", format!("{label} `{line}` needs a {separator}")));
        };
        let name = name.trim();
        if name.is_empty() {
            return Err(text_err("failed", format!("{label} name is empty")));
        }
        out.push((name.to_string(), value.trim().to_string()));
    }
    Ok(out)
}

fn build_request(input: AddInput) -> Result<AddRequest, UiError> {
    let save_dir = input.save_dir.trim();
    if save_dir.is_empty() {
        return Err(text_err("failed", "save directory is required"));
    }
    let mut request = AddRequest::new(input.url.trim(), PathBuf::from(save_dir));
    request.filename = blank(input.filename);
    request.segments = input.segments;
    request.start = true;
    request.referer = blank(input.referer);
    request.user_agent = blank(input.user_agent);
    request.headers = parse_pairs(input.headers.as_deref().unwrap_or(""), ':', "header")?;
    validate_headers(&request.headers).map_err(ui_err)?;
    request.cookies = parse_pairs(input.cookies.as_deref().unwrap_or(""), '=', "cookie")?;
    request.proxy = blank(input.proxy);
    request.speed_limit_bps = input.speed_limit_bps;
    request.username = blank(input.username);
    request.password = blank(input.password);
    request.insecure_tls = input.insecure_tls;
    if let Some(policy) = blank(input.duplicate_policy) {
        request.duplicate_policy = DuplicatePolicy::parse(&policy).map_err(ui_err)?;
    }
    request.checksum_algo = match blank(input.checksum_algo) {
        Some(name) => Some(ChecksumAlgo::parse(&name).map_err(ui_err)?),
        None => None,
    };
    request.checksum_expected = blank(input.checksum_expected);
    if request.checksum_algo.is_some() != request.checksum_expected.is_some() {
        return Err(text_err("failed", "checksum needs both an algorithm and a value"));
    }
    Ok(request)
}

async fn remote(forward: &Forward, method: &str, path: &str, body: &str) -> Result<String, UiError> {
    let (status, text) = control_exchange(forward.addr, &forward.token, method, path, body, None, None)
        .await
        .map_err(ui_err)?;
    if !(200..300).contains(&status) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            let code = value.get("code").and_then(|item| item.as_str()).unwrap_or("failed");
            let message = value.get("error").and_then(|item| item.as_str()).unwrap_or(text.as_str());
            return Err(text_err(code, message));
        }
        return Err(text_err("failed", text));
    }
    Ok(text)
}

#[tauri::command]
async fn engine_mode(state: State<'_, AppState>) -> Result<String, UiError> {
    Ok(session(&state).await?.mode.clone())
}

#[tauri::command]
async fn list_downloads(state: State<'_, AppState>) -> Result<Vec<Download>, UiError> {
    let ready = session(&state).await?;
    if let Some(engine) = &ready.engine {
        return engine.list().map_err(ui_err);
    }
    let forward = ready.forward.as_ref().ok_or_else(|| text_err("shell", "no engine"))?;
    let text = remote(forward, "GET", "/v1/downloads", "").await?;
    serde_json::from_str(&text).map_err(|err| text_err("failed", err.to_string()))
}

#[tauri::command]
async fn add_download(state: State<'_, AppState>, input: AddInput) -> Result<String, UiError> {
    let request = build_request(input)?;
    let ready = session(&state).await?;
    if let Some(engine) = &ready.engine {
        let id = engine.add(request).await.map_err(ui_err)?;
        return Ok(id.to_string());
    }
    let forward = ready.forward.as_ref().ok_or_else(|| text_err("shell", "no engine"))?;
    let body = serde_json::json!({
        "url": request.url,
        "save_dir": request.save_dir,
        "filename": request.filename,
        "segments": request.segments,
        "start": request.start,
        "referer": request.referer,
        "user_agent": request.user_agent,
        "headers": request.headers,
        "insecure_tls": request.insecure_tls,
        "duplicate_policy": request.duplicate_policy,
        "checksum_algo": request.checksum_algo,
        "checksum_expected": request.checksum_expected,
        "cookies": request.cookies,
        "proxy": request.proxy,
        "speed_limit_bps": request.speed_limit_bps,
        "username": request.username,
        "password": request.password,
    });
    let text = remote(forward, "POST", "/v1/downloads", &body.to_string()).await?;
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|value| value.get("id").and_then(|id| id.as_str()).map(str::to_string))
        .ok_or_else(|| text_err("failed", "response has no id"))
}

#[tauri::command]
async fn pause_download(state: State<'_, AppState>, id: String) -> Result<(), UiError> {
    let ready = session(&state).await?;
    if let Some(engine) = &ready.engine {
        let resolved = engine.resolve(&id).map_err(ui_err)?;
        return engine.pause(resolved).await.map_err(ui_err);
    }
    let forward = ready.forward.as_ref().ok_or_else(|| text_err("shell", "no engine"))?;
    remote(forward, "POST", &format!("/v1/downloads/{id}/pause"), "").await?;
    Ok(())
}

#[tauri::command]
async fn resume_download(state: State<'_, AppState>, id: String) -> Result<(), UiError> {
    let ready = session(&state).await?;
    if let Some(engine) = &ready.engine {
        let resolved = engine.resolve(&id).map_err(ui_err)?;
        return engine.resume(resolved).await.map_err(ui_err);
    }
    let forward = ready.forward.as_ref().ok_or_else(|| text_err("shell", "no engine"))?;
    remote(forward, "POST", &format!("/v1/downloads/{id}/resume"), "").await?;
    Ok(())
}

#[tauri::command]
async fn remove_download(state: State<'_, AppState>, id: String, delete_file: bool) -> Result<(), UiError> {
    let ready = session(&state).await?;
    if let Some(engine) = &ready.engine {
        let resolved = engine.resolve(&id).map_err(ui_err)?;
        return engine.remove(resolved, delete_file).await.map_err(ui_err);
    }
    let forward = ready.forward.as_ref().ok_or_else(|| text_err("shell", "no engine"))?;
    let body = serde_json::json!({ "delete_file": delete_file }).to_string();
    remote(forward, "POST", &format!("/v1/downloads/{id}/remove"), &body).await?;
    Ok(())
}

#[tauri::command]
async fn download_segments(state: State<'_, AppState>, id: String) -> Result<Vec<Segment>, UiError> {
    let ready = session(&state).await?;
    if let Some(engine) = &ready.engine {
        let resolved = engine.resolve(&id).map_err(ui_err)?;
        return engine.segments(resolved).map_err(ui_err);
    }
    let forward = ready.forward.as_ref().ok_or_else(|| text_err("shell", "no engine"))?;
    let text = remote(forward, "GET", &format!("/v1/downloads/{id}/segments"), "").await?;
    serde_json::from_str(&text).map_err(|err| text_err("failed", err.to_string()))
}

#[tauri::command]
async fn get_settings(state: State<'_, AppState>) -> Result<serde_json::Map<String, serde_json::Value>, UiError> {
    let ready = session(&state).await?;
    let mut map = serde_json::Map::new();
    if let Some(engine) = &ready.engine {
        for key in SETTING_KEYS {
            let value = engine.setting_value(key).map_err(ui_err)?.unwrap_or(serde_json::Value::Null);
            map.insert((*key).to_string(), value);
        }
        return Ok(map);
    }
    let forward = ready.forward.as_ref().ok_or_else(|| text_err("shell", "no engine"))?;
    for key in SETTING_KEYS {
        let text = remote(forward, "GET", &format!("/v1/settings/{key}"), "").await?;
        let value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
        map.insert((*key).to_string(), value);
    }
    Ok(map)
}

#[tauri::command]
async fn set_setting(state: State<'_, AppState>, key: String, value: serde_json::Value) -> Result<(), UiError> {
    let ready = session(&state).await?;
    if let Some(engine) = &ready.engine {
        return engine.set_setting(&key, &value).map_err(ui_err);
    }
    let forward = ready.forward.as_ref().ok_or_else(|| text_err("shell", "no engine"))?;
    let body = serde_json::json!({ "key": key, "value": value }).to_string();
    remote(forward, "POST", "/v1/settings", &body).await?;
    Ok(())
}

#[tauri::command]
fn default_save_dir() -> String {
    lock::downloads_dir().display().to_string()
}

async fn start_owner(handle: AppHandle, owner: Owner) -> Result<Session, String> {
    let engine = Engine::open(EngineConfig::new(owner.data_dir.join("forge.db")))
        .await
        .map_err(|err| err.to_string())?;
    let token = match random_token() {
        Ok(token) => token,
        Err(err) => {
            engine.shutdown().await;
            return Err(err.to_string());
        }
    };
    let addr = match control::serve(engine.clone(), token.clone()).await {
        Ok(addr) => addr,
        Err(err) => {
            engine.shutdown().await;
            return Err(err.to_string());
        }
    };
    if let Err(err) = lock::write_info(&owner.data_dir, addr.port(), &token) {
        engine.shutdown().await;
        return Err(err.to_string());
    }
    let mut events = engine.subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    let _ = handle.emit("engine", &event);
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    Ok(Session { mode: "owner".into(), engine: Some(engine), forward: None, _owner: Some(owner) })
}

fn start_forward(handle: AppHandle, forward: &Forward) {
    let addr = forward.addr;
    let token = forward.token.clone();
    tauri::async_runtime::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_millis(500));
        let client = Forward { addr, token };
        loop {
            ticker.tick().await;
            if let Ok(text) = remote(&client, "GET", "/v1/downloads", "").await {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                    let _ = handle.emit("snapshot", value);
                }
            }
        }
    });
}

async fn start_session(handle: AppHandle) -> Result<Session, String> {
    let dir = lock::data_dir();
    match lock::attach(&dir).map_err(|err| err.to_string())? {
        Attach::Owner(owner) => start_owner(handle, owner).await,
        Attach::Forward { addr, token } => {
            let forward = Forward { addr, token };
            start_forward(handle.clone(), &forward);
            Ok(Session { mode: "forward".into(), engine: None, forward: Some(forward), _owner: None })
        }
    }
}

fn main() {
    let (ready, hold) = watch::channel(Boot::Pending);
    tauri::Builder::default()
        .setup(move |app| {
            let handle = app.handle().clone();
            let tx = ready.clone();
            app.manage(AppState { ready, _hold: hold });
            tauri::async_runtime::spawn(async move {
                let boot = match start_session(handle).await {
                    Ok(session) => Boot::Ready(Arc::new(session)),
                    Err(message) => Boot::Failed(message),
                };
                let _ = tx.send(boot);
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            engine_mode,
            list_downloads,
            add_download,
            pause_download,
            resume_download,
            remove_download,
            download_segments,
            get_settings,
            set_setting,
            default_save_dir
        ])
        .run(tauri::generate_context!())
        .expect("error while running the desktop shell");
}
