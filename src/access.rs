//! Inbound connection policy — the "Protocol" switch on the Home page.
//!
//! Three modes:
//! - [`AccessMode::Public`]    — any device may connect.
//! - [`AccessMode::Private`]   — every inbound handshake pauses until the
//!   operator accepts or declines it in the dashboard ([`APPROVAL_TIMEOUT`]
//!   elapsing counts as a decline).
//! - [`AccessMode::Protected`] — any device may connect, but its HELLO frame
//!   must carry the password chosen by the operator.
//!
//! The dashboard sets the mode with a `set-access` WebSocket message; the
//! choice lives in the browser (localStorage) so it survives server restarts
//! and is re-applied every time a dashboard reconnects. The password is kept
//! in server memory only.

use crate::protocol::ServerMsg;
use crate::util::{log_bus, nonce, now_ms};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::{broadcast, oneshot};

/// How long an unanswered Private-mode request waits before auto-declining.
/// The initiating peer's handshake timeout must be comfortably larger.
pub const APPROVAL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AccessMode {
    Public,
    Private,
    Protected,
}

impl AccessMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Private => "private",
            Self::Protected => "protected",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "public" => Some(Self::Public),
            "private" => Some(Self::Private),
            "protected" => Some(Self::Protected),
            _ => None,
        }
    }
}

/// Outcome of a Private-mode approval request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Approval {
    Accepted,
    Declined,
    TimedOut,
}

impl Approval {
    /// `reason` value sent in the rejecting HELLO_ACK frame.
    pub fn reason(self) -> &'static str {
        match self {
            Self::Accepted => "",
            Self::Declined => "declined",
            Self::TimedOut => "timeout",
        }
    }
}

/// Who is asking to connect (shown in the dashboard approval dialog).
#[derive(Clone, Debug)]
pub struct ConnectRequestInfo {
    pub name: String,
    pub platform: String,
    pub addr: String,
}

struct PendingApproval {
    tx: oneshot::Sender<Approval>,
}

pub struct AccessControl {
    bus: broadcast::Sender<ServerMsg>,
    mode: RwLock<AccessMode>,
    password: RwLock<String>,
    pending: Mutex<HashMap<String, PendingApproval>>,
}

impl AccessControl {
    pub fn new(bus: broadcast::Sender<ServerMsg>) -> Arc<Self> {
        Arc::new(Self {
            bus,
            mode: RwLock::new(AccessMode::Private), // default: ask first
            password: RwLock::new(String::new()),
            pending: Mutex::new(HashMap::new()),
        })
    }

    pub fn mode(&self) -> AccessMode {
        *self.mode.read().unwrap()
    }

    /// Current policy as a broadcastable WS snapshot.
    pub fn snapshot(&self) -> ServerMsg {
        ServerMsg::AccessState {
            mode: self.mode().as_str().to_string(),
            has_password: !self.password.read().unwrap().is_empty(),
        }
    }

    /// Apply a `set-access` command from the dashboard. The password is
    /// optional — an empty one keeps the previously stored value (so the
    /// browser can re-apply its saved state without resending the secret).
    pub fn set(&self, mode: &str, password: &str) -> Result<AccessMode, String> {
        let mode = AccessMode::parse(mode).ok_or_else(|| format!("unknown mode “{mode}”"))?;
        if mode == AccessMode::Protected {
            let stored = self.password.read().unwrap();
            if password.is_empty() && stored.is_empty() {
                return Err("protected mode needs a password — set one first".into());
            }
        }
        *self.mode.write().unwrap() = mode;
        if !password.is_empty() {
            *self.password.write().unwrap() = password.to_string();
        }
        let _ = self.bus.send(self.snapshot());
        Ok(mode)
    }

    /// Protected-mode gate: does the HELLO's password match ours?
    pub fn password_matches(&self, candidate: &str) -> bool {
        let stored = self.password.read().unwrap();
        !stored.is_empty() && stored.as_str() == candidate
    }

    /// Private-mode gate: surface the request in the dashboard and wait.
    /// Returns the operator's decision (or [`Approval::TimedOut`]).
    pub async fn request_approval(&self, info: ConnectRequestInfo) -> Approval {
        let id = format!("req-{}", nonce());
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id.clone(), PendingApproval { tx });

        let _ = self.bus.send(ServerMsg::ConnectRequest {
            id: id.clone(),
            name: info.name.clone(),
            platform: info.platform.clone(),
            addr: info.addr.clone(),
            ts: now_ms(),
        });
        log_bus(
            &self.bus,
            "info",
            format!("connection request from “{}” ({}) — waiting for approval ({} s)",
                info.name, info.addr, APPROVAL_TIMEOUT.as_secs()),
        );

        let outcome = match tokio::time::timeout(APPROVAL_TIMEOUT, rx).await {
            Ok(Ok(approval)) => approval,
            Ok(Err(_)) | Err(_) => Approval::TimedOut,
        };
        self.pending.lock().unwrap().remove(&id); // housekeeping (no-op if answered)

        match outcome {
            Approval::Accepted => {
                log_bus(&self.bus, "ok", format!("connection with “{}” approved", info.name));
            }
            other => {
                log_bus(
                    &self.bus,
                    "warn",
                    format!("connection with “{}” {} — handshake rejected",
                        info.name,
                        if other == Approval::Declined { "declined" } else { "timed out" }),
                );
            }
        }
        outcome
    }

    /// Dashboard answered a `connect-request`. Returns false when the id is
    /// unknown (already answered or expired).
    pub fn respond(&self, id: &str, accept: bool) -> bool {
        if let Some(p) = self
            .pending
            .lock()
            .unwrap()
            .remove(id)
        {
            let _ = p.tx.send(if accept { Approval::Accepted } else { Approval::Declined });
            true
        } else {
            false
        }
    }
}
