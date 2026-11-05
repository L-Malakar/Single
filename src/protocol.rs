//! JSON message schemas for the three hops of the system:
//! 1. `ServerMsg` / `ClientMsg` — WebSocket control channel (browser ⇄ Rust server)
//! 2. `Wire` — newline-delimited JSON frames on the peer-to-peer TCP channel
//! 3. DTOs shared by both (`Identity`, `DeviceDto`, `SessionDto`, …)

use serde::{Deserialize, Serialize};

/// Who we are on the LAN (shown in the dashboard header, sent in handshakes).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Identity {
    /// mDNS service fullname, e.g. `"Pixel 8._p2pbridge._tcp.local."`
    pub id: String,
    /// Human display name.
    pub name: String,
    /// `android` | `windows` | `linux` | `macos` | `unknown`
    pub platform: String,
    /// Primary LAN IPv4.
    pub ip: String,
    /// P2P TCP port.
    pub p2p_port: u16,
}

/// A device discovered through mDNS browsing.
#[derive(Serialize, Clone, Debug)]
pub struct DeviceDto {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub ip: String,
    pub port: u16,
    pub hostname: String,
    pub first_seen: i64,
    pub last_seen: i64,
}

/// One entry of the application-level handshake timeline (SYN → SYN-ACK → ACK).
#[derive(Serialize, Clone, Debug)]
pub struct HandshakeStep {
    /// `connect` | `hello` | `hello-ack` | `ack` | `established` | `fail`
    pub step: String,
    pub label: String,
    pub detail: String,
    pub ts: i64,
    /// Duration of this step in ms (sub-ms precision), when meaningful.
    pub ms: Option<f64>,
}

/// Chat line in a P2P session.
#[derive(Serialize, Clone, Debug)]
pub struct ChatLine {
    /// `in` | `out` | `sys`
    pub dir: String,
    pub text: String,
    pub ts: i64,
}

/// File transfer progress inside a session.
#[derive(Serialize, Clone, Debug)]
pub struct TransferDto {
    pub tid: String,
    /// `in` | `out`
    pub direction: String,
    pub name: String,
    pub bytes: u64,
    pub total: u64,
    /// `in-progress` | `done` | `error`
    pub status: String,
    pub detail: String,
}

/// Live state of one P2P session, pushed to the dashboard.
#[derive(Serialize, Clone, Debug)]
pub struct SessionDto {
    pub peer_id: String,
    pub peer_name: String,
    pub peer_platform: String,
    /// Remote `ip:port`.
    pub peer_addr: String,
    /// `out` = we initiated, `in` = peer connected to us.
    pub direction: String,
    /// `connecting` | `established` | `closed` | `error`
    pub state: String,
    pub error: Option<String>,
    pub steps: Vec<HandshakeStep>,
    pub rtt_ms: Option<u64>,
    pub started_ts: i64,
    pub transfers: Vec<TransferDto>,
    /// Full chat history — `Some` only on session creation / initial sync,
    /// `None` on routine updates (chat lines arrive as separate `Chat` events).
    pub chat: Option<Vec<ChatLine>>,
}

// ---------------------------------------------------------------------------
// WebSocket: server → browser
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone, Debug)]
#[serde(tag = "type")]
pub enum ServerMsg {
    #[serde(rename = "hello")]
    Hello {
        self_identity: Identity,
        service_type: String,
        server_time: i64,
    },
    #[serde(rename = "devices")]
    Devices { devices: Vec<DeviceDto> },
    #[serde(rename = "session")]
    Session { session: SessionDto },
    #[serde(rename = "chat")]
    Chat { peer_id: String, line: ChatLine },
    #[serde(rename = "file")]
    File { peer_id: String, event: FileEvent },
    #[serde(rename = "log")]
    Log { level: String, msg: String, ts: i64 },
    #[serde(rename = "pong")]
    Pong,
}

/// Granular file-transfer event (progress bars in the chat panel).
#[derive(Serialize, Clone, Debug)]
pub struct FileEvent {
    pub tid: String,
    pub direction: String,
    /// `start` | `progress` | `done` | `error`
    pub status: String,
    pub name: String,
    pub bytes: u64,
    pub total: u64,
    pub detail: String,
}

// ---------------------------------------------------------------------------
// WebSocket: browser → server
// ---------------------------------------------------------------------------

#[derive(Deserialize, Clone, Debug)]
#[serde(tag = "type")]
pub enum ClientMsg {
    #[serde(rename = "ping")]
    Ping,
    #[serde(rename = "refresh")]
    Refresh,
    #[serde(rename = "rename")]
    Rename { name: String },
    #[serde(rename = "connect")]
    Connect { device_id: String },
    #[serde(rename = "disconnect")]
    Disconnect { session_id: String },
    #[serde(rename = "chat")]
    Chat { device_id: String, text: String },
    #[serde(rename = "file-begin")]
    FileBegin {
        device_id: String,
        tid: String,
        name: String,
        size: u64,
        /// Hex SHA-256; empty string skips verification (insecure origin fallback).
        sha256: String,
    },
    #[serde(rename = "file-chunk")]
    FileChunk {
        device_id: String,
        tid: String,
        seq: u64,
        /// Base64 payload, ~48 KB raw per chunk.
        data: String,
    },
    #[serde(rename = "file-end")]
    FileEnd { device_id: String, tid: String },
}

// ---------------------------------------------------------------------------
// P2P TCP channel: newline-delimited JSON frames between peers
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type")]
pub enum Wire {
    /// Initiator → responder. Application-level SYN.
    #[serde(rename = "hello")]
    Hello {
        magic: String,
        ver: u32,
        id: String,
        name: String,
        platform: String,
        nonce: String,
    },
    /// Responder → initiator. Application-level SYN-ACK.
    #[serde(rename = "hello-ack")]
    HelloAck {
        ok: bool,
        id: String,
        name: String,
        platform: String,
        echo_nonce: String,
        nonce: String,
    },
    /// Initiator → responder. Application-level ACK completing the handshake.
    #[serde(rename = "ack")]
    Ack { echo_nonce: String },
    /// Text chat.
    #[serde(rename = "msg")]
    Msg { text: String, ts: i64 },
    /// Latency probe; `t` is the initiator's timestamp in ms.
    #[serde(rename = "ping")]
    Ping { t: i64 },
    #[serde(rename = "pong")]
    Pong { t: i64 },
    /// File transfer: sender announces the file.
    #[serde(rename = "file-start")]
    FileStart {
        tid: String,
        name: String,
        size: u64,
        sha256: String,
    },
    /// File transfer: one base64 chunk.
    #[serde(rename = "file-chunk")]
    FileChunk { tid: String, seq: u64, data: String },
    /// Receiver → sender: ack of a chunk (drives the sender's progress bar).
    #[serde(rename = "file-progress")]
    FileProgress { tid: String, seq: u64, bytes: u64 },
    /// Sender closes a file transfer.
    #[serde(rename = "file-end")]
    FileEnd { tid: String },
    /// Receiver → sender: transfer finished (or failed).
    #[serde(rename = "file-done")]
    FileDone { tid: String, ok: bool, detail: String },
    /// Graceful close.
    #[serde(rename = "bye")]
    Bye { reason: String },
    /// Protocol error frame.
    #[serde(rename = "err")]
    Err { msg: String },
}
