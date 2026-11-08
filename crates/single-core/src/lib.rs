//! `single-core` — the engine behind **Single**.
//!
//! Single connects every device in a room into one shared space:
//!
//! * devices find each other automatically on the LAN (Wi-Fi / Ethernet,
//!   mDNS + broadcast beacons; manual add-by-IP as a fallback),
//! * pairing uses a PIN **and** a manual approval on both ends,
//! * all traffic is TLS-encrypted and certificate-pinned,
//! * files are served over an HTTPS/WebDAV subset so that Windows can mount
//!   paired devices as network drives in "This PC" and any other app can
//!   open the files directly,
//! * on rooted Android, per-device folders are bind-mounted at `/single`.
//!
//! The Tauri shell (`src-tauri`) only calls into this crate; all protocol
//! logic lives here and is unit-tested.

pub mod client;
pub mod config;
pub mod davserver;
pub mod discovery;
pub mod httpio;
pub mod identity;
pub mod pairing;
pub mod platform;
pub mod transfers;
pub mod xmlutil;

pub use config::{Config, Paths, APP_VERSION};
pub use pairing::{CoreEvent, IncomingReq, Pairing};
pub use pairing::TransferSnapshot;
pub use xmlutil::FsEntry;

use anyhow::{anyhow, Context as _, Result};
use discovery::{Discovery, PeerRecord};
use pairing::PairManager;
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::broadcast;
use transfers::Transfers;

/// How a device shows up in the UI.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DeviceView {
    pub id: String,
    pub name: String,
    pub addr: Option<IpAddr>,
    pub port: u16,
    pub paired: bool,
    pub online: bool,
    pub drive_letter: Option<String>,
    pub source: String,
    pub is_self: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SelfInfo {
    pub id: String,
    pub name: String,
    pub ca_fp: String,
    pub ips: Vec<IpAddr>,
    pub port: u16,
    pub share_dir: String,
    pub version: &'static str,
    pub platform: &'static str,
    pub theme: String,
    pub auto_map_drives: bool,
    pub root_mode: bool,
}

pub struct SingleApp {
    pub paths: Paths,
    pub events: broadcast::Sender<CoreEvent>,
    pub pairs: Arc<PairManager>,
    pub transfers: Arc<Transfers>,

    cfg: RwLock<Config>,
    identity: RwLock<Option<Arc<identity::Identity>>>,
    discovery: RwLock<Option<Arc<Discovery>>>,
    online: Mutex<HashMap<String, bool>>,
    effective_port: AtomicU16,
    server_offs: Mutex<Vec<tokio::sync::watch::Sender<bool>>>,
    started: AtomicBool,
}

impl SingleApp {
    pub fn new(base_dir: PathBuf) -> Result<Arc<Self>> {
        let paths = Paths::new(base_dir)?;
        let share = paths.ensure_layout()?;
        let _ = share;
        let cfg = paths.load_config();
        let (events, _) = broadcast::channel(256);
        let pairs = PairManager::load(paths.pairings_file(), events.clone());
        let transfers = Transfers::new(events.clone());
        Ok(Arc::new(SingleApp {
            paths,
            events,
            pairs,
            transfers,
            cfg: RwLock::new(cfg),
            identity: RwLock::new(None),
            discovery: RwLock::new(None),
            online: Mutex::new(HashMap::new()),
            effective_port: AtomicU16::new(config::DEFAULT_TCP_PORT),
            server_offs: Mutex::new(Vec::new()),
            started: AtomicBool::new(false),
        }))
    }

    // ------------------------------------------------------------- config

    pub fn config(&self) -> Config {
        self.cfg.read().unwrap().clone()
    }

    pub fn share_root(&self) -> PathBuf {
        self.cfg
            .read()
            .unwrap()
            .share_dir
            .clone()
            .unwrap_or_else(|| self.paths.share_root())
    }

    pub fn effective_port(&self) -> u16 {
        self.effective_port.load(Ordering::Relaxed)
    }

    pub fn identity(&self) -> Result<Arc<identity::Identity>> {
        self.identity
            .read()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow!("app not started"))
    }

    /// Update settings; restarts the network stack when needed.
    pub async fn set_settings(
        self: &Arc<Self>,
        device_name: Option<String>,
        theme: Option<String>,
        tcp_port: Option<u16>,
        share_dir: Option<String>,
        auto_map_drives: Option<bool>,
        root_mode: Option<bool>,
    ) -> Result<SelfInfo> {
        let mut restart = false;
        {
            let mut cfg = self.cfg.write().unwrap();
            if let Some(n) = device_name {
                let n = n.trim().to_string();
                if !n.is_empty() {
                    cfg.device_name = n;
                    restart = true;
                }
            }
            if let Some(t) = theme {
                cfg.theme = if t == "light" { "light".into() } else { "dark".into() };
            }
            if let Some(p) = tcp_port {
                if p >= 1024 && p != cfg.tcp_port {
                    cfg.tcp_port = p;
                    restart = true;
                }
            }
            if let Some(sd) = share_dir {
                let p = PathBuf::from(&sd);
                if !p.is_dir() {
                    return Err(anyhow!("shared folder is not a directory: {}", sd));
                }
                cfg.share_dir = Some(p);
                restart = true;
            }
            if let Some(a) = auto_map_drives {
                cfg.auto_map_drives = a;
            }
            if let Some(r) = root_mode {
                cfg.root_mode = r;
            }
        }
        let cfg = self.cfg.read().unwrap().clone();
        self.paths.save_config(&cfg)?;
        if restart && self.started.load(Ordering::SeqCst) {
            self.restart_network().await?;
        }
        Ok(self.self_info())
    }

    // -------------------------------------------------------------- start

    /// Start the HTTPS server + discovery + heartbeat. Idempotent.
    pub async fn start(self: &Arc<Self>) -> Result<()> {
        if self.started.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.started.store(true, Ordering::SeqCst);
        self.start_network().await?;

        // Android root-mode folders (best effort)
        if cfg!(target_os = "android") && self.config().root_mode {
            let this = self.clone();
            tokio::spawn(async move {
                let _ = this.setup_android_root().await;
            });
        }
        Ok(())
    }

    async fn start_network(self: &Arc<Self>) -> Result<()> {
        let cfg = self.config();
        let ips = discovery::local_ipv4s();

        // identity (created on first run, reused afterwards)
        let identity = identity::load_or_create(&self.paths, &cfg.device_name, &ips)?;
        *self.identity.write().unwrap() = Some(identity.clone());
        self.pairs.set_my_fp(identity.ca_fp().to_string());

        // bind the TLS server
        let mut port = cfg.tcp_port;
        let listener = loop {
            match tokio::net::TcpListener::bind(("0.0.0.0", port)).await {
                Ok(l) => break l,
                Err(_) if port < cfg.tcp_port + 20 => port += 1,
                Err(e) => return Err(anyhow!("cannot bind port {}: {}", cfg.tcp_port, e)),
            }
        };
        self.effective_port.store(port, Ordering::Relaxed);

        let server = davserver::DavServer::new(self.share_root(), identity.clone(), self.pairs.clone());
        let (off_tx, off_rx) = tokio::sync::watch::channel(false);
        self.server_offs.lock().unwrap().push(off_tx);
        let tls = tokio_rustls::TlsAcceptor::from(Arc::new(identity.tls_server_config()?));
        tokio::spawn(server.serve(listener, tls, off_rx));

        // discovery
        let disc = Discovery::new(identity.device_id().to_string(), self.events.clone());
        disc.start(cfg.device_name.clone(), port);
        *self.discovery.write().unwrap() = Some(disc.clone());

        // heartbeat: mark paired devices online/offline
        let this = self.clone();
        tokio::spawn(async move { heartbeat_loop(this).await });

        Ok(())
    }

    pub async fn restart_network(self: &Arc<Self>) -> Result<()> {
        // stop everything
        if let Some(disc) = self.discovery.write().unwrap().take() {
            disc.stop();
        }
        for tx in self.server_offs.lock().unwrap().drain(..) {
            let _ = tx.send(true);
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        self.start_network().await
    }

    // -------------------------------------------------------------- info

    pub fn self_info(&self) -> SelfInfo {
        let cfg = self.config();
        let identity = self.identity.read().unwrap().clone();
        SelfInfo {
            id: identity.as_ref().map(|i| i.device_id().to_string()).unwrap_or_default(),
            name: cfg.device_name,
            ca_fp: identity.as_ref().map(|i| i.ca_fp().to_string()).unwrap_or_default(),
            ips: discovery::local_ipv4s(),
            port: self.effective_port(),
            share_dir: self.share_root().to_string_lossy().to_string(),
            version: APP_VERSION,
            platform: platform::platform_name(),
            theme: cfg.theme,
            auto_map_drives: cfg.auto_map_drives,
            root_mode: cfg.root_mode,
        }
    }

    pub async fn list_devices(&self) -> Vec<DeviceView> {
        let my_id = self
            .identity
            .read()
            .unwrap()
            .as_ref()
            .map(|i| i.device_id().to_string())
            .unwrap_or_default();
        let pairings = self.pairs.list_pairings().await;
        let peers = self
            .discovery
            .read()
            .unwrap()
            .as_ref()
            .map(|d| d.list())
            .unwrap_or_default();
        let online = self.online.lock().unwrap().clone();

        let mut out: Vec<DeviceView> = Vec::new();
        for p in pairings {
            let rec = peers.iter().find(|r| r.id == p.id);
            let online = online.get(&p.id).copied().unwrap_or(false);
            out.push(DeviceView {
                id: p.id.clone(),
                name: p.name,
                addr: rec.map(|r| r.addr),
                port: rec.map(|r| r.port).unwrap_or(self.effective_port()),
                paired: true,
                online,
                drive_letter: p.drive_letter,
                source: rec.map(|r| r.source.to_string()).unwrap_or("paired".into()),
                is_self: p.id == my_id,
            });
        }
        for r in peers {
            if out.iter().any(|d| d.id == r.id) {
                continue;
            }
            out.push(DeviceView {
                id: r.id.clone(),
                name: r.name,
                addr: Some(r.addr),
                port: r.port,
                paired: false,
                online: true,
                drive_letter: None,
                source: r.source.to_string(),
                is_self: r.id == my_id,
            });
        }
        out.sort_by(|a, b| b.paired.cmp(&a.paired).then(a.name.cmp(&b.name)));
        out
    }

    // --------------------------------------------------------- connections

    async fn pinned_fps(&self) -> Vec<String> {
        self.pairs
            .list_pairings()
            .await
            .iter()
            .map(|p| p.ca_fp.clone())
            .collect()
    }

    async fn connect_to(
        &self,
        addr: IpAddr,
        port: u16,
        allow_prefix: Option<String>,
        accept_any: bool,
        timeout: Duration,
    ) -> Result<client::Conn> {
        let identity = self.identity()?;
        let pinned = self.pinned_fps().await;
        client::connect(
            addr,
            port,
            &pinned,
            allow_prefix,
            accept_any,
            &identity,
            timeout,
        )
        .await
    }

    async fn connect_paired(&self, pairing: &Pairing) -> Result<client::Conn> {
        let rec = self
            .discovery
            .read()
            .unwrap()
            .as_ref()
            .and_then(|d| d.get(&pairing.id));
        let (addr, port) = match rec {
            Some(r) => (r.addr, r.port),
            None => return Err(anyhow!("device is offline (not discovered right now)")),
        };
        // pin both to the recorded fp and (paranoia) the device id prefix
        self.connect_to(addr, port, Some(pairing.id.clone()), false, Duration::from_secs(6))
            .await
    }

    #[allow(dead_code)]
    async fn connect_any_id(&self, peer_id: &str) -> Result<client::Conn> {
        let rec = self
            .discovery
            .read()
            .unwrap()
            .as_ref()
            .and_then(|d| d.get(peer_id))
            .ok_or_else(|| anyhow!("device not found on the network"))?;
        self.connect_to(rec.addr, rec.port, Some(peer_id.to_string()), false, Duration::from_secs(6))
            .await
    }

    // ------------------------------------------------------------- pairing

    /// A-side: ask a discovered device to pair. Returns the request id that
    /// the UI polls with `pair_status` / submits with `pair_submit_pin`.
    pub async fn pair_start(self: &Arc<Self>, peer_id: &str) -> Result<String> {
        if self.pairs.pairing_by_id(peer_id).await.is_some() {
            return Err(anyhow!("already paired with this device"));
        }
        let rec = self
            .discovery
            .read()
            .unwrap()
            .as_ref()
            .and_then(|d| d.get(peer_id))
            .ok_or_else(|| anyhow!("device not found on the network"))?;

        let identity = self.identity()?;
        let conn = self
            .connect_to(rec.addr, rec.port, Some(peer_id.to_string()), false, Duration::from_secs(6))
            .await?;
        let mut conn = conn;
        let (_status, v) = client::post_json(
            &mut conn,
            "/single/pair/request",
            &serde_json::json!({
                "id": identity.device_id(),
                "name": identity.device_name,
                "fp": identity.ca_fp(),
            }),
            None,
        )
        .await
        .context("could not reach device (is Single running there?)")?;

        // already paired on their side but not here? complete immediately
        if v["status"] == "paired" {
            let token = v["token"].as_str().unwrap_or("").to_string();
            let fp = conn
                .peer_ca_fp
                .clone()
                .unwrap_or_default();
            if token.is_empty() || fp.is_empty() {
                return Err(anyhow!("pairing reply was incomplete"));
            }
            self.pairs
                .complete_outgoing(
                    "",
                    Pairing {
                        id: peer_id.to_string(),
                        name: v["name"].as_str().unwrap_or("Device").to_string(),
                        ca_fp: fp,
                        token,
                        added_at: now(),
                        drive_letter: None,
                    },
                )
                .await;
            self.after_pair_success(peer_id).await;
            return Ok(String::new());
        }

        let req_id = v["req_id"]
            .as_str()
            .ok_or_else(|| anyhow!("device did not accept pairing request"))?
            .to_string();
        let addr = format!("{}:{}", rec.addr, rec.port);
        self.pairs.start_outgoing(&addr, Some(peer_id.to_string())).await;
        // remember the real (remote) request id by replacing the placeholder
        self.pairs.rename_outgoing(req_id.clone()).await;
        Ok(req_id)
    }

    /// A-side: submit the PIN shown on the target device.
    pub async fn pair_submit_pin(self: &Arc<Self>, req_id: &str, pin: &str) -> Result<String> {
        let target = self
            .pairs
            .outgoing_by_req(req_id)
            .await
            .ok_or_else(|| anyhow!("unknown pairing request"))?;
        let target_id = target.target_id.unwrap_or_default();
        let (ip, port) = parse_addr(&target.target_addr)?;

        let conn = self
            .connect_to(ip, port, Some(target_id.clone()), false, Duration::from_secs(6))
            .await?;
        let mut conn = conn;
        let (status, v) = client::post_json(
            &mut conn,
            "/single/pair/verify",
            &serde_json::json!({"req_id": req_id, "pin": pin}),
            None,
        )
        .await?;

        match (status, v["status"].as_str().unwrap_or("")) {
            (200, "verified") | (200, "pin_ok") => {
                if v["status"] == "verified" {
                    let token = v["token"].as_str().unwrap_or("").to_string();
                    let fp = conn
                        .peer_ca_fp
                        .clone()
                        .or_else(|| v["fp"].as_str().map(|s| s.to_string()))
                        .unwrap_or_default();
                    if token.is_empty() || fp.is_empty() {
                        return Err(anyhow!("device replied without a token"));
                    }
                    self.pairs
                        .complete_outgoing(
                            req_id,
                            Pairing {
                                id: v["id"].as_str().unwrap_or(&target_id).to_string(),
                                name: v["name"].as_str().unwrap_or("Device").to_string(),
                                ca_fp: fp,
                                token,
                                added_at: now(),
                                drive_letter: None,
                            },
                        )
                        .await;
                    let id = v["id"].as_str().unwrap_or(&target_id).to_string();
                    self.after_pair_success(&id).await;
                    return Ok("verified".into());
                }
                Ok("pin_ok".into())
            }
            (_, "bad_pin") | (403, _) => Err(anyhow!("wrong PIN")),
            (s, other) => Err(anyhow!("pairing failed ({}: {})", s, other)),
        }
    }

    /// A-side: poll the state of a pairing request.
    pub async fn pair_status(self: &Arc<Self>, req_id: &str) -> Result<String> {
        let target = self
            .pairs
            .outgoing_by_req(req_id)
            .await
            .ok_or_else(|| anyhow!("unknown pairing request"))?;
        let target_id = target.target_id.unwrap_or_default();
        let (ip, port) = parse_addr(&target.target_addr)?;

        let conn = self
            .connect_to(ip, port, Some(target_id.clone()), false, Duration::from_secs(6))
            .await?;
        let mut conn = conn;
        let (status, v) = client::post_json(
            &mut conn,
            "/single/pair/check",
            &serde_json::json!({"req_id": req_id}),
            None,
        )
        .await?;

        let state = v["status"].as_str().unwrap_or("unknown").to_string();
        if status == 200 && state == "verified" {
            let token = v["token"].as_str().unwrap_or("").to_string();
            let fp = conn
                .peer_ca_fp
                .clone()
                .or_else(|| v["fp"].as_str().map(|s| s.to_string()))
                .unwrap_or_default();
            if !token.is_empty() && !fp.is_empty() {
                let id = v["id"].as_str().unwrap_or(&target_id).to_string();
                self.pairs
                    .complete_outgoing(
                        req_id,
                        Pairing {
                            id: id.clone(),
                            name: v["name"].as_str().unwrap_or("Device").to_string(),
                            ca_fp: fp,
                            token,
                            added_at: now(),
                            drive_letter: None,
                        },
                    )
                    .await;
                self.after_pair_success(&id).await;
            }
        }
        Ok(state)
    }

    /// B-side: incoming requests waiting for approval.
    pub async fn pair_incoming(&self) -> Vec<IncomingReq> {
        self.pairs.list_incoming().await
    }

    pub async fn pair_approve(&self, req_id: &str) -> Result<()> {
        if !self.pairs.approve(req_id).await {
            return Err(anyhow!("request not found (expired?)"));
        }
        Ok(())
    }

    pub async fn pair_deny(&self, req_id: &str) -> Result<()> {
        if !self.pairs.deny(req_id).await {
            return Err(anyhow!("request not found (expired?)"));
        }
        Ok(())
    }

    pub async fn unpair(self: &Arc<Self>, peer_id: &str) -> Result<()> {
        // tell the peer (best effort)
        if let Some(pairing) = self.pairs.pairing_by_id(peer_id).await {
            if let Ok(mut conn) = self.connect_paired(&pairing).await {
                let identity = self.identity()?;
                let _ = client::post_json(
                    &mut conn,
                    "/single/pair/unpair",
                    &serde_json::json!({"id": identity.device_id()}),
                    Some(&pairing.token),
                )
                .await;
            }
        }
        self.pairs.remove_pairing(peer_id).await;
        self.online.lock().unwrap().remove(peer_id);
        #[cfg(windows)]
        {
            // remove a mapped drive if we created one
            if let Some(p) = self.pairs.pairing_by_id(peer_id).await {
                if let Some(letter) = p.drive_letter {
                    let _ = platform::win::unmap_drive(&letter).await;
                }
            }
        }
        Ok(())
    }

    async fn after_pair_success(self: &Arc<Self>, peer_id: &str) {
        let cfg = self.config();
        // auto-create the Android per-device folder
        if cfg!(target_os = "android") {
            if let Some(p) = self.pairs.pairing_by_id(peer_id).await {
                let dir = self.paths.device_folders();
                let folder = dir.join(format!(
                    "{} ({})",
                    config::safe_folder_name(&p.name),
                    &p.id[..6.min(p.id.len())]
                ));
                let _ = std::fs::create_dir_all(&folder);
            }
        }
        // auto-map a drive on Windows
        if cfg!(windows) && cfg.auto_map_drives {
            let this = self.clone();
            let id = peer_id.to_string();
            tokio::spawn(async move {
                match this.win_map_drive(&id).await {
                    Ok(letter) => {
                        let _ = this.events.send(CoreEvent::Toast {
                            message: format!("Mapped drive {}:", letter),
                        });
                    }
                    Err(e) => {
                        let _ = this.events.send(CoreEvent::Toast {
                            message: format!("Paired, but drive mapping failed: {}", e),
                        });
                    }
                }
            });
        }
        let _ = self.events.send(CoreEvent::DevicesChanged);
    }

    // -------------------------------------------------------------- files

    pub async fn browse_local(&self, rel: &str) -> Result<Vec<FsEntry>> {
        let root = self.share_root();
        let full = safe_join(&root, rel)?;
        if !full.exists() {
            return Err(anyhow!("folder not found"));
        }
        let mut out = Vec::new();
        let mut rd = tokio::fs::read_dir(&full).await?;
        while let Some(e) = rd.next_entry().await? {
            let name = e.file_name().to_string_lossy().to_string();
            let md = match e.metadata().await {
                Ok(m) => m,
                Err(_) => continue,
            };
            let mtime = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let child_rel = if rel.trim_matches('/').is_empty() {
                name.clone()
            } else {
                format!("{}/{}", rel.trim_matches('/'), name)
            };
            out.push(FsEntry {
                is_dir: md.is_dir(),
                size: if md.is_dir() { 0 } else { md.len() },
                mtime,
                name,
                path: child_rel,
            });
        }
        out.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        Ok(out)
    }

    pub async fn browse_peer(&self, peer_id: &str, rel: &str) -> Result<Vec<FsEntry>> {
        let pairing = self
            .pairs
            .pairing_by_id(peer_id)
            .await
            .ok_or_else(|| anyhow!("not paired with this device"))?;
        let mut conn = self.connect_paired(&pairing).await?;
        client::propfind(&mut conn, &pairing.token, rel).await
    }

    pub async fn local_mkdir(&self, rel: &str) -> Result<()> {
        let full = safe_join(&self.share_root(), rel)?;
        tokio::fs::create_dir_all(&full).await?;
        Ok(())
    }

    pub async fn local_rename(&self, rel: &str, dest: &str) -> Result<()> {
        let root = self.share_root();
        let from = safe_join(&root, rel)?;
        let to = safe_join(&root, dest)?;
        tokio::fs::rename(&from, &to).await?;
        Ok(())
    }

    pub async fn local_delete(&self, rel: &str) -> Result<()> {
        let full = safe_join(&self.share_root(), rel)?;
        if full == self.share_root() {
            return Err(anyhow!("cannot delete the share root"));
        }
        let md = tokio::fs::symlink_metadata(&full).await?;
        if md.is_dir() {
            tokio::fs::remove_dir_all(&full).await?;
        } else {
            tokio::fs::remove_file(&full).await?;
        }
        Ok(())
    }

    pub async fn peer_mkdir(&self, peer_id: &str, rel: &str) -> Result<()> {
        let pairing = self.pairs.pairing_by_id(peer_id).await.ok_or_else(|| anyhow!("not paired"))?;
        let mut conn = self.connect_paired(&pairing).await?;
        client::mkdir(&mut conn, &pairing.token, rel).await
    }

    pub async fn peer_delete(&self, peer_id: &str, rel: &str) -> Result<()> {
        let pairing = self.pairs.pairing_by_id(peer_id).await.ok_or_else(|| anyhow!("not paired"))?;
        let mut conn = self.connect_paired(&pairing).await?;
        client::delete(&mut conn, &pairing.token, rel).await
    }

    pub async fn peer_rename(&self, peer_id: &str, rel: &str, dest: &str) -> Result<()> {
        let pairing = self.pairs.pairing_by_id(peer_id).await.ok_or_else(|| anyhow!("not paired"))?;
        let mut conn = self.connect_paired(&pairing).await?;
        client::rename(&mut conn, &pairing.token, rel, dest).await
    }

    /// Upload local files to a peer folder. Returns transfer ids.
    pub async fn upload_files(
        self: &Arc<Self>,
        peer_id: &str,
        remote_dir: &str,
        local_paths: Vec<String>,
    ) -> Vec<u64> {
        let mut ids = Vec::new();
        for lp in local_paths {
            let path = PathBuf::from(&lp);
            let meta = match tokio::fs::metadata(&path).await {
                Ok(m) if m.is_file() => m,
                _ => continue,
            };
            let total = meta.len();
            let fname = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "file".into());
            let remote = format!("{}/{}", remote_dir.trim_end_matches('/'), fname);
            let tid = self.transfers.begin("upload", peer_id, &self.peer_name(peer_id).await, &remote, total);
            ids.push(tid);

            let this = self.clone();
            let peer_id = peer_id.to_string();
            let remote = remote.clone();
            tokio::spawn(async move {
                let res = async {
                    let pairing = this
                        .pairs
                        .pairing_by_id(&peer_id)
                        .await
                        .ok_or_else(|| anyhow!("not paired"))?;
                    let mut file = tokio::fs::File::open(&path).await?;
                    let mut conn = this.connect_paired(&pairing).await?;
                    let mut prog = transfers::Progress::new(tid, this.transfers.clone());
                    client::upload_from(
                        &mut conn,
                        &pairing.token,
                        &remote,
                        &mut file,
                        total,
                        |done| prog.tick(done),
                    )
                    .await
                }
                .await;
                match res {
                    Ok(_) => {
                        this.transfers.finish(tid, "done");
                        let _ = this.events.send(CoreEvent::Toast {
                            message: format!("Uploaded {}", remote.rsplit('/').next().unwrap_or("")),
                        });
                    }
                    Err(e) => {
                        this.transfers.finish(tid, "failed");
                        let _ = this.events.send(CoreEvent::Toast {
                            message: format!("Upload failed: {}", e),
                        });
                    }
                }
            });
        }
        ids
    }

    /// Download a remote file to a local folder (default: Downloads).
    pub async fn download_file(
        self: &Arc<Self>,
        peer_id: &str,
        remote_path: &str,
        local_dir: Option<String>,
    ) -> Result<u64> {
        let _pairing = self
            .pairs
            .pairing_by_id(peer_id)
            .await
            .ok_or_else(|| anyhow!("not paired"))?;
        let dir = match local_dir {
            Some(d) => PathBuf::from(d),
            None => self.paths.downloads(),
        };
        tokio::fs::create_dir_all(&dir).await?;
        let fname = remote_path.rsplit('/').next().unwrap_or("file");
        let mut dest = dir.join(fname);
        // avoid overwriting
        let stem = dest
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".into());
        let ext = dest
            .extension()
            .map(|s| format!(".{}", s.to_string_lossy()))
            .unwrap_or_default();
        let mut n = 1;
        while dest.exists() {
            dest = dir.join(format!("{} ({}){}", stem, n, ext));
            n += 1;
        }
        let total = self
            .browse_peer(peer_id, &parent_of(remote_path))
            .await
            .ok()
            .and_then(|v| {
                v.iter()
                    .find(|e| e.path == remote_path.trim_matches('/'))
                    .map(|e| e.size)
            })
            .unwrap_or(0);

        let tid = self.transfers.begin("download", peer_id, &self.peer_name(peer_id).await, remote_path, total);
        let this = self.clone();
        let peer_id = peer_id.to_string();
        let remote_path = remote_path.to_string();
        let dest2 = dest.clone();
        tokio::spawn(async move {
            let res: anyhow::Result<u64> = async {
                let pairing = this
                    .pairs
                    .pairing_by_id(&peer_id)
                    .await
                    .ok_or_else(|| anyhow!("not paired"))?;
                let mut file = tokio::fs::File::create(&dest2).await?;
                let mut conn = this.connect_paired(&pairing).await?;
                let mut prog = transfers::Progress::new(tid, this.transfers.clone());
                let written = client::download_to(
                    &mut conn,
                    &pairing.token,
                    &remote_path,
                    &mut file,
                    |done| prog.tick(done),
                )
                .await?;
                prog.final_tick(written);
                Ok(written)
            }
            .await;
            match res {
                Ok(_) => this.transfers.finish(tid, "done"),
                Err(e) => {
                    this.transfers.finish(tid, "failed");
                    let _ = tokio::fs::remove_file(&dest2).await;
                    let _ = this.events.send(CoreEvent::Toast {
                        message: format!("Download failed: {}", e),
                    });
                }
            }
        });
        Ok(tid)
    }

    async fn peer_name(&self, peer_id: &str) -> String {
        self.pairs
            .pairing_by_id(peer_id)
            .await
            .map(|p| p.name)
            .unwrap_or_else(|| "Device".into())
    }

    // ------------------------------------------------------------- windows

    pub async fn win_map_drive(self: &Arc<Self>, peer_id: &str) -> Result<String> {
        let pairing = self
            .pairs
            .pairing_by_id(peer_id)
            .await
            .ok_or_else(|| anyhow!("not paired"))?;
        let rec = self
            .discovery
            .read()
            .unwrap()
            .as_ref()
            .and_then(|d| d.get(peer_id))
            .ok_or_else(|| anyhow!("device is offline"))?;
        let letter = platform::win::map_drive(&rec.addr.to_string(), rec.port, &pairing.token).await?;
        self.pairs
            .set_drive_letter(peer_id, Some(letter.clone()))
            .await;
        Ok(letter)
    }

    pub async fn win_unmap_drive(&self, letter: &str) -> Result<()> {
        platform::win::unmap_drive(letter).await?;
        Ok(())
    }

    pub fn win_list_drives(&self) -> Vec<(String, String)> {
        platform::win::list_mapped()
    }

    pub fn win_drive_support(&self) -> Result<()> {
        let ca = self.paths.ca_cert();
        platform::win::enable_drive_support(&ca, self.effective_port())
    }

    // ------------------------------------------------------------- android

    pub async fn setup_android_root(&self) -> Result<Vec<String>> {
        let pairings = self.pairs.list_pairings().await;
        let dir = self.paths.device_folders();
        let _ = std::fs::create_dir_all(&dir);
        let mut peers = Vec::new();
        for p in &pairings {
            let folder = format!(
                "{} ({})",
                config::safe_folder_name(&p.name),
                &p.id[..6.min(p.id.len())]
            );
            let _ = std::fs::create_dir_all(dir.join(&folder));
            peers.push((p.name.clone(), p.id.clone()));
        }
        let report = platform::android::setup_root_folders(&dir, &peers)?;
        Ok(report.lines)
    }

    // --------------------------------------------------------- manual add

    /// Manually add a device by IP:port (discovery fallback). Returns its
    /// name when reachable.
    pub async fn add_manual(self: &Arc<Self>, ip: IpAddr, port: u16) -> Result<String> {
        let conn = self
            .connect_to(ip, port, None, true, Duration::from_secs(5))
            .await?;
        let mut conn = conn;
        let info = client::ping(&mut conn).await?;
        if info.id.len() < 8 {
            return Err(anyhow!("that address is not running Single"));
        }
        // cross-check the pinged identity against the presented certificate
        if let Some(fp) = &conn.peer_ca_fp {
            if !fp.eq_ignore_ascii_case(&info.ca_fp) {
                return Err(anyhow!(
                    "device identity mismatch (conn={} ping={})",
                    fp,
                    info.ca_fp
                ));
            }
        }
        if let Some(disc) = self.discovery.read().unwrap().clone() {
            disc.upsert(PeerRecord {
                id: info.id.clone(),
                name: info.name.clone(),
                port,
                addr: ip,
                last_seen: now(),
                source: "manual",
            });
        }
        Ok(info.name)
    }
}

// ------------------------------------------------------------------ helpers

async fn heartbeat_loop(app: Arc<SingleApp>) {
    loop {
        tokio::time::sleep(Duration::from_secs(6)).await;
        let pairings = app.pairs.list_pairings().await;
        for p in pairings {
            let mut online = false;
            if let Some(disc) = app.discovery.read().unwrap().clone() {
                if let Some(rec) = disc.get(&p.id) {
                    if rec.last_seen + 20 >= now() {
                        online = true;
                    }
                }
            }
            if !online {
                // direct probe
                online = (async {
                    let mut conn = app.connect_paired(&p).await.ok()?;
                    client::ping(&mut conn).await.ok()?;
                    Some(true)
                })
                .await
                .unwrap_or(false);
            }
            let mut map = app.online.lock().unwrap();
            let changed = map.get(&p.id).copied().unwrap_or(false) != online;
            map.insert(p.id.clone(), online);
            drop(map);
            if changed {
                let _ = app.events.send(CoreEvent::DevicesChanged);
            }
        }
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn parent_of(path: &str) -> String {
    let t = path.trim_end_matches('/');
    match t.rfind('/') {
        Some(i) => t[..i].to_string(),
        None => String::new(),
    }
}

fn parse_addr(s: &str) -> Result<(IpAddr, u16)> {
    let (ip, port) = s
        .rsplit_once(':')
        .ok_or_else(|| anyhow!("bad address: {}", s))?;
    Ok((
        ip.parse().map_err(|_| anyhow!("bad ip: {}", ip))?,
        port.parse().map_err(|_| anyhow!("bad port: {}", port))?,
    ))
}

/// Join a share-root-relative path safely (no traversal, no windows junk).
pub fn safe_join(root: &Path, rel: &str) -> Result<PathBuf> {
    let mut p = root.to_path_buf();
    for seg in rel.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." || seg.contains('\\') || seg.contains('\0') || seg.contains(':') {
            return Err(anyhow!("invalid path segment"));
        }
        p.push(seg);
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addr_parsing() {
        assert_eq!(
            parse_addr("192.168.1.5:27500").unwrap(),
            ("192.168.1.5".parse().unwrap(), 27500)
        );
        assert!(parse_addr(" nonsense ").is_err());
    }

    #[test]
    fn parent_paths() {
        assert_eq!(parent_of("a/b/c.txt"), "a/b");
        assert_eq!(parent_of("root.txt"), "");
        assert_eq!(parent_of("/x/y/"), "/x");
    }

    #[test]
    fn safe_join_rejects_traversal() {
        let root = Path::new("/srv");
        assert!(safe_join(root, "a/b").is_ok());
        assert!(safe_join(root, "../etc").is_err());
        assert!(safe_join(root, "a\\b").is_err());
        assert!(safe_join(root, "a/../b").is_err());
    }
}
