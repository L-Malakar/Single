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
    /// True once the session runs on AES-256-GCM sealed frames (both sides
    /// completed the X25519 key exchange during the handshake).
    pub encrypted: bool,
    pub error: Option<String>,
    pub steps: Vec<HandshakeStep>,
    pub rtt_ms: Option<u64>,
    pub started_ts: i64,
    pub transfers: Vec<TransferDto>,
    /// Full chat history — `Some` only on session creation / initial sync,
    /// `None` on routine updates (chat lines arrive as separate `Chat` events).
    pub chat: Option<Vec<ChatLine>>,
}

/// One entry of a remote directory listing (files bridge).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct FsEntryDto {
    /// `dir` | `file`
    pub kind: String,
    pub name: String,
    /// Bytes (files only, 0 for dirs).
    pub size: u64,
    /// Modified time, ms since epoch (0 when unknown).
    pub modified: i64,
}

/// A folder this device shares with connected peers.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ShareRootDto {
    /// How the folder appears in the remote file browser.
    pub title: String,
    /// Absolute local path.
    pub path: String,
}

/// One connected device as seen by the Files page / OS mounts.
#[derive(Serialize, Clone, Debug)]
pub struct BridgePeerDto {
    pub peer_id: String,
    pub peer_name: String,
    pub peer_platform: String,
    /// `established` | anything else → not bridgeable yet.
    pub state: String,
    pub encrypted: bool,
    /// Windows only: drive letter currently mapped in This PC (e.g. `"Z:"`),
    /// `None` on other platforms or while mapping.
    pub drive: Option<String>,
}

/// Full snapshot of the file-bridge state, pushed to dashboards and served
/// by `GET /api/bridge/status` (the Android documents provider uses it too).
#[derive(Serialize, Clone, Debug)]
pub struct BridgeDto {
    /// Folders this device shares with peers.
    pub shares: Vec<ShareRootDto>,
    /// Connected peers (all sessions, any state).
    pub peers: Vec<BridgePeerDto>,
    /// Loopback WebDAV port (`http://127.0.0.1:<port>/dav/<peer>`), Windows
    /// maps one drive letter per peer from it.
    pub dav_port: u16,
    /// True on Windows when the drive-mapping machinery is active.
    pub drives_enabled: bool,
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
    /// Current inbound policy (Home › Protocol), pushed on connect + on change.
    #[serde(rename = "access-state")]
    AccessState { mode: String, has_password: bool },
    /// Files-bridge snapshot (shared folders + connected peers + drive
    /// letters), pushed on change and on connect.
    #[serde(rename = "bridge")]
    Bridge { bridge: BridgeDto },
    /// Private mode: a peer wants in — the dashboard must accept / decline.
    #[serde(rename = "connect-request")]
    ConnectRequest {
        id: String,
        name: String,
        platform: String,
        addr: String,
        ts: i64,
    },
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
    Connect {
        device_id: String,
        /// Only used when the target device runs in Protected mode.
        #[serde(default)]
        password: String,
    },
    /// Home › Protocol — set the inbound connection policy.
    #[serde(rename = "set-access")]
    SetAccess {
        mode: String,
        /// Empty = keep whatever password the server already holds.
        #[serde(default)]
        password: String,
    },
    /// Answer to a Private-mode `connect-request`.
    #[serde(rename = "connect-reply")]
    ConnectReply { id: String, accept: bool },
    /// Files page — replace the list of shared folders.
    #[serde(rename = "set-shares")]
    SetShares { shares: Vec<ShareRootDto> },
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
        /// Only meaningful when the responder runs in Protected mode.
        #[serde(default)]
        password: String,
        /// Base64 X25519 ephemeral public key. When both sides send one, every
        /// frame after the ACK travels inside AES-256-GCM envelopes (`Sealed`).
        #[serde(default)]
        e2ee: Option<String>,
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
        /// When `ok` is false: `auth` (password needed/wrong), `declined`
        /// (Private mode, operator said no) or `timeout` (nobody answered).
        #[serde(default)]
        reason: String,
        /// Base64 X25519 ephemeral public key (see [`Wire::Hello`]).
        #[serde(default)]
        e2ee: Option<String>,
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

    // ------------------------------------------------------------------
    // Encrypted envelope: after a successful ECDH both sides wrap every
    // further frame. The plaintext is the JSON of the inner [`Wire`].
    // ------------------------------------------------------------------
    #[serde(rename = "e")]
    Sealed {
        /// Base64 12-byte AES-GCM nonce.
        n: String,
        /// Base64 ciphertext || 16-byte tag.
        c: String,
    },

    // ------------------------------------------------------------------
    // Files bridge — request/response frames multiplexed by `rid` over the
    // (usually sealed) P2P channel. Paths are logical: "/" lists the peer's
    // shared roots, "/<root>/…" descends into one.
    // ------------------------------------------------------------------
    /// List a directory.
    #[serde(rename = "fs-list-req")]
    FsListReq { rid: String, path: String },
    #[serde(rename = "fs-list-res")]
    FsListRes {
        rid: String,
        ok: bool,
        #[serde(default)]
        error: String,
        #[serde(default)]
        entries: Vec<FsEntryDto>,
    },
    /// Stat a single path.
    #[serde(rename = "fs-stat-req")]
    FsStatReq { rid: String, path: String },
    #[serde(rename = "fs-stat-res")]
    FsStatRes {
        rid: String,
        ok: bool,
        #[serde(default)]
        error: String,
        #[serde(default)]
        entry: Option<FsEntryDto>,
    },
    /// Read `len` bytes from `offset` (`len` = 0 → to EOF); answered with a
    /// stream of `fs-data` frames, optionally terminated by `fs-err`.
    #[serde(rename = "fs-read-req")]
    FsReadReq { rid: String, path: String, offset: u64, len: u64 },
    #[serde(rename = "fs-data")]
    FsData {
        rid: String,
        seq: u64,
        /// Base64 payload chunk (≤ 256 KB).
        data: String,
        eof: bool,
    },
    /// Aborts a pending `rid` (read or write).
    #[serde(rename = "fs-err")]
    FsErr { rid: String, error: String },
    /// Create / truncate a file on the peer, then stream `fs-data` frames
    /// (same shape as reads) and close with `fs-write-end`.
    #[serde(rename = "fs-write-start")]
    FsWriteStart { rid: String, path: String, size: u64 },
    #[serde(rename = "fs-write-end")]
    FsWriteEnd { rid: String },
    /// Generic success/failure reply for mkdir / delete / write-end.
    #[serde(rename = "fs-ok")]
    FsOk {
        rid: String,
        ok: bool,
        #[serde(default)]
        error: String,
    },
    #[serde(rename = "fs-mkdir-req")]
    FsMkdirReq { rid: String, path: String },
    #[serde(rename = "fs-delete-req")]
    FsDeleteReq {
        rid: String,
        path: String,
        recursive: bool,
    },
}
