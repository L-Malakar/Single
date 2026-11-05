//! mDNS auto-discovery module.
//!
//! One [`ServiceDaemon`] per process handles both directions:
//! - **broadcast**: registers this host as `"<name>._p2pbridge._tcp.local."`
//!   with a TXT record carrying the platform tag (android / windows / …)
//! - **browse**: watches the same service type on the local Wi-Fi and emits
//!   [`ServiceEvent`]s consumed by [`crate::registry`].

use crate::config::SERVICE_TYPE;
use crate::protocol::Identity;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub struct Mdns {
    pub daemon: Arc<ServiceDaemon>,
    /// Stable DNS hostname for our A records (survives display-name renames).
    pub hostname: String,
    registered: Mutex<Option<String>>,
}

impl Mdns {
    pub fn new(loopback: bool) -> Result<Self, mdns_sd::Error> {
        let daemon = ServiceDaemon::new()?;
        if loopback {
            // Containers / offline dev: also join the loopback interface so
            // processes on the same host can discover each other.
            daemon.enable_interface("lo")?;
        }
        let hostname = format!(
            "{}-{}.local.",
            sanitize_host(&default_hostname()),
            &crate::util::nonce()[..4]
        );
        Ok(Self {
            daemon: Arc::new(daemon),
            hostname,
            registered: Mutex::new(None),
        })
    }

    /// Register our service. Returns the mDNS fullname (our id on the LAN).
    pub fn announce(&self, id: &Identity) -> Result<String, mdns_sd::Error> {
        let txt: HashMap<String, String> = HashMap::from([
            ("platform".to_string(), id.platform.clone()),
            (
                "proto".to_string(),
                format!("{}/{}", crate::config::MAGIC, crate::config::PROTO_VER),
            ),
        ]);
        let info = ServiceInfo::new(
            SERVICE_TYPE,
            &id.name,          // instance name (display name)
            &self.hostname,    // host for A records
            id.ip.clone(),     // our LAN address
            id.p2p_port,       // the P2P TCP port we advertise
            txt,
        )?;
        let fullname = info.get_fullname().to_string();
        self.daemon.register(info)?;
        *self.registered.lock().unwrap() = Some(fullname.clone());
        Ok(fullname)
    }

    /// Send mDNS goodbye packets for our service.
    pub fn unannounce(&self) {
        let current = self.registered.lock().unwrap().take();
        if let Some(fullname) = current {
            if let Ok(rx) = self.daemon.unregister(&fullname) {
                // UnregisterStatus arrives asynchronously; log it, don't block.
                std::thread::spawn(move || {
                    if let Ok(status) = rx.recv() {
                        println!("[mdns] unregistered {fullname}: {status:?}");
                    }
                });
            }
        }
    }

    /// Re-register with a new display name (called on dashboard rename).
    pub fn reannounce(&self, id: &Identity) -> Result<String, mdns_sd::Error> {
        self.unannounce();
        self.announce(id)
    }

    pub fn registered_fullname(&self) -> Option<String> {
        self.registered.lock().unwrap().clone()
    }

    pub fn shutdown(&self) {
        self.unannounce();
        let _ = self.daemon.shutdown();
    }
}

fn default_hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "p2p-host".to_string())
}

fn sanitize_host(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let trimmed = trimmed_leading(&cleaned, '-');
    if trimmed.is_empty() {
        "host".to_string()
    } else {
        trimmed.chars().take(20).collect()
    }
}

fn trimmed_leading(s: &str, c: char) -> String {
    s.trim_matches(c).to_string()
}
