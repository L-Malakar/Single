//! Minimal HTTP/1.1 plumbing over Tokio (no hyper): request head parsing,
//! body readers (Content-Length + chunked), response writer with file
//! streaming and Range support helpers.

use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_HEAD_BYTES: usize = 64 * 1024;
/// Cap for in-memory bodies (pair endpoints, small requests).
pub const MAX_INLINE_BODY: u64 = 8 * 1024 * 1024;

#[derive(Debug)]
pub struct RequestHead {
    pub method: String,
    /// raw request target (e.g. `/dav/My%20File.txt`)
    pub target: String,
    /// percent-decoded path (no query)
    pub path: String,
    /// decoded query string (may be empty)
    pub query: String,
    pub version_11: bool,
    pub headers: HashMap<String, String>,
}

impl RequestHead {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(&name.to_ascii_lowercase()).map(|s| s.as_str())
    }
    pub fn wants_keep_alive(&self) -> bool {
        match self.header("connection") {
            Some(c) if c.eq_ignore_ascii_case("close") => false,
            Some(c) if c.eq_ignore_ascii_case("keep-alive") => true,
            _ => self.version_11,
        }
    }
    pub fn content_length(&self) -> Option<u64> {
        self.header("content-length").and_then(|v| v.trim().parse().ok())
    }
    pub fn is_chunked(&self) -> bool {
        self.header("transfer-encoding")
            .map(|v| v.to_ascii_lowercase().contains("chunked"))
            .unwrap_or(false)
    }
}

/// Wraps "bytes already read past the head" + the live stream so every body
/// reader sees one continuous stream.
pub struct BodyReader<R> {
    buf: std::io::Cursor<Vec<u8>>,
    inner: R,
}

impl<R> BodyReader<R> {
    pub fn new(leftover: Vec<u8>, inner: R) -> Self {
        BodyReader {
            buf: std::io::Cursor::new(leftover),
            inner,
        }
    }
    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for BodyReader<R> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        out: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = &mut *self;
        if (this.buf.position() as usize) < this.buf.get_ref().len() {
            let cap = out.remaining().max(1);
            let mut tmp = vec![0u8; cap.min(64 * 1024)];
            let n = std::io::Read::read(&mut this.buf, &mut tmp)?;
            out.put_slice(&tmp[..n]);
            return std::task::Poll::Ready(Ok(()));
        }
        std::pin::Pin::new(&mut this.inner).poll_read(cx, out)
    }
}

pub enum ReadOutcome {
    /// A parsed request plus any body bytes already buffered past the head.
    Head(RequestHead, Vec<u8>),
    /// The peer closed the connection cleanly between requests.
    Closed,
}

/// Read a request head; if the client sent `Expect: 100-continue`, the
/// interim response is written immediately (required by the Windows
/// WebClient redirector for larger PUTs).
pub async fn read_head<R, W>(reader: &mut R, writer: &mut W) -> Result<ReadOutcome>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 2048];
    let head_end = loop {
        if let Some(pos) = find_head_end(&buf) {
            break pos;
        }
        if buf.len() > MAX_HEAD_BYTES {
            return Err(anyhow!("request head too large"));
        }
        let n = reader.read(&mut chunk).await?;
        if n == 0 {
            if buf.is_empty() {
                return Ok(ReadOutcome::Closed);
            }
            return Err(anyhow!("connection closed mid-request"));
        }
        buf.extend_from_slice(&chunk[..n]);
    };

    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let leftover = buf[head_end + 4..].to_vec();
    let req = parse_head(&head)?;

    if let Some(exp) = req.header("expect") {
        if exp.eq_ignore_ascii_case("100-continue") {
            writer
                .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
                .await
                .ok();
            writer.flush().await.ok();
        }
    }

    Ok(ReadOutcome::Head(req, leftover))
}

fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

pub fn parse_head(head: &str) -> Result<RequestHead> {
    let mut lines = head.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| anyhow!("empty request"))?
        .trim()
        .to_string();
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| anyhow!("missing method"))?
        .to_ascii_uppercase();
    let target = parts
        .next()
        .ok_or_else(|| anyhow!("missing target"))?
        .to_string();
    let version = parts.next().unwrap_or("HTTP/1.1");
    let version_11 = version.eq_ignore_ascii_case("HTTP/1.1");

    let mut headers = HashMap::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }

    let (path_raw, query) = match target.split_once('?') {
        Some((p, q)) => (p, q.to_string()),
        None => (target.as_str(), String::new()),
    };
    let path = percent_encoding::percent_decode_str(path_raw)
        .decode_utf8_lossy()
        .to_string();

    Ok(RequestHead {
        method,
        target,
        path,
        query,
        version_11,
        headers,
    })
}

/// Read the whole body into memory (bounded). Any bytes already buffered by
/// the caller should be wrapped in a `BodyReader` first.
pub async fn read_body_inline<R>(reader: &mut R, head: &RequestHead) -> Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut out = Vec::new();
    if head.is_chunked() {
        loop {
            let chunk = read_chunk(reader).await?;
            match chunk {
                Some(data) => {
                    out.extend_from_slice(&data);
                    if out.len() as u64 > MAX_INLINE_BODY {
                        return Err(anyhow!("inline body too large"));
                    }
                }
                None => break,
            }
        }
        Ok(out)
    } else {
        let len = head.content_length().unwrap_or(0);
        if len > MAX_INLINE_BODY {
            return Err(anyhow!("inline body too large"));
        }
        // out may already contain part of the body
        while (out.len() as u64) < len {
            let want = std::cmp::min(8192, (len as usize) - out.len());
            let mut buf = vec![0u8; want];
            let n = reader.read(&mut buf).await?;
            if n == 0 {
                return Err(anyhow!("connection closed mid-body"));
            }
            out.extend_from_slice(&buf[..n]);
        }
        out.truncate(len as usize);
        Ok(out)
    }
}

/// Read one chunked-transfer chunk. Returns None at terminal chunk.
pub async fn read_chunk<R>(reader: &mut R) -> Result<Option<Vec<u8>>>
where
    R: AsyncRead + Unpin,
{
    // read size line
    let mut line = Vec::new();
    loop {
        let mut b = [0u8; 1];
        let n = reader.read(&mut b).await?;
        if n == 0 {
            return Err(anyhow!("closed in chunk header"));
        }
        if b[0] == b'\n' {
            break;
        }
        if b[0] != b'\r' {
            line.push(b[0]);
        }
        if line.len() > 32 {
            return Err(anyhow!("chunk size line too long"));
        }
    }
    let size_str = String::from_utf8_lossy(&line);
    let size_hex = size_str.split(';').next().unwrap_or("").trim();
    let size = usize::from_str_radix(size_hex, 16).context("bad chunk size")?;
    if size == 0 {
        // terminal chunk: consume trailer lines (if any) until an empty
        // line. The old code could stop one CRLF short, leaving stray bytes
        // in the stream and desyncing the next request on a keep-alive
        // connection.
        let mut trailing = Vec::new();
        loop {
            let mut b = [0u8; 1];
            let n = reader.read(&mut b).await?;
            if n == 0 {
                return Err(anyhow!("closed in trailers"));
            }
            trailing.push(b[0]);
            if b[0] == b'\n' {
                // a line that was only \r\n (or \n) ends the trailer block
                if trailing.len() <= 2 {
                    break;
                }
                trailing.clear();
            }
            if trailing.len() > 8192 {
                return Err(anyhow!("trailers too large"));
            }
        }
        return Ok(None);
    }
    let mut data = vec![0u8; size];
    reader.read_exact(&mut data).await?;
    let mut crlf = [0u8; 2];
    reader.read_exact(&mut crlf).await?;
    Ok(Some(data))
}

/// Copy exactly `len` bytes from `reader` into `writer`, reporting progress.
pub async fn copy_exact<R, W, F>(
    reader: &mut R,
    writer: &mut W,
    mut len: u64,
    mut progress: F,
) -> Result<u64>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    F: FnMut(u64),
{
    let mut copied: u64 = 0;
    let mut buf = vec![0u8; 128 * 1024];
    while len > 0 {
        let want = std::cmp::min(buf.len() as u64, len) as usize;
        let n = reader.read(&mut buf[..want]).await?;
        if n == 0 {
            return Err(anyhow!("connection closed mid-transfer"));
        }
        writer.write_all(&buf[..n]).await?;
        len -= n as u64;
        copied += n as u64;
        progress(copied);
    }
    Ok(copied)
}

/// A response body: either bytes or a file range to stream.
pub enum Body {
    Empty,
    Bytes(Vec<u8>),
    File {
        file: tokio::fs::File,
        offset: u64,
        len: u64,
    },
}

pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Body,
}

impl Response {
    pub fn new(status: u16) -> Self {
        Response {
            status,
            headers: Vec::new(),
            body: Body::Empty,
        }
    }
    pub fn header(mut self, k: &str, v: &str) -> Self {
        self.headers.push((k.to_string(), v.to_string()));
        self
    }
    pub fn text(status: u16, body: &str) -> Self {
        Response::new(status).body_bytes(body.as_bytes().to_vec()).header(
            "Content-Type",
            "text/plain; charset=utf-8",
        )
    }
    pub fn json(status: u16, body: String) -> Self {
        Response::new(status)
            .body_bytes(body.into_bytes())
            .header("Content-Type", "application/json; charset=utf-8")
    }
    pub fn html(status: u16, body: String) -> Self {
        Response::new(status)
            .body_bytes(body.into_bytes())
            .header("Content-Type", "text/html; charset=utf-8")
    }
    pub fn xml(status: u16, body: String) -> Self {
        Response::new(status)
            .body_bytes(body.into_bytes())
            .header("Content-Type", "application/xml; charset=utf-8")
    }
    pub fn body_bytes(mut self, b: Vec<u8>) -> Self {
        self.body = Body::Bytes(b);
        self
    }
}

pub fn status_text(code: u16) -> &'static str {
    match code {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        206 => "Partial Content",
        207 => "Multi-Status",
        301 => "Moved Permanently",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        412 => "Precondition Failed",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        416 => "Range Not Satisfiable",
        422 => "Unprocessable Entity",
        423 => "Locked",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        507 => "Insufficient Storage",
        _ => "Unknown",
    }
}

/// Write a response. Handles HEAD (no body), Content-Length and file
/// streaming. `include_body` is false for HEAD requests.
pub async fn write_response<W>(
    writer: &mut W,
    resp: &mut Response,
    include_body: bool,
    keep_alive: bool,
) -> Result<u64>
where
    W: AsyncWrite + Unpin,
{
    let mut head = format!("HTTP/1.1 {} {}\r\n", resp.status, status_text(resp.status));
    let mut has_content_length = false;
    for (k, v) in &resp.headers {
        if k.eq_ignore_ascii_case("content-length") {
            has_content_length = true;
        }
        head.push_str(&format!("{}: {}\r\n", k, v));
    }

    let _body_len = match &resp.body {
        Body::Empty => {
            if !has_content_length && resp.status != 204 && resp.status != 304 {
                head.push_str("Content-Length: 0\r\n");
            }
            0
        }
        Body::Bytes(b) => {
            if !has_content_length {
                head.push_str(&format!("Content-Length: {}\r\n", b.len()));
            }
            b.len() as u64
        }
        Body::File { len, .. } => {
            if !has_content_length {
                head.push_str(&format!("Content-Length: {}\r\n", len));
            }
            *len
        }
    };

    head.push_str(&format!(
        "Connection: {}\r\n\r\n",
        if keep_alive { "keep-alive" } else { "close" }
    ));
    writer.write_all(head.as_bytes()).await?;

    let mut sent: u64 = 0;
    if include_body {
        match &mut resp.body {
            Body::Empty => {}
            Body::Bytes(b) => {
                writer.write_all(b).await?;
                sent = b.len() as u64;
            }
            Body::File { file, offset, len } => {
                use tokio::io::AsyncSeekExt;
                if *offset > 0 {
                    file.seek(std::io::SeekFrom::Start(*offset)).await?;
                }
                let mut buf = vec![0u8; 128 * 1024];
                let mut remaining = *len;
                while remaining > 0 {
                    let want = std::cmp::min(buf.len() as u64, remaining) as usize;
                    let n = file.read(&mut buf[..want]).await?;
                    if n == 0 {
                        break;
                    }
                    writer.write_all(&buf[..n]).await?;
                    remaining -= n as u64;
                    sent += n as u64;
                }
            }
        }
    }
    writer.flush().await?;
    Ok(sent)
}

/// Parse a single-range `Range: bytes=...` header.
/// Returns (start, end) inclusive, or None for unsatisfiable/multi ranges.
pub fn parse_range(header: &str, file_len: u64) -> Option<(u64, u64)> {
    let spec = header.trim().strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None; // multi-range: serve full content instead
    }
    let spec = spec.trim();
    if let Some(suffix) = spec.strip_prefix('-') {
        let n: u64 = suffix.trim().parse().ok()?;
        if n == 0 || file_len == 0 {
            return None;
        }
        let start = file_len.saturating_sub(n);
        return Some((start, file_len - 1));
    }
    let (start_s, end_s) = spec.split_once('-')?;
    let start: u64 = start_s.trim().parse().ok()?;
    let end: u64 = if end_s.trim().is_empty() {
        file_len.saturating_sub(1)
    } else {
        end_s.trim().parse().ok()?
    };
    if file_len == 0 || start >= file_len || end < start {
        return None;
    }
    Some((start, end.min(file_len - 1)))
}

/// Guess a Content-Type from a file extension (small, useful map).
pub fn content_type_for(path: &std::path::Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",
        "mp4" | "m4v" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "avi" => "video/x-msvideo",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "flac" => "audio/flac",
        "m4a" => "audio/mp4",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "txt" | "md" | "log" => "text/plain; charset=utf-8",
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "xml" => "application/xml",
        "doc" | "docx" => "application/msword",
        "apk" => "application/vnd.android.package-archive",
        "exe" | "msi" => "application/x-msdownload",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn parses_head_with_body_and_leftover() {
        let raw = b"PUT /dav/a.txt HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\n\r\nhello";
        let mut r = std::io::Cursor::new(raw.to_vec());
        let mut w = tokio::io::sink();
        let out = read_head(&mut r, &mut w).await.unwrap();
        let (head, leftover) = match out {
            ReadOutcome::Head(h, l) => (h, l),
            _ => panic!("expected head"),
        };
        assert_eq!(head.method, "PUT");
        assert_eq!(head.path, "/dav/a.txt");
        let mut br = BodyReader::new(leftover, r);
        let body = read_body_inline(&mut br, &head).await.unwrap();
        assert_eq!(body, b"hello");
    }

    #[tokio::test]
    async fn reads_chunked_body() {
        let raw = b"4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n";
        let mut r = std::io::Cursor::new(raw.to_vec());
        let mut headers = HashMap::new();
        headers.insert("transfer-encoding".into(), "chunked".into());
        let head = RequestHead {
            method: "PUT".into(),
            target: "/x".into(),
            path: "/x".into(),
            query: String::new(),
            version_11: true,
            headers,
        };
        let mut br = BodyReader::new(raw.to_vec(), r);
        let body = read_body_inline(&mut br, &head).await.unwrap();
        assert_eq!(body, b"Wikipedia");
    }

    #[tokio::test]
    async fn sends_100_continue() {
        let raw = b"PUT /dav/a.txt HTTP/1.1\r\nExpect: 100-continue\r\nContent-Length: 2\r\n\r\nhi";
        let mut r = std::io::Cursor::new(raw.to_vec());
        let mut w: Vec<u8> = Vec::new();
        let out = read_head(&mut r, &mut w).await.unwrap();
        assert!(matches!(out, ReadOutcome::Head(_, _)));
        assert!(w.starts_with(b"HTTP/1.1 100 Continue"));
    }

    #[test]
    fn range_parsing() {
        assert_eq!(parse_range("bytes=0-4", 100), Some((0, 4)));
        assert_eq!(parse_range("bytes=10-", 100), Some((10, 99)));
        assert_eq!(parse_range("bytes=-10", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=95-200", 100), Some((95, 99)));
        assert_eq!(parse_range("bytes=0-1,5-6", 100), None);
        assert_eq!(parse_range("bytes=100-", 100), None);
        assert_eq!(parse_range("bytes=-10", 5), Some((0, 4)));
    }

    #[test]
    fn keep_alive_defaults() {
        let h = parse_head("GET / HTTP/1.1\r\nHost: a\r\n\r\n").unwrap();
        assert!(h.wants_keep_alive());
        let h0 = parse_head("GET / HTTP/1.0\r\n\r\n").unwrap();
        assert!(!h0.wants_keep_alive());
    }
}
