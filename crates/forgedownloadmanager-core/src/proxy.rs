use url::Url;

use crate::error::Error;
use crate::Result;

/// What one reqwest client should use. `Direct` calls `no_proxy()`, including when the mode is `direct`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedProxy {
    Direct,
    /// `Proxy::all`, used for HTTP and SOCKS5.
    All(String),
    /// `Proxy::https`.
    Https(String),
}

impl ResolvedProxy {
    pub fn cache_key(&self) -> String {
        match self {
            Self::Direct => "direct".into(),
            Self::All(url) => format!("all|{url}"),
            Self::Https(url) => format!("https|{url}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyOverride {
    pub mode: String,
    pub url: Option<String>,
}

pub fn parse_override(raw: &str) -> Result<ProxyOverride> {
    let value: serde_json::Value = serde_json::from_str(raw).map_err(|err| Error::Failed(format!("proxy: {err}")))?;
    let mode = value
        .get("mode")
        .and_then(|item| item.as_str())
        .ok_or_else(|| Error::Failed("proxy mode is missing".into()))?
        .to_string();
    let url = value.get("url").and_then(|item| item.as_str()).map(str::to_string);
    Ok(ProxyOverride { mode, url })
}

/// CLI `--proxy` value: `direct`, `system`, or a proxy URL.
pub fn override_from_flag(value: &str) -> Result<String> {
    let value = value.trim();
    let (mode, url) = if value.eq_ignore_ascii_case("direct") {
        ("direct", None)
    } else if value.eq_ignore_ascii_case("system") {
        ("system", None)
    } else if let Some(rest) = value.strip_prefix("socks5://").or_else(|| value.strip_prefix("socks5h://")) {
        if rest.is_empty() {
            return Err(Error::Failed("socks5 proxy needs a host".into()));
        }
        ("socks5", Some(normalize_proxy_url(value, "socks5")))
    } else if value.starts_with("https://") {
        ("https", Some(value.to_string()))
    } else if value.starts_with("http://") {
        ("http", Some(value.to_string()))
    } else {
        return Err(Error::Failed(
            "proxy must be direct, system, http://..., https://..., or socks5://...".into(),
        ));
    };
    Ok(serde_json::json!({ "mode": mode, "url": url }).to_string())
}

pub fn resolve_proxy(
    mode: &str,
    configured_url: Option<&str>,
    target: &Url,
    env_get: impl Fn(&str) -> Option<String>,
) -> Result<ResolvedProxy> {
    match mode {
        "direct" | "" => Ok(ResolvedProxy::Direct),
        "system" => Ok(system_proxy(target, &env_get).unwrap_or(ResolvedProxy::Direct)),
        "http" => Ok(ResolvedProxy::All(require_url(configured_url, "http")?)),
        "https" => Ok(ResolvedProxy::Https(require_url(configured_url, "https")?)),
        "socks5" => {
            let url = require_url(configured_url, "socks5")?;
            let url = if url.contains("://") { url } else { format!("socks5://{url}") };
            Ok(ResolvedProxy::All(url))
        }
        other => Err(Error::Failed(format!("unknown proxy mode {other}"))),
    }
}

fn require_url(url: Option<&str>, scheme: &str) -> Result<String> {
    let url = url.map(str::trim).filter(|value| !value.is_empty() && *value != "null");
    let Some(url) = url else {
        return Err(Error::Failed(format!("proxy mode {scheme} needs a proxy URL")));
    };
    Ok(normalize_proxy_url(url, scheme))
}

fn normalize_proxy_url(raw: &str, default_scheme: &str) -> String {
    let raw = raw.trim();
    if raw.contains("://") {
        raw.to_string()
    } else {
        format!("{default_scheme}://{raw}")
    }
}

fn system_proxy(target: &Url, env_get: &impl Fn(&str) -> Option<String>) -> Option<ResolvedProxy> {
    let host = target.host_str().unwrap_or("");
    if let Some(no_proxy) = first_env(env_get, &["NO_PROXY", "no_proxy"]) {
        if host_bypassed(host, &no_proxy) {
            return Some(ResolvedProxy::Direct);
        }
    }
    let names: &[&str] = if target.scheme() == "https" {
        &["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"]
    } else {
        &["HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"]
    };
    if let Some(raw) = first_env(env_get, names) {
        return Some(proxy_from_env(&raw));
    }
    platform_proxy(target.scheme())
}

fn first_env(env_get: &impl Fn(&str) -> Option<String>, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| env_get(name).map(|value| value.trim().to_string()).filter(|value| !value.is_empty()))
}

fn proxy_from_env(raw: &str) -> ResolvedProxy {
    let raw = raw.trim().trim_matches('"');
    if raw.starts_with("socks5://") || raw.starts_with("socks5h://") || raw.starts_with("http://") {
        ResolvedProxy::All(raw.to_string())
    } else if raw.starts_with("https://") {
        ResolvedProxy::Https(raw.to_string())
    } else {
        ResolvedProxy::All(format!("http://{raw}"))
    }
}

pub fn host_bypassed(host: &str, no_proxy: &str) -> bool {
    let host = host.trim().trim_matches(['[', ']']).to_ascii_lowercase();
    no_proxy.split([',', ' ']).map(str::trim).filter(|item| !item.is_empty()).any(|item| {
        let item = item.trim_start_matches('*').trim().to_ascii_lowercase();
        if item.is_empty() || item == "." {
            return true;
        }
        let item = item.trim_start_matches('.');
        host == item || host.ends_with(&format!(".{item}"))
    })
}

/// `host:port` or `http=host:port;https=host:port`.
pub fn parse_proxy_server(value: &str, scheme: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if !value.contains('=') {
        return Some(normalize_proxy_url(value, "http"));
    }
    let mut fallback = None;
    for part in value.split(';') {
        let Some((key, addr)) = part.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let addr = addr.trim();
        if addr.is_empty() {
            continue;
        }
        if key == scheme || (scheme == "https" && key == "https") || (scheme == "http" && key == "http") {
            return Some(normalize_proxy_url(addr, "http"));
        }
        if key == "socks" {
            fallback = Some(normalize_proxy_url(addr, "socks5"));
        }
    }
    fallback
}

#[cfg(windows)]
fn platform_proxy(scheme: &str) -> Option<ResolvedProxy> {
    let server = windows_proxy_server()?;
    parse_proxy_server(&server, scheme).map(|url| {
        if url.starts_with("https://") {
            ResolvedProxy::Https(url)
        } else {
            ResolvedProxy::All(url)
        }
    })
}

#[cfg(not(windows))]
fn platform_proxy(_scheme: &str) -> Option<ResolvedProxy> {
    None
}

#[cfg(windows)]
fn windows_proxy_server() -> Option<String> {
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
    };
    let subkey: Vec<u16> = wide(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings");
    let mut key: HKEY = std::ptr::null_mut();
    let opened = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut key) };
    if opened != 0 {
        return None;
    }
    let enabled = query_dword(key, "ProxyEnable").unwrap_or(0);
    let server = if enabled == 0 { None } else { query_sz(key, "ProxyServer") };
    unsafe {
        RegCloseKey(key);
    }
    server.filter(|value| !value.is_empty())
}

#[cfg(windows)]
fn query_dword(key: windows_sys::Win32::System::Registry::HKEY, name: &str) -> Option<u32> {
    use windows_sys::Win32::System::Registry::{RegQueryValueExW, REG_DWORD};
    let name = wide(name);
    let mut kind = 0u32;
    let mut data = [0u8; 4];
    let mut len = data.len() as u32;
    let status = unsafe { RegQueryValueExW(key, name.as_ptr(), std::ptr::null(), &mut kind, data.as_mut_ptr(), &mut len) };
    if status != 0 || kind != REG_DWORD || len < 4 {
        return None;
    }
    Some(u32::from_le_bytes(data))
}

#[cfg(windows)]
fn query_sz(key: windows_sys::Win32::System::Registry::HKEY, name: &str) -> Option<String> {
    use windows_sys::Win32::System::Registry::{RegQueryValueExW, REG_SZ};
    let name = wide(name);
    let mut kind = 0u32;
    let mut len = 0u32;
    let status = unsafe { RegQueryValueExW(key, name.as_ptr(), std::ptr::null(), &mut kind, std::ptr::null_mut(), &mut len) };
    if status != 0 || kind != REG_SZ || len == 0 {
        return None;
    }
    let mut data = vec![0u8; len as usize];
    let status = unsafe { RegQueryValueExW(key, name.as_ptr(), std::ptr::null(), &mut kind, data.as_mut_ptr(), &mut len) };
    if status != 0 {
        return None;
    }
    let words: Vec<u16> = data
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .take_while(|unit| *unit != 0)
        .collect();
    Some(String::from_utf16_lossy(&words))
}

#[cfg(windows)]
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(raw: &str) -> Url {
        Url::parse(raw).unwrap()
    }

    #[test]
    fn direct_mode_ignores_the_environment() {
        let resolved = resolve_proxy("direct", None, &url("https://example.com/a"), |_| Some("http://127.0.0.1:9".into())).unwrap();
        assert_eq!(resolved, ResolvedProxy::Direct);
    }

    #[test]
    fn system_mode_prefers_https_proxy_then_all_proxy() {
        let resolved = resolve_proxy("system", None, &url("https://example.com/a"), |name| {
            match name {
                "ALL_PROXY" => Some("http://all.example:1".into()),
                "HTTPS_PROXY" => Some("http://secure.example:2".into()),
                _ => None,
            }
        })
        .unwrap();
        assert_eq!(resolved, ResolvedProxy::All("http://secure.example:2".into()));
    }

    #[test]
    fn no_proxy_matches_the_host_and_a_suffix() {
        assert!(host_bypassed("example.com", "example.com"));
        assert!(host_bypassed("a.example.com", ".example.com"));
        assert!(!host_bypassed("example.org", "example.com"));
        let resolved = resolve_proxy("system", None, &url("http://localhost/file"), |name| match name {
            "NO_PROXY" => Some("localhost".into()),
            "HTTP_PROXY" => Some("http://127.0.0.1:9".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(resolved, ResolvedProxy::Direct);
    }

    #[test]
    fn windows_proxy_server_string() {
        assert_eq!(parse_proxy_server("127.0.0.1:8888", "https").as_deref(), Some("http://127.0.0.1:8888"));
        assert_eq!(
            parse_proxy_server("http=10.0.0.1:8080;https=10.0.0.2:8443", "https").as_deref(),
            Some("http://10.0.0.2:8443")
        );
    }

    #[test]
    fn flag_parses_socks_and_direct() {
        let direct = override_from_flag("direct").unwrap();
        assert!(direct.contains("direct"));
        let socks = override_from_flag("socks5://127.0.0.1:1080").unwrap();
        let parsed = parse_override(&socks).unwrap();
        assert_eq!(parsed.mode, "socks5");
        assert_eq!(parsed.url.as_deref(), Some("socks5://127.0.0.1:1080"));
    }
}
