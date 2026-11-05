//! Live device registry fed by mDNS browse events.
//!
//! A single task owns the [`ServiceEvent`] receiver and:
//! - upserts devices on every `ServiceResolved` (mDNS refresh cycle),
//! - removes devices on `ServiceRemoved` (goodbye packet),
//! - reaps devices whose last refresh is older than `DEVICE_TTL_MS`,
//! - restarts the browse every `BROWSE_CYCLE_SECS` (mdns-sd retransmission
//!   backs off exponentially; a fresh browse keeps queries flowing),
//! - fans device-list snapshots out to all dashboards over the WS bus.

use crate::config::{BROWSE_CYCLE_SECS, DEVICE_TTL_MS, SERVICE_TYPE};
use crate::protocol::{DeviceDto, ServerMsg};
use crate::util::{log_bus, now_ms};
use mdns_sd::{ServiceDaemon, ServiceEvent};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::broadcast;
use tokio::time::{interval, Duration, MissedTickBehavior};

#[derive(Clone, Debug)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub ip: String,
    pub port: u16,
    pub hostname: String,
    pub first_seen: i64,
    pub last_seen: i64,
}

#[derive(Clone)]
pub struct Registry {
    inner: Arc<Mutex<HashMap<String, Device>>>,
    bus: broadcast::Sender<ServerMsg>,
    /// Our own mDNS fullname — never list ourselves as a peer.
    pub self_id: Arc<RwLock<String>>,
}

impl Registry {
    pub fn new(bus: broadcast::Sender<ServerMsg>, self_id: Arc<RwLock<String>>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            bus,
            self_id,
        }
    }

    pub fn get(&self, id: &str) -> Option<Device> {
        self.inner.lock().unwrap().get(id).cloned()
    }

    pub fn snapshot(&self) -> Vec<DeviceDto> {
        let map = self.inner.lock().unwrap();
        let mut list: Vec<DeviceDto> = map
            .values()
            .map(|d| DeviceDto {
                id: d.id.clone(),
                name: d.name.clone(),
                platform: d.platform.clone(),
                ip: d.ip.clone(),
                port: d.port,
                hostname: d.hostname.clone(),
                first_seen: d.first_seen,
                last_seen: d.last_seen,
            })
            .collect();
        list.sort_by(|a, b| {
            platform_rank(&a.platform)
                .cmp(&platform_rank(&b.platform))
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        list
    }

    pub fn broadcast_devices(&self) {
        let devices = self.snapshot();
        let _ = self.bus.send(ServerMsg::Devices { devices });
    }
}

fn platform_rank(p: &str) -> u8 {
    match p {
        "android" => 0,
        "windows" => 1,
        _ => 2,
    }
}

/// Run the mDNS browse loop forever (spawned from `main`).
pub async fn run_discovery(daemon: Arc<ServiceDaemon>, registry: Registry) {
    let mut rx = match daemon.browse(SERVICE_TYPE) {
        Ok(rx) => rx,
        Err(e) => {
            log_bus(&registry.bus, "error", format!("mDNS browse failed: {e}"));
            return;
        }
    };
    log_bus(
        &registry.bus,
        "info",
        format!("browsing {SERVICE_TYPE} on the local network…"),
    );

    let mut refresh = interval(Duration::from_secs(BROWSE_CYCLE_SECS));
    refresh.set_missed_tick_behavior(MissedTickBehavior::Delay);
    refresh.tick().await; // first tick fires immediately; skip it
    let mut reaper = interval(Duration::from_secs(3));
    reaper.set_missed_tick_behavior(MissedTickBehavior::Delay);
    reaper.tick().await;

    loop {
        tokio::select! {
            ev = rx.recv_async() => match ev {
                Ok(ServiceEvent::ServiceResolved(info)) => {
                    if let Some((id, dev)) = to_device(&info, &registry) {
                        upsert(&registry, &id, dev);
                    }
                }
                Ok(ServiceEvent::ServiceRemoved(_ty, fullname)) => {
                    let self_id = registry.self_id.read().unwrap().clone();
                    if fullname != self_id {
                        let removed = registry.inner.lock().unwrap().remove(&fullname);
                        if let Some(d) = removed {
                            log_bus(
                                &registry.bus,
                                "warn",
                                format!("device left: {} ({})", d.name, d.platform),
                            );
                        }
                        registry.broadcast_devices();
                    }
                }
                Ok(_) => {}
                Err(_) => {
                    // Channel closes on stop_browse; the refresh tick re-arms it.
                }
            },
            _ = refresh.tick() => {
                // Restart the browse to reset the query back-off.
                let _ = daemon.stop_browse(SERVICE_TYPE);
                if let Ok(new_rx) = daemon.browse(SERVICE_TYPE) {
                    rx = new_rx;
                }
            }
            _ = reaper.tick() => {
                let now = now_ms();
                let mut map = registry.inner.lock().unwrap();
                let stale: Vec<String> = map
                    .iter()
                    .filter(|(_, d)| now - d.last_seen > DEVICE_TTL_MS)
                    .map(|(k, d)| (k.clone(), d.name.clone()))
                    .map(|(k, _)| k)
                    .collect();
                for id in stale {
                    if let Some(d) = map.remove(&id) {
                        log_bus(
                            &registry.bus,
                            "warn",
                            format!("device timed out (no mDNS refresh): {}", d.name),
                        );
                    }
                }
                drop(map);
                registry.broadcast_devices();
            }
        }
    }
}

fn to_device(info: &mdns_sd::ResolvedService, registry: &Registry) -> Option<(String, Device)> {
    let self_id = registry.self_id.read().unwrap().clone();
    if info.fullname == self_id {
        return None; // ourselves
    }
    // Prefer an IPv4 address; fall back to anything resolved.
    let ip = info
        .addresses
        .iter()
        .find(|a| a.is_ipv4() && !a.to_ip_addr().is_loopback())
        .or_else(|| info.addresses.iter().find(|a| a.is_ipv4()))
        .map(|a| a.to_ip_addr().to_string())?;

    let platform = info
        .txt_properties
        .get_property_val_str("platform")
        .unwrap_or("unknown")
        .to_string();

    let suffix = format!(".{}", info.ty_domain);
    let name = info
        .fullname
        .strip_suffix(&suffix)
        .unwrap_or(&info.fullname)
        .replace("\\.", ".");

    Some((
        info.fullname.clone(),
        Device {
            id: info.fullname.clone(),
            name,
            platform,
            ip,
            port: info.port,
            hostname: info.host.clone(),
            first_seen: now_ms(),
            last_seen: now_ms(),
        },
    ))
}

fn upsert(registry: &Registry, id: &str, dev: Device) {
    let mut map = registry.inner.lock().unwrap();
    if let Some(existing) = map.get_mut(id) {
        let first_seen = existing.first_seen;
        let name = existing.name.clone();
        let is_new_addr = existing.ip != dev.ip || existing.port != dev.port;
        let new_addr = format!("{}:{}", dev.ip, dev.port);
        let mut merged = dev;
        merged.first_seen = first_seen;
        *existing = merged;
        drop(map);
        if is_new_addr {
            log_bus(
                &registry.bus,
                "info",
                format!("device address changed: {name} → {new_addr}"),
            );
        }
    } else {
        map.insert(id.to_string(), dev.clone());
        drop(map);
        log_bus(
            &registry.bus,
            "info",
            format!(
                "discovered {} [{}] @ {}:{}",
                dev.name, dev.platform, dev.ip, dev.port
            ),
        );
    }
    registry.broadcast_devices();
}
