use std::path::{Component, Path, PathBuf};

use url::Url;

use crate::error::Error;
use crate::model::DuplicatePolicy;
use crate::Result;

const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM0", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
    "COM8", "COM9", "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Turn a header or URL fragment into a single path component.
pub fn sanitize_filename(raw: &str) -> String {
    let raw = raw.trim().trim_matches('"');
    let raw = raw.replace('\\', "/");
    let last = raw.rsplit('/').next().unwrap_or(raw.as_str()).trim();
    let mut out = String::with_capacity(last.len());
    for ch in last.chars() {
        if ch.is_control() || matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '/') {
            out.push('_');
            continue;
        }
        // Bidirectional overrides hide a different extension from the user.
        if ('\u{202A}'..='\u{202E}').contains(&ch) || ('\u{2066}'..='\u{2069}').contains(&ch) {
            continue;
        }
        out.push(ch);
    }
    let trimmed = out.trim().trim_end_matches(['.', ' ']).trim().to_string();
    let mut name = if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        "download".to_string()
    } else {
        trimmed
    };
    let stem = name.split('.').next().unwrap_or(name.as_str());
    if RESERVED.iter().any(|word| stem.eq_ignore_ascii_case(word)) {
        name.insert(0, '_');
    }
    trim_to_bytes(&name, 180)
}

fn trim_to_bytes(name: &str, max: usize) -> String {
    if name.len() <= max {
        return name.to_string();
    }
    let (stem, ext) = split_ext(name);
    let ext_budget = if ext.is_empty() { 0 } else { ext.len() + 1 };
    let stem_budget = max.saturating_sub(ext_budget).max(1);
    let stem_end = stem.floor_char_boundary(stem_budget.min(stem.len()));
    if ext.is_empty() {
        stem[..stem_end].to_string()
    } else {
        format!("{}.{}", &stem[..stem_end], ext)
    }
}

pub fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(idx) if idx > 0 && name.len() - idx <= 8 => (&name[..idx], &name[idx + 1..]),
        _ => (name, ""),
    }
}

pub fn filename_from_url(url: &Url) -> Option<String> {
    let segment = url.path_segments()?.next_back()?.trim();
    if segment.is_empty() {
        return None;
    }
    let decoded = percent_decode(segment)?;
    let sanitized = sanitize_filename(&decoded);
    if sanitized == "download" && segment.starts_with('.') {
        return None;
    }
    Some(sanitized)
}

pub fn filename_from_content_disposition(header: &str) -> Option<String> {
    if let Some(star) = disposition_param(header, "filename*") {
        let encoded = star.splitn(3, '\'').nth(2).unwrap_or(star.as_str());
        let decoded = percent_decode(encoded)?;
        let name = sanitize_filename(&decoded);
        if !name.is_empty() {
            return Some(name);
        }
    }
    disposition_param(header, "filename").map(|value| sanitize_filename(&value))
}

pub fn extension_for_mime(mime: &str) -> Option<&'static str> {
    let mime = mime.split(';').next().unwrap_or(mime).trim().to_ascii_lowercase();
    Some(match mime.as_str() {
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "video/x-matroska" => "mkv",
        "audio/mpeg" => "mp3",
        "audio/flac" => "flac",
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/ogg" => "ogg",
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "application/pdf" => "pdf",
        "application/zip" => "zip",
        "application/gzip" => "gz",
        "application/x-7z-compressed" => "7z",
        "application/x-tar" => "tar",
        "text/plain" => "txt",
        "text/html" => "html",
        "application/json" => "json",
        "application/octet-stream" => return None,
        _ => return None,
    })
}

pub fn partial_name(filename: &str) -> String {
    format!("{filename}.forgepart")
}

pub fn allocate_unique(dir: &Path, filename: &str, policy: DuplicatePolicy) -> Result<String> {
    let filename = sanitize_filename(filename);
    if !occupied(dir, &filename) {
        return Ok(filename);
    }
    match policy {
        DuplicatePolicy::Overwrite => Ok(filename),
        DuplicatePolicy::Ask => Err(Error::DuplicateAsk),
        DuplicatePolicy::Rename => {
            let (stem, ext) = split_ext(&filename);
            for n in 1..10_000 {
                let candidate = if ext.is_empty() {
                    format!("{stem} ({n})")
                } else {
                    format!("{stem} ({n}).{ext}")
                };
                if !occupied(dir, &candidate) {
                    return Ok(candidate);
                }
            }
            Err(Error::Failed("too many files share this name".into()))
        }
    }
}

fn occupied(dir: &Path, filename: &str) -> bool {
    dir.join(filename).exists() || dir.join(partial_name(filename)).exists()
}

/// `file` must be a direct child of `dir`. `dir` has to exist.
pub fn ensure_direct_child(dir: &Path, file: &Path) -> Result<()> {
    let dir = dir.canonicalize().map_err(|_| Error::PathEscape)?;
    let name = file.file_name().ok_or(Error::PathEscape)?;
    let name_str = name.to_string_lossy();
    if name_str.is_empty()
        || name_str.contains('/')
        || name_str.contains('\\')
        || name_str == "."
        || name_str == ".."
    {
        return Err(Error::PathEscape);
    }
    let parent = file.parent().ok_or(Error::PathEscape)?;
    let parent = if parent.exists() {
        parent.canonicalize().map_err(|_| Error::PathEscape)?
    } else {
        lexical(parent)
    };
    if parent != dir {
        return Err(Error::PathEscape);
    }
    Ok(())
}

pub fn validate_headers(headers: &[(String, String)]) -> Result<()> {
    if headers.len() > 32 {
        return Err(Error::Failed("too many headers".into()));
    }
    for (name, value) in headers {
        if !valid_header_name(name) {
            return Err(Error::Failed(format!("invalid header name {name}")));
        }
        if blocked_header(name) {
            return Err(Error::Failed(format!("header {name} is reserved")));
        }
        if value.len() > 8 * 1024 || value.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0) {
            return Err(Error::Failed(format!("invalid header value for {name}")));
        }
    }
    Ok(())
}

fn valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|b| {
            matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'!' | b'#' | b'$' | b'%' | b'&'
                | b'\'' | b'*' | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~')
        })
}

fn blocked_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "host"
            | "content-length"
            | "transfer-encoding"
            | "connection"
            | "range"
            | "accept-encoding"
            | "content-encoding"
            | "te"
            | "trailer"
            | "upgrade"
    )
}

fn disposition_param(header: &str, name: &str) -> Option<String> {
    let bytes = header.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i] == b';' || bytes[i].is_ascii_whitespace()) {
            i += 1;
        }
        let start = i;
        while i < bytes.len() && bytes[i] != b'=' && bytes[i] != b';' {
            i += 1;
        }
        let key = header[start..i].trim();
        if i >= bytes.len() || bytes[i] != b'=' {
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let value = if i < bytes.len() && bytes[i] == b'"' {
            i += 1;
            let mut value = String::new();
            while i < bytes.len() && bytes[i] != b'"' {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 1;
                }
                value.push(bytes[i] as char);
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'"' {
                i += 1;
            }
            value
        } else {
            let start = i;
            while i < bytes.len() && bytes[i] != b';' {
                i += 1;
            }
            header[start..i].trim().to_string()
        };
        if key.eq_ignore_ascii_case(name) {
            return Some(value);
        }
    }
    None
}

pub fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'+' {
            out.push(b' ');
            i += 1;
            continue;
        }
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = from_hex(bytes[i + 1])?;
            let lo = from_hex(bytes[i + 2])?;
            out.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}

fn from_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_traversal_to_the_final_component() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("..\\..\\evil.bin"), "evil.bin");
        assert_eq!(sanitize_filename(".."), "download");
        assert_eq!(sanitize_filename("CON.txt"), "_CON.txt");
        assert_eq!(sanitize_filename("report:final?.pdf"), "report_final_.pdf");
    }

    #[test]
    fn prefers_the_rfc5987_name() {
        let header = "attachment; filename=\"fallback.bin\"; filename*=UTF-8''na%C3%AFve.txt";
        assert_eq!(filename_from_content_disposition(header).as_deref(), Some("naïve.txt"));
    }

    #[test]
    fn parses_a_plain_disposition_name() {
        assert_eq!(
            filename_from_content_disposition("attachment; filename=\"foo bar.zip\"").as_deref(),
            Some("foo bar.zip")
        );
    }

    #[test]
    fn rejects_header_injection() {
        assert!(validate_headers(&[("X-Test".into(), "ok".into())]).is_ok());
        assert!(validate_headers(&[("X-Test".into(), "bad\r\nHost: evil".into())]).is_err());
        assert!(validate_headers(&[("Range".into(), "bytes=0-1".into())]).is_err());
    }

    #[test]
    fn duplicate_rename_and_direct_child() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.bin"), b"x").unwrap();
        assert_eq!(allocate_unique(dir.path(), "a.bin", DuplicatePolicy::Rename).unwrap(), "a (1).bin");
        assert!(allocate_unique(dir.path(), "a.bin", DuplicatePolicy::Ask).is_err());
        let child = dir.path().join("a.bin");
        assert!(ensure_direct_child(dir.path(), &child).is_ok());
        let escape = dir.path().join("..").join("outside.bin");
        assert!(ensure_direct_child(dir.path(), &escape).is_err());
    }
}
