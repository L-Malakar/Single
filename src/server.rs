//! Axum HTTP + WebSocket server.
//!
//! Routes:
//! - `GET /`        — embedded single-page dashboard (or `--static-dir` from disk)
//! - `GET /style.css`, `/app.js`
//! - `GET /ws`      — WebSocket control channel (live devices / sessions / chat / files)
//! - `GET /api/health` — JSON status probe

use crate::config::{Config, SERVICE_TYPE};
use crate::mdns::Mdns;
use crate::p2p::P2pManager;
use crate::protocol::*;
use crate::registry::Registry;
use crate::util::{log_bus, now_ms};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::header;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::{Json, Router};
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::sync::{Arc, RwLock};
use tokio::sync::{broadcast, mpsc};

// The dashboard is embedded into the binary at compile time, so the server
// ships as a single self-contained executable. `--static-dir` overrides it
// for live UI development without rebuilds.
const INDEX_HTML: &str = include_str!("../static/index.html");
const STYLE_CSS: &str = include_str!("../static/style.css");
const APP_JS: &str = include_str!("../static/app.js");

pub struct AppState {
    pub cfg: Config,
    pub bus: broadcast::Sender<ServerMsg>,
    pub registry: Registry,
    pub mdns: Arc<Mdns>,
    pub self_identity: Arc<RwLock<Identity>>,
    pub p2p: Arc<P2pManager>,
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
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
}

fn build_router(state: Arc<AppState>) -> Router {
    let mut app = Router::new()
        .route("/ws", get(ws_upgrade))
        .route("/api/health", get(health));

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
    }))
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
        ClientMsg::Connect { device_id } => match st.registry.get(&device_id) {
            Some(device) => st.p2p.connect_to(&device),
            None => {
                log_bus(&st.bus, "warn", "connect: device no longer in registry");
            }
        },
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
