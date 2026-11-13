//! Pairing state machine ("PIN + approval").
//!
//! Two devices pair like this:
//!
//! ```text
//!  A (initiator)                          B (target)
//!  -------------                          ----------
//!  POST /single/pair/request  --------->  popup: "DeviceX wants to pair"
//!                                         PIN 483921 shown
//!  (A's user types the PIN)               user clicks [Allow] or [Deny]
//!  POST /single/pair/verify   --------->  pin matches -> pairing stored on B
//!  POST /single/pair/check    <-------    status=verified + shared token
//!  pairing stored on A
//! ```
//!
//! The order of "approval" and "PIN entry" may be swapped; the state machine
//! handles both. The shared token is derived on both sides from
//! `SHA-256(both fingerprints + pin)` and only ever travels inside TLS.

use crate::identity::derive_token;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, Mutex as AsyncMutex};

const REQUEST_TTL_SECS: u64 = 180;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pairing {
    pub id: String,
    pub name: String,
    pub ca_fp: String,
    pub token: String,
    pub added_at: u64,
    #[serde(default)]
    pub drive_letter: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReqState {
    Pending,
    Approved,
    Denied,
    Verified,
}

#[derive(Debug, Clone, Serialize)]
pub struct IncomingReq {
    pub req_id: String,
    pub id: String,
    pub name: String,
    pub ca_fp: String,
    pub pin: String,
    pub created: u64,
    pub state: ReqState,
    pub pin_ok: bool,
}

impl IncomingReq {
    pub fn state_str(&self) -> &'static str {
        match self.state {
            ReqState::Pending => "pending",
            ReqState::Approved => "approved",
            ReqState::Denied => "denied",
            ReqState::Verified => "verified",
        }
    }
}

/// A-side record of a pairing attempt.
#[derive(Debug, Clone, Serialize)]
pub struct OutgoingReq {
    pub req_id: String,
    pub target_id: Option<String>,
    pub target_addr: String,
    pub created: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CoreEvent {
    DevicesChanged,
    PairIncoming { req: IncomingReq },
    PairResolved { req_id: String, status: String },
    Transfer(TransferSnapshot),
    Toast { message: String },
}

impl CoreEvent {
    pub fn name(&self) -> &'static str {
        match self {
            CoreEvent::DevicesChanged => "devices-changed",
            CoreEvent::PairIncoming { .. } => "pair-incoming",
            CoreEvent::PairResolved { .. } => "pair-resolved",
            CoreEvent::Transfer(_) => "transfer",
            CoreEvent::Toast { .. } => "toast",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TransferSnapshot {
    pub id: u64,
    pub kind: &'static str,
    pub peer_id: String,
    pub peer_name: String,
    pub name: String,
    pub path: String,
    pub done: u64,
    pub total: u64,
    pub state: &'static str,
}

pub enum ConfirmOutcome {
    PinOk,
    Verified(String),
}

struct Inner {
    pairings: Vec<Pairing>,
    incoming: Vec<IncomingReq>,
    outgoing: Vec<OutgoingReq>,
}

pub struct PairManager {
    path: PathBuf,
    inner: AsyncMutex<Inner>,
    pub events: broadcast::Sender<CoreEvent>,
    /// this device's CA fingerprint (needed to derive the shared token)
    my_fp: Mutex<Option<String>>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn rand_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    rand::rngs::OsRng.fill_bytes(&mut b);
    hex::encode(b)
}

impl PairManager {
    pub fn load(path: PathBuf, events: broadcast::Sender<CoreEvent>) -> Arc<Self> {
        let pairings = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Vec<Pairing>>(&b).ok())
            .unwrap_or_default();
        Arc::new(PairManager {
            path,
            inner: AsyncMutex::new(Inner {
                pairings,
                incoming: Vec::new(),
                outgoing: Vec::new(),
            }),
            events,
            my_fp: Mutex::new(None),
        })
    }

    /// Must be called once at startup so pairing tokens can be derived.
    pub fn set_my_fp(&self, fp: String) {
        *self.my_fp.lock().unwrap() = Some(fp);
    }

    fn persist(&self, pairings: &[Pairing]) {
        if let Ok(json) = serde_json::to_vec_pretty(pairings) {
            let _ = std::fs::write(&self.path, json);
        }
    }

    // ----------------------------------------------------------- pairings

    pub async fn list_pairings(&self) -> Vec<Pairing> {
        self.inner.lock().await.pairings.clone()
    }

    pub async fn pairing_by_id(&self, id: &str) -> Option<Pairing> {
        self.inner
            .lock()
            .await
            .pairings
            .iter()
            .find(|p| p.id == id)
            .cloned()
    }

    pub async fn pairing_by_token(&self, token: &str) -> Option<Pairing> {
        self.inner
            .lock()
            .await
            .pairings
            .iter()
            .find(|p| p.token == token)
            .cloned()
    }

    pub async fn add_pairing(&self, p: Pairing) {
        let mut inner = self.inner.lock().await;
        inner.pairings.retain(|x| x.id != p.id);
        inner.pairings.push(p.clone());
        self.persist(&inner.pairings);
    }

    pub async fn remove_pairing(&self, id: &str) {
        let mut inner = self.inner.lock().await;
        if inner.pairings.iter().any(|p| p.id == id) {
            inner.pairings.retain(|p| p.id != id);
            self.persist(&inner.pairings);
        }
        drop(inner);
        let _ = self.events.send(CoreEvent::DevicesChanged);
    }

    pub async fn set_drive_letter(&self, id: &str, letter: Option<String>) {
        let mut inner = self.inner.lock().await;
        if let Some(p) = inner.pairings.iter_mut().find(|p| p.id == id) {
            p.drive_letter = letter;
            self.persist(&inner.pairings);
        }
    }

    // --------------------------------------------------- incoming (B side)

    pub async fn add_incoming(&self, id: &str, name: &str, ca_fp: &str) -> IncomingReq {
        let mut inner = self.inner.lock().await;
        inner.incoming.retain(|r| r.created + REQUEST_TTL_SECS > now());
        // replace any older request from the same device
        inner.incoming.retain(|r| r.id != id);
        let req = IncomingReq {
            req_id: rand_hex(16),
            id: id.to_string(),
            name: name.to_string(),
            ca_fp: ca_fp.to_string(),
            pin: crate::identity::random_pin(),
            created: now(),
            state: ReqState::Pending,
            pin_ok: false,
        };
        inner.incoming.push(req.clone());
        drop(inner);
        let _ = self.events.send(CoreEvent::PairIncoming { req: req.clone() });
        req
    }

    pub async fn incoming_by_req(&self, req_id: &str, caller_fp: &str) -> Option<IncomingReq> {
        let inner = self.inner.lock().await;
        inner
            .incoming
            .iter()
            .filter(|r| r.created + REQUEST_TTL_SECS > now())
            .find(|r| r.req_id == req_id && r.ca_fp == caller_fp)
            .cloned()
    }

    pub async fn list_incoming(&self) -> Vec<IncomingReq> {
        let mut inner = self.inner.lock().await;
        inner.incoming.retain(|r| r.created + REQUEST_TTL_SECS > now());
        inner.incoming.clone()
    }

    /// B-side user clicked Allow.
    pub async fn approve(&self, req_id: &str) -> bool {
        let maybe_finalize = {
            let mut inner = self.inner.lock().await;
            match inner.incoming.iter_mut().find(|r| r.req_id == req_id) {
                Some(r) => {
                    r.state = ReqState::Approved;
                    r.pin_ok
                }
                None => return false,
            }
        };
        if maybe_finalize {
            self.finalize(req_id).await;
        } else {
            let _ = self.events.send(CoreEvent::PairResolved {
                req_id: req_id.to_string(),
                status: "approved".into(),
            });
        }
        true
    }

    /// B-side user clicked Deny.
    pub async fn deny(&self, req_id: &str) -> bool {
        let mut inner = self.inner.lock().await;
        match inner.incoming.iter_mut().find(|r| r.req_id == req_id) {
            Some(r) => {
                r.state = ReqState::Denied;
                true
            }
            None => false,
        }
    }

    /// B side: the caller proved the PIN. Complete the pairing if the user
    /// already approved, otherwise just remember `pin_ok`.
    pub async fn confirm_pin(&self, req_id: &str, caller_fp: &str, _pin: &str) -> ConfirmOutcome {
        let ready = {
            let mut inner = self.inner.lock().await;
            let Some(r) = inner
                .incoming
                .iter_mut()
                .find(|r| r.req_id == req_id && r.ca_fp == caller_fp)
            else {
                return ConfirmOutcome::PinOk; // unknown/expired — caller gets 404
            };
            r.pin_ok = true;
            let was_approved = r.state == ReqState::Approved;
            if was_approved {
                r.state = ReqState::Verified;
            }
            was_approved
        };

        if !ready {
            let _ = self.events.send(CoreEvent::PairResolved {
                req_id: req_id.to_string(),
                status: "pin_ok".into(),
            });
            return ConfirmOutcome::PinOk;
        }
        let token = self.finalize(req_id).await;
        ConfirmOutcome::Verified(token)
    }

    /// Turn an approved + pin-verified incoming request into a stored pairing.
    async fn finalize(&self, req_id: &str) -> String {
        let my_fp = self
            .my_fp
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_default();
        let (peer_id, peer_name, peer_fp, pin) = {
            let mut inner = self.inner.lock().await;
            let Some(r) = inner.incoming.iter_mut().find(|r| r.req_id == req_id) else {
                return String::new();
            };
            r.state = ReqState::Verified;
            (
                r.id.clone(),
                r.name.clone(),
                r.ca_fp.clone(),
                r.pin.clone(),
            )
        };
        let token = derive_token(&my_fp, &peer_fp, &pin);
        self.add_pairing(Pairing {
            id: peer_id,
            name: peer_name,
            ca_fp: peer_fp,
            token: token.clone(),
            added_at: now(),
            drive_letter: None,
        })
        .await;
        {
            let mut inner = self.inner.lock().await;
            inner.incoming.retain(|r| r.req_id != req_id);
        }
        let _ = self.events.send(CoreEvent::DevicesChanged);
        let _ = self.events.send(CoreEvent::PairResolved {
            req_id: req_id.to_string(),
            status: "verified".into(),
        });
        token
    }

    // --------------------------------------------------- outgoing (A side)

    pub async fn start_outgoing(&self, target_addr: &str, target_id: Option<String>) -> OutgoingReq {
        let req = OutgoingReq {
            req_id: rand_hex(16),
            target_id,
            target_addr: target_addr.to_string(),
            created: now(),
        };
        let mut inner = self.inner.lock().await;
        inner
            .outgoing
            .retain(|r| r.created + REQUEST_TTL_SECS > now());
        inner.outgoing.push(req.clone());
        req
    }

    /// The initiator stores the *remote* request id once the target replies.
    /// Keyed by the local id from `start_outgoing` — renaming "the last
    /// entry" could hit a concurrent pairing attempt's record.
    pub async fn rename_outgoing(&self, old_req_id: &str, new_req_id: &str) {
        let mut inner = self.inner.lock().await;
        if let Some(rec) = inner
            .outgoing
            .iter_mut()
            .find(|r| r.req_id == old_req_id)
        {
            rec.req_id = new_req_id.to_string();
        }
    }

    pub async fn outgoing_by_req(&self, req_id: &str) -> Option<OutgoingReq> {
        let inner = self.inner.lock().await;
        inner
            .outgoing
            .iter()
            .find(|r| r.req_id == req_id && r.created + REQUEST_TTL_SECS > now())
            .cloned()
    }

    pub async fn complete_outgoing(&self, req_id: &str, peer: Pairing) {
        let mut inner = self.inner.lock().await;
        inner.outgoing.retain(|r| r.req_id != req_id);
        inner.pairings.retain(|p| p.id != peer.id);
        inner.pairings.push(peer.clone());
        self.persist(&inner.pairings);
        drop(inner);
        let _ = self.events.send(CoreEvent::DevicesChanged);
    }

}

/// Human readable size helper (also used by dir listings).
pub fn human_size(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{} {}", n, UNITS[0])
    } else {
        format!("{:.1} {}", v, UNITS[u])
    }
}

/// How often the initiator polls `/single/pair/check`.
pub fn poll_interval() -> Duration {
    Duration::from_millis(1500)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_size_format() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn pairing_json_roundtrip() {
        let p = Pairing {
            id: "abcd1234abcd1234".into(),
            name: "Kitchen PC".into(),
            ca_fp: "ff".repeat(32),
            token: "ee".repeat(32),
            added_at: 1_700_000_000,
            drive_letter: Some("S".into()),
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: Pairing = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "Kitchen PC");
        assert_eq!(back.drive_letter.as_deref(), Some("S"));
    }
}
