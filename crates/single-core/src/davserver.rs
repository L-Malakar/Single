//! The encrypted file server.
//!
//! Everything runs on ONE TLS port (default 27500):
//!
//! * `/dav/<path>`   — a WebDAV (class 1+2 subset) view of the shared folder.
//!   Authenticated with the pairing token (Basic `single:<token>` or
//!   `Bearer <token>`). This is what Windows maps as a network drive, what
//!   other apps can mount, and what the Single app itself talks to.
//! * `/single/pair/*` — the PIN + approval pairing JSON API (no token yet;
//!   requests are bound to the caller's TLS client certificate).
//! * `/single/ping`   — heartbeat used for online/offline status.
//! * `/`              — tiny landing page.

use crate::httpio::{
    self, parse_range, read_body_inline, read_chunk, write_response, Body, ReadOutcome,
    RequestHead, Response,
};
use crate::identity::Identity;
use crate::pairing::{PairManager, ReqState};
use crate::xmlutil::{escape, multistatus, FsEntry};
use anyhow::{anyhow, Result};
use base64::Engine;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio_rustls::TlsAcceptor;

const DAV_PREFIX: &str = "/dav";
const LOCK_TTL_SECS: u64 = 3600;

pub struct DavServer {
    pub share_root: PathBuf,
    /// canonicalized share root (once, at construction). On Windows
    /// `canonicalize()` returns verbatim `\\?\C:\...` paths, so every
    /// containment check must compare like-for-like against this — comparing
    /// against the raw `share_root` used to reject *every* write with
    /// "path escapes share root".
    root_canon: PathBuf,
    pub identity: Arc<Identity>,
    pub pairs: Arc<PairManager>,
    locks: Mutex<HashMap<String, Lock>>,
    requests_counter: AtomicU64,
}

#[derive(Clone)]
struct Lock {
    token: String,
    expires: u64,
}

impl DavServer {
    pub fn new(share_root: PathBuf, identity: Arc<Identity>, pairs: Arc<PairManager>) -> Arc<Self> {
        let root_canon = std::fs::canonicalize(&share_root).unwrap_or(share_root.clone());
        Arc::new(DavServer {
            share_root,
            root_canon,
            identity,
            pairs,
            locks: Mutex::new(HashMap::new()),
            requests_counter: AtomicU64::new(0),
        })
    }

    /// Accept loop. Exits when `shutdown` flips to true.
    pub async fn serve(
        self: Arc<Self>,
        listener: TcpListener,
        tls: TlsAcceptor,
        mut shutdown: watch::Receiver<bool>,
    ) {
        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    if *shutdown.borrow() { break; }
                }
                accepted = listener.accept() => {
                    let (stream, _addr) = match accepted {
                        Ok(x) => x,
                        // transient errors (EMFILE, a disappearing Wi-Fi
                        // interface) must not turn into a busy loop
                        Err(_) => {
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            continue;
                        }
                    };
                    let this = self.clone();
                    let tls = tls.clone();
                    tokio::spawn(async move {
                        let _ = this.handle_connection(stream, tls).await;
                    });
                }
            }
        }
    }

    async fn handle_connection(self: Arc<Self>, stream: TcpStream, tls: TlsAcceptor) -> Result<()> {
        stream.set_nodelay(true).ok();
        let tls_stream = match tokio::time::timeout(Duration::from_secs(10), tls.accept(stream)).await
        {
            Ok(Ok(s)) => s,
            _ => return Ok(()), // handshake failed / timeout — drop silently
        };

        // Read the caller's certificate chain (Single peers present one).
        let caller_fp = peer_ca_fp(&tls_stream);

        let (mut rd, mut wr) = tokio::io::split(tls_stream);
        loop {
            let outcome = match tokio::time::timeout(
                Duration::from_secs(75),
                httpio::read_head(&mut rd, &mut wr),
            )
            .await
            {
                Ok(Ok(o)) => o,
                _ => break, // timeout or read error
            };
            let (head, leftover) = match outcome {
                ReadOutcome::Closed => break,
                ReadOutcome::Head(h, l) => (h, l),
            };

            let is_head = head.method == "HEAD";
            let mut resp = self
                .route(&head, leftover, &mut rd, caller_fp.clone())
                .await
                .unwrap_or_else(|e| Response::text(500, &format!("error: {}", e)));

            // Never leak a stack-trace-ish body on 204/304
            let keep = head.wants_keep_alive();
            write_response(&mut wr, &mut resp, !is_head, keep).await?;
            if !keep {
                break;
            }
        }
        Ok(())
    }

    async fn route<R>(
        &self,
        head: &RequestHead,
        leftover: Vec<u8>,
        rd: &mut R,
        caller_fp: Option<String>,
    ) -> Result<Response>
    where
        R: AsyncRead + Unpin,
    {
        let mut body = httpio::BodyReader::new(leftover, rd);
        self.route_with(head, &mut body, caller_fp).await
    }

    async fn route_with<B>(
        &self,
        head: &RequestHead,
        body: &mut B,
        caller_fp: Option<String>,
    ) -> Result<Response>
    where
        B: AsyncRead + Unpin,
    {
        self.requests_counter.fetch_add(1, Ordering::Relaxed);
        let path = head.path.clone();

        // Global OPTIONS (WebClient preflight) — no auth.
        if head.method == "OPTIONS" {
            return Ok(options_response());
        }

        // Pairing API — no token auth, but bound to the TLS client cert.
        if path == "/single/pair/request"
            || path == "/single/pair/check"
            || path == "/single/pair/verify"
        {
            if head.method != "POST" {
                return Ok(Response::text(405, "POST only"));
            }
            let payload = read_body_inline(body, head).await?;
            let fp = caller_fp.ok_or_else(|| anyhow!("client certificate required"))?;
            return self.pair_endpoint(&path, &payload, &fp).await;
        }

        if path == "/single/pair/unpair" {
            if head.method != "POST" {
                return Ok(Response::text(405, "POST only"));
            }
            let payload = read_body_inline(body, head).await?;
            let token = self.bearer_token(head);
            return self.unpair_endpoint(&payload, token.as_deref()).await;
        }

        // Landing page.
        if path == "/" && (head.method == "GET" || head.method == "HEAD") {
            return Ok(Response::html(
                200,
                landing_page(&self.identity),
            ));
        }

        if path == "/single/ping" {
            // deliberately unauthenticated: id/name are already broadcast by
            // mDNS + beacons; file access still requires the pairing token
            return Ok(Response::json(
                200,
                serde_json::json!({
                    "ok": true,
                    "id": self.identity.device_id,
                    "name": self.identity.device_name,
                    "fp": self.identity.ca_fp,
                    "ver": crate::config::APP_VERSION,
                })
                .to_string(),
            ));
        }

        // Everything below is WebDAV and requires a valid pairing token.
        if !path.starts_with(DAV_PREFIX) {
            return Ok(Response::text(404, "not found"));
        }
        match self.authorize(head).await {
            Some(_) => {}
            None => {
                return Ok(Response::text(401, "unauthorized")
                    .header("WWW-Authenticate", "Basic realm=\"Single\""))
            }
        }

        let rel = match dav_rel_path(&path) {
            Some(r) => r,
            None => return Ok(Response::text(400, "bad path")),
        };

        match head.method.as_str() {
            "GET" | "HEAD" => self.handle_get(head, &rel).await,
            "PUT" => self.handle_put(head, body, &rel).await,
            "PROPFIND" => {
                let _ = read_body_inline(body, head).await; // ignored (allprop)
                self.handle_propfind(&rel).await
            }
            "MKCOL" => self.handle_mkcol(head, &rel).await,
            "DELETE" => self.handle_delete(head, &rel).await,
            "MOVE" => self.handle_move_copy(head, &rel, true).await,
            "COPY" => self.handle_move_copy(head, &rel, false).await,
            "PROPPATCH" => {
                let _ = read_body_inline(body, head).await;
                Ok(Response::xml(
                    207,
                    format!(
                        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<D:multistatus xmlns:D=\"DAV:\"><D:response><D:href>{}</D:href><D:propstat><D:prop/><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response></D:multistatus>",
                        escape(&head.path)
                    ),
                ))
            }
            "LOCK" => {
                let _ = read_body_inline(body, head).await;
                self.handle_lock(&rel, head)
            }
            "UNLOCK" => self.handle_unlock(&rel, head),
            _ => Ok(Response::text(405, "method not allowed")
                .header("Allow", ALLOW_METHODS)),
        }
    }

    fn bearer_token(&self, head: &RequestHead) -> Option<String> {
        let auth = head.header("authorization")?;
        if let Some(tok) = auth.strip_prefix("Bearer ").or(auth.strip_prefix("bearer ")) {
            return Some(tok.trim().to_string());
        }
        if let Some(b64) = auth.strip_prefix("Basic ").or(auth.strip_prefix("basic ")) {
            let raw = base64::engine::general_purpose::STANDARD
                .decode(b64.trim())
                .ok()?;
            let cred = String::from_utf8_lossy(&raw);
            let cred = cred.trim_end_matches(':'); // user may be empty
            let (_, pass) = match cred.split_once(':') {
                Some(x) => x,
                None => ("", cred),
            };
            return Some(pass.to_string());
        }
        None
    }

    async fn authorize(&self, head: &RequestHead) -> Option<crate::pairing::Pairing> {
        let token = self.bearer_token(head)?;
        self.pairs.pairing_by_token(&token).await
    }

    // ------------------------------------------------------------------ GET

    async fn handle_get(&self, head: &RequestHead, rel: &str) -> Result<Response> {
        let full = self.resolve_read(rel)?;
        let meta = match tokio::fs::metadata(&full).await {
            Ok(m) => m,
            Err(_) => return Ok(Response::text(404, "not found")),
        };

        if meta.is_dir() {
            if head.method == "HEAD" {
                return Ok(Response::new(200));
            }
            let html = self.dir_listing(&full, rel).await;
            return Ok(Response::html(200, html));
        }

        let size = meta.len();
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let etag = format!("\"{:x}-{:x}\"", mtime, size);

        if let Some(inm) = head.header("if-none-match") {
            if inm.split(',').any(|t| t.trim() == etag) {
                return Ok(Response::new(304).header("ETag", &etag));
            }
        }

        let last_mod = httpdate::fmt_http_date(
            std::time::UNIX_EPOCH + std::time::Duration::from_secs(mtime),
        );
        let ctype = httpio::content_type_for(Path::new(rel));

        // Range?
        if let Some(rng) = head.header("range").and_then(|h| parse_range(h, size)) {
            let (start, end) = rng;
            let len = end - start + 1;
            let file = match tokio::fs::File::open(&full).await {
                Ok(f) => f,
                Err(_) => return Ok(Response::text(404, "not found")),
            };
            return Ok(Response::new(206)
                .header("Content-Range", &format!("bytes {}-{}/{}", start, end, size))
                .header("Accept-Ranges", "bytes")
                .header("ETag", &etag)
                .header("Last-Modified", &last_mod)
                .header("Content-Type", ctype)
                .body_file(file, start, len));
        }

        let file = match tokio::fs::File::open(&full).await {
            Ok(f) => f,
            Err(_) => return Ok(Response::text(404, "not found")),
        };
        Ok(Response::new(200)
            .header("Accept-Ranges", "bytes")
            .header("ETag", &etag)
            .header("Last-Modified", &last_mod)
            .header("Content-Type", ctype)
            .body_file(file, 0, size))
    }

    async fn dir_listing(&self, dir: &Path, rel: &str) -> String {
        let mut rows = String::new();
        let mut entries: Vec<FsEntry> = Vec::new();
        if let Ok(mut rd) = tokio::fs::read_dir(dir).await {
            while let Ok(Some(e)) = rd.next_entry().await {
                let name = e.file_name().to_string_lossy().to_string();
                let ok = e.metadata().await.ok();
                if let Some(md) = ok {
                    let mtime = md
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    entries.push(FsEntry {
                        name: name.clone(),
                        path: name,
                        is_dir: md.is_dir(),
                        size: if md.is_dir() { 0 } else { md.len() },
                        mtime,
                    });
                }
            }
        }
        entries.sort_by(|a, b| {
            b.is_dir.cmp(&a.is_dir).then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        for e in &entries {
            let href = crate::xmlutil::encode_path(&format!(
                "{}/{}",
                rel.trim_end_matches('/'),
                e.path
            ));
            let href = if e.is_dir {
                format!("{}/", href)
            } else {
                href
            };
            let size = if e.is_dir {
                "—".to_string()
            } else {
                crate::pairing::human_size(e.size)
            };
            rows.push_str(&format!(
                "<tr><td>{}</td><td><a href=\"{}\">{}</a></td><td class=\"s\">{}</td></tr>\n",
                if e.is_dir { "📁" } else { "📄" },
                escape(&href),
                escape(&e.name),
                size
            ));
        }
        format!(
            "<!doctype html><html><head><meta charset=\"utf-8\"><title>Single — {}</title>\
             <style>body{{font-family:system-ui,sans-serif;background:#12141a;color:#e8eaf0;max-width:860px;margin:40px auto;padding:0 16px}}\
             table{{width:100%;border-collapse:collapse}}td{{padding:8px 6px;border-bottom:1px solid #262a35}}\
             a{{color:#39d2c0;text-decoration:none}}a:hover{{text-decoration:underline}}\
             .s{{color:#8b93a7;text-align:right;white-space:nowrap}}\
             h1{{font-weight:600}}.m{{color:#8b93a7;font-size:13px}}</style></head><body>\
             <h1>{} — shared folder</h1><p class=\"m\">Served by Single v{} · device {}</p>\
             <table>{}</table></body></html>",
            escape(rel),
            escape(&self.identity.device_name),
            crate::config::APP_VERSION,
            &self.identity.device_id[..8],
            rows
        )
    }

    // ----------------------------------------------------------------- PUT

    async fn handle_put<B>(
        &self,
        head: &RequestHead,
        rd: &mut B,
        rel: &str,
    ) -> Result<Response>
    where
        B: AsyncRead + Unpin,
    {
        if rel.is_empty() {
            return Ok(Response::text(400, "bad path"));
        }
        if let Some(r) = self.check_lock(rel, Some(head)) {
            return Ok(r);
        }
        let full = match self.resolve_write(rel).await {
            Ok(p) => p,
            Err(e) => return Ok(Response::text(403, &format!("forbidden: {}", e))),
        };
        if full.is_dir() {
            return Ok(Response::text(409, "target is a directory"));
        }
        let existed = full.exists();
        if let Some(parent) = full.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let tmp = full.with_extension(format!(
            "single-partial-{}",
            self.requests_counter.fetch_add(1, Ordering::Relaxed)
        ));
        {
            let mut out = tokio::fs::File::create(&tmp).await?;
            if head.is_chunked() {
                loop {
                    match read_chunk(rd).await? {
                        Some(chunk) => out.write_all(&chunk).await?,
                        None => break,
                    }
                }
            } else {
                // Content-Length: copy exactly N bytes (leftover bytes are
                // already transparently part of the BodyReader)
                let len = head.content_length().unwrap_or(0);
                let mut buf = vec![0u8; 128 * 1024];
                let mut remaining = len;
                while remaining > 0 {
                    let want = std::cmp::min(buf.len() as u64, remaining) as usize;
                    let n = rd.read(&mut buf[..want]).await?;
                    if n == 0 {
                        let _ = tokio::fs::remove_file(&tmp).await;
                        return Ok(Response::text(400, "connection closed mid-upload"));
                    }
                    out.write_all(&buf[..n]).await?;
                    remaining -= n as u64;
                }
            }
            out.flush().await?;
        }
        tokio::fs::rename(&tmp, &full).await?;
        Ok(Response::new(if existed { 204 } else { 201 }))
    }

    // ------------------------------------------------------------- PROPFIND

    async fn handle_propfind(&self, rel: &str) -> Result<Response> {
        let full = match self.resolve_read(rel) {
            Ok(p) => p,
            Err(_) => return Ok(Response::text(404, "not found")),
        };
        let meta = match tokio::fs::metadata(&full).await {
            Ok(m) => m,
            Err(_) => return Ok(Response::text(404, "not found")),
        };
        if !meta.is_dir() {
            // single-file propfind
            let entry = self.entry_for(&full, rel).await?;
            let xml = multistatus(
                &format!("{}/{}", DAV_PREFIX, rel.trim_end_matches('/')),
                false,
                &[entry],
            );
            return Ok(Response::xml(207, xml));
        }

        let mut entries = Vec::new();
        if let Ok(mut rd) = tokio::fs::read_dir(&full).await {
            loop {
                match rd.next_entry().await {
                    Ok(Some(e)) => {
                        let name = e.file_name().to_string_lossy().to_string();
                        let child_rel = if rel.is_empty() {
                            name.clone()
                        } else {
                            format!("{}/{}", rel.trim_end_matches('/'), name)
                        };
                        if let Ok(entry) = self.entry_for(&e.path(), &child_rel).await {
                            entries.push(entry);
                        }
                    }
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
        }
        entries.sort_by(|a, b| {
            b.is_dir.cmp(&a.is_dir).then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });

        let dir_href = format!("{}/", dav_url(rel));
        let xml = multistatus(&dir_href, true, &entries);
        Ok(Response::xml(207, xml)
            .header("Cache-Control", "no-cache"))
    }

    async fn entry_for(&self, path: &Path, rel: &str) -> Result<FsEntry> {
        let md = tokio::fs::metadata(path).await?;
        let mtime = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let name = rel.rsplit('/').next().unwrap_or(rel).to_string();
        Ok(FsEntry {
            name,
            path: rel.to_string(),
            is_dir: md.is_dir(),
            size: if md.is_dir() { 0 } else { md.len() },
            mtime,
        })
    }

    // -------------------------------------------------- MKCOL / DELETE / MOVE

    async fn handle_mkcol(&self, head: &RequestHead, rel: &str) -> Result<Response> {
        if let Some(r) = self.check_lock(rel, Some(head)) {
            return Ok(r);
        }
        let full = match self.resolve_write(rel).await {
            Ok(p) => p,
            Err(e) => return Ok(Response::text(403, &format!("forbidden: {}", e))),
        };
        if full.exists() {
            return Ok(Response::text(405, "already exists"));
        }
        if tokio::fs::create_dir(&full).await.is_err() {
            // parent missing?
            if full.parent().map(|p| !p.exists()).unwrap_or(false) {
                return Ok(Response::text(409, "parent does not exist"));
            }
            return Ok(Response::text(403, "cannot create"));
        }
        Ok(Response::new(201))
    }

    async fn handle_delete(&self, head: &RequestHead, rel: &str) -> Result<Response> {
        if let Some(r) = self.check_lock(rel, Some(head)) {
            return Ok(r);
        }
        let full = match self.resolve_write(rel).await {
            Ok(p) => p,
            Err(e) => return Ok(Response::text(403, &format!("forbidden: {}", e))),
        };
        let meta = match tokio::fs::symlink_metadata(&full).await {
            Ok(m) => m,
            Err(_) => return Ok(Response::text(404, "not found")),
        };
        if rel.is_empty() {
            return Ok(Response::text(403, "cannot delete share root"));
        }
        let res = if meta.is_dir() {
            tokio::fs::remove_dir_all(&full).await
        } else {
            tokio::fs::remove_file(&full).await
        };
        if res.is_err() {
            return Ok(Response::text(403, "delete failed"));
        }
        Ok(Response::new(204))
    }

    async fn handle_move_copy(&self, head: &RequestHead, rel: &str, is_move: bool) -> Result<Response> {
        let dest_header = match head.header("destination") {
            Some(d) => d.to_string(),
            None => return Ok(Response::text(400, "missing Destination header")),
        };
        let dest_rel = match destination_to_rel(&dest_header) {
            Some(r) => r,
            None => return Ok(Response::text(400, "bad Destination")),
        };
        let overwrite = head
            .header("overwrite")
            .map(|o| !o.eq_ignore_ascii_case("F"))
            .unwrap_or(true);

        let src = self.resolve_read(rel)?;
        let dst = match self.resolve_write(&dest_rel).await {
            Ok(p) => p,
            Err(e) => return Ok(Response::text(403, &format!("forbidden: {}", e))),
        };
        if !src.exists() {
            return Ok(Response::text(404, "source not found"));
        }
        if dst.exists() {
            if !overwrite {
                return Ok(Response::text(412, "destination exists"));
            }
            if dst.is_dir() {
                tokio::fs::remove_dir_all(&dst).await?;
            } else {
                tokio::fs::remove_file(&dst).await?;
            }
        }
        if let Some(parent) = dst.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let res = if is_move {
            tokio::fs::rename(&src, &dst).await
        } else {
            copy_recursive(&src, &dst).await
        };
        if res.is_err() {
            return Ok(Response::text(500, "operation failed"));
        }
        Ok(Response::new(201))
    }

    // ------------------------------------------------------------ LOCK / UNLOCK

    fn handle_lock(&self, rel: &str, head: &RequestHead) -> Result<Response> {
        let now = now_secs();
        let mut locks = self.locks.lock().unwrap();
        locks.retain(|_, l| l.expires > now);

        let token = format!("opaquelocktoken:{}", pseudo_uuid());
        if let Some(existing) = locks.get(rel) {
            // refresh if the client already owns it
            let if_header = head.header("if").unwrap_or("");
            if if_header.contains(&existing.token) {
                let tok = existing.token.clone();
                locks.insert(rel.to_string(), Lock { token: tok.clone(), expires: now + LOCK_TTL_SECS });
                return Ok(lock_response(&tok, rel));
            }
            return Ok(Response::text(423, "already locked"));
        }
        locks.insert(rel.to_string(), Lock { token: token.clone(), expires: now + LOCK_TTL_SECS });
        Ok(lock_response(&token, rel))
    }

    fn handle_unlock(&self, rel: &str, head: &RequestHead) -> Result<Response> {
        let tok_header = head.header("lock-token").unwrap_or("");
        let mut locks = self.locks.lock().unwrap();
        if let Some(existing) = locks.get(rel) {
            if tok_header.contains(&existing.token) {
                locks.remove(rel);
                return Ok(Response::new(204));
            }
            return Ok(Response::text(403, "lock token mismatch"));
        }
        Ok(Response::new(204))
    }

    fn check_lock(&self, rel: &str, head: Option<&RequestHead>) -> Option<Response> {
        let now = now_secs();
        let mut locks = self.locks.lock().unwrap();
        locks.retain(|_, l| l.expires > now);
        if let Some(lock) = locks.get(rel) {
            // The real request head carries the `If:` token; a `fake()` head
            // used to make every DELETE/MKCOL on a locked resource fail
            // with 423 — even for the lock owner.
            let if_header = head.and_then(|h| h.header("if")).unwrap_or("");
            if !if_header.contains(&lock.token) {
                return Some(Response::text(423, "resource is locked"));
            }
        }
        None
    }

    // ------------------------------------------------------------ path safety

    /// Resolve a WebDAV-relative path for reading (must exist inside root).
    fn resolve_read(&self, rel: &str) -> Result<PathBuf> {
        let full = join_under_root(&self.share_root, rel)?;
        // Defense in depth: when the target exists, verify it really lives
        // inside the share root and is not a symlink pointing outside.
        if let Ok(canon) = std::fs::canonicalize(&full) {
            if !path_starts_with_root(&self.root_canon, &canon) {
                return Err(anyhow!("path escapes share root"));
            }
        }
        Ok(full)
    }

    /// Resolve for writing; verifies that the nearest existing ancestor is
    /// inside the share root. Both sides of the comparison are canonical
    /// (see `root_canon`), otherwise Windows verbatim `\\?\` paths never
    /// match and every write is rejected.
    async fn resolve_write(&self, rel: &str) -> Result<PathBuf> {
        let full = join_under_root(&self.share_root, rel)?;
        let mut ancestor = full.as_path();
        loop {
            if ancestor.exists() {
                let canon = tokio::fs::canonicalize(ancestor)
                    .await
                    .unwrap_or_else(|_| ancestor.to_path_buf());
                if !path_starts_with_root(&self.root_canon, &canon) {
                    return Err(anyhow!("path escapes share root"));
                }
                break;
            }
            match ancestor.parent() {
                Some(p) => ancestor = p,
                None => return Err(anyhow!("no ancestor")),
            }
        }
        Ok(full)
    }

    // ------------------------------------------------------------ pairing API

    async fn pair_endpoint(&self, path: &str, body: &[u8], caller_fp: &str) -> Result<Response> {
        let body: serde_json::Value = serde_json::from_slice(body).unwrap_or(serde_json::json!({}));
        match path {
            "/single/pair/request" => {
                let claimed_fp = body["fp"].as_str().unwrap_or("").to_lowercase();
                let claimed_id = body["id"].as_str().unwrap_or("").to_lowercase();
                let name = body["name"].as_str().unwrap_or("Device").to_string();
                let caller_prefix = caller_fp.get(..16).unwrap_or("");
                if claimed_fp != caller_fp || (!claimed_id.is_empty() && claimed_id != caller_prefix) {
                    return Ok(Response::json(
                        403,
                        serde_json::json!({"error": "identity mismatch"}).to_string(),
                    ));
                }
                // already paired? hand back the token directly
                if let Some(p) = self.pairs.pairing_by_id(&claimed_id).await {
                    return Ok(Response::json(
                        200,
                        serde_json::json!({"status": "paired", "req_id": "", "token": p.token, "name": self.identity.device_name}).to_string(),
                    ));
                }
                let req = self.pairs.add_incoming(&claimed_id, &name, caller_fp).await;
                Ok(Response::json(
                    200,
                    serde_json::json!({"status": "pending", "req_id": req.req_id, "name": self.identity.device_name}).to_string(),
                ))
            }
            "/single/pair/check" => {
                let req_id = body["req_id"].as_str().unwrap_or("");
                match self.pairs.incoming_by_req(req_id, caller_fp).await {
                    Some(req) => {
                        let token = if req.state == ReqState::Verified {
                            self.pairs.pairing_by_id(&req.id).await.map(|p| p.token)
                        } else {
                            None
                        };
                        Ok(Response::json(
                            200,
                            serde_json::json!({
                                "status": req.state_str(),
                                "id": self.identity.device_id,
                                "name": self.identity.device_name,
                                "fp": self.identity.ca_fp,
                                "token": token,
                            })
                            .to_string(),
                        ))
                    }
                    None => Ok(Response::json(
                        404,
                        serde_json::json!({"status": "expired"}).to_string(),
                    )),
                }
            }
            "/single/pair/verify" => {
                let req_id = body["req_id"].as_str().unwrap_or("");
                let pin = body["pin"].as_str().unwrap_or("");
                match self.pairs.incoming_by_req(req_id, caller_fp).await {
                    Some(req) => {
                        if pin != req.pin {
                            return Ok(Response::json(
                                403,
                                serde_json::json!({"status": "bad_pin"}).to_string(),
                            ));
                        }
                        match self.pairs.confirm_pin(req_id, caller_fp, pin).await {
                            crate::pairing::ConfirmOutcome::Verified(token) => Ok(Response::json(
                                200,
                                serde_json::json!({
                                    "status": "verified",
                                    "id": self.identity.device_id,
                                    "name": self.identity.device_name,
                                    "fp": self.identity.ca_fp,
                                    "token": token,
                                })
                                .to_string(),
                            )),
                            crate::pairing::ConfirmOutcome::PinOk => Ok(Response::json(
                                200,
                                serde_json::json!({"status": "pin_ok"}).to_string(),
                            )),
                        }
                    }
                    None => Ok(Response::json(
                        404,
                        serde_json::json!({"status": "expired"}).to_string(),
                    )),
                }
            }
            _ => Ok(Response::text(404, "not found")),
        }
    }

    async fn unpair_endpoint(&self, body: &[u8], token: Option<&str>) -> Result<Response> {
        let token = match token {
            Some(t) => t,
            None => return Ok(Response::text(401, "unauthorized")),
        };
        let pairing = match self.pairs.pairing_by_token(token).await {
            Some(p) => p,
            None => return Ok(Response::text(401, "unknown token")),
        };
        let body: serde_json::Value = serde_json::from_slice(body).unwrap_or(serde_json::json!({}));
        let target = body["id"].as_str().unwrap_or("");
        if !target.is_empty() && target != pairing.id {
            return Ok(Response::text(403, "token does not match device"));
        }
        self.pairs.remove_pairing(&pairing.id).await;
        Ok(Response::json(200, serde_json::json!({"ok": true}).to_string()))
    }
}

// ---------------------------------------------------------------- helpers

const ALLOW_METHODS: &str = "OPTIONS, GET, HEAD, PUT, DELETE, PROPFIND, PROPPATCH, MKCOL, MOVE, COPY, LOCK, UNLOCK";

fn options_response() -> Response {
    Response::new(200)
        .header("DAV", "1, 2")
        .header("MS-Author-Via", "DAV")
        .header("Allow", ALLOW_METHODS)
        .header("Public", ALLOW_METHODS)
        .header("Accept-Ranges", "bytes")
}

impl Response {
    fn body_file(self, file: tokio::fs::File, offset: u64, len: u64) -> Self {
        Response {
            status: self.status,
            headers: self.headers,
            body: Body::File { file, offset, len },
        }
    }
}

fn landing_page(id: &Identity) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Single</title></head>\
         <body style=\"font-family:system-ui,sans-serif;background:#12141a;color:#e8eaf0;display:flex;align-items:center;justify-content:center;height:100vh;margin:0\">\
         <div style=\"text-align:center\"><h1>{} is sharing with Single</h1>\
         <p style=\"color:#8b93a7\">Device ID <code>{}</code> · Single v{} · by L. Malakar</p></div></body></html>",
        escape(&id.device_name),
        id.device_id,
        crate::config::APP_VERSION
    )
}

fn lock_response(token: &str, rel: &str) -> Response {
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <D:prop xmlns:D=\"DAV:\"><D:lockdiscovery><D:activelock>\
         <D:locktype><D:write/></D:locktype>\
         <D:lockscope><D:exclusive/></D:lockscope>\
         <D:depth>0</D:depth>\
         <D:owner><D:href>Single</D:href></D:owner>\
         <D:timeout>Second-{}</D:timeout>\
         <D:locktoken><D:href>{}</D:href></D:locktoken>\
         <D:lockroot><D:href>{}/{}</D:href></D:lockroot>\
         </D:activelock></D:lockdiscovery></D:prop>",
        LOCK_TTL_SECS,
        escape(token),
        DAV_PREFIX,
        escape(rel)
    );
    Response::xml(200, xml).header("Lock-Token", &format!("<{}>", token))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn pseudo_uuid() -> String {
    let mut b = [0u8; 16];
    use rand::RngCore;
    rand::rngs::OsRng.fill_bytes(&mut b);
    let h = hex::encode(b);
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// `/dav/a/b.txt` -> `a/b.txt` (decoded). Rejects traversal.
fn dav_rel_path(path: &str) -> Option<String> {
    let rel = path.strip_prefix(DAV_PREFIX)?.to_string();
    let rel = rel.trim_start_matches('/').to_string();
    for seg in rel.split('/') {
        if seg == ".." || seg.contains('\\') || seg.contains('\0') || seg.contains(':') {
            return None;
        }
    }
    Some(rel)
}

fn join_under_root(root: &Path, rel: &str) -> Result<PathBuf> {
    if rel.split('/').any(|s| s == ".." || s.contains('\\') || s.contains('\0') || s.contains(':')) {
        return Err(anyhow!("invalid path segment"));
    }
    let mut p = root.to_path_buf();
    for seg in rel.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        p.push(seg);
    }
    Ok(p)
}

/// Containment check used by `resolve_read` / `resolve_write`.
///
/// `std::fs::canonicalize` on Windows returns *verbatim* paths
/// (`\\?\C:\...`) and the on-disk casing, neither of which matches a
/// user-typed path component-wise. Normalize both sides: strip the verbatim
/// prefix, lowercase on Windows, then compare on a separator boundary.
pub fn path_starts_with_root(root: &Path, candidate: &Path) -> bool {
    let norm = |p: &Path| -> String {
        let s = p.to_string_lossy().replace("\\\\?\\", "");
        if cfg!(windows) {
            s.to_ascii_lowercase()
        } else {
            s
        }
    };
    let r = norm(root);
    let c = norm(candidate);
    if r.is_empty() {
        return true;
    }
    if r.ends_with('/') || r.ends_with('\\') {
        return c.starts_with(&r);
    }
    c == r || c.starts_with(&format!("{}/", r)) || c.starts_with(&format!("{}\\", r))
}

/// Convert a `Destination:` header (absolute URL or absolute path) into a
/// WebDAV-relative path.
fn destination_to_rel(dest: &str) -> Option<String> {
    let path = if let Some(rest) = dest.splitn(2, "://").nth(1) {
        // https://host:port/dav/x -> /dav/x
        let slash = rest.find('/')?;
        &rest[slash..]
    } else {
        dest
    };
    let decoded = percent_encoding::percent_decode_str(path.split('?').next()?)
        .decode_utf8_lossy()
        .to_string();
    dav_rel_path(&decoded)
}

fn copy_recursive<'a>(src: &'a Path, dst: &'a Path) -> std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<()>> + Send + 'a>> {
    Box::pin(async move {
        let meta = tokio::fs::metadata(src).await?;
        if meta.is_dir() {
            tokio::fs::create_dir_all(dst).await?;
            let mut rd = tokio::fs::read_dir(src).await?;
            while let Some(e) = rd.next_entry().await? {
                let child_dst = dst.join(e.file_name());
                copy_recursive(&e.path(), &child_dst).await?;
            }
            Ok(())
        } else {
            tokio::fs::copy(src, dst).await.map(|_| ())
        }
    })
}

/// Public-key fingerprint of the TLS *client* certificate chain.
pub fn peer_ca_fp(stream: &tokio_rustls::server::TlsStream<TcpStream>) -> Option<String> {
    let (_, conn) = stream.get_ref();
    let certs = conn.peer_certificates()?;
    crate::identity::cert_fp(certs)
}

impl RequestHead {
    /// A minimal head for lock checks where the original is unavailable.
    pub fn fake() -> RequestHead {
        RequestHead {
            method: String::new(),
            target: String::new(),
            path: String::new(),
            query: String::new(),
            version_11: true,
            headers: HashMap::new(),
        }
    }
}

/// dav URL for a relative path (no trailing slash logic).
pub fn dav_url(rel: &str) -> String {
    if rel.is_empty() {
        format!("{}/", DAV_PREFIX)
    } else {
        format!("{}/{}", DAV_PREFIX, rel)
    }
}
