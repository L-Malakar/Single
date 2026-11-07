//! Axum HTTP + WebSocket server.
//!
//! Routes:
//! - `GET /`        — embedded single-page dashboard (or `--static-dir` from disk)
//! - `GET /style.css`, `/app.js`
//! - `GET /ws`      — WebSocket control channel (live devices / sessions / chat / files)
//! - `GET /api/health` — JSON status probe
//! - `GET /api/bridge/status|list|file` + `PUT /api/bridge/upload`, `POST /api/bridge/roots`
//!   — files-bridge API for the dashboard and the Android documents provider.
//!     **Loopback-only** (remote browsers get 403): browsing a peer's files
//!     goes through the authenticated P2P session, never through this HTTP
//!     surface.

use crate::access::AccessControl;
use crate::bridge::BridgeManager;
use crate::config::{Config, SERVICE_TYPE};
use crate::mdns::Mdns;
use crate::p2p::P2pManager;
use crate::protocol::*;
use crate::registry::Registry;
use crate::util::{log_bus, now_ms};
use crate::webdav::DavState;
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use futures_util::stream::{SplitSink, Stream};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use tokio::sync::{broadcast, mpsc};

// The dashboard is embedded into the binary at compile time, so the server
// ships as a single self-contained executable. `--static-dir` overrides it
// for live UI development without rebuilds.
const INDEX_HTML: &str = include_str!("../static/index.html");
const STYLE_CSS: &str = include_str!("../static/style.css");
const APP_JS: &str = include_str!("../static/app.js");
const LOGO_SVG: &str = include_str!("../static/logo.svg");

pub struct AppState {
    pub cfg: Config,
    pub bus: broadcast::Sender<ServerMsg>,
    pub registry: Registry,
    pub mdns: Arc<Mdns>,
    pub self_identity: Arc<RwLock<Identity>>,
    pub p2p: Arc<P2pManager>,
    pub access: Arc<AccessControl>,
    pub bridge: Arc<BridgeManager>,
    pub dav: Arc<DavState>,
    pub started: std::time::Instant,
}

pub async fn serve(state: Arc<AppState>) -> std::io::Result<()> {
    let app = build_router(state.clone());
    let addr = format!("0.0.0.0:{}", state.cfg.http_port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    log_bus(
        &state.bus,
        "ok",
        format!("dashboard listening on http://{}:{}", state.self_identity.read().unwrap().ip, state.cfg.http_port),
    );

    // Bridge snapshot pump: pushes `bridge` messages to dashboards whenever
    // peers / shares / drive letters change (and every 10 s regardless).
    spawn_bridge_status_loop(state.clone());

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
}

fn build_router(state: Arc<AppState>) -> Router {
    let mut app = Router::new()
        .route("/ws", get(ws_upgrade))
        .route("/api/health", get(health))
        .route("/api/bridge/status", get(bridge_status))
        .route("/api/bridge/list", get(bridge_list))
        .route("/api/bridge/file", get(bridge_file))
        .route("/api/bridge/upload", put(bridge_upload))
        .route("/api/bridge/mkdir", post(bridge_mkdir));

    match &state.cfg.static_dir {
        Some(dir) if dir.join("index.html").is_file() => {
            log_bus(
                &state.bus,
                "info",
                format!("serving UI from disk: {}", dir.display()),
            );
            app = app.fallback_service(tower_http::services::ServeDir::new(dir));
        }
        _ => {
            app = app
                .route("/", get(|| async { Html(INDEX_HTML) }))
                .route("/style.css", get(|| async {
                    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], STYLE_CSS)
                }))
                .route("/app.js", get(|| async {
                    ([(header::CONTENT_TYPE, "application/javascript; charset=utf-8")], APP_JS)
                }))
                .route("/logo.svg", get(|| async {
                    ([(header::CONTENT_TYPE, "image/svg+xml")], LOGO_SVG)
                }));
        }
    }
    app.with_state(state)
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

// ---------------------------------------------------------------------------
// REST
// ---------------------------------------------------------------------------

async fn health(State(st): State<Arc<AppState>>) -> impl IntoResponse {
    let ident = st.self_identity.read().unwrap().clone();
    Json(json!({
        "status": "ok",
        "name": ident.name,
        "platform": ident.platform,
        "ip": ident.ip,
        "service_type": SERVICE_TYPE,
        "uptime_secs": st.started.elapsed().as_secs(),
        "devices_discovered": st.registry.snapshot().len(),
        "p2p_sessions": st.p2p.session_count(),
        "p2p_established": st.p2p.established_count(),
        "access_mode": st.access.mode().as_str(),
    }))
}

// ---------------------------------------------------------------------------
// Files bridge REST API — loopback only
// ---------------------------------------------------------------------------

/// 403 for anything that did not originate on this device.
fn loopback_guard(addr: &SocketAddr) -> Result<(), Response> {
    if addr.ip().is_loopback() {
        Ok(())
    } else {
        Err(
            Response::builder()
                .status(StatusCode::FORBIDDEN)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    "{\"ok\":false,\"error\":\"the files bridge is only reachable from this device — open the dashboard on the device itself\"}",
                ))
                .unwrap(),
        )
    }
}

/// Assemble the current bridge snapshot (shares, peers, drive letters).
pub fn bridge_dto(st: &AppState) -> BridgeDto {
    let mounts = st.dav.snapshot();
    let peers = st
        .p2p
        .all_sessions(false)
        .into_iter()
        .map(|s| BridgePeerDto {
            peer_id: s.peer_id.clone(),
            peer_name: s.peer_name.clone(),
            peer_platform: s.peer_platform.clone(),
            state: s.state.clone(),
            encrypted: s.encrypted,
            drive: mounts.get(&s.peer_id).cloned(),
        })
        .collect();
    BridgeDto {
        shares: st.bridge.shares(),
        peers,
        dav_port: st.cfg.dav_port,
        drives_enabled: cfg!(windows),
    }
}

fn spawn_bridge_status_loop(st: Arc<AppState>) {
    tokio::spawn(async move {
        let mut last = String::new();
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
        loop {
            tick.tick().await;
            // keep the WebDAV peer table fresh even before mounts need it
            st.dav.refresh(st.p2p.established_sessions());
            let dto = bridge_dto(&st);
            let sig = serde_json::to_string(&dto).unwrap_or_default();
            let changed = sig != last;
            if changed {
                last = sig;
                let _ = st.bus.send(ServerMsg::Bridge { bridge: dto });
            }
        }
    });
}

#[derive(Deserialize)]
struct PeerPath {
    peer: String,
    #[serde(default)]
    path: String,
}

async fn bridge_status(
    State(st): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    if let Err(e) = loopback_guard(&addr) {
        return e;
    }
    Json(serde_json::to_value(bridge_dto(&st)).unwrap()).into_response()
}

async fn bridge_list(
    State(st): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Query(q): Query<PeerPath>,
) -> Response {
    if let Err(e) = loopback_guard(&addr) {
        return e;
    }
    match st.bridge.list(&q.peer, &q.path).await {
        Ok(entries) => Json(json!({ "ok": true, "entries": entries })).into_response(),
        Err(e) => Json(json!({ "ok": false, "error": e })).into_response(),
    }
}

async fn bridge_file(
    State(st): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Query(q): Query<PeerPath>,
    headers: HeaderMap,
) -> Response {
    if let Err(e) = loopback_guard(&addr) {
        return e;
    }
    let entry = match st.bridge.stat(&q.peer, &q.path).await {
        Ok(e) => e,
        Err(e) => {
            return Json(json!({ "ok": false, "error": e }))
                .into_response()
        }
    };
    if entry.kind == "dir" {
        return Json(json!({ "ok": false, "error": "cannot download a folder" })).into_response();
    }

    // optional single range (video players in the Files app seek this way)
    let (offset, len, partial) = match headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
    {
        Some(spec) => match parse_http_range(&spec, entry.size) {
            Some(r) => (r.0, r.1, true),
            None => {
                return (
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    Json(json!({ "ok": false, "error": "bad Range header" })),
                )
                    .into_response()
            }
        },
        None => (0, 0, false),
    };

    let rx = match st.bridge.open_read(&q.peer, &q.path, offset, len).await {
        Ok(rx) => rx,
        Err(e) => return Json(json!({ "ok": false, "error": e })).into_response(),
    };

    let stream = chunk_stream(rx);
    let mut builder = Response::builder()
        .status(if partial { StatusCode::PARTIAL_CONTENT } else { StatusCode::OK })
        .header(header::CONTENT_TYPE, crate::webdav::content_type_for(&entry.name))
        .header(
            header::CONTENT_DISPOSITION,
            format!(
                "attachment; filename=\"{}\"",
                entry.name.replace('"', "_")
            ),
        )
        .header(header::ACCEPT_RANGES, "bytes");
    if partial {
        let total = if len > 0 { len } else { entry.size.saturating_sub(offset) };
        builder = builder
            .header(header::CONTENT_LENGTH, total.to_string())
            .header(
                header::CONTENT_RANGE,
                format!(
                    "bytes {}-{}/{}",
                    offset,
                    offset + total.saturating_sub(1),
                    entry.size
                ),
            );
    } else {
        builder = builder.header(header::CONTENT_LENGTH, entry.size.to_string());
    }
    builder.body(Body::from_stream(stream)).unwrap()
}

/// `bytes=a-b` / `bytes=a-` / `bytes=-n` → (offset, len); len 0 = to EOF.
fn parse_http_range(spec: &str, size: u64) -> Option<(u64, u64)> {
    let spec = spec.trim().strip_prefix("bytes=")?;
    if let Some((a, b)) = spec.split_once('-') {
        if a.trim().is_empty() {
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

fn chunk_stream(
    rx: tokio::sync::mpsc::Receiver<Result<crate::bridge::FsChunk, String>>,
) -> impl Stream<Item = std::io::Result<axum::body::Bytes>> {
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

async fn bridge_upload(
    State(st): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Query(q): Query<PeerPath>,
    headers: HeaderMap,
    req: axum::extract::Request,
) -> Response {
    if let Err(e) = loopback_guard(&addr) {
        return e;
    }
    let size = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let name = q.path.rsplit('/').next().unwrap_or("file.bin").to_string();
    let rid = match st.bridge.write_open(&q.peer, &q.path, size).await {
        Ok(rid) => rid,
        Err(e) => return Json(json!({ "ok": false, "error": e })).into_response(),
    };

    let mut body = req.into_body().into_data_stream();
    let mut buf: Vec<u8> = Vec::with_capacity(crate::bridge::READ_CHUNK);
    while let Some(chunk) = body.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                return Json(json!({ "ok": false, "error": format!("upload body error: {e}") }))
                    .into_response()
            }
        };
        buf.extend_from_slice(&chunk);
        if buf.len() >= crate::bridge::READ_CHUNK {
            if let Err(e) = st
                .bridge
                .write_chunk(&q.peer, &rid, std::mem::take(&mut buf))
                .await
            {
                return Json(json!({ "ok": false, "error": e })).into_response();
            }
        }
    }
    if !buf.is_empty() {
        if let Err(e) = st.bridge.write_chunk(&q.peer, &rid, buf).await {
            return Json(json!({ "ok": false, "error": e })).into_response();
        }
    }
    match st.bridge.write_close(&q.peer, &rid).await {
        Ok(()) => Json(json!({ "ok": true, "name": name })).into_response(),
        Err(e) => Json(json!({ "ok": false, "error": e })).into_response(),
    }
}

async fn bridge_mkdir(
    State(st): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Query(q): Query<PeerPath>,
) -> Response {
    if let Err(e) = loopback_guard(&addr) {
        return e;
    }
    match st.bridge.mkdir(&q.peer, &q.path).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => Json(json!({ "ok": false, "error": e })).into_response(),
    }
}

// ---------------------------------------------------------------------------
// WebSocket control channel
// ---------------------------------------------------------------------------

async fn ws_upgrade(ws: WebSocketUpgrade, State(st): State<Arc<AppState>>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_session(socket, st))
}

async fn ws_session(socket: WebSocket, st: Arc<AppState>) {
    let (mut tx, mut rx) = socket.split();
    let mut bus_rx = st.bus.subscribe();

    // Bridge between the broadcast bus (multi-producer) and this socket.
    let (out_tx, mut out_rx) = mpsc::channel::<String>(256);
    let pump = tokio::spawn(async move {
        loop {
            match bus_rx.recv().await {
                Ok(msg) => {
                    if let Ok(s) = serde_json::to_string(&msg) {
                        if out_tx.send(s).await.is_err() {
                            break;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    // This client fell behind; ask it to re-sync.
                    let _ = out_tx
                        .send(serde_json::to_string(&ServerMsg::Log {
                            level: "warn".into(),
                            msg: format!("missed {n} events, refreshing…"),
                            ts: now_ms(),
                        })
                        .unwrap())
                        .await;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Initial snapshot: who we are, what's on the LAN, open sessions.
    let hello = ServerMsg::Hello {
        self_identity: st.self_identity.read().unwrap().clone(),
        service_type: SERVICE_TYPE.to_string(),
        server_time: now_ms(),
    };
    let _ = tx
        .send(Message::Text(serde_json::to_string(&hello).unwrap().into()))
        .await;
    let _ = tx
        .send(
            Message::Text(
                serde_json::to_string(&ServerMsg::Devices {
                    devices: st.registry.snapshot(),
                })
                .unwrap()
                .into(),
            ),
        )
        .await;
    for session in st.p2p.all_sessions(true) {
        let _ = tx
            .send(Message::Text(serde_json::to_string(&ServerMsg::Session { session }).unwrap().into()))
            .await;
    }
    // Inbound connection policy (Home › Protocol) — the browser mirrors it
    // into localStorage and re-applies it on every reconnect.
    let _ = tx
        .send(Message::Text(serde_json::to_string(&st.access.snapshot()).unwrap().into()))
        .await;
    // Files bridge snapshot (Files page + mounts).
    let _ = tx
        .send(
            Message::Text(
                serde_json::to_string(&ServerMsg::Bridge {
                    bridge: bridge_dto(&st),
                })
                .unwrap()
                .into(),
            ),
        )
        .await;

    loop {
        tokio::select! {
            Some(out) = out_rx.recv() => {
                if tx.send(Message::Text(out.into())).await.is_err() {
                    break;
                }
            }
            msg = rx.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        let client: Result<ClientMsg, _> = serde_json::from_str(text.as_str());
                        match client {
                            Ok(msg) => handle_client(&st, msg, &mut tx).await,
                            Err(e) => {
                                let _ = tx
                                    .send(Message::Text(
                                        serde_json::to_string(&ServerMsg::Log {
                                            level: "warn".into(),
                                            msg: format!("bad client message: {e}"),
                                            ts: now_ms(),
                                        })
                                        .unwrap()
                                        .into(),
                                    ))
                                    .await;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
            else => break,
        }
    }
    pump.abort();
}

async fn handle_client(st: &Arc<AppState>, msg: ClientMsg, tx: &mut SplitSink<WebSocket, Message>) {
    match msg {
        ClientMsg::Ping => {
            let _ = tx
                .send(Message::Text(
                    serde_json::to_string(&ServerMsg::Pong).unwrap().into(),
                ))
                .await;
        }
        ClientMsg::Refresh => {
            st.registry.broadcast_devices();
        }
        ClientMsg::Rename { name } => {
            let name = name.trim().chars().take(48).collect::<String>();
            if name.is_empty() {
                return;
            }
            if !st.cfg.announce {
                log_bus(&st.bus, "warn", "renaming disabled: --no-announce mode");
                return;
            }
            {
                let mut id = st.self_identity.write().unwrap();
                id.name = name.clone();
                match st.mdns.reannounce(&id) {
                    Ok(fullname) => {
                        id.id = fullname.clone();
                        *st.registry.self_id.write().unwrap() = fullname;
                    }
                    Err(e) => {
                        log_bus(&st.bus, "error", format!("mDNS re-announce failed: {e}"));
                    }
                }
            }
            let hello = ServerMsg::Hello {
                self_identity: st.self_identity.read().unwrap().clone(),
                service_type: SERVICE_TYPE.to_string(),
                server_time: now_ms(),
            };
            let _ = st.bus.send(hello);
            log_bus(
                &st.bus,
                "ok",
                format!("renamed — now broadcasting as “{name}” ({SERVICE_TYPE})"),
            );
        }
        ClientMsg::Connect { device_id, password } => match st.registry.get(&device_id) {
            Some(device) => st.p2p.connect_to(&device, &password),
            None => {
                log_bus(&st.bus, "warn", "connect: device no longer in registry");
            }
        },
        ClientMsg::SetAccess { mode, password } => {
            if let Err(e) = st.access.set(&mode, &password) {
                log_bus(&st.bus, "warn", format!("protocol change rejected: {e}"));
            }
            // Always echo the current policy so the dashboard self-corrects.
            let _ = st.bus.send(st.access.snapshot());
        }
        ClientMsg::ConnectReply { id, accept } => {
            if !st.access.respond(&id, accept) {
                log_bus(&st.bus, "warn", "connect reply arrived after the request expired");
            }
        }
        ClientMsg::SetShares { shares } => {
            st.bridge.set_roots(shares);
            log_bus(
                &st.bus,
                "ok",
                format!(
                    "sharing {} folder{} over the encrypted bridge",
                    st.bridge.shares().len(),
                    if st.bridge.shares().len() == 1 { "" } else { "s" }
                ),
            );
            let _ = st.bus.send(ServerMsg::Bridge {
                bridge: bridge_dto(st),
            });
        }
        ClientMsg::Disconnect { session_id } => {
            st.p2p.disconnect(&session_id);
        }
        ClientMsg::Chat { device_id, text } => {
            let text = text.trim().chars().take(2000).collect::<String>();
            if text.is_empty() {
                return;
            }
            if let Err(e) = st.p2p.send_chat(&device_id, &text).await {
                log_bus(&st.bus, "error", format!("chat: {e}"));
            }
        }
        ClientMsg::FileBegin {
            device_id,
            tid,
            name,
            size,
            sha256,
        } => {
            if let Err(e) = st
                .p2p
                .file_begin(&device_id, &tid, &name, size, &sha256)
                .await
            {
                log_bus(&st.bus, "error", format!("file-begin: {e}"));
            }
        }
        ClientMsg::FileChunk {
            device_id,
            tid,
            seq,
            data,
        } => {
            if let Err(e) = st.p2p.file_chunk(&device_id, &tid, seq, &data).await {
                log_bus(&st.bus, "error", format!("file-chunk: {e}"));
            }
        }
        ClientMsg::FileEnd { device_id, tid } => {
            if let Err(e) = st.p2p.file_end(&device_id, &tid).await {
                log_bus(&st.bus, "error", format!("file-end: {e}"));
            }
        }
    }
}
