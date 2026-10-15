use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use std::io;
use std::net::SocketAddr;

#[derive(Debug)]
pub struct RawRequest {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

pub fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str())
}

pub async fn read_request(sock: &mut TcpStream, max_body: usize) -> io::Result<RawRequest> {
    let (head, mut rest) = read_head(sock).await?;
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("/");
    let path = target.split('?').next().unwrap_or("/").to_string();
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        headers.push((name.trim().to_string(), value.trim().to_string()));
    }
    let content_length = header(&headers, "content-length").and_then(|value| value.parse::<usize>().ok()).unwrap_or(0);
    if content_length > max_body {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "body too large"));
    }
    while rest.len() < content_length {
        let mut tmp = [0u8; 2048];
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        rest.extend_from_slice(&tmp[..n]);
    }
    rest.truncate(content_length);
    Ok(RawRequest { method, path, headers, body: rest })
}

pub async fn read_response(sock: &mut TcpStream) -> io::Result<(u16, String)> {
    let (head, mut rest) = read_head(sock).await?;
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    let status = status_line.split_whitespace().nth(1).and_then(|value| value.parse().ok()).unwrap_or(0);
    let mut headers = Vec::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    let content_length = header(&headers, "content-length").and_then(|value| value.parse::<usize>().ok()).unwrap_or(0);
    while rest.len() < content_length {
        let mut tmp = [0u8; 2048];
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        rest.extend_from_slice(&tmp[..n]);
    }
    rest.truncate(content_length);
    let body = String::from_utf8_lossy(&rest).into_owned();
    Ok((status, body))
}

async fn read_head(sock: &mut TcpStream) -> io::Result<(String, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    let split_at = loop {
        if buf.len() > 64 * 1024 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "headers too large"));
        }
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed"));
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(index) = find_subsequence(&buf, b"\r\n\r\n") {
            break index;
        }
    };
    let head = String::from_utf8_lossy(&buf[..split_at]).into_owned();
    let rest = buf[split_at + 4..].to_vec();
    Ok((head, rest))
}

pub async fn exchange(
    addr: SocketAddr,
    method: &str,
    path: &str,
    extra_headers: &[(&str, &str)],
    body: &[u8],
) -> io::Result<(u16, String)> {
    let mut sock = TcpStream::connect(addr).await?;
    let has_host = extra_headers.iter().any(|(name, _)| name.eq_ignore_ascii_case("host"));
    let mut text = format!("{method} {path} HTTP/1.1\r\n");
    if !has_host {
        text.push_str(&format!("Host: 127.0.0.1:{}\r\n", addr.port()));
    }
    text.push_str("Connection: close\r\n");
    for (name, value) in extra_headers {
        text.push_str(name);
        text.push_str(": ");
        text.push_str(value);
        text.push_str("\r\n");
    }
    if !body.is_empty() || method == "POST" {
        text.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    text.push_str("\r\n");
    sock.write_all(text.as_bytes()).await?;
    if !body.is_empty() {
        sock.write_all(body).await?;
    }
    read_response(&mut sock).await
}

pub fn json_response(status: u16, body: &str) -> Vec<u8> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let mut out = head.into_bytes();
    out.extend_from_slice(body.as_bytes());
    out
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}
