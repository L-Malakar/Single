//! Peer-to-peer TCP channel.
//!
//! **Handshake** (application-level SYN / SYN-ACK / ACK over a fresh TCP
//! connection, newline-delimited JSON):
//!
//! ```text
//!   initiator                                 responder
//!   ──────── TCP connect (kernel SYN/ACK) ────────────────▶
//!   ── HELLO  {magic, ver, id, name, platform, nonce} ──▶
//!   ◀── HELLO_ACK {echo_nonce, nonce, …} ──────────────────
//!   ── ACK {echo_nonce} ────────────────────────────────▶
//!   channel ESTABLISHED → Msg / Ping / file frames / Bye
//! ```
//!
//! Nonces are echoed and validated on both sides (like TCP sequence numbers).
//! After the handshake each session runs a writer task (mpsc → socket), a
//! 3-second latency ping loop, and a reader task that fans chat / file /
//! state events onto the WebSocket broadcast bus.

use crate::access::{AccessControl, AccessMode, Approval, ConnectRequestInfo};
use crate::config::{MAGIC, PROTO_VER};
use crate::files::{self, InboundTransfer, MAX_FILE_BYTES};
use crate::protocol::*;
use crate::registry::Device;
use crate::util::{human_bytes, log_bus, now_ms, nonce};
use base64::Engine;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
/// Generous on purpose: in Private mode the responder may legally sit on the
/// HELLO_ACK for up to `APPROVAL_TIMEOUT` (30 s) while its operator decides,
/// so the initiator must out-wait that window instead of giving up after 5 s.
/// Genuine dead peers still fail fast at the TCP-connect stage.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(40);
const PING_INTERVAL: Duration = Duration::from_secs(3);
const CHAT_HISTORY_CAP: usize = 200;
const TRANSFER_LIST_CAP: usize = 12;

type WireLines = Lines<BufReader<tokio::net::tcp::OwnedReadHalf>>;

pub struct SessionState {
    pub peer_id: String,
    pub peer_name: String,
    pub peer_platform: String,
    pub peer_addr: String,
    pub direction: String,
    pub state: String,
    pub error: Option<String>,
    pub steps: Vec<HandshakeStep>,
    pub rtt_ms: Option<u64>,
    pub started_ts: i64,
    pub chat: Vec<ChatLine>,
    pub transfers: Vec<TransferDto>,
    /// Password to offer in HELLO when the peer runs in Protected mode.
    pub password: String,
    pub to_peer: Option<mpsc::Sender<Wire>>,
    inbound: Option<InboundTransfer>,
}

impl SessionState {
    fn new(peer: (String, String, String, String), direction: &str) -> Self {
        let (peer_id, peer_name, peer_platform, peer_addr) = peer;
        Self {
            peer_id,
            peer_name,
            peer_platform,
            peer_addr,
            direction: direction.to_string(),
            state: "connecting".to_string(),
            error: None,
            steps: Vec::new(),
            rtt_ms: None,
            started_ts: now_ms(),
            chat: Vec::new(),
            transfers: Vec::new(),
            password: String::new(),
            to_peer: None,
            inbound: None,
        }
    }
}

pub struct P2pManager {
    bus: broadcast::Sender<ServerMsg>,
    self_identity: Arc<RwLock<Identity>>,
    received_dir: PathBuf,
    access: Arc<AccessControl>,
    sessions: Arc<Mutex<HashMap<String, SessionState>>>,
}

impl P2pManager {
    pub fn new(
        bus: broadcast::Sender<ServerMsg>,
        self_identity: Arc<RwLock<Identity>>,
        received_dir: PathBuf,
        access: Arc<AccessControl>,
    ) -> Arc<Self> {
        Arc::new(Self {
            bus,
            self_identity,
            received_dir,
            access,
            sessions: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Bind the P2P TCP listener and start accepting peers.
    pub fn spawn_listener(self: &Arc<Self>, port: u16) -> std::io::Result<()> {
        let std_listener = std::net::TcpListener::bind(("0.0.0.0", port))?;
        std_listener.set_nonblocking(true)?;
        let listener = TcpListener::from_std(std_listener)?;
        let this = self.clone();
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((sock, addr)) => {
                        let this = this.clone();
                        tokio::spawn(this.handle_inbound(sock, addr));
                    }
                    Err(e) => {
                        log_bus(&this.bus, "error", format!("TCP accept failed: {e}"));
                        break;
                    }
                }
            }
        });
        Ok(())
    }

    pub fn session_count(&self) -> usize {
        self.sessions.lock().unwrap().len()
    }

    pub fn established_count(&self) -> usize {
        self.sessions
            .lock()
            .unwrap()
            .values()
            .filter(|s| s.state == "established")
            .count()
    }

    pub fn all_sessions(&self, include_chat: bool) -> Vec<SessionDto> {
        let map = self.sessions.lock().unwrap();
        let mut list: Vec<SessionDto> = map.values().map(|s| dto(s, include_chat)).collect();
        list.sort_by_key(|s| s.started_ts);
        list
    }

    // ------------------------------------------------------------------
    // Session bookkeeping
    // ------------------------------------------------------------------

    fn sessions_lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, SessionState>> {
        self.sessions.lock().unwrap()
    }

    fn is_current(&self, peer_id: &str, started_ts: i64) -> bool {
        self.sessions_lock()
            .get(peer_id)
            .map(|s| s.started_ts == started_ts)
            .unwrap_or(false)
    }

    fn broadcast_session(&self, peer_id: &str, include_chat: bool) {
        let dto = {
            let map = self.sessions_lock();
            map.get(peer_id).map(|s| dto(s, include_chat))
        };
        if let Some(session) = dto {
            let _ = self.bus.send(ServerMsg::Session { session });
        }
    }

    fn add_step(
        &self,
        peer_id: &str,
        step: &str,
        label: &str,
        detail: impl Into<String>,
        ms: Option<f64>,
    ) {
        {
            let mut map = self.sessions_lock();
            if let Some(s) = map.get_mut(peer_id) {
                s.steps.push(HandshakeStep {
                    step: step.to_string(),
                    label: label.to_string(),
                    detail: detail.into(),
                    ts: now_ms(),
                    ms,
                });
            }
        }
        self.broadcast_session(peer_id, false);
    }

    fn set_state(&self, peer_id: &str, state: &str, error: Option<String>) {
        {
            let mut map = self.sessions_lock();
            if let Some(s) = map.get_mut(peer_id) {
                s.state = state.to_string();
                s.error = error.clone();
            }
        }
        self.broadcast_session(peer_id, false);
    }

    fn push_chat_line(&self, peer_id: &str, dir: &str, text: impl Into<String>) -> Option<ChatLine> {
        let line = ChatLine {
            dir: dir.to_string(),
            text: text.into(),
            ts: now_ms(),
        };
        {
            let mut map = self.sessions_lock();
            if let Some(s) = map.get_mut(peer_id) {
                s.chat.push(line.clone());
                if s.chat.len() > CHAT_HISTORY_CAP {
                    s.chat.drain(..s.chat.len() - CHAT_HISTORY_CAP);
                }
            } else {
                return None;
            }
        }
        let _ = self.bus.send(ServerMsg::Chat {
            peer_id: peer_id.to_string(),
            line: line.clone(),
        });
        Some(line)
    }

    fn close_session(&self, peer_id: &str, reason: &str) {
        {
            let mut map = self.sessions_lock();
            if let Some(s) = map.get_mut(peer_id) {
                s.state = "closed".to_string();
                s.error = if reason.is_empty() { None } else { Some(reason.to_string()) };
                s.to_peer = None;
                s.inbound = None;
            }
        }
        self.broadcast_session(peer_id, false);
    }

    /// Replace a session (used when re-connecting to the same peer).
    fn drop_session(&self, peer_id: &str) {
        let old = {
            let mut map = self.sessions_lock();
            if let Some(s) = map.get_mut(peer_id) {
                if let Some(tx) = s.to_peer.take() {
                    let _ = tx.try_send(Wire::Bye {
                        reason: "reconnecting".to_string(),
                    });
                }
            }
            map.remove(peer_id)
        };
        if old.is_some() {
            log_bus(&self.bus, "info", format!("replacing session with {peer_id}"));
        }
    }

    fn fail_session(&self, peer_id: &str, msg: &str) {
        log_bus(&self.bus, "error", format!("session {peer_id}: {msg}"));
        self.set_state(peer_id, "error", Some(msg.to_string()));
    }

    // ------------------------------------------------------------------
    // Outgoing: dashboard-initiated connection
    // ------------------------------------------------------------------

    pub fn connect_to(self: &Arc<Self>, device: &Device, password: &str) {
        self.drop_session(&device.id);
        let mut state = SessionState::new(
            (
                device.id.clone(),
                device.name.clone(),
                device.platform.clone(),
                format!("{}:{}", device.ip, device.port),
            ),
            "out",
        );
        state.password = password.to_string();
        self.sessions_lock().insert(device.id.clone(), state);
        self.broadcast_session(&device.id, true);
        log_bus(
            &self.bus,
            "info",
            format!("initiating TCP handshake → {} @ {}:{}", device.name, device.ip, device.port),
        );
        let this = self.clone();
        let dev = device.clone();
        tokio::spawn(async move {
            this.initiate(dev).await;
        });
    }

    pub fn disconnect(&self, peer_id: &str) {
        {
            let mut map = self.sessions_lock();
            if let Some(s) = map.get_mut(peer_id) {
                if let Some(tx) = s.to_peer.take() {
                    let _ = tx.try_send(Wire::Bye {
                        reason: "closed by operator".to_string(),
                    });
                }
            }
        }
        self.close_session(peer_id, "");
    }

    async fn initiate(self: Arc<Self>, device: Device) {
        let peer_id = device.id.clone();

        // Step 1: TCP connect (kernel-level SYN / SYN-ACK / ACK).
        let t0 = std::time::Instant::now();
        let sock = match tokio::time::timeout(
            CONNECT_TIMEOUT,
            TcpStream::connect((device.ip.as_str(), device.port)),
        )
        .await
        {
            Err(_) => {
                self.fail_session(&peer_id, "connect timeout (is the peer online?)");
                return;
            }
            Ok(Err(e)) => {
                self.fail_session(&peer_id, &format!("TCP connect failed: {e}"));
                return;
            }
            Ok(Ok(s)) => s,
        };
        let connect_ms = t0.elapsed().as_secs_f64() * 1000.0;
        self.add_step(
            &peer_id,
            "connect",
            "TCP connect",
            format!("{}:{} · kernel SYN/SYN-ACK/ACK in {connect_ms:.1} ms", device.ip, device.port),
            Some(connect_ms),
        );

        let (rd, mut wr) = sock.into_split();
        let mut lines = BufReader::new(rd).lines();

        // Step 2: application-level SYN.
        let my_nonce = nonce();
        let ident = self.self_identity.read().unwrap().clone();
        let password = self
            .sessions_lock()
            .get(&peer_id)
            .map(|s| s.password.clone())
            .unwrap_or_default();
        let hello = Wire::Hello {
            magic: MAGIC.to_string(),
            ver: PROTO_VER,
            id: ident.id.clone(),
            name: ident.name.clone(),
            platform: ident.platform.clone(),
            nonce: my_nonce.clone(),
            password,
        };
        if write_wire(&mut wr, &hello).await.is_err() {
            self.fail_session(&peer_id, "failed to send HELLO");
            return;
        }
        self.add_step(&peer_id, "hello", "HELLO →", format!("nonce {my_nonce} (app-level SYN)"), None);

        // Step 3: application-level SYN-ACK.
        let ack = match read_handshake(&mut lines).await {
            ReadOutcome::Wire(w @ Wire::HelloAck { .. }) => w,
            ReadOutcome::Wire(_) => {
                self.fail_session(&peer_id, "expected HELLO_ACK, got a different frame");
                return;
            }
            ReadOutcome::Eof => {
                self.fail_session(&peer_id, "peer closed during handshake");
                return;
            }
            ReadOutcome::Io(e) | ReadOutcome::Bad(e) => {
                self.fail_session(&peer_id, &e);
                return;
            }
        };
        let Wire::HelloAck {
            ok,
            id: peer_fn,
            name: peer_name,
            platform: peer_platform,
            echo_nonce,
            nonce: peer_nonce,
            reason,
        } = ack
        else {
            unreachable!()
        };
        if !ok {
            self.fail_session(
                &peer_id,
                &match reason.as_str() {
                    "auth" => "auth-required: this device is protected — a password is needed".to_string(),
                    "declined" => "the device declined the connection".to_string(),
                    "timeout" => "approval timed out on the device".to_string(),
                    other if other.is_empty() => "HELLO_ACK rejected".to_string(),
                    other => format!("HELLO_ACK rejected ({other})"),
                },
            );
            return;
        }
        if echo_nonce != my_nonce {
            self.fail_session(&peer_id, "HELLO_ACK nonce mismatch");
            return;
        }
        let started_ts = self
            .sessions_lock()
            .get(&peer_id)
            .map(|s| s.started_ts)
            .unwrap_or(0);
        {
            let mut map = self.sessions_lock();
            if let Some(s) = map.get_mut(&peer_id) {
                s.peer_name = peer_name.clone();
                s.peer_platform = peer_platform.clone();
                s.peer_id = peer_fn.clone();
            }
        }
        self.add_step(
            &peer_id,
            "hello-ack",
            "← HELLO_ACK",
            format!("nonce echoed ok · peer “{peer_name}” [{peer_platform}]"),
            None,
        );

        // Step 4: application-level ACK.
        if write_wire(&mut wr, &Wire::Ack { echo_nonce: peer_nonce.clone() }).await.is_err() {
            self.fail_session(&peer_id, "failed to send ACK");
            return;
        }
        self.add_step(
            &peer_id,
            "ack",
            "ACK →",
            format!("echoed peer nonce {peer_nonce} — handshake complete"),
            None,
        );
        self.set_state(&peer_id, "established", None);
        self.add_step(
            &peer_id,
            "established",
            "ESTABLISHED",
            "channel ready — chat / files / latency ping",
            None,
        );
        let total_ms = t0.elapsed().as_secs_f64() * 1000.0;
        log_bus(
            &self.bus,
            "ok",
            format!("handshake complete with {peer_name} in {total_ms:.1} ms"),
        );

        self.run_established(wr, lines, &peer_fn, started_ts).await;
    }

    // ------------------------------------------------------------------
    // Incoming: peer connects to our TCP listener
    // ------------------------------------------------------------------

    async fn handle_inbound(self: Arc<Self>, sock: TcpStream, addr: std::net::SocketAddr) {
        let (rd, mut wr) = sock.into_split();
        let mut lines = BufReader::new(rd).lines();

        let hello = match read_handshake(&mut lines).await {
            ReadOutcome::Wire(w @ Wire::Hello { .. }) => w,
            ReadOutcome::Wire(_) => {
                let _ = write_wire(&mut wr, &Wire::Err { msg: "expected HELLO".into() }).await;
                return;
            }
            ReadOutcome::Eof => return,
            ReadOutcome::Io(_) | ReadOutcome::Bad(_) => return,
        };
        let Wire::Hello {
            magic,
            ver,
            id: peer_fn,
            name: peer_name,
            platform: peer_platform,
            nonce: peer_nonce,
            password: hello_password,
        } = hello
        else {
            unreachable!()
        };

        let my_id = self.self_identity.read().unwrap().id.clone();
        if magic != MAGIC || ver != PROTO_VER || peer_fn == my_id {
            let _ = write_wire(
                &mut wr,
                &Wire::HelloAck {
                    ok: false,
                    id: my_id,
                    name: String::new(),
                    platform: String::new(),
                    echo_nonce: String::new(),
                    nonce: String::new(),
                    reason: "invalid".into(),
                },
            )
            .await;
            log_bus(
                &self.bus,
                "warn",
                format!("rejected handshake from {addr}: magic/ver mismatch"),
            );
            return;
        }

        // ---- access gate (Home › Protocol) -------------------------------
        let mode = self.access.mode();
        let approved_note = match mode {
            AccessMode::Public => "public mode".to_string(),
            AccessMode::Protected => {
                if !self.access.password_matches(&hello_password) {
                    let _ = write_wire(
                        &mut wr,
                        &Wire::HelloAck {
                            ok: false,
                            id: my_id.clone(),
                            name: String::new(),
                            platform: String::new(),
                            echo_nonce: String::new(),
                            nonce: String::new(),
                            reason: "auth".into(),
                        },
                    )
                    .await;
                    log_bus(
                        &self.bus,
                        "warn",
                        format!("rejected handshake from {addr}: wrong or missing password (protected mode)"),
                    );
                    return;
                }
                "password accepted".to_string()
            }
            AccessMode::Private => {
                let approval = self
                    .access
                    .request_approval(ConnectRequestInfo {
                        name: peer_name.clone(),
                        platform: peer_platform.clone(),
                        addr: addr.to_string(),
                    })
                    .await;
                if approval != Approval::Accepted {
                    let _ = write_wire(
                        &mut wr,
                        &Wire::HelloAck {
                            ok: false,
                            id: my_id.clone(),
                            name: String::new(),
                            platform: String::new(),
                            echo_nonce: String::new(),
                            nonce: String::new(),
                            reason: approval.reason().into(),
                        },
                    )
                    .await;
                    return;
                }
                "approved by operator".to_string()
            }
        };

        // SYN-ACK.
        let my_nonce = nonce();
        let ident = self.self_identity.read().unwrap().clone();
        if write_wire(
            &mut wr,
            &Wire::HelloAck {
                ok: true,
                id: ident.id.clone(),
                name: ident.name.clone(),
                platform: ident.platform.clone(),
                echo_nonce: peer_nonce,
                nonce: my_nonce.clone(),
                reason: String::new(),
            },
        )
        .await
        .is_err()
        {
            return;
        }

        // ACK.
        match read_handshake(&mut lines).await {
            ReadOutcome::Wire(Wire::Ack { echo_nonce }) if echo_nonce == my_nonce => {}
            _ => {
                log_bus(&self.bus, "warn", format!("handshake ACK failed from {addr}"));
                return;
            }
        }

        // Register the inbound session.
        self.drop_session(&peer_fn);
        let state = SessionState::new(
            (
                peer_fn.clone(),
                peer_name.clone(),
                peer_platform.clone(),
                addr.to_string(),
            ),
            "in",
        );
        let started_ts = state.started_ts;
        {
            let mut map = self.sessions_lock();
            map.insert(peer_fn.clone(), state);
            if let Some(s) = map.get_mut(&peer_fn) {
                s.state = "established".to_string();
                s.steps.push(HandshakeStep {
                    step: "established".into(),
                    label: "ESTABLISHED".into(),
                    detail: format!("inbound HELLO/HELLO_ACK/ACK with “{peer_name}” · {approved_note}"),
                    ts: now_ms(),
                    ms: None,
                });
            }
        }
        self.broadcast_session(&peer_fn, true);
        self.push_chat_line(&peer_fn, "sys", format!("🔌 {peer_name} connected from {addr}"));
        log_bus(&self.bus, "ok", format!("inbound handshake complete: {peer_name} ({addr})"));

        self.run_established(wr, lines, &peer_fn, started_ts).await;
    }

    // ------------------------------------------------------------------
    // Established-session machinery shared by both directions
    // ------------------------------------------------------------------

    async fn run_established(
        self: Arc<Self>,
        wr: OwnedWriteHalf,
        mut lines: WireLines,
        peer_id: &str,
        started_ts: i64,
    ) {
        let (tx, rx) = mpsc::channel::<Wire>(256);
        {
            let mut map = self.sessions_lock();
            if let Some(s) = map.get_mut(peer_id) {
                s.to_peer = Some(tx.clone());
            }
        }

        // Writer: drains the mpsc queue onto the socket.
        let this_w = self.clone();
        let peer_id_w = peer_id.to_string();
        tokio::spawn(async move {
            this_w.run_writer(wr, rx, peer_id_w).await;
        });

        // Latency ping loop.
        let this_p = self.clone();
        let peer_id_p = peer_id.to_string();
        spawn_ping_loop(this_p, peer_id_p, started_ts);

        // Reader: this task, inline.
        loop {
            match lines.next_line().await {
                Ok(None) => {
                    if self.is_current(peer_id, started_ts) {
                        self.push_chat_line(peer_id, "sys", "⛔ connection closed by peer");
                        self.close_session(peer_id, "peer closed connection");
                        log_bus(&self.bus, "warn", format!("peer {peer_id} closed the connection"));
                    }
                    return;
                }
                Err(e) => {
                    if self.is_current(peer_id, started_ts) {
                        self.push_chat_line(peer_id, "sys", format!("⛔ connection error: {e}"));
                        self.close_session(peer_id, &format!("io error: {e}"));
                    }
                    return;
                }
                Ok(Some(line)) => {
                    if !self.is_current(peer_id, started_ts) {
                        return; // session was replaced; stop touching state
                    }
                    match serde_json::from_str::<Wire>(&line) {
                        Ok(wire) => {
                            if self.handle_wire(peer_id, started_ts, wire).await.is_break() {
                                return;
                            }
                        }
                        Err(e) => {
                            log_bus(
                                &self.bus,
                                "warn",
                                format!("bad frame from {peer_id}: {e}"),
                            );
                        }
                    }
                }
            }
        }
    }

    async fn run_writer(&self, mut wr: OwnedWriteHalf, mut rx: mpsc::Receiver<Wire>, _peer: String) {
        while let Some(wire) = rx.recv().await {
            if write_wire(&mut wr, &wire).await.is_err() {
                break;
            }
        }
    }

    /// Handle one frame from an established peer. Returns `ControlFlow::Break`
    /// when the session ended.
    async fn handle_wire(
        &self,
        peer_id: &str,
        started_ts: i64,
        wire: Wire,
    ) -> std::ops::ControlFlow<()> {
        let send = |this: &Self, w: Wire| {
            let tx = this
                .sessions_lock()
                .get(peer_id)
                .and_then(|s| s.to_peer.clone());
            async move {
                if let Some(tx) = tx {
                    let _ = tx.send(w).await;
                }
            }
        };
        match wire {
            Wire::Msg { text, ts } => {
                let line = ChatLine {
                    dir: "in".into(),
                    text,
                    ts,
                };
                {
                    let mut map = self.sessions_lock();
                    if let Some(s) = map.get_mut(peer_id) {
                        s.chat.push(line.clone());
                        if s.chat.len() > CHAT_HISTORY_CAP {
                            s.chat.drain(..s.chat.len() - CHAT_HISTORY_CAP);
                        }
                    }
                }
                let _ = self.bus.send(ServerMsg::Chat {
                    peer_id: peer_id.to_string(),
                    line,
                });
            }
            Wire::Ping { t } => {
                send(self, Wire::Pong { t }).await;
            }
            Wire::Pong { t } => {
                let rtt = (now_ms() - t).max(0) as u64;
                let mut changed = false;
                {
                    let mut map = self.sessions_lock();
                    if let Some(s) = map.get_mut(peer_id) {
                        if s.rtt_ms != Some(rtt) {
                            s.rtt_ms = Some(rtt);
                            changed = true;
                        }
                    }
                }
                if changed {
                    self.broadcast_session(peer_id, false);
                }
            }
            Wire::FileStart {
                tid,
                name,
                size,
                sha256,
            } => {
                if size > MAX_FILE_BYTES {
                    send(
                        self,
                        Wire::FileDone {
                            tid: tid.clone(),
                            ok: false,
                            detail: "file too large".into(),
                        },
                    )
                    .await;
                    return std::ops::ControlFlow::Continue(());
                }
                {
                    let mut map = self.sessions_lock();
                    if let Some(s) = map.get_mut(peer_id) {
                        s.inbound = Some(InboundTransfer {
                            tid: tid.clone(),
                            name: name.clone(),
                            size,
                            sha256,
                            buf: Vec::with_capacity(size as usize),
                        });
                        push_transfer(
                            s,
                            TransferDto {
                                tid: tid.clone(),
                                direction: "in".into(),
                                name: name.clone(),
                                bytes: 0,
                                total: size,
                                status: "in-progress".into(),
                                detail: String::new(),
                            },
                        );
                    }
                }
                self.broadcast_session(peer_id, false);
                let detail = format!("receiving “{name}” ({})", human_bytes(size));
                let _ = self.bus.send(ServerMsg::File {
                    peer_id: peer_id.to_string(),
                    event: FileEvent {
                        tid,
                        direction: "in".into(),
                        status: "start".into(),
                        name,
                        bytes: 0,
                        total: size,
                        detail,
                    },
                });
            }
            Wire::FileChunk { tid, seq, data } => {
                let decoded = base64::engine::general_purpose::STANDARD.decode(&data);
                let mut bytes_now = 0u64;
                let mut total = 0u64;
                match decoded {
                    Ok(bytes) => {
                        let mut map = self.sessions_lock();
                        if let Some(s) = map.get_mut(peer_id) {
                            if let Some(tr) = s.inbound.as_mut() {
                                if tr.tid == tid {
                                    tr.buf.extend_from_slice(&bytes);
                                    bytes_now = tr.received();
                                    total = tr.size;
                                }
                            }
                            if let Some(t) = s.transfers.iter_mut().find(|t| t.tid == tid && t.direction == "in") {
                                t.bytes = bytes_now;
                            }
                        }
                    }
                    Err(e) => {
                        send(
                            self,
                            Wire::FileDone {
                                tid: tid.clone(),
                                ok: false,
                                detail: format!("bad base64: {e}"),
                            },
                        )
                        .await;
                    }
                }
                if bytes_now > 0 {
                    self.broadcast_session(peer_id, false);
                    let _ = self.bus.send(ServerMsg::File {
                        peer_id: peer_id.to_string(),
                        event: FileEvent {
                            tid: tid.clone(),
                            direction: "in".into(),
                            status: "progress".into(),
                            name: String::new(),
                            bytes: bytes_now,
                            total,
                            detail: String::new(),
                        },
                    });
                    // Chunk ack drives the sender's progress bar.
                    send(
                        self,
                        Wire::FileProgress {
                            tid,
                            seq,
                            bytes: bytes_now,
                        },
                    )
                    .await;
                }
            }
            Wire::FileProgress { tid, bytes, .. } => {
                // Progress of an outbound transfer (we are the sender).
                let mut updated: Option<TransferDto> = None;
                {
                    let mut map = self.sessions_lock();
                    if let Some(s) = map.get_mut(peer_id) {
                        if let Some(t) = s
                            .transfers
                            .iter_mut()
                            .find(|t| t.tid == tid && t.direction == "out")
                        {
                            t.bytes = bytes;
                            updated = Some(t.clone());
                        }
                    }
                }
                if let Some(t) = updated {
                    let _ = self.bus.send(ServerMsg::File {
                        peer_id: peer_id.to_string(),
                        event: FileEvent {
                            tid: t.tid,
                            direction: "out".into(),
                            status: "progress".into(),
                            name: t.name,
                            bytes: t.bytes,
                            total: t.total,
                            detail: String::new(),
                        },
                    });
                }
            }
            Wire::FileEnd { tid } => {
                let inbound = {
                    let mut map = self.sessions_lock();
                    map.get_mut(peer_id).and_then(|s| s.inbound.take())
                };
                if let Some(tr) = inbound {
                    if tr.tid != tid {
                        // stale; keep nothing
                    } else {
                        let sha_ok =
                            tr.sha256.is_empty() || files::sha256_hex(&tr.buf) == tr.sha256;
                        let (ok, detail) = if tr.buf.len() as u64 != tr.size {
                            (false, "size mismatch".to_string())
                        } else if !sha_ok {
                            (false, "sha256 mismatch".to_string())
                        } else {
                            match files::save(&self.received_dir, &tr.name, &tr.buf) {
                                Ok(path) => (true, path.display().to_string()),
                                Err(e) => (false, format!("save failed: {e}")),
                            }
                        };
                        send(
                            self,
                            Wire::FileDone {
                                tid: tid.clone(),
                                ok,
                                detail: detail.clone(),
                            },
                        )
                        .await;
                        {
                            let mut map = self.sessions_lock();
                            if let Some(s) = map.get_mut(peer_id) {
                                if let Some(t) = s
                                    .transfers
                                    .iter_mut()
                                    .find(|t| t.tid == tid && t.direction == "in")
                                {
                                    t.status = if ok { "done" } else { "error" }.into();
                                    t.bytes = tr.buf.len() as u64;
                                    t.detail = detail.clone();
                                }
                            }
                        }
                        if ok {
                            self.push_chat_line(
                                peer_id,
                                "sys",
                                format!(
                                    "📥 received “{}” ({}) → {}",
                                    tr.name,
                                    human_bytes(tr.buf.len() as u64),
                                    detail
                                ),
                            );
                            log_bus(
                                &self.bus,
                                "ok",
                                format!("file received: {} ({})", tr.name, human_bytes(tr.buf.len() as u64)),
                            );
                        } else {
                            self.push_chat_line(
                                peer_id,
                                "sys",
                                format!("⚠ file “{}” rejected: {}", tr.name, detail),
                            );
                        }
                        self.broadcast_session(peer_id, false);
                    }
                }
            }
            Wire::FileDone { tid, ok, detail } => {
                let updated = {
                    let mut map = self.sessions_lock();
                    let mut found = None;
                    if let Some(s) = map.get_mut(peer_id) {
                        if let Some(t) = s
                            .transfers
                            .iter_mut()
                            .find(|t| t.tid == tid && t.direction == "out")
                        {
                            t.status = if ok { "done" } else { "error" }.into();
                            t.detail = detail.clone();
                            if ok && t.bytes < t.total {
                                t.bytes = t.total;
                            }
                            found = Some(t.clone());
                        }
                    }
                    found
                };
                if let Some(t) = updated {
                    let _ = self.bus.send(ServerMsg::File {
                        peer_id: peer_id.to_string(),
                        event: FileEvent {
                            tid: t.tid,
                            direction: "out".into(),
                            status: if ok { "done" } else { "error" }.into(),
                            name: t.name,
                            bytes: t.bytes,
                            total: t.total,
                            detail: detail.clone(),
                        },
                    });
                }
                if ok {
                    self.push_chat_line(peer_id, "sys", format!("📤 transfer complete: {detail}"));
                } else {
                    self.push_chat_line(peer_id, "sys", format!("⚠ transfer failed: {detail}"));
                }
                self.broadcast_session(peer_id, false);
            }
            Wire::Bye { reason } => {
                if self.is_current(peer_id, started_ts) {
                    self.push_chat_line(peer_id, "sys", format!("👋 peer said bye ({reason})"));
                    self.close_session(peer_id, &reason);
                }
                return std::ops::ControlFlow::Break(());
            }
            Wire::Err { msg } => {
                log_bus(&self.bus, "warn", format!("peer {peer_id} error frame: {msg}"));
            }
            _ => {
                log_bus(&self.bus, "warn", format!("unexpected frame from {peer_id}"));
            }
        }
        std::ops::ControlFlow::Continue(())
    }

    // ------------------------------------------------------------------
    // Dashboard → peer operations
    // ------------------------------------------------------------------

    pub async fn send_chat(&self, device_id: &str, text: &str) -> Result<(), String> {
        let tx = {
            let map = self.sessions_lock();
            map.get(device_id)
                .filter(|s| s.state == "established")
                .and_then(|s| s.to_peer.clone())
        };
        let tx = tx.ok_or("no established session with this device")?;
        tx.send(Wire::Msg {
            text: text.to_string(),
            ts: now_ms(),
        })
        .await
        .map_err(|_| "send failed (connection lost)".to_string())?;
        self.push_chat_line(device_id, "out", text);
        Ok(())
    }

    pub async fn file_begin(
        &self,
        device_id: &str,
        tid: &str,
        name: &str,
        size: u64,
        sha256: &str,
    ) -> Result<(), String> {
        let tx = {
            let mut map = self.sessions_lock();
            let s = map
                .get_mut(device_id)
                .ok_or("no session with this device")?;
            if s.state != "established" {
                return Err("session not established".into());
            }
            push_transfer(
                s,
                TransferDto {
                    tid: tid.to_string(),
                    direction: "out".into(),
                    name: name.to_string(),
                    bytes: 0,
                    total: size,
                    status: "in-progress".into(),
                    detail: String::new(),
                },
            );
            s.to_peer.clone()
        };
        let tx = tx.ok_or("connection lost")?;
        self.broadcast_session(device_id, false);
        tx.send(Wire::FileStart {
            tid: tid.to_string(),
            name: name.to_string(),
            size,
            sha256: sha256.to_string(),
        })
        .await
        .map_err(|_| "send failed".to_string())?;
        Ok(())
    }

    pub async fn file_chunk(&self, device_id: &str, tid: &str, seq: u64, data: &str) -> Result<(), String> {
        let tx = {
            let map = self.sessions_lock();
            map.get(device_id)
                .filter(|s| s.state == "established")
                .and_then(|s| s.to_peer.clone())
        };
        let tx = tx.ok_or("connection lost")?;
        tx.send(Wire::FileChunk {
            tid: tid.to_string(),
            seq,
            data: data.to_string(),
        })
        .await
        .map_err(|_| "send failed".to_string())?;
        Ok(())
    }

    pub async fn file_end(&self, device_id: &str, tid: &str) -> Result<(), String> {
        let tx = {
            let map = self.sessions_lock();
            map.get(device_id)
                .filter(|s| s.state == "established")
                .and_then(|s| s.to_peer.clone())
        };
        let tx = tx.ok_or("connection lost")?;
        tx.send(Wire::FileEnd { tid: tid.to_string() })
            .await
            .map_err(|_| "send failed".to_string())?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn dto(s: &SessionState, include_chat: bool) -> SessionDto {
    SessionDto {
        peer_id: s.peer_id.clone(),
        peer_name: s.peer_name.clone(),
        peer_platform: s.peer_platform.clone(),
        peer_addr: s.peer_addr.clone(),
        direction: s.direction.clone(),
        state: s.state.clone(),
        error: s.error.clone(),
        steps: s.steps.clone(),
        rtt_ms: s.rtt_ms,
        started_ts: s.started_ts,
        transfers: s.transfers.clone(),
        chat: if include_chat {
            Some(s.chat.clone())
        } else {
            None
        },
    }
}

fn push_transfer(s: &mut SessionState, t: TransferDto) {
    // Drop the oldest finished transfer beyond the cap.
    if s.transfers.len() >= TRANSFER_LIST_CAP {
        if let Some(pos) = s
            .transfers
            .iter()
            .position(|x| x.status != "in-progress")
            .or(Some(0))
        {
            s.transfers.remove(pos);
        }
    }
    if let Some(existing) = s.transfers.iter_mut().find(|x| x.tid == t.tid) {
        *existing = t;
    } else {
        s.transfers.push(t);
    }
}

enum ReadOutcome {
    Wire(Wire),
    Eof,
    Io(String),
    Bad(String),
}

async fn read_handshake(lines: &mut WireLines) -> ReadOutcome {
    match tokio::time::timeout(HANDSHAKE_TIMEOUT, lines.next_line()).await {
        Err(_) => ReadOutcome::Io("handshake timeout".to_string()),
        Ok(Err(e)) => ReadOutcome::Io(e.to_string()),
        Ok(Ok(None)) => ReadOutcome::Eof,
        Ok(Ok(Some(line))) => match serde_json::from_str::<Wire>(&line) {
            Ok(w) => ReadOutcome::Wire(w),
            Err(e) => ReadOutcome::Bad(format!("bad frame: {e}")),
        },
    }
}

async fn write_wire(wr: &mut OwnedWriteHalf, wire: &Wire) -> std::io::Result<()> {
    let mut s = serde_json::to_string(wire)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    s.push('\n');
    wr.write_all(s.as_bytes()).await
}

fn spawn_ping_loop(this: Arc<P2pManager>, peer_id: String, started_ts: i64) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(PING_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tick.tick().await;
        loop {
            tick.tick().await;
            let tx = {
                let map = this.sessions_lock();
                map.get(&peer_id)
                    .filter(|s| s.state == "established" && s.started_ts == started_ts)
                    .and_then(|s| s.to_peer.clone())
            };
            match tx {
                Some(tx) => {
                    if tx.send(Wire::Ping { t: now_ms() }).await.is_err() {
                        break;
                    }
                }
                None => break,
            }
        }
    });
}
