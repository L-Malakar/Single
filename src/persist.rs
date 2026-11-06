//! Persistence — "everything returns to where the user left it".
//!
//! A single JSON file (`single-state.json`, stored next to the received-files
//! folder) survives server restarts and holds:
//! - the dashboard-renamed device name (a `--name` flag is only the default),
//! - the Protocol mode + password (Public / Private / Protected),
//! - every conversation: chat lines, file-transfer cards, peer metadata,
//! - the tail of the activity log (replayed into freshly opened dashboards).
//!
//! Writing is debounced (flush task every 1.5 s when dirty) and atomic
//! (temp file + rename) so a crash never corrupts the file. Chat/transfer
//! lines are recorded by [`crate::p2p`], the access policy by
//! [`crate::access`], renames by [`crate::server`], and log lines by a
//! background bus subscriber spawned here.

use crate::protocol::{ChatLine, ServerMsg, TransferDto};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

pub const STATE_FILE: &str = "single-state.json";

/// Chat lines kept per conversation (mirrors `p2p::CHAT_HISTORY_CAP`).
const CHAT_CAP: usize = 200;
/// Activity-log tail that survives a restart.
const LOG_CAP: usize = 80;
/// Transfer cards kept per conversation.
const TRANSFER_CAP: usize = 12;

/// Peer metadata + history for one conversation.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ChatArchive {
    pub peer_name: String,
    pub peer_platform: String,
    pub peer_addr: String,
    pub direction: String,
    pub chat: Vec<ChatLine>,
    pub transfers: Vec<TransferDto>,
}

/// One activity-log line (mirrors `ServerMsg::Log`).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct LogLine {
    pub level: String,
    pub msg: String,
    pub ts: i64,
}

/// Everything persisted to disk. `v` allows future schema migrations.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Store {
    pub v: u32,
    /// Name chosen via the dashboard (overrides the `--name` default).
    pub name: Option<String>,
    pub access_mode: Option<String>,
    pub access_password: Option<String>,
    pub chats: HashMap<String, ChatArchive>,
    pub logs: Vec<LogLine>,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            v: 1,
            name: None,
            access_mode: None,
            access_password: None,
            chats: HashMap::new(),
            logs: Vec::new(),
        }
    }
}

struct Shared {
    store: Mutex<Store>,
    dirty: AtomicBool,
    path: PathBuf,
}

pub struct Persist {
    shared: Arc<Shared>,
}

impl Persist {
    /// Load the state file (a missing or unreadable file simply starts fresh).
    pub fn load(path: PathBuf) -> Arc<Self> {
        let store = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Store>(&bytes).ok())
            .unwrap_or_default();
        Arc::new(Self {
            shared: Arc::new(Shared {
                store: Mutex::new(store),
                dirty: AtomicBool::new(false),
                path,
            }),
        })
    }

    /// Where the state file lives (for the boot banner).
    pub fn path(&self) -> &std::path::Path {
        &self.shared.path
    }

    /// Full copy of the store (used once at boot to restore sessions).
    pub fn snapshot(&self) -> Store {
        self.shared.store.lock().unwrap().clone()
    }

    /// Archive of one peer (used to re-seed history when reconnecting).
    pub fn archive_for(&self, peer_id: &str) -> Option<ChatArchive> {
        self.shared
            .store
            .lock()
            .unwrap()
            .chats
            .get(peer_id)
            .cloned()
    }

    pub fn set_name(&self, name: &str) {
        let mut s = self.shared.store.lock().unwrap();
        s.name = Some(name.to_string());
        drop(s);
        self.shared.dirty.store(true, Ordering::Relaxed);
    }

    pub fn set_access(&self, mode: &str, password: &str) {
        let mut s = self.shared.store.lock().unwrap();
        s.access_mode = Some(mode.to_string());
        s.access_password = Some(password.to_string());
        drop(s);
        self.shared.dirty.store(true, Ordering::Relaxed);
    }

    /// Append one chat line (and refresh peer metadata so the conversation
    /// can be restored standalone after a restart).
    pub fn record_chat(
        &self,
        peer_id: &str,
        peer_name: &str,
        peer_platform: &str,
        peer_addr: &str,
        direction: &str,
        line: ChatLine,
    ) {
        let mut s = self.shared.store.lock().unwrap();
        let arch = s.chats.entry(peer_id.to_string()).or_default();
        arch.peer_name = peer_name.to_string();
        arch.peer_platform = peer_platform.to_string();
        arch.peer_addr = peer_addr.to_string();
        arch.direction = direction.to_string();
        arch.chat.push(line);
        if arch.chat.len() > CHAT_CAP {
            arch.chat.drain(..arch.chat.len() - CHAT_CAP);
        }
        drop(s);
        self.shared.dirty.store(true, Ordering::Relaxed);
    }

    /// Insert / update one transfer card in a conversation.
    #[allow(clippy::too_many_arguments)]
    pub fn record_transfer(
        &self,
        peer_id: &str,
        peer_name: &str,
        peer_platform: &str,
        peer_addr: &str,
        direction: &str,
        t: TransferDto,
    ) {
        let mut s = self.shared.store.lock().unwrap();
        let arch = s.chats.entry(peer_id.to_string()).or_default();
        arch.peer_name = peer_name.to_string();
        arch.peer_platform = peer_platform.to_string();
        arch.peer_addr = peer_addr.to_string();
        arch.direction = direction.to_string();
        if let Some(existing) = arch.transfers.iter_mut().find(|x| x.tid == t.tid) {
            *existing = t;
        } else {
            arch.transfers.push(t);
            if arch.transfers.len() > TRANSFER_CAP {
                arch.transfers.remove(0);
            }
        }
        drop(s);
        self.shared.dirty.store(true, Ordering::Relaxed);
    }

    /// Write the store atomically (temp file + rename). Best-effort.
    pub fn flush_now(&self) {
        let (bytes, path) = {
            let s = self.shared.store.lock().unwrap();
            match serde_json::to_string_pretty(&*s) {
                Ok(json) => (json.into_bytes(), self.shared.path.clone()),
                Err(_) => return,
            }
        };
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, &bytes).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
            self.shared.dirty.store(false, Ordering::Relaxed);
        }
    }

    /// Background task: collect log lines from the bus and flush to disk
    /// every ~1.5 s when something changed.
    pub fn spawn(self: &Arc<Self>, bus: broadcast::Sender<ServerMsg>) {
        let shared = self.shared.clone();
        let mut rx = bus.subscribe();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_millis(1500));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            tick.tick().await; // first tick fires immediately; skip it
            loop {
                tokio::select! {
                    ev = rx.recv() => match ev {
                        Ok(ServerMsg::Log { level, msg, ts }) => {
                            record_log(&shared, &level, &msg, ts);
                        }
                        Ok(_) => {}
                        Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => break,
                    },
                    _ = tick.tick() => {
                        if shared.dirty.swap(false, Ordering::Relaxed) {
                            flush_to_disk(&shared);
                        }
                    }
                }
            }
        });
    }
}

fn record_log(shared: &Arc<Shared>, level: &str, msg: &str, ts: i64) {
    let mut s = shared.store.lock().unwrap();
    s.logs.push(LogLine {
        level: level.to_string(),
        msg: msg.to_string(),
        ts,
    });
    if s.logs.len() > LOG_CAP {
        let cut = s.logs.len() - LOG_CAP;
        s.logs.drain(..cut);
    }
    drop(s);
    shared.dirty.store(true, Ordering::Relaxed);
}

fn flush_to_disk(shared: &Arc<Shared>) {
    let bytes = {
        let s = shared.store.lock().unwrap();
        match serde_json::to_string_pretty(&*s) {
            Ok(json) => json.into_bytes(),
            Err(_) => return,
        }
    };
    let tmp = shared.path.with_extension("json.tmp");
    if std::fs::write(&tmp, &bytes).is_ok() {
        let _ = std::fs::rename(&tmp, &shared.path);
    }
}
