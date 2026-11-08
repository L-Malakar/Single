//! Outbound HTTPS client for talking to peer Single devices.
//!
//! Deliberately hand-rolled on tokio + rustls (no reqwest): the server
//! certificate must be *pinned* to the pairing record rather than validated
//! against public CAs, and the request set is tiny (WebDAV verbs + JSON
//! endpoints). One connection per operation keeps state handling trivial;
//! an internal read buffer keeps leftover body bytes safe between steps.

use crate::identity::Identity;
use crate::xmlutil::{parse_multistatus, FsEntry};
use anyhow::{anyhow, Context, Result};
use rustls::pki_types::ServerName;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

const MAX_HEAD: usize = 64 * 1024;
const MAX_INLINE_BODY: u64 = 32 * 1024 * 1024;

pub struct Conn {
    rd: tokio::io::ReadHalf<tokio_rustls::client::TlsStream<TcpStream>>,
    wr: tokio::io::WriteHalf<tokio_rustls::client::TlsStream<TcpStream>>,
    buf: Vec<u8>,
    /// SHA-256 fingerprint of the peer's CA certificate (last cert in chain)
    pub peer_ca_fp: Option<String>,
}

pub struct RespHead {
    pub status: u16,
    pub headers: HashMap<String, String>,
}

impl RespHead {
    pub fn header(&self, k: &str) -> Option<&str> {
        self.headers.get(&k.to_ascii_lowercase()).map(|s| s.as_str())
    }
    pub fn content_length(&self) -> Option<u64> {
        self.header("content-length").and_then(|v| v.parse().ok())
    }
    pub fn chunked(&self) -> bool {
        self.header("transfer-encoding")
            .map(|v| v.to_ascii_lowercase().contains("chunked"))
            .unwrap_or(false)
    }
    pub fn ok(&self) -> bool {
        matches!(self.status, 200 | 201 | 204 | 206 | 207)
    }
}

/// Open a pinned TLS connection. `pinned` = accepted peer CA fingerprints;
/// `allow_prefix` lets an *unpaired* device be contacted during pairing;
/// `accept_any` is for the manual add-by-IP probe only.
pub async fn connect(
    ip: IpAddr,
    port: u16,
    pinned: &[String],
    allow_prefix: Option<String>,
    accept_any: bool,
    identity: &Identity,
    timeout: Duration,
) -> Result<Conn> {
    let config = identity.client_config(pinned, allow_prefix, accept_any)?;
    let connector = TlsConnector::from(Arc::new(config));
    let addr = std::net::SocketAddr::new(ip, port);

    let tcp = tokio::time::timeout(timeout, TcpStream::connect(addr))
        .await
        .map_err(|_| anyhow!("connect timed out"))?
        .context("tcp connect failed")?;
    tcp.set_nodelay(true).ok();

    let server_name = ServerName::IpAddress(ip.into());
    let tls = tokio::time::timeout(timeout, connector.connect(server_name, tcp))
        .await
        .map_err(|_| anyhow!("tls handshake timed out"))?
        .context("tls handshake failed (device unknown or unpinned)")?;

    let peer_ca_fp = tls
        .get_ref()
        .1
        .peer_certificates()
        .and_then(crate::identity::cert_fp);

    let (rd, wr) = tokio::io::split(tls);
    Ok(Conn {
        rd,
        wr,
        buf: Vec::new(),
        peer_ca_fp,
    })
}

impl Conn {
    async fn fill(&mut self) -> Result<usize> {
        let mut chunk = [0u8; 16 * 1024];
        let n = self.rd.read(&mut chunk).await?;
        if n > 0 {
            self.buf.extend_from_slice(&chunk[..n]);
        }
        Ok(n)
    }

    /// Ensure the buffer contains a full response head; return its length.
    async fn buffered_head(&mut self) -> Result<usize> {
        loop {
            if let Some(pos) = self.buf.windows(4).position(|w| w == b"\r\n\r\n") {
                if pos > MAX_HEAD {
                    return Err(anyhow!("response head too large"));
                }
                return Ok(pos + 4);
            }
            if self.buf.len() > MAX_HEAD {
                return Err(anyhow!("response head too large"));
            }
            if self.fill().await? == 0 {
                return Err(anyhow!("connection closed before response"));
            }
        }
    }

    async fn ensure(&mut self, want: usize) -> Result<()> {
        while self.buf.len() < want {
            if self.fill().await? == 0 {
                return Err(anyhow!("connection closed mid-body"));
            }
        }
        Ok(())
    }

    /// Send a request head (no body).
    pub async fn send_request(
        &mut self,
        method: &str,
        target: &str,
        headers: &[(String, String)],
    ) -> Result<()> {
        let mut head = format!("{} {} HTTP/1.1\r\n", method, target);
        head.push_str("Host: single\r\n");
        head.push_str("Connection: close\r\n");
        for (k, v) in headers {
            head.push_str(&format!("{}: {}\r\n", k, v));
        }
        head.push_str("\r\n");
        self.wr.write_all(head.as_bytes()).await?;
        self.wr.flush().await?;
        Ok(())
    }

    /// Send a request with an in-memory body.
    pub async fn send_request_with_body(
        &mut self,
        method: &str,
        target: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Result<()> {
        let mut all: Vec<(String, String)> =
            vec![("Content-Length".to_string(), body.len().to_string())];
        all.extend(headers.iter().cloned());
        self.send_request(method, target, &all).await?;
        self.wr.write_all(body).await?;
        self.wr.flush().await?;
        Ok(())
    }

    /// Read a response head, skipping interim 1xx responses. Body bytes (if
    /// already buffered) remain in the internal buffer.
    pub async fn read_response(&mut self) -> Result<RespHead> {
        loop {
            let head_len = self.buffered_head().await?;
            let head_bytes: Vec<u8> = self.buf.drain(..head_len).collect();
            let head = String::from_utf8_lossy(&head_bytes).to_string();
            let parsed = crate::httpio::parse_head(&head)?;
            let status = head_status(&head)?;
            if status >= 200 {
                return Ok(RespHead {
                    status,
                    headers: parsed.headers,
                });
            }
            // interim response (e.g. 100 Continue) has no body — loop
        }
    }

    /// Read the entire body (bounded) — used for JSON + PROPFIND.
    pub async fn read_body_bytes(&mut self, rh: &RespHead, max: u64) -> Result<Vec<u8>> {
        if rh.chunked() {
            let mut out = Vec::new();
            loop {
                match self.read_chunked_chunk().await? {
                    Some(c) => {
                        out.extend_from_slice(&c);
                        if out.len() as u64 > max {
                            return Err(anyhow!("body too large"));
                        }
                    }
                    None => break,
                }
            }
            Ok(out)
        } else {
            let len = rh.content_length().unwrap_or(0);
            if len > max {
                return Err(anyhow!("body too large ({} bytes)", len));
            }
            self.ensure(len as usize).await?;
            Ok(self.buf.drain(..len as usize).collect())
        }
    }

    /// Stream the body into a writer with progress callbacks.
    pub async fn stream_body_to<W, F>(
        &mut self,
        rh: &RespHead,
        writer: &mut W,
        mut progress: F,
    ) -> Result<u64>
    where
        W: AsyncWrite + Unpin,
        F: FnMut(u64),
    {
        let mut done: u64 = 0;
        if rh.chunked() {
            loop {
                match self.read_chunked_chunk().await? {
                    Some(c) => {
                        writer.write_all(&c).await?;
                        done += c.len() as u64;
                        progress(done);
                    }
                    None => break,
                }
            }
        } else {
            let mut remaining = rh
                .content_length()
                .unwrap_or(u64::MAX.saturating_sub(done));
            // first drain buffered bytes
            if !self.buf.is_empty() && remaining > 0 {
                let take = std::cmp::min(self.buf.len() as u64, remaining) as usize;
                let chunk: Vec<u8> = self.buf.drain(..take).collect();
                writer.write_all(&chunk).await?;
                done += take as u64;
                remaining -= take as u64;
                progress(done);
            }
            while remaining > 0 {
                let n = self.fill().await?;
                if n == 0 {
                    break;
                }
                let take = std::cmp::min(self.buf.len() as u64, remaining) as usize;
                let chunk: Vec<u8> = self.buf.drain(..take).collect();
                writer.write_all(&chunk).await?;
                done += take as u64;
                remaining -= take as u64;
                progress(done);
            }
        }
        writer.flush().await?;
        Ok(done)
    }

    /// Read one chunked-encoding chunk from the buffer.
    async fn read_chunked_chunk(&mut self) -> Result<Option<Vec<u8>>> {
        // size line
        let line_end = loop {
            if let Some(p) = self.buf.windows(2).position(|w| w == b"\r\n") {
                if p > 32 {
                    return Err(anyhow!("bad chunk header"));
                }
                break p;
            }
            if self.fill().await? == 0 {
                return Err(anyhow!("closed in chunk header"));
            }
        };
        let line: Vec<u8> = self.buf.drain(..line_end + 2).collect();
        let line = String::from_utf8_lossy(&line);
        let size = usize::from_str_radix(
            line.trim().split(';').next().unwrap_or("").trim(),
            16,
        )
        .context("bad chunk size")?;

        if size == 0 {
            // trailers until blank line
            loop {
                if let Some(p) = self.buf.windows(2).position(|w| w == b"\r\n") {
                    if p == 0 {
                        self.buf.drain(..2);
                        break;
                    }
                    self.buf.drain(..p + 2);
                } else if self.fill().await? == 0 {
                    break;
                }
            }
            return Ok(None);
        }

        self.ensure(size + 2).await?;
        let mut data: Vec<u8> = self.buf.drain(..size).collect();
        self.buf.drain(..2); // CRLF
        let _ = &mut data;
        Ok(Some(data))
    }
}

fn head_status(head: &str) -> Result<u16> {
    let line = head.lines().next().unwrap_or("");
    line.split_whitespace()
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or_else(|| anyhow!("bad status line: {}", line))
}

// ------------------------------------------------------------- high level

#[derive(Debug, Clone, serde::Serialize)]
pub struct RemoteIdentity {
    pub id: String,
    pub name: String,
    pub ca_fp: String,
}

pub fn auth_header(token: &str) -> (String, String) {
    ("Authorization".into(), format!("Bearer {}", token))
}

/// GET /single/ping — learn id/name of any Single device.
pub async fn ping(conn: &mut Conn) -> Result<RemoteIdentity> {
    conn.send_request("GET", "/single/ping", &[]).await?;
    let rh = conn.read_response().await?;
    if rh.status != 200 {
        return Err(anyhow!("ping failed with status {}", rh.status));
    }
    let body = conn.read_body_bytes(&rh, 64 * 1024).await?;
    let v: serde_json::Value = serde_json::from_slice(&body).context("bad ping json")?;
    Ok(RemoteIdentity {
        id: v["id"].as_str().unwrap_or("").to_string(),
        name: v["name"].as_str().unwrap_or("Device").to_string(),
        ca_fp: v["fp"].as_str().unwrap_or("").to_string(),
    })
}

pub async fn propfind(conn: &mut Conn, token: &str, path: &str) -> Result<Vec<FsEntry>> {
    let trimmed = path.trim_matches('/');
    let target = if trimmed.is_empty() {
        "/dav/".to_string()
    } else {
        format!("/dav/{}", crate::xmlutil::encode_path(trimmed))
    };
    let headers = vec![
        auth_header(token),
        ("Depth".into(), "1".into()),
        ("Content-Length".into(), "0".into()),
    ];
    conn.send_request("PROPFIND", &target, &headers).await?;
    let rh = conn.read_response().await?;
    if rh.status != 207 {
        let _ = conn.read_body_bytes(&rh, 16 * 1024).await;
        return Err(anyhow!("PROPFIND failed with status {}", rh.status));
    }
    let body = conn.read_body_bytes(&rh, MAX_INLINE_BODY).await?;
    let xml = String::from_utf8_lossy(&body).to_string();
    let base = if trimmed.is_empty() {
        "/dav".to_string()
    } else {
        format!("/dav/{}", trimmed)
    };
    Ok(parse_multistatus(&xml, &base))
}

pub async fn download_to<W, F>(
    conn: &mut Conn,
    token: &str,
    path: &str,
    writer: &mut W,
    progress: F,
) -> Result<u64>
where
    W: AsyncWrite + Unpin,
    F: FnMut(u64),
{
    let target = format!("/dav/{}", crate::xmlutil::encode_path(path));
    let headers = vec![auth_header(token)];
    conn.send_request("GET", &target, &headers).await?;
    let rh = conn.read_response().await?;
    if rh.status != 200 && rh.status != 206 {
        return Err(anyhow!("download failed with status {}", rh.status));
    }
    conn.stream_body_to(&rh, writer, progress).await
}

pub async fn upload_from<R, F>(
    conn: &mut Conn,
    token: &str,
    remote_path: &str,
    reader: &mut R,
    mut len: u64,
    mut progress: F,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    F: FnMut(u64),
{
    let target = format!("/dav/{}", crate::xmlutil::encode_path(remote_path));
    let head = format!(
        "PUT {} HTTP/1.1\r\nHost: single\r\nConnection: close\r\nAuthorization: Bearer {}\r\nContent-Length: {}\r\n\r\n",
        target, token, len
    );
    conn.wr.write_all(head.as_bytes()).await?;

    let mut done: u64 = 0;
    let mut buf = vec![0u8; 128 * 1024];
    while len > 0 {
        let want = std::cmp::min(buf.len() as u64, len) as usize;
        let n = reader.read(&mut buf[..want]).await?;
        if n == 0 {
            return Err(anyhow!("local file ended before upload finished"));
        }
        conn.wr.write_all(&buf[..n]).await?;
        len -= n as u64;
        done += n as u64;
        progress(done);
    }
    conn.wr.flush().await?;

    let rh = conn.read_response().await?;
    if !rh.ok() {
        return Err(anyhow!("upload failed with status {}", rh.status));
    }
    Ok(())
}

pub async fn mkdir(conn: &mut Conn, token: &str, path: &str) -> Result<()> {
    let target = format!("/dav/{}", crate::xmlutil::encode_path(path));
    conn.send_request("MKCOL", &target, &[auth_header(token)])
        .await?;
    let rh = conn.read_response().await?;
    if !matches!(rh.status, 200 | 201 | 204) {
        return Err(anyhow!(
            "mkdir failed ({}): folder may already exist",
            rh.status
        ));
    }
    Ok(())
}

pub async fn delete(conn: &mut Conn, token: &str, path: &str) -> Result<()> {
    let target = format!("/dav/{}", crate::xmlutil::encode_path(path));
    conn.send_request("DELETE", &target, &[auth_header(token)])
        .await?;
    let rh = conn.read_response().await?;
    if !matches!(rh.status, 200 | 204) {
        return Err(anyhow!("delete failed with status {}", rh.status));
    }
    Ok(())
}

pub async fn rename(conn: &mut Conn, token: &str, path: &str, dest: &str) -> Result<()> {
    let target = format!("/dav/{}", crate::xmlutil::encode_path(path));
    let headers = vec![
        auth_header(token),
        (
            "Destination".into(),
            format!("https://single/dav/{}", crate::xmlutil::encode_path(dest)),
        ),
        ("Overwrite".into(), "F".into()),
    ];
    conn.send_request("MOVE", &target, &headers).await?;
    let rh = conn.read_response().await?;
    if !matches!(rh.status, 201 | 204) {
        return Err(anyhow!("rename failed with status {}", rh.status));
    }
    Ok(())
}

/// POST a JSON payload to a pairing endpoint and return (status, body).
pub async fn post_json(
    conn: &mut Conn,
    path: &str,
    json: &serde_json::Value,
    token: Option<&str>,
) -> Result<(u16, serde_json::Value)> {
    let body = serde_json::to_vec(json)?;
    let mut headers = vec![("Content-Type".into(), "application/json".to_string())];
    if let Some(t) = token {
        headers.push(auth_header(t));
    }
    conn.send_request_with_body("POST", path, &headers, &body)
        .await?;
    let rh = conn.read_response().await?;
    let body = conn.read_body_bytes(&rh, 1024 * 1024).await?;
    let v = serde_json::from_slice(&body).unwrap_or(serde_json::json!({}));
    Ok((rh.status, v))
}
