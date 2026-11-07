//! Loopback WebDAV proxy + Windows "This PC" drive mapping.
//!
//! Windows cannot natively browse a remote device's files, but it *can* map
//! any WebDAV server to a drive letter. So the server exposes, on
//! **127.0.0.1 only** (never the LAN), a minimal WebDAV share per connected
//! peer:
//!
//! ```text
//!   http://127.0.0.1:5051/dav/<peer-slug>/…
//!                 └── proxied over the E2EE-encrypted P2P session
//! ```
//!
//! A watcher maps one free drive letter per established session
//! (`net use Z: …`, WebClient service), so every connected device shows up
//! in **This PC** as its own drive — one more device, one more drive.
//! Removing the connection unmaps the drive again. On Android the documents
//! provider replaces this (the OS has no drive letters); there the WebDAV
//! server simply stays unused.
//!
//! Supported verbs: OPTIONS, PROPFIND (depth 0/1), GET/HEAD (+Range), PUT,
//! MKCOL, DELETE, COPY, MOVE, PROPPATCH, LOCK/UNLOCK — the set Windows
//! Explorer and the WebClient service actually issue.

use crate::bridge::{BridgeManager, FsChunk, READ_CHUNK};
use crate::protocol::ServerMsg;
use crate::util::log_bus;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::response::Response;
use axum::routing::any;
use axum::Router;
use base64::Engine;
use futures_util::StreamExt;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::broadcast;

#[derive(Clone, Debug)]
pub struct DavPeer {
    pub peer_id: String,
    pub name: String,
    pub platform: String,
    pub slug: String,
}

pub struct DavState {
    pub bridge: Arc<BridgeManager>,
    bus: broadcast::Sender<ServerMsg>,
    pub port: u16,
    peers: RwLock<Vec<DavPeer>>,
    /// Windows drive letters per peer (peer_id → "Z:").
    pub mounts: Arc<MountManager>,
}

impl DavState {
    pub fn new(bridge: Arc<BridgeManager>, bus: broadcast::Sender<ServerMsg>, port: u16) -> Arc<Self> {
        let mounts = Arc::new(MountManager::new(bus.clone()));
        Arc::new(Self {
            bridge,
            bus,
            port,
            peers: RwLock::new(Vec::new()),
            mounts,
        })
    }

    /// Refresh the peer table (called by the status loop). Slugs stay stable
    /// for known peers so mounted URLs don't churn.
    pub fn refresh(&self, established: Vec<(String, String, String)>) {
        let mut peers = self.peers.write().unwrap();
        let mut out: Vec<DavPeer> = Vec::new();
        for (peer_id, name, platform) in established {
            let slug = match peers.iter().find(|p| p.peer_id == peer_id) {
                Some(existing) => existing.slug.clone(),
                None => {
                    let base = slugify(&name);
                    let mut slug = base.clone();
                    let mut n = 2;
                    while out.iter().any(|p| p.slug.eq_ignore_ascii_case(&slug)) {
                        slug = format!("{base}-{n}");
                        n += 1;
                    }
                    slug
                }
            };
            out.push(DavPeer { peer_id, name, platform, slug });
        }
        *peers = out;
    }

    pub fn peer_by_slug(&self, slug: &str) -> Option<DavPeer> {
        self.peers
            .read()
            .unwrap()
            .iter()
            .find(|p| p.slug.eq_ignore_ascii_case(slug))
            .cloned()
    }

    pub fn peer_by_id(&self, peer_id: &str) -> Option<DavPeer> {
        self.peers
            .read()
            .unwrap()
            .iter()
            .find(|p| p.peer_id == peer_id)
            .cloned()
    }

    pub fn snapshot(&self) -> HashMap<String, String> {
        self.mounts.snapshot()
    }
}

/// Bind the WebDAV proxy on loopback and serve it forever.
pub async fn serve_dav(state: Arc<DavState>) -> std::io::Result<()> {
    let app: Router = Router::new()
        .route("/dav/{*rest}", any(dav))
        .route("/dav", any(dav))
        .route("/", any(root))
        .with_state(state.clone());
    let addr = format!("127.0.0.1:{}", state.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    log_bus(&state.bus, "ok", format!("bridge WebDAV proxy listening on http://{addr}/dav/<device>"));
    let bus = state.bus.clone();
    let port = state.port;
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            log_bus(&bus, "error", format!("WebDAV server stopped: {e} (port {port})"));
        }
    });
    Ok(())
}

async fn root() -> Response {
    text_response(StatusCode::OK, "Single bridge — WebDAV proxy. Use /dav/<device>/")
}

// ---------------------------------------------------------------------------
// The WebDAV handler
// ---------------------------------------------------------------------------

async fn dav(State(st): State<Arc<DavState>>, req: Request) -> Response {
    let method = req.method().clone();
    let uri = req.uri().path().to_string();
    let headers = req.headers().clone();
    let path = uri.trim_start_matches("/dav").to_string();
    let path = path.strip_prefix('/').unwrap_or("").to_string();

    // "slug" is the first segment; the rest is the peer's logical path.
    let (slug, rest) = match path.split_once('/') {
        Some((s, r)) => (s.to_string(), r.to_string()),
        None => (path.clone(), String::new()),
    };

    // OPTIONS works at every level, even before we know the peer.
    if method == Method::OPTIONS {
        return Response::builder()
            .status(StatusCode::OK)
            .header("DAV", "1, 2")
            .header("MS-Author-Via", "DAV")
            .header(
                "Allow",
                "OPTIONS, GET, HEAD, PROPFIND, PROPPATCH, MKCOL, PUT, DELETE, COPY, MOVE, LOCK, UNLOCK",
            )
            .header(header::CONTENT_LENGTH, "0")
            .body(Body::empty())
            .unwrap();
    }

    let Some(peer) = st.peer_by_slug(&slug) else {
        return text_response(StatusCode::NOT_FOUND, "no such connected device (drive mapping is stale?)");
    };
    let logical = logical_from_uri(&rest);
    let pid = peer.peer_id.clone();
    let hdr = headers.clone();

    let result: Result<Response, (StatusCode, String)> = match method.as_str() {
        "PROPFIND" => propfind(&st, &pid, &peer.slug, &logical, &hdr).await,
        "GET" => get(&st, &pid, &logical, &hdr, true).await,
        "HEAD" => get(&st, &pid, &logical, &hdr, false).await,
        "PUT" => put(&st, &pid, &logical, req).await,
        "MKCOL" => mkcol(&st, &pid, &logical).await,
        "DELETE" => delete(&st, &pid, &logical).await,
        "PROPPATCH" => Ok(proppatch(&peer.slug, &logical)),
        "LOCK" => Ok(lock(&hdr)),
        "UNLOCK" => Ok(unlock_response()),
        "COPY" | "MOVE" => copy_move(&st, &pid, &logical, &method, &hdr).await,
        _ => Ok(text_response(StatusCode::METHOD_NOT_ALLOWED, "unsupported method")),
    };
    match result {
        Ok(resp) => resp,
        Err((code, msg)) => text_response(code, &msg),
    }
}

/// `/dav/slug/a/b%20c/` → logical `/a/b c`.
fn logical_from_uri(rest: &str) -> String {
    let decoded: Vec<String> = rest
        .split('/')
        .filter(|s| !s.is_empty())
        .map(percent_decode)
        .collect();
    if decoded.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", decoded.join("/"))
    }
}

fn href_for(slug: &str, logical: &str, is_dir: bool) -> String {
    let segs: Vec<String> = logical
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .map(percent_encode_path)
        .collect();
    let mut href = String::from("/dav/");
    href.push_str(slug);
    for s in &segs {
        href.push('/');
        href.push_str(s);
    }
    if is_dir {
        href.push('/');
    }
    href
}

async fn propfind(
    st: &Arc<DavState>,
    pid: &str,
    slug: &str,
    logical: &str,
    hdr: &HeaderMap,
) -> Result<Response, (StatusCode, String)> {
    let depth = hdr
        .get("Depth")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_lowercase())
        .unwrap_or_else(|| "infinity".into());
    if depth == "infinity" {
        // Recursive listings are a pain and a DoS vector — Windows is happy
        // with depth 0/1 and walks the tree itself.
        return Err((StatusCode::FORBIDDEN, "infinite-depth PROPFIND not supported".into()));
    }

    let self_entry = st
        .bridge
        .stat(pid, logical)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e))?;
    let mut entries = vec![(logical.to_string(), self_entry.clone())];
    if depth == "1" && self_entry.kind == "dir" {
        for child in st
            .bridge
            .list(pid, logical)
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, e))?
        {
            let child_path = if logical == "/" {
                format!("/{}", child.name)
            } else {
                format!("{}/{}", logical.trim_end_matches('/'), child.name)
            };
            entries.push((child_path, child));
        }
    }

    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<D:multistatus xmlns:D=\"DAV:\">");
    for (path, e) in &entries {
        let is_dir = e.kind == "dir";
        let modified = httpdate::fmt_http_date(std::time::SystemTime::UNIX_EPOCH
            + std::time::Duration::from_millis(e.modified.max(0) as u64));
        let created = iso8601(e.modified.max(0));
        xml.push_str(&format!(
            "<D:response><D:href>{}</D:href><D:propstat><D:prop>\
             <D:displayname>{}</D:displayname>\
             <D:getlastmodified>{}</D:getlastmodified>\
             <D:creationdate>{}</D:creationdate>\
             {}<D:getcontentlength>{}</D:getcontentlength>\
             <D:getcontenttype>{}</D:getcontenttype>\
             <D:supportedlock><D:lockentry><D:lockscope><D:exclusive/></D:lockscope><D:locktype><D:write/></D:locktype></D:lockentry></D:supportedlock>\
             </D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>",
            xml_escape(&href_for(slug, path, is_dir)),
            xml_escape(&e.name),
            modified,
            created,
            if is_dir { "<D:resourcetype><D:collection/></D:resourcetype>" } else { "<D:resourcetype/>" },
            e.size,
            if is_dir { "httpd/unix-directory" } else { content_type_for(&e.name) },
        ));
    }
    xml.push_str("</D:multistatus>");
    Ok(Response::builder()
        .status(StatusCode::MULTI_STATUS)
        .header(header::CONTENT_TYPE, "application/xml; charset=utf-8")
        .body(Body::from(xml))
        .unwrap())
}

async fn get(
    st: &Arc<DavState>,
    pid: &str,
    logical: &str,
    hdr: &HeaderMap,
    with_body: bool,
) -> Result<Response, (StatusCode, String)> {
    let entry = st
        .bridge
        .stat(pid, logical)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e))?;
    if entry.kind == "dir" {
        return Err((StatusCode::FORBIDDEN, "cannot GET a folder".into()));
    }

    // Range support (single range only, like Windows WebClient sends).
    let (offset, len, partial) = match hdr.get(header::RANGE).and_then(|v| v.to_str().ok()) {
        Some(spec) => parse_range(spec, entry.size)
            .map(|(o, l)| (o, l, true))
            .ok_or((StatusCode::RANGE_NOT_SATISFIABLE, "bad Range header".to_string()))?,
        None => (0u64, 0u64, false), // len 0 → stream to EOF
    };

    let content_len = if partial {
        len
    } else {
        entry.size
    };

    let mut builder = Response::builder()
        .status(if partial { StatusCode::PARTIAL_CONTENT } else { StatusCode::OK })
        .header(header::CONTENT_TYPE, content_type_for(&entry.name))
        .header(header::CONTENT_LENGTH, content_len.to_string())
        .header(header::ACCEPT_RANGES, "bytes");
    if partial {
        builder = builder.header(
            header::CONTENT_RANGE,
            format!(
                "bytes {}-{}/{}",
                offset,
                offset + content_len.saturating_sub(1),
                entry.size
            ),
        );
    }
    if !with_body {
        return Ok(builder.body(Body::empty()).unwrap());
    }

    let rx = st
        .bridge
        .open_read(pid, logical, offset, len)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
    let stream = chunk_stream(rx);
    Ok(builder.body(Body::from_stream(stream)).unwrap())
}

fn chunk_stream(
    rx: tokio::sync::mpsc::Receiver<Result<FsChunk, String>>,
) -> impl futures_util::Stream<Item = std::io::Result<axum::body::Bytes>> {
    futures_util::stream::unfold(Some(rx), |rx| async move {
        let mut rx = rx?;
        match rx.recv().await {
            Some(Ok(chunk)) => {
                let eof = chunk.eof;
                let bytes = axum::body::Bytes::from(chunk.data);
                if eof {
                    Some((Ok(bytes), None))
                } else {
                    Some((Ok(bytes), Some(rx)))
                }
            }
            Some(Err(e)) => Some((Err(std::io::Error::other(e)), None)),
            None => None,
        }
    })
}

async fn put(
    st: &Arc<DavState>,
    pid: &str,
    logical: &str,
    req: Request,
) -> Result<Response, (StatusCode, String)> {
    let size = req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let rid = st
        .bridge
        .write_open(pid, logical, size)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;

    let mut body = req.into_body().into_data_stream();
    let mut buf: Vec<u8> = Vec::with_capacity(READ_CHUNK);
    let mut fail: Option<String> = None;
    while let Some(chunk) = body.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                fail = Some(format!("upload body error: {e}"));
                break;
            }
        };
        buf.extend_from_slice(&chunk);
        if buf.len() >= READ_CHUNK {
            if let Err(e) = st.bridge.write_chunk(pid, &rid, std::mem::take(&mut buf)).await {
                fail = Some(e);
                break;
            }
        }
    }
    match fail {
        Some(e) => {
            // best effort: tell the peer to drop the partial file
            let _ = st
                .bridge
                .delete(pid, logical, false)
                .await;
            return Err((StatusCode::BAD_GATEWAY, e));
        }
        None => {
            if !buf.is_empty() {
                st.bridge
                    .write_chunk(pid, &rid, buf)
                    .await
                    .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
            }
            st.bridge
                .write_close(pid, &rid)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
        }
    }
    Ok(text_response(StatusCode::CREATED, "created"))
}

async fn mkcol(st: &Arc<DavState>, pid: &str, logical: &str) -> Result<Response, (StatusCode, String)> {
    st.bridge
        .mkdir(pid, logical)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
    Ok(text_response(StatusCode::CREATED, "created"))
}

async fn delete(st: &Arc<DavState>, pid: &str, logical: &str) -> Result<Response, (StatusCode, String)> {
    if logical == "/" {
        return Err((StatusCode::FORBIDDEN, "cannot delete the device root".into()));
    }
    st.bridge
        .delete(pid, logical, true)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
    Ok(Response::builder()
        .status(StatusCode::NO_CONTENT)
        .body(Body::empty())
        .unwrap())
}

fn proppatch(slug: &str, logical: &str) -> Response {
    // Echo success — Windows sets Win32 file attributes after MKCOL/PUT and
    // only needs the request acknowledged.
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <D:multistatus xmlns:D=\"DAV:\">\
         <D:response><D:href>{}</D:href>\
         <D:propstat><D:prop/><D:status>HTTP/1.1 200 OK</D:status></D:propstat>\
         </D:response></D:multistatus>",
        xml_escape(&href_for(slug, logical, false))
    );
    Response::builder()
        .status(StatusCode::MULTI_STATUS)
        .header(header::CONTENT_TYPE, "application/xml; charset=utf-8")
        .body(Body::from(xml))
        .unwrap()
}

fn lock(hdr: &HeaderMap) -> Response {
    let token = format!(
        "opaquelocktoken:{}",
        base64::engine::general_purpose::STANDARD.encode(rand::random::<[u8; 12]>())
    );
    let timeout = hdr
        .get("Timeout")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("Second-600")
        .to_string();
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <D:prop xmlns:D=\"DAV:\"><D:lockdiscovery><D:activelock>\
         <D:locktype><D:write/></D:locktype>\
         <D:lockscope><D:exclusive/></D:lockscope>\
         <D:depth>0</D:depth><D:timeout>{}</D:timeout>\
         <D:locktoken><D:href>{}</D:href></D:locktoken>\
         </D:activelock></D:lockdiscovery></D:prop>",
        xml_escape(&timeout),
        token
    );
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/xml; charset=utf-8")
        .header("Lock-Token", format!("<{token}>"))
        .body(Body::from(xml))
        .unwrap()
}

fn unlock_response() -> Response {
    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .body(Body::empty())
        .unwrap()
}

/// COPY / MOVE — implemented as a remote read → remote write through us,
/// which also covers device-to-device copies (different slugs).
async fn copy_move(
    st: &Arc<DavState>,
    pid: &str,
    logical: &str,
    method: &Method,
    hdr: &HeaderMap,
) -> Result<Response, (StatusCode, String)> {
    let dest_raw = hdr
        .get("Destination")
        .and_then(|v| v.to_str().ok())
        .ok_or((StatusCode::BAD_REQUEST, "Destination header missing".to_string()))?;
    let (dest_path, dest_peer) = parse_destination(st, dest_raw)
        .ok_or((StatusCode::BAD_GATEWAY, "Destination is outside this server".to_string()))?;
    let dest_pid = match dest_peer {
        Some(p) => p.peer_id,
        None => pid.to_string(),
    };

    // Source must exist; honour Overwrite: F.
    let src = st
        .bridge
        .stat(pid, logical)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e))?;
    if src.kind == "dir" {
        return Err((StatusCode::FORBIDDEN, "copying folders between devices is not supported yet — copy the files inside".into()));
    }
    let overwrite = hdr
        .get("Overwrite")
        .and_then(|v| v.to_str().ok())
        .map(|v| !v.trim().eq_ignore_ascii_case("F"))
        .unwrap_or(true);
    if !overwrite && st.bridge.stat(&dest_pid, &dest_path).await.is_ok() {
        return Err((StatusCode::PRECONDITION_FAILED, "destination exists".into()));
    }

    // Stream source → destination.
    let rid = st
        .bridge
        .write_open(&dest_pid, &dest_path, src.size)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
    let mut rx = st
        .bridge
        .open_read(pid, logical, 0, 0)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
    let mut buf: Vec<u8> = Vec::with_capacity(READ_CHUNK);
    while let Some(item) = rx.recv().await {
        match item {
            Ok(chunk) => {
                buf.extend_from_slice(&chunk.data);
                if buf.len() >= READ_CHUNK {
                    st.bridge
                        .write_chunk(&dest_pid, &rid, std::mem::take(&mut buf))
                        .await
                        .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
                }
                if chunk.eof {
                    break;
                }
            }
            Err(e) => return Err((StatusCode::BAD_GATEWAY, e)),
        }
    }
    if !buf.is_empty() {
        st.bridge
            .write_chunk(&dest_pid, &rid, buf)
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
    }
    st.bridge
        .write_close(&dest_pid, &rid)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;

    if method == "MOVE" {
        st.bridge
            .delete(pid, logical, false)
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
    }
    Ok(Response::builder()
        .status(StatusCode::NO_CONTENT)
        .body(Body::empty())
        .unwrap())
}

/// `Destination: http://127.0.0.1:5051/dav/<slug>/a/b` → (`/a/b`, peer).
/// Same-server absolute paths (`/dav/<slug>/a`) are accepted too.
fn parse_destination(st: &Arc<DavState>, raw: &str) -> Option<(String, Option<DavPeer>)> {
    // strip scheme://host:port if present
    let path_only = if let Some(pos) = raw.find("/dav/") {
        &raw[pos..]
    } else if raw.starts_with("/dav") {
        raw
    } else {
        return None;
    };
    let rest = path_only.trim_start_matches("/dav").trim_start_matches('/');
    let (slug, tail) = rest.split_once('/')?;
    let peer = st.peer_by_slug(slug);
    Some((logical_from_uri(tail), peer))
}

fn text_response(code: StatusCode, msg: &str) -> Response {
    Response::builder()
        .status(code)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from(msg.to_string()))
        .unwrap()
}

// ---------------------------------------------------------------------------
// Windows drive mapping (net use + WebClient service)
// ---------------------------------------------------------------------------

pub struct MountManager {
    bus: broadcast::Sender<ServerMsg>,
    map: Mutex<HashMap<String, MountedDrive>>,
    elevation_tried: AtomicBool,
    webclient_state: Mutex<String>, // "ok" | "missing" | "needs-elevation" | "starting"
}

#[derive(Clone, Debug)]
struct MountedDrive {
    letter: char,
    #[allow(dead_code)]
    slug: String,
    name: String,
}

impl MountManager {
    fn new(bus: broadcast::Sender<ServerMsg>) -> Self {
        Self {
            bus,
            map: Mutex::new(HashMap::new()),
            elevation_tried: AtomicBool::new(false),
            webclient_state: Mutex::new("unknown".into()),
        }
    }

    pub fn snapshot(&self) -> HashMap<String, String> {
        self.map
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), format!("{}:", v.letter)))
            .collect()
    }

    /// Watch established sessions; map / unmap drive letters to match.
    /// Every blocking OS call runs on the blocking pool.
    pub async fn watch(self: Arc<Self>, dav: Arc<DavState>, p2p: Arc<crate::p2p::P2pManager>) {
        if !cfg!(windows) {
            log_bus(&self.bus, "info", "drive mapping is Windows-only — using the in-app Files browser here");
            return;
        }
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(3));
        loop {
            tick.tick().await;
            let established = p2p.established_sessions();
            dav.refresh(established.clone());

            // 1. unmap drives whose session is gone
            let gone: Vec<(String, MountedDrive)> = {
                let map = self.map.lock().unwrap();
                map.iter()
                    .filter(|(pid, _)| !established.iter().any(|(p, _, _)| p == *pid))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            };
            for (pid, drive) in gone {
                let letter = drive.letter;
                let this = self.clone();
                let _ = tokio::task::spawn_blocking(move || this.unmap(letter)).await;
                self.map.lock().unwrap().remove(&pid);
                log_bus(
                    &self.bus,
                    "info",
                    format!("drive {}: disconnected — “{}” went offline", drive.letter, drive.name),
                );
            }

            // 2. map missing drives
            for (pid, name, _platform) in &established {
                if self.map.lock().unwrap().contains_key(pid) {
                    continue;
                }
                let Some(peer) = dav.peer_by_id(pid) else { continue };
                if !self.clone().ensure_webclient().await {
                    continue; // status loop explains why in the dashboard log
                }
                let url = format!("http://127.0.0.1:{}/dav/{}", dav.port, peer.slug);
                let this = self.clone();
                let url_c = url.clone();
                let letter = tokio::task::spawn_blocking(move || this.map_drive(&url_c)).await;
                match letter.unwrap_or(None) {
                    Some(l) => {
                        self.map.lock().unwrap().insert(
                            pid.clone(),
                            MountedDrive { letter: l, slug: peer.slug.clone(), name: name.clone() },
                        );
                        log_bus(
                            &self.bus,
                            "ok",
                            format!("📁 “{name}” is now drive {l}: in This PC (WebDAV bridge, encrypted)"),
                        );
                    }
                    None => {
                        log_bus(
                            &self.bus,
                            "warn",
                            format!("no free drive letter left for “{name}” — disconnect a drive or use the Files page"),
                        );
                        // don't spam: wait for the next tick anyway
                    }
                }
            }
        }
    }

    /// Try letters Z→E; returns the first that mapped cleanly.
    fn map_drive(&self, url: &str) -> Option<char> {
        for l in ('E'..='Z').rev() {
            let out = std::process::Command::new("net")
                .args(["use", &format!("{l}:"), url, "/persistent:no"])
                .output();
            match out {
                Ok(o) if o.status.success() => return Some(l),
                _ => continue, // in use / unavailable → next letter
            }
        }
        None
    }

    fn unmap(&self, letter: char) {
        let _ = std::process::Command::new("net")
            .args(["use", &format!("{letter}:"), "/delete", "/y"])
            .output();
    }

    /// WebClient (the Windows WebDAV client service) must run for `net use`
    /// with http:// URLs. Starting it needs admin — we ask once via UAC and
    /// set it to auto-start so it survives reboots.
    async fn ensure_webclient(self: Arc<Self>) -> bool {
        loop {
            let state = {
                let this = self.clone();
                tokio::task::spawn_blocking(move || this.query_webclient())
                    .await
                    .unwrap_or(WebClientState::Missing)
            };
            match state {
                WebClientState::Running => {
                    let mut s = self.webclient_state.lock().unwrap();
                    if s.as_str() != "ok" {
                        *s = "ok".into();
                        log_bus(&self.bus, "ok", "Windows WebClient service is running");
                    }
                    return true;
                }
                WebClientState::Missing => {
                    let mut s = self.webclient_state.lock().unwrap();
                    if s.as_str() != "missing" {
                        *s = "missing".into();
                        log_bus(
                            &self.bus,
                            "error",
                            "the Windows “WebClient” service is not installed (Windows N/LTSC or removed) — drives cannot be mapped; use the Files page in the dashboard",
                        );
                    }
                    return false;
                }
                WebClientState::Stopped => {
                    if !self.elevation_tried.swap(true, Ordering::SeqCst) {
                        log_bus(
                            &self.bus,
                            "warn",
                            "starting the Windows WebClient service — accept the UAC prompt (one time only)",
                        );
                        let this = self.clone();
                        let _ = tokio::task::spawn_blocking(move || this.elevate_webclient()).await;
                        tokio::time::sleep(std::time::Duration::from_secs(6)).await;
                        continue; // re-check after the elevation attempt
                    }
                    // elevation already attempted once; retry a plain start
                    let this = self.clone();
                    let _ = tokio::task::spawn_blocking(move || this.start_webclient()).await;
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    let mut s = self.webclient_state.lock().unwrap();
                    if s.as_str() != "starting" {
                        *s = "starting".into();
                        log_bus(
                            &self.bus,
                            "warn",
                            "waiting for the WebClient service (run enable-drives.bat as admin if drives never appear)",
                        );
                    }
                    return false;
                }
            }
        }
    }

    fn query_webclient(&self) -> WebClientState {
        let Ok(out) = std::process::Command::new("sc")
            .args(["query", "WebClient"])
            .output()
        else {
            return WebClientState::Missing;
        };
        let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
        if out.status.success() && text.contains("running") {
            WebClientState::Running
        } else if out.status.success() {
            WebClientState::Stopped
        } else {
            WebClientState::Missing
        }
    }

    fn start_webclient(&self) {
        let _ = std::process::Command::new("sc")
            .args(["start", "WebClient"])
            .output();
    }

    /// One UAC prompt: auto-start the service, start it now, and lift the
    /// 50 MB WebClient upload limit.
    fn elevate_webclient(&self) {
        let script = "sc config WebClient start= auto & sc start WebClient & reg add \"HKLM\\SYSTEM\\CurrentControlSet\\Services\\WebClient\\Parameters\" /v FileSizeLimitInBytes /t REG_DWORD /d 4294967295 /f";
        let _ = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!("Start-Process -Verb RunAs -FilePath 'cmd.exe' -ArgumentList '/c {script}'"),
            ])
            .output();
    }
}

enum WebClientState {
    Running,
    Stopped,
    Missing,
}

// ---------------------------------------------------------------------------
// small helpers
// ---------------------------------------------------------------------------

fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    let mut s: String = trimmed.chars().take(32).collect();
    if s.is_empty() {
        s = "device".into();
    }
    s
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() + 1 && i + 2 < bytes.len() + 1 {
            if let (Some(h), Some(l)) = (
                bytes.get(i + 1).and_then(|b| (*b as char).to_digit(16)),
                bytes.get(i + 2).and_then(|b| (*b as char).to_digit(16)),
            ) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn percent_encode_path(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'(' | b')' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `bytes=a-b` / `bytes=a-` / `bytes=-n` → (offset, len); len 0 = to EOF.
fn parse_range(spec: &str, size: u64) -> Option<(u64, u64)> {
    let spec = spec.trim().strip_prefix("bytes=")?;
    if let Some((a, b)) = spec.split_once('-') {
        if a.trim().is_empty() {
            // suffix form: bytes=-N (last N bytes)
            let n: u64 = b.trim().parse().ok()?;
            if n == 0 || size == 0 {
                return None;
            }
            let n = n.min(size);
            Some((size - n, n))
        } else {
            let start: u64 = a.trim().parse().ok()?;
            if start >= size {
                return None;
            }
            if b.trim().is_empty() {
                return Some((start, 0)); // to EOF
            }
            let end: u64 = b.trim().parse().ok()?;
            if end < start {
                return None;
            }
            let end = end.min(size - 1);
            Some((start, end - start + 1))
        }
    } else {
        None
    }
}

fn iso8601(ms: i64) -> String {
    // days since epoch → civil date (Howard Hinnant's algorithm)
    let secs = ms / 1000;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, mi, s) = (rem / 3600, (rem / 60) % 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

pub fn content_type_for(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "txt" | "log" | "ini" | "cfg" | "conf" => "text/plain",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "xml" => "application/xml",
        "md" => "text/markdown",
        "csv" => "text/csv",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",
        "pdf" => "application/pdf",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "ogg" => "audio/ogg",
        "m4a" => "audio/mp4",
        "mp4" | "m4v" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "avi" => "video/x-msvideo",
        "mov" => "video/quicktime",
        "zip" => "application/zip",
        "rar" => "application/vnd.rar",
        "7z" => "application/x-7z-compressed",
        "tar" => "application/x-tar",
        "gz" => "application/gzip",
        "exe" | "msi" => "application/x-msdownload",
        "apk" => "application/vnd.android.package-archive",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "epub" => "application/epub+zip",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_parsing() {
        assert_eq!(parse_range("bytes=0-99", 1000), Some((0, 100)));
        assert_eq!(parse_range("bytes=500-", 1000), Some((500, 0)));
        assert_eq!(parse_range("bytes=-100", 1000), Some((900, 100)));
        assert_eq!(parse_range("bytes=1000-", 1000), None);
        assert_eq!(parse_range("bytes=0-2000", 1000), Some((0, 1000)));
    }

    #[test]
    fn slugs() {
        assert_eq!(slugify("Pixel 8"), "Pixel-8");
        assert_eq!(slugify("DESKTOP-WIN11-4F2A"), "DESKTOP-WIN11-4F2A");
        assert_eq!(slugify("***"), "device");
    }

    #[test]
    fn percent_roundtrip() {
        assert_eq!(percent_decode("my%20file.txt"), "my file.txt");
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_encode_path("a b.pdf"), "a%20b.pdf");
        assert_eq!(percent_decode(&percent_encode_path("a b.pdf")), "a b.pdf");
    }

    #[test]
    fn dates() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(1_736_956_800_000), "2025-01-15T16:00:00Z");
    }

    #[test]
    fn xml_escapes() {
        assert_eq!(xml_escape("a<b>&c"), "a&lt;b&gt;&amp;c");
    }
}
