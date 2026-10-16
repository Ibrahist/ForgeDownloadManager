use std::sync::{Arc, Mutex};

use base64::Engine;
use sha2::Digest;
use url::Url;

use crate::error::Error;
use crate::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthScheme {
    Any,
    Basic,
    Digest,
}

impl AuthScheme {
    pub fn parse(value: &str) -> Self {
        match value {
            "basic" => Self::Basic,
            "digest" => Self::Digest,
            _ => Self::Any,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::Basic => "basic",
            Self::Digest => "digest",
        }
    }

    fn allows_basic(self) -> bool {
        matches!(self, Self::Any | Self::Basic)
    }

    fn allows_digest(self) -> bool {
        matches!(self, Self::Any | Self::Digest)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum HashAlgo {
    Md5,
    Sha256,
}

/// In-memory credential. The cached header is shared by every request of one download.
#[derive(Clone)]
pub struct AuthMaterial {
    pub username: String,
    pub password: String,
    pub scheme: AuthScheme,
    cached: Arc<Mutex<Option<String>>>,
}

impl AuthMaterial {
    pub fn new(username: impl Into<String>, password: impl Into<String>, scheme: AuthScheme) -> Self {
        Self {
            username: username.into(),
            password: password.into(),
            scheme,
            cached: Arc::new(Mutex::new(None)),
        }
    }

    pub fn cached_header(&self) -> Option<String> {
        self.cached.lock().unwrap_or_else(|err| err.into_inner()).clone()
    }

    pub fn store_header(&self, header: String) {
        *self.cached.lock().unwrap_or_else(|err| err.into_inner()) = Some(header);
    }
}

impl std::fmt::Debug for AuthMaterial {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthMaterial")
            .field("username", &self.username)
            .field("scheme", &self.scheme)
            .finish()
    }
}

pub fn credential_host(url: &Url) -> String {
    let host = url.host_str().unwrap_or("");
    let port = url.port_or_known_default().unwrap_or(0);
    format!("{host}:{port}")
}

/// Origin-form request target used as the Digest URI.
pub fn request_target(url: &str) -> String {
    let Ok(url) = Url::parse(url) else {
        return "/".to_string();
    };
    let mut target = url.path().to_string();
    if target.is_empty() {
        target = "/".to_string();
    }
    if let Some(query) = url.query() {
        target.push('?');
        target.push_str(query);
    }
    target
}

pub fn basic_header(username: &str, password: &str) -> String {
    let token = base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
    format!("Basic {token}")
}

/// Answer one `WWW-Authenticate` challenge. `None` means we have nothing to send.
pub fn authorization_from_challenge(
    auth: &AuthMaterial,
    www_authenticate: &str,
    method: &str,
    uri: &str,
) -> Result<Option<String>> {
    if auth.scheme.allows_digest() {
        if let Some(header) = digest_header(auth, www_authenticate, method, uri)? {
            return Ok(Some(header));
        }
    }
    if auth.scheme.allows_basic() && www_authenticate.to_ascii_lowercase().contains("basic") {
        return Ok(Some(basic_header(&auth.username, &auth.password)));
    }
    if auth.scheme.allows_basic() && www_authenticate.trim().is_empty() {
        return Ok(Some(basic_header(&auth.username, &auth.password)));
    }
    Ok(None)
}

fn digest_header(auth: &AuthMaterial, header: &str, method: &str, uri: &str) -> Result<Option<String>> {
    let Some(params) = digest_params(header) else {
        return Ok(None);
    };
    let realm = params.iter().find(|(key, _)| key == "realm").map(|(_, value)| value.as_str());
    let nonce = params.iter().find(|(key, _)| key == "nonce").map(|(_, value)| value.as_str());
    let (Some(realm), Some(nonce)) = (realm, nonce) else {
        return Ok(None);
    };
    let algorithm = params
        .iter()
        .find(|(key, _)| key == "algorithm")
        .map(|(_, value)| value.as_str())
        .unwrap_or("MD5");
    let algo = match algorithm.to_ascii_uppercase().as_str() {
        "MD5" => HashAlgo::Md5,
        "SHA-256" | "SHA256" => HashAlgo::Sha256,
        "MD5-SESS" | "SHA-256-SESS" => {
            return Err(Error::Failed("digest session algorithms are not supported".into()));
        }
        other => return Err(Error::Failed(format!("unsupported digest algorithm {other}"))),
    };
    let qop_raw = params.iter().find(|(key, _)| key == "qop").map(|(_, value)| value.as_str());
    let qop = match qop_raw {
        Some(value) => {
            let modes: Vec<_> = value.split(',').map(str::trim).filter(|item| !item.is_empty()).collect();
            if modes.iter().any(|item| item.eq_ignore_ascii_case("auth")) {
                Some("auth")
            } else {
                return Err(Error::Failed("digest qop auth-int is not supported".into()));
            }
        }
        None => None,
    };
    let cnonce = random_cnonce();
    let nc = "00000001";
    let response = digest_response(
        algo,
        &auth.username,
        realm,
        &auth.password,
        method,
        uri,
        nonce,
        nc,
        &cnonce,
        qop,
    );
    let mut parts = vec![
        format!("username=\"{}\"", escape_quote(&auth.username)),
        format!("realm=\"{}\"", escape_quote(realm)),
        format!("nonce=\"{}\"", escape_quote(nonce)),
        format!("uri=\"{}\"", escape_quote(uri)),
        format!("response=\"{response}\""),
        format!("algorithm={}", algorithm_token(algo)),
    ];
    if qop.is_some() {
        parts.push(format!("qop=auth, nc={nc}, cnonce=\"{cnonce}\""));
    }
    if let Some((_, opaque)) = params.iter().find(|(key, _)| key == "opaque") {
        parts.push(format!("opaque=\"{}\"", escape_quote(opaque)));
    }
    Ok(Some(format!("Digest {}", parts.join(", "))))
}

fn algorithm_token(algo: HashAlgo) -> &'static str {
    match algo {
        HashAlgo::Md5 => "MD5",
        HashAlgo::Sha256 => "SHA-256",
    }
}

fn escape_quote(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn random_cnonce() -> String {
    let mut bytes = [0u8; 8];
    let _ = getrandom::fill(&mut bytes);
    hex::encode(bytes)
}

/// RFC 2617 response, with SHA-256 using the same construction when requested.
pub(crate) fn digest_response(
    algo: HashAlgo,
    username: &str,
    realm: &str,
    password: &str,
    method: &str,
    uri: &str,
    nonce: &str,
    nc: &str,
    cnonce: &str,
    qop: Option<&str>,
) -> String {
    let ha1 = hash_hex(algo, &format!("{username}:{realm}:{password}"));
    let ha2 = hash_hex(algo, &format!("{method}:{uri}"));
    let joined = if qop.is_some() {
        format!("{ha1}:{nonce}:{nc}:{cnonce}:auth:{ha2}")
    } else {
        format!("{ha1}:{nonce}:{ha2}")
    };
    hash_hex(algo, &joined)
}

fn hash_hex(algo: HashAlgo, data: &str) -> String {
    match algo {
        HashAlgo::Md5 => {
            let mut hasher = md5::Md5::new();
            hasher.update(data.as_bytes());
            hex::encode(hasher.finalize())
        }
        HashAlgo::Sha256 => {
            let mut hasher = sha2::Sha256::new();
            hasher.update(data.as_bytes());
            hex::encode(hasher.finalize())
        }
    }
}

/// Parameters of the first Digest challenge in the header.
fn digest_params(header: &str) -> Option<Vec<(String, String)>> {
    let lower = header.to_ascii_lowercase();
    let start = lower.find("digest ")?;
    let rest = header[start + "digest ".len()..].trim();
    // Stop before a following scheme token such as ", Basic".
    let mut params = Vec::new();
    for piece in split_params(rest) {
        let Some((key, value)) = piece.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        if key.is_empty() {
            continue;
        }
        let value = value.trim().trim_matches('"').to_string();
        params.push((key, value));
    }
    if params.is_empty() {
        None
    } else {
        Some(params)
    }
}

fn split_params(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for ch in value.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                current.push(ch);
            }
            ',' if !quoted => {
                if !current.trim().is_empty() {
                    out.push(current.trim().to_string());
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        out.push(current.trim().to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc2617_digest_vector() {
        let response = digest_response(
            HashAlgo::Md5,
            "Mufasa",
            "testrealm@host.com",
            "Circle Of Life",
            "GET",
            "/dir/index.html",
            "dcd98b7102dd2f0e8b11d0f600bfb0c093",
            "00000001",
            "0a4f113b",
            Some("auth"),
        );
        assert_eq!(response, "6629fae49393a05397450978507c4ef1");
    }

    #[test]
    fn basic_header_is_standard_base64() {
        assert_eq!(basic_header("aladdin", "opensesame"), "Basic YWxhZGRpbjpvcGVuc2VzYW1l");
    }

    #[test]
    fn challenge_builds_a_basic_header() {
        let auth = AuthMaterial::new("aladdin", "opensesame", AuthScheme::Any);
        let header = authorization_from_challenge(&auth, "Basic realm=\"forge\"", "GET", "/file").unwrap();
        assert_eq!(header.as_deref(), Some("Basic YWxhZGRpbjpvcGVuc2VzYW1l"));
    }
}
