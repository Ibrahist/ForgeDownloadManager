//! Native Messaging Host implementation for browser extensions.
//!
//! Protocol:
//! - 4 bytes unsigned 32-bit integer (native endian) length prefix
//! - UTF-8 encoded JSON payload
//! - Handled actions: ping, status, site_rules, add

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use forgedownloadmanager_core::control;
use forgedownloadmanager_core::model::{AddRequest, SiteRule};
use forgedownloadmanager_core::secrets::KeyringSecrets;
use forgedownloadmanager_core::store::Store;
use forgedownloadmanager_core::insert_request;

use crate::lock;

pub const HOST_NAME: &str = "com.forgedownloadmanager.host";
pub const FIREFOX_EXTENSION_ID: &str = "forge-dm@forgedownloadmanager.com";

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum HostMessage {
    #[serde(rename = "ping")]
    Ping,
    #[serde(rename = "status")]
    Status,
    #[serde(rename = "site_rules")]
    SiteRules,
    #[serde(rename = "add")]
    Add {
        url: String,
        #[serde(default)]
        filename: Option<String>,
        #[serde(default)]
        dir: Option<String>,
        #[serde(default)]
        cookies: Option<Value>,
        #[serde(default)]
        headers: Option<Value>,
        #[serde(default)]
        referer: Option<String>,
        #[serde(default)]
        user_agent: Option<String>,
        #[serde(default)]
        queue_id: Option<String>,
        #[serde(default)]
        priority: Option<i64>,
    },
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
pub enum HostResponse {
    #[serde(rename = "pong")]
    Pong {
        version: String,
        running: bool,
        port: Option<u16>,
    },
    #[serde(rename = "status")]
    Status {
        running: bool,
        port: Option<u16>,
        downloads: Value,
    },
    #[serde(rename = "site_rules")]
    SiteRules {
        rules: Vec<SiteRule>,
    },
    #[serde(rename = "add_response")]
    AddResponse {
        success: bool,
        id: Option<String>,
        error: Option<String>,
        offline: bool,
    },
    #[serde(rename = "error")]
    Error {
        message: String,
    },
}

pub async fn run_host(data_dir: &Path) -> Result<i32, String> {
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();

    loop {
        let mut len_bytes = [0u8; 4];
        match stdin.read_exact(&mut len_bytes) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(format!("native host stdin read error: {e}")),
        }
        let len = u32::from_ne_bytes(len_bytes) as usize;
        if len == 0 || len > 10 * 1024 * 1024 {
            break;
        }
        let mut buf = vec![0u8; len];
        stdin.read_exact(&mut buf).map_err(|e| format!("reading message body: {e}"))?;

        let response = handle_host_message(data_dir, &buf).await;
        let response_bytes = serde_json::to_vec(&response).map_err(|e| format!("serializing response: {e}"))?;
        let resp_len = (response_bytes.len() as u32).to_ne_bytes();
        stdout.write_all(&resp_len).map_err(|e| format!("writing response length: {e}"))?;
        stdout.write_all(&response_bytes).map_err(|e| format!("writing response body: {e}"))?;
        stdout.flush().map_err(|e| format!("flushing response: {e}"))?;
    }

    Ok(0)
}

pub async fn handle_host_message(data_dir: &Path, raw: &[u8]) -> HostResponse {
    let msg: HostMessage = match serde_json::from_slice(raw) {
        Ok(m) => m,
        Err(e) => return HostResponse::Error { message: format!("invalid JSON message: {e}") },
    };

    let supervisor = lock::supervisor(data_dir).ok().flatten();

    match msg {
        HostMessage::Ping => {
            HostResponse::Pong {
                version: env!("CARGO_PKG_VERSION").to_string(),
                running: supervisor.is_some(),
                port: supervisor.map(|(addr, _)| addr.port()),
            }
        }
        HostMessage::Status => {
            if let Some((addr, token)) = supervisor {
                match control::control_exchange(addr, &token, "GET", "/v1/downloads", "", None, None).await {
                    Ok((200, text)) => {
                        let val: Value = serde_json::from_str(&text).unwrap_or(Value::Array(Vec::new()));
                        HostResponse::Status {
                            running: true,
                            port: Some(addr.port()),
                            downloads: val,
                        }
                    }
                    _ => HostResponse::Status {
                        running: true,
                        port: Some(addr.port()),
                        downloads: Value::Array(Vec::new()),
                    },
                }
            } else {
                let db_path = data_dir.join("forge.db");
                let list = Store::list_if_exists(&db_path).unwrap_or_default();
                HostResponse::Status {
                    running: false,
                    port: None,
                    downloads: serde_json::to_value(list).unwrap_or(Value::Array(Vec::new())),
                }
            }
        }
        HostMessage::SiteRules => {
            if let Some((addr, token)) = supervisor {
                match control::control_exchange(addr, &token, "GET", "/v1/site-rules", "", None, None).await {
                    Ok((200, text)) => {
                        let rules: Vec<SiteRule> = serde_json::from_str(&text).unwrap_or_default();
                        HostResponse::SiteRules { rules }
                    }
                    _ => HostResponse::SiteRules { rules: Vec::new() },
                }
            } else {
                let db_path = data_dir.join("forge.db");
                let rules = match Store::open(&db_path) {
                    Ok(store) => store.list_site_rules().unwrap_or_default(),
                    Err(_) => Vec::new(),
                };
                HostResponse::SiteRules { rules }
            }
        }
        HostMessage::Add {
            url,
            filename,
            dir,
            cookies,
            headers,
            referer,
            user_agent,
            queue_id,
            priority,
        } => {
            let parsed_cookies = parse_json_cookies(cookies);
            let parsed_headers = parse_json_headers(headers);
            let parsed_queue_id = queue_id.as_deref().and_then(|s| Uuid::parse_str(s).ok());

            if let Some((addr, token)) = supervisor {
                // Online: forward to running engine via HTTP control API
                let body = serde_json::json!({
                    "url": url,
                    "filename": filename,
                    "dir": dir,
                    "cookies": parsed_cookies,
                    "headers": parsed_headers,
                    "referer": referer,
                    "user_agent": user_agent,
                    "queue_id": parsed_queue_id,
                    "priority": priority,
                });
                match control::control_exchange(addr, &token, "POST", "/v1/downloads", &body.to_string(), None, None).await {
                    Ok((200, text)) => {
                        let v: Value = serde_json::from_str(&text).unwrap_or_default();
                        let id = v.get("id").and_then(|s| s.as_str()).map(|s| s.to_string());
                        HostResponse::AddResponse {
                            success: true,
                            id,
                            error: None,
                            offline: false,
                        }
                    }
                    Ok((_, text)) => HostResponse::AddResponse {
                        success: false,
                        id: None,
                        error: Some(text),
                        offline: false,
                    },
                    Err(e) => HostResponse::AddResponse {
                        success: false,
                        id: None,
                        error: Some(e.to_string()),
                        offline: false,
                    },
                }
            } else {
                // Offline: insert directly into SQLite
                let db_path = data_dir.join("forge.db");
                let save_dir = dir
                    .map(PathBuf::from)
                    .unwrap_or_else(default_download_dir);

                let mut req = AddRequest::new(url, save_dir);
                req.filename = filename;
                req.start = false; // stays queued until engine opens
                req.cookies = parsed_cookies;
                req.headers = parsed_headers;
                req.referer = referer;
                req.user_agent = user_agent;
                req.queue_id = parsed_queue_id;
                req.priority = priority;

                match Store::open(&db_path) {
                    Ok(store) => {
                        let segments = store.setting_u64("default_segments").map_err(|e| e.to_string()).unwrap_or(Some(8)).unwrap_or(8) as u32;
                        let ua = store.setting_string("user_agent").map_err(|e| e.to_string()).unwrap_or(None).unwrap_or_else(|| forgedownloadmanager_core::config::DEFAULT_USER_AGENT.to_string());
                        let secrets = KeyringSecrets;
                        match insert_request(&store, &req, segments, &ua, &secrets) {
                            Ok(id) => HostResponse::AddResponse {
                                success: true,
                                id: Some(id.to_string()),
                                error: None,
                                offline: true,
                            },
                            Err(e) => HostResponse::AddResponse {
                                success: false,
                                id: None,
                                error: Some(e.to_string()),
                                offline: true,
                            },
                        }
                    }
                    Err(e) => HostResponse::AddResponse {
                        success: false,
                        id: None,
                        error: Some(e.to_string()),
                        offline: true,
                    },
                }
            }
        }
    }
}

fn parse_json_cookies(val: Option<Value>) -> Vec<(String, String)> {
    let mut cookies = Vec::new();
    let Some(val) = val else { return cookies };

    match val {
        Value::Array(arr) => {
            for item in arr {
                if let Value::Object(obj) = item {
                    if let (Some(Value::String(k)), Some(Value::String(v))) = (obj.get("name"), obj.get("value")) {
                        cookies.push((k.clone(), v.clone()));
                    }
                }
            }
        }
        Value::String(s) => {
            for part in s.split(';') {
                let part = part.trim();
                if let Some((k, v)) = part.split_once('=') {
                    cookies.push((k.trim().to_string(), v.trim().to_string()));
                }
            }
        }
        _ => {}
    }
    cookies
}

fn parse_json_headers(val: Option<Value>) -> Vec<(String, String)> {
    let mut headers = Vec::new();
    let Some(val) = val else { return headers };

    match val {
        Value::Object(map) => {
            for (k, v) in map {
                if let Value::String(s) = v {
                    headers.push((k, s));
                }
            }
        }
        Value::Array(arr) => {
            for item in arr {
                if let Value::Array(pair) = item {
                    if pair.len() == 2 {
                        if let (Some(Value::String(k)), Some(Value::String(v))) = (pair.get(0), pair.get(1)) {
                            headers.push((k.clone(), v.clone()));
                        }
                    }
                }
            }
        }
        _ => {}
    }
    headers
}

fn default_download_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Ok(userprofile) = std::env::var("USERPROFILE") {
            let p = PathBuf::from(userprofile).join("Downloads");
            if p.exists() {
                return p;
            }
        }
    }
    #[cfg(not(windows))]
    {
        if let Ok(home) = std::env::var("HOME") {
            let p = PathBuf::from(home).join("Downloads");
            if p.exists() {
                return p;
            }
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

pub fn install_host(data_dir: &Path) -> Result<i32, String> {
    let current_exe = std::env::current_exe().map_err(|e| format!("cannot determine current exe path: {e}"))?;
    let exe_path_str = current_exe.to_string_lossy().to_string();

    std::fs::create_dir_all(data_dir).map_err(|e| format!("creating data dir: {e}"))?;

    // 1. Chrome/Edge Manifest
    let chrome_manifest_path = data_dir.join("com.forgedownloadmanager.host.json");
    let chrome_manifest = serde_json::json!({
        "name": HOST_NAME,
        "description": "Forge Download Manager Native Messaging Host",
        "path": exe_path_str,
        "type": "stdio",
        "allowed_origins": [
            "chrome-extension://*/*",
            "chrome-extension://*/"
        ]
    });
    std::fs::write(&chrome_manifest_path, serde_json::to_string_pretty(&chrome_manifest).unwrap())
        .map_err(|e| format!("writing chrome host manifest: {e}"))?;

    // 2. Firefox Manifest
    let firefox_manifest_path = data_dir.join("com.forgedownloadmanager.host.firefox.json");
    let firefox_manifest = serde_json::json!({
        "name": HOST_NAME,
        "description": "Forge Download Manager Native Messaging Host",
        "path": exe_path_str,
        "type": "stdio",
        "allowed_extensions": [
            FIREFOX_EXTENSION_ID
        ]
    });
    std::fs::write(&firefox_manifest_path, serde_json::to_string_pretty(&firefox_manifest).unwrap())
        .map_err(|e| format!("writing firefox host manifest: {e}"))?;

    #[cfg(windows)]
    {
        // Register in Windows Registry
        register_windows_registry("Google\\Chrome", &chrome_manifest_path)?;
        register_windows_registry("Microsoft\\Edge", &chrome_manifest_path)?;
        register_windows_registry("Mozilla", &firefox_manifest_path)?;
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(home) = std::env::var("HOME") {
            let home_p = PathBuf::from(home);
            let chrome_dir = home_p.join(".config/google-chrome/NativeMessagingHosts");
            let chromium_dir = home_p.join(".config/chromium/NativeMessagingHosts");
            let mozilla_dir = home_p.join(".mozilla/native-messaging-hosts");

            let _ = std::fs::create_dir_all(&chrome_dir);
            let _ = std::fs::copy(&chrome_manifest_path, chrome_dir.join(format!("{HOST_NAME}.json")));

            let _ = std::fs::create_dir_all(&chromium_dir);
            let _ = std::fs::copy(&chrome_manifest_path, chromium_dir.join(format!("{HOST_NAME}.json")));

            let _ = std::fs::create_dir_all(&mozilla_dir);
            let _ = std::fs::copy(&firefox_manifest_path, mozilla_dir.join(format!("{HOST_NAME}.json")));
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            let home_p = PathBuf::from(home);
            let chrome_dir = home_p.join("Library/Application Support/Google/Chrome/NativeMessagingHosts");
            let mozilla_dir = home_p.join("Library/Application Support/Mozilla/NativeMessagingHosts");

            let _ = std::fs::create_dir_all(&chrome_dir);
            let _ = std::fs::copy(&chrome_manifest_path, chrome_dir.join(format!("{HOST_NAME}.json")));

            let _ = std::fs::create_dir_all(&mozilla_dir);
            let _ = std::fs::copy(&firefox_manifest_path, mozilla_dir.join(format!("{HOST_NAME}.json")));
        }
    }

    println!("Registered native messaging host: {HOST_NAME}");
    println!("Chrome/Edge manifest: {}", chrome_manifest_path.display());
    println!("Firefox manifest: {}", firefox_manifest_path.display());
    Ok(0)
}

#[cfg(windows)]
fn register_windows_registry(browser_vendor: &str, manifest_path: &Path) -> Result<(), String> {
    let key = format!(r#"HKCU\Software\{browser_vendor}\NativeMessagingHosts\{HOST_NAME}"#);
    let output = std::process::Command::new("reg")
        .args([
            "add",
            &key,
            "/ve",
            "/t",
            "REG_SZ",
            "/d",
            &manifest_path.to_string_lossy(),
            "/f",
        ])
        .output()
        .map_err(|e| format!("failed to invoke reg.exe: {e}"))?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!("reg add failed for {key}: {err}"));
    }
    Ok(())
}

pub fn uninstall_host(data_dir: &Path) -> Result<i32, String> {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("reg")
            .args(["delete", &format!(r#"HKCU\Software\Google\Chrome\NativeMessagingHosts\{HOST_NAME}"#), "/f"])
            .output();
        let _ = std::process::Command::new("reg")
            .args(["delete", &format!(r#"HKCU\Software\Microsoft\Edge\NativeMessagingHosts\{HOST_NAME}"#), "/f"])
            .output();
        let _ = std::process::Command::new("reg")
            .args(["delete", &format!(r#"HKCU\Software\Mozilla\NativeMessagingHosts\{HOST_NAME}"#), "/f"])
            .output();
    }

    let chrome_manifest_path = data_dir.join("com.forgedownloadmanager.host.json");
    let firefox_manifest_path = data_dir.join("com.forgedownloadmanager.host.firefox.json");
    let _ = std::fs::remove_file(chrome_manifest_path);
    let _ = std::fs::remove_file(firefox_manifest_path);

    println!("Unregistered native messaging host: {HOST_NAME}");
    Ok(0)
}
