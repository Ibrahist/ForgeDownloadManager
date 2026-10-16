//! Small FTP / explicit FTPS client. One REST stream, not a parallel range plan.
//! SFTP is a later phase. Anonymous login is `anonymous` / `forge@localhost`.

use std::net::{Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use rustls_platform_verifier::ConfigVerifierExt;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::download::{commit_chunk, SegmentRt};
use crate::error::Error;
use crate::limiter::Pace;
use crate::Result;

pub struct FtpInfo {
    pub total_bytes: Option<u64>,
}

pub fn ftp_path(url: &Url) -> String {
    let path = url.path();
    if path.is_empty() { "/".to_string() } else { path.to_string() }
}

pub async fn probe_size(url: &Url, insecure: bool, username: Option<&str>, password: Option<&str>) -> Result<FtpInfo> {
    let mut session = connect(url, insecure, username, password).await?;
    let total = session.size(&ftp_path(url)).await?;
    let _ = session.ctrl.cmd("QUIT").await;
    Ok(FtpInfo { total_bytes: total })
}

pub async fn fetch(
    url: &Url,
    insecure: bool,
    username: Option<&str>,
    password: Option<&str>,
    segment: &Arc<SegmentRt>,
    pace: &Pace,
    cancel: &CancellationToken,
) -> Result<()> {
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let path = ftp_path(url);
    let mut session = connect(url, insecure, username, password).await?;
    let cursor = segment.cursor.load(std::sync::atomic::Ordering::Acquire);
    if let Some(end) = segment.live_end() {
        if cursor >= end {
            return Ok(());
        }
    }
    if cursor > 0 {
        if !segment.bounded {
            return Err(Error::NotResumable);
        }
        let reply = session.ctrl.cmd(&format!("REST {cursor}")).await?;
        if reply.code != 350 {
            return Err(Error::Failed(format!("FTP REST {}", reply.code)));
        }
    }
    let pasv = session.ctrl.cmd("PASV").await?;
    if pasv.code != 227 {
        return Err(Error::Failed(format!("FTP PASV {}", pasv.code)));
    }
    let data_addr = parse_pasv(&pasv.text).ok_or_else(|| Error::Failed("FTP PASV reply has no address".into()))?;
    let data = TcpStream::connect(data_addr).await?;
    data.set_nodelay(true)?;
    let reply = session.ctrl.cmd(&format!("RETR {path}")).await?;
    if reply.code != 150 && reply.code != 125 {
        return Err(Error::Failed(format!("FTP RETR {}", reply.code)));
    }
    let mut data = session.wrap_data(data).await?;
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let offset = segment.cursor.load(std::sync::atomic::Ordering::Acquire);
        if segment.live_end().is_some_and(|end| offset >= end) {
            break;
        }
        let read = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(Error::Cancelled),
            next = data.read(&mut buf) => next?,
        };
        if read == 0 {
            break;
        }
        let mut bytes = &buf[..read];
        if let Some(end) = segment.live_end() {
            if offset >= end {
                break;
            }
            let room = usize::try_from(end - offset).unwrap_or(usize::MAX);
            if bytes.len() > room {
                bytes = &bytes[..room];
            }
        }
        if bytes.is_empty() {
            break;
        }
        pace.acquire(bytes.len() as u64, cancel).await?;
        if commit_chunk(segment, bytes).await? {
            break;
        }
    }
    drop(data);
    let done = session.ctrl.read_reply().await?;
    if segment.finished() {
        if !segment.bounded {
            segment.eof.store(true, std::sync::atomic::Ordering::Release);
        }
        return Ok(());
    }
    if done.code == 226 || done.code == 250 {
        if !segment.bounded {
            segment.eof.store(true, std::sync::atomic::Ordering::Release);
            return Ok(());
        }
    }
    Err(Error::Incomplete)
}

struct Session {
    ctrl: Ctrl<FtpStream>,
    tls: Option<Arc<ClientConfig>>,
    name: ServerName<'static>,
}

/// Plain TCP or a rustls stream. One enum keeps `BufReader` on a concrete type.
enum FtpStream {
    Plain(TcpStream),
    Tls(tokio_rustls::client::TlsStream<TcpStream>),
}

impl AsyncRead for FtpStream {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        match &mut *self {
            Self::Plain(stream) => Pin::new(stream).poll_read(cx, buf),
            Self::Tls(stream) => Pin::new(stream).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for FtpStream {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        match &mut *self {
            Self::Plain(stream) => Pin::new(stream).poll_write(cx, buf),
            Self::Tls(stream) => Pin::new(stream).poll_write(cx, buf),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match &mut *self {
            Self::Plain(stream) => Pin::new(stream).poll_flush(cx),
            Self::Tls(stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match &mut *self {
            Self::Plain(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Tls(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}

impl Session {
    async fn size(&mut self, path: &str) -> Result<Option<u64>> {
        let reply = self.ctrl.cmd(&format!("SIZE {path}")).await?;
        if reply.code != 213 {
            return Ok(None);
        }
        Ok(reply.text.split_whitespace().nth(1).and_then(|text| text.parse().ok()))
    }

    async fn wrap_data(&self, tcp: TcpStream) -> Result<FtpStream> {
        let Some(config) = &self.tls else {
            return Ok(FtpStream::Plain(tcp));
        };
        let stream = TlsConnector::from(Arc::clone(config))
            .connect(self.name.clone(), tcp)
            .await
            .map_err(|err| Error::Failed(format!("FTPS data handshake: {err}")))?;
        Ok(FtpStream::Tls(stream))
    }
}

struct Ctrl<S> {
    io: BufReader<S>,
}

impl<S> Ctrl<S>
where
    S: AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    async fn cmd(&mut self, line: &str) -> Result<Reply> {
        self.io.write_all(format!("{line}\r\n").as_bytes()).await?;
        self.io.flush().await?;
        self.read_reply().await
    }

    async fn read_reply(&mut self) -> Result<Reply> {
        let mut line = String::new();
        let n = tokio::io::AsyncBufReadExt::read_line(&mut self.io, &mut line).await?;
        if n == 0 {
            return Err(Error::Incomplete);
        }
        let code = line_code(&line)?;
        let mut text = line.clone();
        if line.len() >= 4 && line.as_bytes()[3] == b'-' {
            loop {
                line.clear();
                let n = tokio::io::AsyncBufReadExt::read_line(&mut self.io, &mut line).await?;
                if n == 0 {
                    break;
                }
                let finished = line.len() >= 4 && line.as_bytes()[3] == b' ' && line.starts_with(&format!("{code:03}"));
                text.push_str(&line);
                if finished {
                    break;
                }
            }
        }
        Ok(Reply { code, text })
    }
}

struct Reply {
    code: u16,
    text: String,
}

async fn connect(url: &Url, insecure: bool, username: Option<&str>, password: Option<&str>) -> Result<Session> {
    let host = url.host_str().ok_or_else(|| Error::InvalidUrl("URL has no host".into()))?;
    let port = url.port_or_known_default().unwrap_or(21);
    let tcp = tokio::time::timeout(Duration::from_secs(20), TcpStream::connect((host, port)))
        .await
        .map_err(|_| Error::Failed("FTP connect timed out".into()))??;
    tcp.set_nodelay(true)?;
    let secure = url.scheme() == "ftps";
    let name = ServerName::try_from(host.to_string()).map_err(|_| Error::InvalidUrl("FTP host is not a DNS name".into()))?;
    let tls = if secure { Some(tls_config(insecure)?) } else { None };
    let mut ctrl = if let Some(config) = &tls {
        // Greeting is read on the plain socket, then AUTH TLS wraps it.
        // Bytes already buffered past that reply are dropped; the server waits for ClientHello.
        let mut plain = Ctrl { io: BufReader::new(FtpStream::Plain(tcp)) };
        expect_greeting(&mut plain).await?;
        let reply = plain.cmd("AUTH TLS").await?;
        if reply.code != 234 {
            return Err(Error::Failed(format!("FTP AUTH TLS {}", reply.code)));
        }
        let FtpStream::Plain(tcp) = plain.io.into_inner() else {
            return Err(Error::Failed("FTP control connection is already encrypted".into()));
        };
        let stream = TlsConnector::from(Arc::clone(config))
            .connect(name.clone(), tcp)
            .await
            .map_err(|err| Error::Failed(format!("FTPS handshake: {err}")))?;
        let mut secured = Ctrl { io: BufReader::new(FtpStream::Tls(stream)) };
        let pbsz = secured.cmd("PBSZ 0").await?;
        if pbsz.code != 200 {
            return Err(Error::Failed(format!("FTP PBSZ {}", pbsz.code)));
        }
        let prot = secured.cmd("PROT P").await?;
        if prot.code != 200 {
            return Err(Error::Failed(format!("FTP PROT {}", prot.code)));
        }
        secured
    } else {
        let mut plain = Ctrl { io: BufReader::new(FtpStream::Plain(tcp)) };
        expect_greeting(&mut plain).await?;
        plain
    };
    let (user, pass) = match (username, password) {
        (Some(user), Some(pass)) => (user.to_string(), pass.to_string()),
        (Some(user), None) => (user.to_string(), String::new()),
        _ => ("anonymous".to_string(), "forge@localhost".to_string()),
    };
    let reply = ctrl.cmd(&format!("USER {user}")).await?;
    if reply.code == 331 {
        let reply = ctrl.cmd(&format!("PASS {pass}")).await?;
        if reply.code == 530 {
            return Err(Error::AuthRequired);
        }
        if reply.code != 230 {
            return Err(Error::Failed(format!("FTP PASS {}", reply.code)));
        }
    } else if reply.code != 230 {
        return Err(Error::Failed(format!("FTP USER {}", reply.code)));
    }
    let typed = ctrl.cmd("TYPE I").await?;
    if typed.code != 200 {
        return Err(Error::Failed(format!("FTP TYPE {}", typed.code)));
    }
    Ok(Session { ctrl, tls, name })
}

async fn expect_greeting<S>(ctrl: &mut Ctrl<S>) -> Result<()>
where
    S: AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let greeting = ctrl.read_reply().await?;
    if greeting.code == 120 {
        let greeting = ctrl.read_reply().await?;
        if greeting.code != 220 {
            return Err(Error::Failed(format!("FTP greeting {}", greeting.code)));
        }
    } else if greeting.code != 220 {
        return Err(Error::Failed(format!("FTP greeting {}", greeting.code)));
    }
    Ok(())
}

fn line_code(line: &str) -> Result<u16> {
    line.get(..3)
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| Error::Failed(format!("FTP reply is not a code: {line}")))
}

pub fn parse_pasv(text: &str) -> Option<SocketAddr> {
    let start = text.rfind('(')?;
    let end = text[start + 1..].find(')')? + start + 1;
    let numbers: Vec<u8> = text[start + 1..end].split(',').map(|part| part.trim().parse().ok()).collect::<Option<_>>()?;
    if numbers.len() != 6 {
        return None;
    }
    let ip = Ipv4Addr::new(numbers[0], numbers[1], numbers[2], numbers[3]);
    let port = u16::from(numbers[4]) * 256 + u16::from(numbers[5]);
    Some(SocketAddr::from((ip, port)))
}

fn tls_config(insecure: bool) -> Result<Arc<ClientConfig>> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let config = if insecure {
        ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAll))
            .with_no_client_auth()
    } else {
        ClientConfig::with_platform_verifier().map_err(|err| Error::Failed(format!("TLS verifier: {err}")))?
    };
    Ok(Arc::new(config))
}

#[derive(Debug)]
struct AcceptAll;

impl ServerCertVerifier for AcceptAll {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ED25519,
            SignatureScheme::RSA_PSS_SHA256,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasv_address_uses_p1_times_256() {
        let addr = parse_pasv("227 Entering Passive Mode (127,0,0,1,4,1).").unwrap();
        assert_eq!(addr, SocketAddr::from((Ipv4Addr::new(127, 0, 0, 1), 4 * 256 + 1)));
    }

    #[test]
    fn multiline_code_is_the_first_three_digits() {
        assert_eq!(line_code("227-extra\r\n").unwrap(), 227);
        assert!(line_code("nope").is_err());
    }
}
