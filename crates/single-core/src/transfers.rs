//! Transfer registry: tracks upload/download progress and emits events.

use crate::pairing::{CoreEvent, TransferSnapshot};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

pub struct Transfers {
    map: Mutex<HashMap<u64, TransferSnapshot>>,
    counter: AtomicU64,
    events: broadcast::Sender<CoreEvent>,
}

impl Transfers {
    pub fn new(events: broadcast::Sender<CoreEvent>) -> Arc<Self> {
        Arc::new(Transfers {
            map: Mutex::new(HashMap::new()),
            counter: AtomicU64::new(1),
            events,
        })
    }

    pub fn begin(
        &self,
        kind: &'static str,
        peer_id: &str,
        peer_name: &str,
        path: &str,
        total: u64,
    ) -> u64 {
        let id = self.counter.fetch_add(1, Ordering::Relaxed);
        let snap = TransferSnapshot {
            id,
            kind,
            peer_id: peer_id.to_string(),
            peer_name: peer_name.to_string(),
            name: path.rsplit('/').next().unwrap_or(path).to_string(),
            path: path.to_string(),
            done: 0,
            total,
            state: "active",
        };
        self.map.lock().unwrap().insert(id, snap.clone());
        let _ = self.events.send(CoreEvent::Transfer(snap));
        id
    }

    /// Update progress; emits at most every `emit_every` bytes of movement.
    pub fn update(&self, id: u64, done: u64) {
        let mut map = self.map.lock().unwrap();
        if let Some(t) = map.get_mut(&id) {
            t.done = done;
            let snap = t.clone();
            drop(map);
            let _ = self.events.send(CoreEvent::Transfer(snap));
        }
    }

    pub fn finish(&self, id: u64, state: &'static str) {
        let mut map = self.map.lock().unwrap();
        if let Some(t) = map.get_mut(&id) {
            t.state = state;
            let snap = t.clone();
            drop(map);
            let _ = self.events.send(CoreEvent::Transfer(snap));
        }
    }

    pub fn list(&self) -> Vec<TransferSnapshot> {
        let mut v: Vec<TransferSnapshot> = self.map.lock().unwrap().values().cloned().collect();
        v.sort_by(|a, b| b.id.cmp(&a.id));
        v.truncate(50);
        v
    }
}

/// Throttled progress reporter: emits at most every 512 KiB.
pub struct Progress {
    id: u64,
    transfers: Arc<Transfers>,
    last: u64,
    step: u64,
}

impl Progress {
    pub fn new(id: u64, transfers: Arc<Transfers>) -> Self {
        Progress {
            id,
            transfers,
            last: 0,
            step: 512 * 1024,
        }
    }
    pub fn tick(&mut self, done: u64) {
        if done >= self.last + self.step {
            self.last = done;
            self.transfers.update(self.id, done);
        }
    }
    pub fn final_tick(&mut self, done: u64) {
        self.transfers.update(self.id, done);
    }
}
