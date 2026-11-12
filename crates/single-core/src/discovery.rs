//! LAN discovery.
//!
//! Three complementary mechanisms, all zero-config:
//!
//! 1. **mDNS/DNS-SD** (`_single._tcp.local.`) — the primary mechanism on
//!    desktop (Wi-Fi and Ethernet). Bluetooth devices that expose a network
//!    interface (PAN) are picked up too; dedicated Bluetooth discovery is a
//!    roadmap item.
//! 2. **UDP broadcast beacons** on port 27501 every 3 s — works where
//!    multicast is filtered (some Wi-Fi APs / Android power saving).
//! 3. **Manual "add by IP"** — the always-works fallback.

use crate::config::{APP_VERSION, BEACON_PORT};
use crate::pairing::CoreEvent;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;
use tokio::sync::watch;

pub const SERVICE_TYPE: &str = "_single._tcp.local.";

#[derive(Debug, Clone, Serialize)]
pub struct PeerRecord {
    pub id: String,
    pub name: String,
    pub port: u16,
    pub addr: IpAddr,
    pub last_seen: u64,
    pub source: &'static str,
}

#[derive(Debug, Serialize, Deserialize)]
struct Beacon {
    id: String,
    name: String,
    port: u16,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub struct Discovery {
    my_id: String,
    peers: Mutex<HashMap<String, PeerRecord>>,
    /// mDNS fullname -> peer id, so goodbye events can remove the right peer
    mdns_names: Mutex<HashMap<String, String>>,
    events: broadcast::Sender<CoreEvent>,
    stop_tx: watch::Sender<bool>,
    stop_rx: watch::Receiver<bool>,
}

impl Discovery {
    pub fn new(my_id: String, events: broadcast::Sender<CoreEvent>) -> Arc<Self> {
        let (stop_tx, stop_rx) = watch::channel(false);
        Arc::new(Discovery {
            my_id,
            peers: Mutex::new(HashMap::new()),
            mdns_names: Mutex::new(HashMap::new()),
            events,
            stop_tx,
            stop_rx,
        })
    }

    /// Start advertising + browsing. Safe to call again after `stop()`
    /// (e.g. when the device name changes).
    pub fn start(self: &Arc<Self>, name: String, tcp_port: u16) {
        let mdns_discovery = self.clone();
        let name_a = name.clone();
        tokio::spawn(async move { mdns_discovery.mdns_task(name_a, tcp_port).await });

        let beacon_discovery = self.clone();
        tokio::spawn(async move { beacon_discovery.beacon_task(name, tcp_port).await });

        let prune_discovery = self.clone();
        tokio::spawn(async move { prune_discovery.prune_task().await });
    }

    pub fn stop(&self) {
        let _ = self.stop_tx.send(true);
    }

    // ------------------------------------------------------------ mDNS

    async fn mdns_task(self: Arc<Self>, name: String, tcp_port: u16) {
        let daemon: ServiceDaemon = match ServiceDaemon::new() {
            Ok(d) => d,
            Err(_) => return, // mDNS unavailable — beacons still work
        };

        let id6 = &self.my_id[..6.min(self.my_id.len())];
        let instance = format!("{}-{}", sanitize_instance(&name), id6);
        let host = format!("single-{}.local.", id6);
        let props = [
            ("id", self.my_id.as_str()),
            ("name", name.as_str()),
            ("ver", APP_VERSION),
            ("port", &tcp_port.to_string()),
        ];
        let info = match ServiceInfo::new(
            SERVICE_TYPE,
            &instance,
            &host,
            "0.0.0.0",
            tcp_port,
            &props[..],
        ) {
            Ok(i) => i.enable_addr_auto(),
            Err(_) => return,
        };
        if daemon.register(info).is_err() {
            return;
        }
        let receiver = match daemon.browse(SERVICE_TYPE) {
            Ok(r) => r,
            Err(_) => return,
        };

        let mut stop = self.stop_rx.clone();
        loop {
            let ev = tokio::select! {
                _ = stop.changed() => {
                    if *stop.borrow() {
                        let _ = daemon.unregister(&format!("{}._single._tcp.local.", instance));
                        let _ = daemon.stop_browse(SERVICE_TYPE);
                        break;
                    }
                    continue;
                }
                ev = receiver.recv_async() => ev,
            };
            let Ok(event) = ev else { break };
            match event {
                ServiceEvent::ServiceResolved(info) => {
                    let txt = |k: &str| -> String {
                        info
                            .get_property_val_str(k)
                            .unwrap_or("")
                            .to_string()
                    };
                    let id = txt("id");
                    if id.is_empty() || id == self.my_id {
                        continue;
                    }
                    let peer_name = txt("name");
                    let peer_name = if peer_name.is_empty() {
                        info.get_fullname().split('.').next().unwrap_or("Device").to_string()
                    } else {
                        peer_name
                    };
                    let port = info.get_port();
                    let addr = pick_addr(info.get_addresses());
                    if let Some(addr) = addr {
                        let fullname = info.get_fullname().to_string();
                        self.mdns_names
                            .lock()
                            .unwrap()
                            .insert(fullname, id.clone());
                        self.upsert(PeerRecord {
                            id,
                            name: peer_name,
                            port,
                            addr,
                            last_seen: now(),
                            source: "mdns",
                        });
                    }
                }
                ServiceEvent::ServiceRemoved(_ty, fullname) => {
                    let map = self.mdns_names.lock().unwrap();
                    if let Some(id) = map.get(&fullname) {
                        let id = id.clone();
                        drop(map);
                        self.remove(&id);
                    }
                }
                _ => {}
            }
        }
    }

    // ------------------------------------------------------------ beacons

    async fn beacon_task(self: Arc<Self>, name: String, tcp_port: u16) {
        use socket2::{Domain, Protocol, Socket, Type};
        let socket = match Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)) {
            Ok(s) => s,
            Err(_) => return,
        };
        let _ = socket.set_reuse_address(true);
        let _ = socket.set_broadcast(true);
        let _ = socket.set_nonblocking(true);
        let addr: std::net::SocketAddr = format!("0.0.0.0:{}", BEACON_PORT).parse().unwrap();
        if socket.bind(&addr.into()).is_err() {
            return;
        }
        let std_sock: std::net::UdpSocket = socket.into();
        let sock = match tokio::net::UdpSocket::from_std(std_sock) {
            Ok(s) => s,
            Err(_) => return,
        };

        let beacon = serde_json::json!({
            "id": self.my_id,
            "name": name,
            "port": tcp_port,
            "ver": APP_VERSION,
        })
        .to_string()
        .into_bytes();

        let mut buf = [0u8; 2048];
        let mut interval = tokio::time::interval(Duration::from_secs(3));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut stop = self.stop_rx.clone();

        loop {
            tokio::select! {
                _ = stop.changed() => {
                    if *stop.borrow() { break; }
                }
                _ = interval.tick() => {
                    let target = format!("255.255.255.255:{}", BEACON_PORT);
                    let _ = sock.send_to(&beacon, &target).await;
                }
                r = sock.recv_from(&mut buf) => {
                    match r {
                        Ok((n, src)) => {
                            let Ok(txt) = std::str::from_utf8(&buf[..n]) else { continue };
                            let Ok(b) = serde_json::from_str::<Beacon>(txt) else { continue };
                            if b.id == self.my_id || b.id.len() < 8 {
                                continue;
                            }
                            if src.ip().is_loopback() {
                                continue;
                            }
                            self.upsert(PeerRecord {
                                id: b.id,
                                name: b.name,
                                port: b.port,
                                addr: src.ip(),
                                last_seen: now(),
                                source: "beacon",
                            });
                        }
                        Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------ prune

    async fn prune_task(self: Arc<Self>) {
        let stop = self.stop_rx.clone();
        loop {
            tokio::time::sleep(Duration::from_secs(15)).await;
            if *stop.borrow() {
                break;
            }
            let cutoff = now().saturating_sub(45);
            let mut peers = self.peers.lock().unwrap();
            let before = peers.len();
            peers.retain(|_, p| p.source == "manual" || p.last_seen >= cutoff);
            if peers.len() != before {
                drop(peers);
                let _ = self.events.send(CoreEvent::DevicesChanged);
            }
        }
    }

    // ------------------------------------------------------------ shared

    /// Insert or refresh a peer. Emits DevicesChanged when something changed.
    pub fn upsert(&self, rec: PeerRecord) {
        let mut peers = self.peers.lock().unwrap();
        match peers.get_mut(&rec.id) {
            Some(existing) => {
                let changed = existing.name != rec.name
                    || existing.addr != rec.addr
                    || existing.port != rec.port;
                existing.name = rec.name;
                existing.addr = rec.addr;
                existing.port = rec.port;
                existing.last_seen = rec.last_seen;
                if rec.source == "manual" {
                    existing.source = "manual";
                }
                if changed {
                    drop(peers);
                    let _ = self.events.send(CoreEvent::DevicesChanged);
                }
            }
            None => {
                peers.insert(rec.id.clone(), rec);
                drop(peers);
                let _ = self.events.send(CoreEvent::DevicesChanged);
            }
        }
    }

    pub fn remove(&self, id: &str) {
        let mut peers = self.peers.lock().unwrap();
        if peers.remove(id).is_some() {
            drop(peers);
            let _ = self.events.send(CoreEvent::DevicesChanged);
        }
    }

    pub fn list(&self) -> Vec<PeerRecord> {
        let peers = self.peers.lock().unwrap();
        let mut v: Vec<PeerRecord> = peers.values().cloned().collect();
        v.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        v
    }

    pub fn get(&self, id: &str) -> Option<PeerRecord> {
        self.peers.lock().unwrap().get(id).cloned()
    }
}

fn sanitize_instance(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    if trimmed.is_empty() {
        "device".to_string()
    } else {
        trimmed.chars().take(40).collect()
    }
}

fn pick_addr(addrs: &std::collections::HashSet<IpAddr>) -> Option<IpAddr> {
    // prefer global IPv4, then any IPv4, then anything
    let v4: Vec<&IpAddr> = addrs.iter().filter(|a| a.is_ipv4()).collect();
    for a in &v4 {
        if let IpAddr::V4(ip) = a {
            if !ip.is_loopback() && !ip.is_link_local() {
                return Some(**a);
            }
        }
    }
    v4.first().map(|a| **a).or_else(|| addrs.iter().next().copied())
}

/// Local IPv4 addresses (for TLS SANs and drive-mapping hints).
pub fn local_ipv4s() -> Vec<IpAddr> {
    let mut out = Vec::new();
    if let Ok(addrs) = if_addrs::get_if_addrs() {
        for a in addrs {
            if a.ip().is_ipv4() && !a.ip().is_loopback() {
                out.push(a.ip());
            }
        }
    }
    if out.is_empty() {
        out.push("127.0.0.1".parse().unwrap());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_sanitizer() {
        assert_eq!(sanitize_instance("My PC"), "My-PC");
        assert_eq!(sanitize_instance("!!!"), "device");
        assert_eq!(sanitize_instance("-Dev-"), "Dev");
    }

    #[test]
    fn addr_picker_prefers_global_v4() {
        let mut set = std::collections::HashSet::new();
        set.insert("127.0.0.1".parse::<IpAddr>().unwrap());
        set.insert("192.168.1.7".parse::<IpAddr>().unwrap());
        assert_eq!(pick_addr(&set), Some("192.168.1.7".parse().unwrap()));
    }

    #[test]
    fn beacon_json_shape() {
        let b = Beacon {
            id: "x".repeat(16),
            name: "N".into(),
            port: 27500,
        };
        let _ = serde_json::to_string(&b).unwrap();
    }
}
