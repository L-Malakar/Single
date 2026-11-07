//! The files bridge: share folders with connected devices, browse theirs.
//!
//! Two halves live here:
//!
//! * **Serving** — when a peer sends `fs-list-req` / `fs-read-req` / … we
//!   answer from the *shared roots* (configured on the Files page of the
//!   dashboard, or `--share` on the command line). Every path is resolved
//!   strictly inside a root — `..`, absolute paths and drive letters are
//!   rejected, and the final path is canonicalized and prefix-checked, so a
//!   peer can never escape the folders this device chose to share.
//!
//! * **Requesting** — the dashboard browser, the Windows WebDAV proxy and the
//!   Android documents provider all call [`BridgeManager::list`] /
//!   [`open_read`] / `write_*`, which multiplex `fs-*` frames over the
//!   (end-to-end encrypted) P2P session by `rid` and reassemble the replies.
//!
//! Logical paths are platform independent: `/` lists the shared roots as
//! folders, `/<root title>/…` descends into one.

use crate::protocol::*;
use crate::util::log_bus;
use base64::Engine;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock, Weak};
use tokio::sync::{broadcast, mpsc, oneshot};

/// Read chunk size — one `fs-data` frame carries up to this many bytes.
pub const READ_CHUNK: usize = 256 * 1024;

/// A decoded chunk of a remote read stream.
pub struct FsChunk {
    pub data: Vec<u8>,
    pub eof: bool,
}

/// Client-side pending request, keyed by `rid`.
enum Pending {
    List {
        peer: String,
        tx: oneshot::Sender<Result<Vec<FsEntryDto>, String>>,
    },
    Stat {
        peer: String,
        tx: oneshot::Sender<Result<FsEntryDto, String>>,
    },
    Read {
        peer: String,
        tx: mpsc::Sender<Result<FsChunk, String>>,
        next_seq: u64,
    },
    Ack {
        peer: String,
        tx: oneshot::Sender<Result<(), String>>,
    },
}

/// Server-side inbound write session (a peer is uploading into a share).
struct InboundWrite {
    peer: String,
    file: std::fs::File,
    path: PathBuf,
    written: u64,
    size: u64,
}

pub struct BridgeManager {
    bus: broadcast::Sender<ServerMsg>,
    roots: RwLock<Vec<ShareRootDto>>,
    pending: Mutex<HashMap<String, Pending>>,
    writes: Mutex<HashMap<String, InboundWrite>>,
    /// Set right after construction — used to send frames to peers.
    p2p: RwLock<Option<Weak<crate::p2p::P2pManager>>>,
}

impl BridgeManager {
    pub fn new(bus: broadcast::Sender<ServerMsg>) -> Arc<Self> {
        Arc::new(Self {
            bus,
            roots: RwLock::new(Vec::new()),
            pending: Mutex::new(HashMap::new()),
            writes: Mutex::new(HashMap::new()),
            p2p: RwLock::new(None),
        })
    }

    /// Break the construction cycle: `P2pManager` owns the bridge, the bridge
    /// borrows the manager weakly.
    pub fn attach(&self, p2p: &Arc<crate::p2p::P2pManager>) {
        *self.p2p.write().unwrap() = Some(Arc::downgrade(p2p));
    }

    // ==================================================================
    // Shared roots
    // ==================================================================

    pub fn set_roots(&self, roots: Vec<ShareRootDto>) {
        let mut cleaned: Vec<ShareRootDto> = Vec::new();
        for r in roots {
            let path = PathBuf::from(r.path.trim());
            if path.as_os_str().is_empty() {
                continue;
            }
            // Drop roots that no longer exist on disk (renamed / unmounted).
            if !path.exists() {
                log_bus(&self.bus, "warn", format!("share “{}” ignored: {} not found", r.title, path.display()));
                continue;
            }
            let mut title = r.title.trim().chars().take(48).collect::<String>();
            if title.is_empty() {
                title = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "Share".into());
            }
            // Titles are path segments for peers — keep them unique & sane.
            title = title.replace('/', "-").replace('\\', "-");
            let mut title = title;
            let mut n = 2;
            while cleaned.iter().any(|c| c.title.eq_ignore_ascii_case(&title)) {
                title = format!("{}-{n}", r.title.trim().chars().take(44).collect::<String>());
                n += 1;
            }
            cleaned.push(ShareRootDto {
                title,
                path: path.display().to_string(),
            });
        }
        *self.roots.write().unwrap() = cleaned;
    }

    pub fn shares(&self) -> Vec<ShareRootDto> {
        self.roots.read().unwrap().clone()
    }

    /// The default share for this platform: the user's home area —
    /// `%USERPROFILE%` on Windows, `/storage/emulated/0` on Android,
    /// `$HOME` elsewhere.
    pub fn default_share() -> Option<ShareRootDto> {
        let (path, title): (PathBuf, &str) = if cfg!(target_os = "android") {
            (PathBuf::from("/storage/emulated/0"), "Storage")
        } else if cfg!(target_os = "windows") {
            (std::env::var_os("USERPROFILE").map(PathBuf::from)?, "Home")
        } else {
            (std::env::var_os("HOME").map(PathBuf::from)?, "Home")
        };
        if !path.is_dir() {
            return None;
        }
        Some(ShareRootDto {
            title: title.to_string(),
            path: path.display().to_string(),
        })
    }

    // ==================================================================
    // Path resolution (sandboxed)
    // ==================================================================

    /// Resolve a logical peer path to a local directory entry for serving.
    /// `None` when it escapes the shares or does not exist.
    fn resolve(&self, logical: &str) -> Result<(PathBuf, Option<FsEntryDto>), String> {
        let roots = self.roots.read().unwrap();
        if roots.is_empty() {
            return Err("this device shares no folders (Files page → Sharing)".into());
        }
        let segs = split_logical(logical);
        if segs.is_empty() {
            return Ok((PathBuf::new(), None)); // "/" — virtual listing of roots
        }
        let root = roots
            .iter()
            .find(|r| r.title == segs[0])
            .or_else(|| roots.iter().find(|r| r.title.eq_ignore_ascii_case(&segs[0])))
            .ok_or_else(|| format!("no shared folder named “{}”", segs[0]))?;
        let base = PathBuf::from(&root.path);
        if segs.len() == 1 {
            let meta = std::fs::metadata(&base).map_err(|e| format!("{}: {e}", root.title))?;
            return Ok((
                base.clone(),
                Some(entry_from_meta(&root.title, &meta)),
            ));
        }
        let rel = segs[1..].join("/");
        let target = safe_join(&base, &rel)?;
        let meta = std::fs::metadata(&target).map_err(|_| format!("“{logical}” not found"))?;
        let canon_target = std::fs::canonicalize(&target).map_err(|_| format!("“{logical}” not found"))?;
        let canon_base = std::fs::canonicalize(&base).unwrap_or_else(|_| base.clone());
        if !canon_target.starts_with(&canon_base) {
            return Err("path escapes the shared folder".into());
        }
        let name = target.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        Ok((target, Some(entry_from_meta(&name, &meta))))
    }

    /// Where a peer-created file/folder should live. The parent must exist
    /// and stay inside a share.
    fn resolve_target(&self, logical: &str) -> Result<(PathBuf, String), String> {
        let segs = split_logical(logical);
        if segs.len() < 2 {
            return Err("cannot write to the share root itself".into());
        }
        let roots = self.roots.read().unwrap();
        let root = roots
            .iter()
            .find(|r| r.title == segs[0])
            .ok_or_else(|| format!("no shared folder named “{}”", segs[0]))?;
        let base = PathBuf::from(&root.path);
        let target = safe_join(&base, &segs[1..].join("/"))?;
        let parent = target.parent().ok_or("invalid target")?;
        let canon_parent = std::fs::canonicalize(parent).map_err(|_| "parent folder does not exist".to_string())?;
        let canon_base = std::fs::canonicalize(&base).unwrap_or_else(|_| base.clone());
        if !canon_parent.starts_with(&canon_base) {
            return Err("path escapes the shared folder".into());
        }
        let name = target.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "file.bin".into());
        Ok((target, name))
    }

    // ==================================================================
    // Serving side — a peer asked us
    // ==================================================================

    pub fn serve_list(&self, logical: &str) -> Result<Vec<FsEntryDto>, String> {
        let (path, entry) = self.resolve(logical)?;
        if entry.is_none() {
            // "/" — the shared roots themselves.
            return Ok(self
                .roots
                .read()
                .unwrap()
                .iter()
                .map(|r| FsEntryDto {
                    kind: "dir".into(),
                    name: r.title.clone(),
                    size: 0,
                    modified: std::fs::metadata(&r.path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0),
                })
                .collect());
        }
        let e = entry.unwrap();
        if e.kind != "dir" {
            return Err(format!("“{}” is not a folder", e.name));
        }
        let mut out = Vec::new();
        let rd = std::fs::read_dir(&path).map_err(|e| format!("cannot open folder: {e}"))?;
        for ent in rd.flatten() {
            let Ok(meta) = ent.metadata() else { continue };
            let name = ent.file_name().to_string_lossy().to_string();
            if name.is_empty() {
                continue;
            }
            out.push(entry_from_meta(&name, &meta));
        }
        out.sort_by(|a, b| {
            (a.kind != "dir", a.name.to_lowercase()).cmp(&(b.kind != "dir", b.name.to_lowercase()))
        });
        Ok(out)
    }

    pub fn serve_stat(&self, logical: &str) -> Result<FsEntryDto, String> {
        let (_, entry) = self.resolve(logical)?;
        if entry.is_none() {
            // "/" — the virtual root that contains the shared roots (WebDAV
            // PROPFINDs it when Windows opens a mapped drive).
            let roots = self.roots.read().unwrap();
            let modified = roots
                .iter()
                .map(|r| {
                    std::fs::metadata(&r.path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0)
                })
                .max()
                .unwrap_or(0);
            return Ok(FsEntryDto {
                kind: "dir".into(),
                name: "Shares".into(),
                size: 0,
                modified,
            });
        }
        entry.ok_or_else(|| "not found".into())
    }

    /// Open a local file for a peer read. Returns the file + its size.
    pub fn serve_open(&self, logical: &str) -> Result<(std::fs::File, u64), String> {
        let (path, entry) = self.resolve(logical)?;
        let e = entry.ok_or("cannot read the share root")?;
        if e.kind == "dir" {
            return Err(format!("“{}” is a folder", e.name));
        }
        let file = std::fs::File::open(&path).map_err(|e| format!("open failed: {e}"))?;
        Ok((file, e.size))
    }

    /// A peer wants to create/truncate a file inside one of our shares.
    pub fn serve_write_start(&self, rid: &str, peer: &str, logical: &str, size: u64) -> Result<(), String> {
        let (target, _) = self.resolve_target(logical)?;
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let file = std::fs::File::create(&target).map_err(|e| format!("create failed: {e}"))?;
        self.writes.lock().unwrap().insert(
            rid.to_string(),
            InboundWrite {
                peer: peer.to_string(),
                file,
                path: target,
                written: 0,
                size,
            },
        );
        Ok(())
    }

    /// Append one decoded chunk to an inbound write. `Ok(false)` = this rid
    /// is not an inbound upload (it's a read-stream reply instead).
    pub fn serve_write_chunk(&self, rid: &str, peer: &str, data: &[u8]) -> Result<bool, String> {
        use std::io::Write;
        let mut writes = self.writes.lock().unwrap();
        match writes.get_mut(rid) {
            Some(w) if w.peer == peer => {
                w.file.write_all(data).map_err(|e| format!("write failed: {e}"))?;
                w.written += data.len() as u64;
                Ok(true)
            }
            Some(_) => Err("upload owned by another session".into()),
            None => Ok(false),
        }
    }

    /// Finish an inbound write; returns the final size.
    pub fn serve_write_end(&self, rid: &str, peer: &str) -> Result<u64, String> {
        let w = {
            let mut writes = self.writes.lock().unwrap();
            match writes.remove(rid) {
                Some(w) if w.peer == peer => w,
                Some(_) => return Err("upload owned by another session".into()),
                None => return Err("no such upload (timed out?)".into()),
            }
        };
        use std::io::Write;
        let mut file = w.file;
        file.flush().map_err(|e| format!("flush failed: {e}"))?;
        if w.size > 0 && w.written != w.size {
            return Err(format!("size mismatch: expected {} bytes, got {}", w.size, w.written));
        }
        log_bus(
            &self.bus,
            "ok",
            format!("received “{}” ({} bytes) over the bridge → {}", w.path.display(), w.written, w.path.display()),
        );
        Ok(w.written)
    }

    /// Drop an inbound write (peer aborted). Returns whether the rid was one.
    pub fn serve_write_abort(&self, rid: &str) -> bool {
        self.writes.lock().unwrap().remove(rid).is_some()
    }

    pub fn serve_mkdir(&self, logical: &str) -> Result<(), String> {
        let (target, _) = self.resolve_target(logical)?;
        std::fs::create_dir_all(&target).map_err(|e| format!("mkdir failed: {e}"))
    }

    pub fn serve_delete(&self, logical: &str, recursive: bool) -> Result<(), String> {
        let (target, _) = self.resolve_target(logical)?;
        let meta = std::fs::symlink_metadata(&target).map_err(|_| "not found".to_string())?;
        if meta.is_dir() {
            if recursive {
                std::fs::remove_dir_all(&target).map_err(|e| format!("delete failed: {e}"))
            } else {
                std::fs::remove_dir(&target).map_err(|e| format!("delete failed (folder not empty?): {e}"))
            }
        } else {
            std::fs::remove_file(&target).map_err(|e| format!("delete failed: {e}"))
        }
    }

    // ==================================================================
    // Requesting side — we asked the peer
    // ==================================================================

    fn send_to_peer(&self, peer_id: &str, wire: Wire) -> Result<(), String> {
        let weak = self.p2p.read().unwrap().clone();
        let p2p = weak
            .and_then(|w| w.upgrade())
            .ok_or("bridge not attached")?;
        // try_send keeps this synchronous — the mpsc has headroom for bursts.
        p2p.try_send_to_peer(peer_id, wire)
    }

    fn new_rid() -> String {
        format!("r{:x}", rand::random::<u64>())
    }

    fn register(&self, rid: &str, pending: Pending) {
        self.pending.lock().unwrap().insert(rid.to_string(), pending);
    }

    /// Ask a peer for a directory listing.
    pub async fn list(&self, peer_id: &str, path: &str) -> Result<Vec<FsEntryDto>, String> {
        let rid = Self::new_rid();
        let (tx, rx) = oneshot::channel();
        self.register(&rid, Pending::List { peer: peer_id.into(), tx });
        self.send_to_peer(peer_id, Wire::FsListReq { rid: rid.clone(), path: path.into() })?;
        match tokio::time::timeout(std::time::Duration::from_secs(10), rx).await {
            Ok(Ok(res)) => res,
            _ => {
                self.pending.lock().unwrap().remove(&rid);
                Err("listing timed out (is the device still connected?)".into())
            }
        }
    }

    pub async fn stat(&self, peer_id: &str, path: &str) -> Result<FsEntryDto, String> {
        let rid = Self::new_rid();
        let (tx, rx) = oneshot::channel();
        self.register(&rid, Pending::Stat { peer: peer_id.into(), tx });
        self.send_to_peer(peer_id, Wire::FsStatReq { rid: rid.clone(), path: path.into() })?;
        match tokio::time::timeout(std::time::Duration::from_secs(10), rx).await {
            Ok(Ok(res)) => res,
            _ => {
                self.pending.lock().unwrap().remove(&rid);
                Err("stat timed out (is the device still connected?)".into())
            }
        }
    }

    /// Ask a peer to stream a file (or a byte range). `len` = 0 → to EOF.
    /// Returns a receiver of decoded chunks.
    pub async fn open_read(
        &self,
        peer_id: &str,
        path: &str,
        offset: u64,
        len: u64,
    ) -> Result<mpsc::Receiver<Result<FsChunk, String>>, String> {
        let rid = Self::new_rid();
        let (tx, rx) = mpsc::channel(8);
        self.register(&rid, Pending::Read { peer: peer_id.into(), tx, next_seq: 0 });
        self.send_to_peer(
            peer_id,
            Wire::FsReadReq { rid, path: path.into(), offset, len },
        )?;
        Ok(rx)
    }

    /// Convenience for small files: gather the whole stream.
    pub async fn read_all(&self, peer_id: &str, path: &str) -> Result<Vec<u8>, String> {
        let mut rx = self.open_read(peer_id, path, 0, 0).await?;
        let mut buf = Vec::new();
        while let Some(item) = rx.recv().await {
            match item {
                Ok(chunk) => {
                    buf.extend_from_slice(&chunk.data);
                    if chunk.eof {
                        break;
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Ok(buf)
    }

    /// Start an upload into a peer's share.
    pub async fn write_open(&self, peer_id: &str, path: &str, size: u64) -> Result<String, String> {
        let rid = Self::new_rid();
        let (tx, rx) = oneshot::channel();
        self.register(&rid, Pending::Ack { peer: peer_id.into(), tx });
        self.send_to_peer(peer_id, Wire::FsWriteStart { rid: rid.clone(), path: path.into(), size })?;
        match tokio::time::timeout(std::time::Duration::from_secs(10), rx).await {
            Ok(Ok(Ok(()))) => Ok(rid.clone()),
            Ok(Ok(Err(e))) => {
                self.pending.lock().unwrap().remove(&rid);
                Err(e)
            }
            _ => {
                self.pending.lock().unwrap().remove(&rid);
                Err("upload start timed out".into())
            }
        }
    }

    pub async fn write_chunk(&self, peer_id: &str, rid: &str, data: Vec<u8>) -> Result<(), String> {
        self.send_to_peer(
            peer_id,
            Wire::FsData {
                rid: rid.into(),
                seq: 0,
                data: base64::engine::general_purpose::STANDARD.encode(&data),
                eof: false,
            },
        )
    }

    pub async fn write_close(&self, peer_id: &str, rid: &str) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        // Replace the Ack slot so the completion reply finds a live channel.
        self.pending
            .lock()
            .unwrap()
            .insert(rid.to_string(), Pending::Ack { peer: peer_id.into(), tx });
        self.send_to_peer(peer_id, Wire::FsWriteEnd { rid: rid.into() })?;
        match tokio::time::timeout(std::time::Duration::from_secs(15), rx).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(e))) => Err(e),
            _ => Err("upload completion timed out".into()),
        }
    }

    pub async fn mkdir(&self, peer_id: &str, path: &str) -> Result<(), String> {
        let rid = Self::new_rid();
        let (tx, rx) = oneshot::channel();
        self.register(&rid, Pending::Ack { peer: peer_id.into(), tx });
        self.send_to_peer(peer_id, Wire::FsMkdirReq { rid, path: path.into() })?;
        match tokio::time::timeout(std::time::Duration::from_secs(10), rx).await {
            Ok(Ok(res)) => res,
            _ => Err("mkdir timed out".into()),
        }
    }

    pub async fn delete(&self, peer_id: &str, path: &str, recursive: bool) -> Result<(), String> {
        let rid = Self::new_rid();
        let (tx, rx) = oneshot::channel();
        self.register(&rid, Pending::Ack { peer: peer_id.into(), tx });
        self.send_to_peer(peer_id, Wire::FsDeleteReq { rid, path: path.into(), recursive })?;
        match tokio::time::timeout(std::time::Duration::from_secs(15), rx).await {
            Ok(Ok(res)) => res,
            _ => Err("delete timed out".into()),
        }
    }

    /// Route one reply frame from a peer into the matching pending request.
    pub async fn dispatch_reply(&self, peer_id: &str, wire: Wire) {
        match wire {
            Wire::FsListRes { rid, ok, error, entries } => {
                if let Some(Pending::List { peer, tx }) = self.pending.lock().unwrap().remove(&rid) {
                    if peer == peer_id {
                        let _ = tx.send(if ok { Ok(entries) } else { Err(error) });
                    }
                }
            }
            Wire::FsStatRes { rid, ok, error, entry } => {
                if let Some(Pending::Stat { peer, tx }) = self.pending.lock().unwrap().remove(&rid) {
                    if peer == peer_id {
                        let _ = tx.send(match (ok, entry) {
                            (true, Some(e)) => Ok(e),
                            (true, None) => Err("not found".into()),
                            (false, _) => Err(error),
                        });
                    }
                }
            }
            Wire::FsData { rid, seq, data, eof } => {
                // Read chunks and (write-side) upload acks share the shape.
                let (tx, expected) = {
                    let mut map = self.pending.lock().unwrap();
                    match map.get_mut(&rid) {
                        Some(Pending::Read { peer, tx, next_seq }) if peer == peer_id => {
                            let t = tx.clone();
                            let s = *next_seq;
                            *next_seq += 1;
                            (Some(t), Some(s))
                        }
                        _ => (None, None),
                    }
                };
                if let (Some(tx), Some(expected)) = (tx, expected) {
                    let decoded = base64::engine::general_purpose::STANDARD
                        .decode(&data)
                        .map_err(|e| format!("bad chunk encoding: {e}"));
                    let item = match decoded {
                        Ok(bytes) if seq == expected => Ok(FsChunk { data: bytes, eof }),
                        Ok(_) => Err("chunk sequence mismatch — stream aborted".into()),
                        Err(e) => Err(e),
                    };
                    if tx.send(item).await.is_err() || eof {
                        self.pending.lock().unwrap().remove(&rid);
                    }
                }
            }
            Wire::FsErr { rid, error } => {
                let removed = self.pending.lock().unwrap().remove(&rid);
                if let Some(p) = removed {
                    let peer_ok = match &p {
                        Pending::List { peer, .. }
                        | Pending::Stat { peer, .. }
                        | Pending::Read { peer, .. }
                        | Pending::Ack { peer, .. } => peer == peer_id,
                    };
                    if peer_ok {
                        match p {
                            Pending::List { tx, .. } => {
                                let _ = tx.send(Err(error));
                            }
                            Pending::Stat { tx, .. } => {
                                let _ = tx.send(Err(error));
                            }
                            Pending::Read { tx, .. } => {
                                let _ = tx.send(Err(error)).await;
                            }
                            Pending::Ack { tx, .. } => {
                                let _ = tx.send(Err(error));
                            }
                        }
                    }
                }
            }
            Wire::FsOk { rid, ok, error } => {
                if let Some(Pending::Ack { peer, tx }) = self.pending.lock().unwrap().remove(&rid) {
                    if peer == peer_id {
                        let _ = tx.send(if ok { Ok(()) } else { Err(error) });
                    }
                }
            }
            _ => {}
        }
    }

    /// The session with a peer ended — fail everything we asked it.
    pub fn peer_closed(&self, peer_id: &str) {
        let mut map = self.pending.lock().unwrap();
        let rids: Vec<String> = map
            .iter()
            .filter(|(_, p)| match p {
                Pending::List { peer, .. }
                | Pending::Stat { peer, .. }
                | Pending::Read { peer, .. }
                | Pending::Ack { peer, .. } => peer == peer_id,
            })
            .map(|(k, _)| k.clone())
            .collect();
        for rid in rids {
            match map.remove(&rid) {
                Some(Pending::List { tx, .. }) => {
                    let _ = tx.send(Err("connection closed".into()));
                }
                Some(Pending::Stat { tx, .. }) => {
                    let _ = tx.send(Err("connection closed".into()));
                }
                Some(Pending::Read { tx, .. }) => {
                    let _ = tx.try_send(Err("connection closed".into()));
                }
                Some(Pending::Ack { tx, .. }) => {
                    let _ = tx.send(Err("connection closed".into()));
                }
                None => {}
            }
        }
        // Inbound uploads from that peer die with the session.
        let mut writes = self.writes.lock().unwrap();
        writes.retain(|_, w| w.peer != peer_id);
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Split a logical path into clean segments.
fn split_logical(path: &str) -> Vec<String> {
    path.replace('\\', "/")
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .map(|s| s.to_string())
        .collect()
}

/// Join `rel` onto `base` rejecting anything that could escape it.
fn safe_join(base: &Path, rel: &str) -> Result<PathBuf, String> {
    let mut p = base.to_path_buf();
    for seg in rel.replace('\\', "/").split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            return Err("“..” is not allowed in shared paths".into());
        }
        if seg.contains(':') {
            return Err("“:” is not allowed in shared paths".into());
        }
        if seg.len() > 255 {
            return Err("path segment too long".into());
        }
        p.push(seg);
    }
    Ok(p)
}

fn entry_from_meta(name: &str, meta: &std::fs::Metadata) -> FsEntryDto {
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    FsEntryDto {
        kind: if meta.is_dir() { "dir".into() } else { "file".into() },
        name: name.to_string(),
        size: if meta.is_dir() { 0 } else { meta.len() },
        modified,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_join_blocks_escapes() {
        let base = Path::new("/share");
        assert!(safe_join(base, "a/b/c.txt").is_ok());
        assert!(safe_join(base, "../etc/passwd").is_err());
        assert!(safe_join(base, "a/../../etc").is_err());
        assert!(safe_join(base, "C:/evil").is_err());
        assert!(safe_join(base, "/etc/passwd").map(|p| p.starts_with("/share")) == Ok(true));
    }

    #[test]
    fn logical_splitting() {
        assert_eq!(split_logical("/Home//Docs/./x"), vec!["Home", "Docs", "x"]);
        assert!(split_logical("/").is_empty());
    }
}
