//! p2p-bridge-server: mDNS-discovered, WebSocket-controlled LAN P2P server.
//!
//! Boot order:
//! 1. detect host identity (name / platform / LAN IP)
//! 2. start the mDNS daemon — broadcast `_p2pbridge._tcp.local.` + browse peers
//! 3. bind the P2P TCP listener (handshake endpoint for peers)
//! 4. serve the embedded dashboard + WebSocket control channel

use p2p_bridge::config::{self, Config, SERVICE_TYPE};
use p2p_bridge::mdns::Mdns;
use p2p_bridge::p2p::P2pManager;
use p2p_bridge::protocol::Identity;
use p2p_bridge::registry::{self, Registry};
use p2p_bridge::server::{self, AppState};
use p2p_bridge::util::{log_bus, now_ms};
use std::sync::{Arc, RwLock};
use tokio::sync::broadcast;

type DynError = Box<dyn std::error::Error>;

#[tokio::main]
async fn main() -> Result<(), DynError> {
    let cfg = Config::parse(std::env::args().collect());
    let ip = config::local_ip();
    let platform = config::detect_platform();

    let (bus, _guard) = broadcast::channel(512);
    let mdns = Arc::new(Mdns::new(cfg.mdns_loopback)?);

    let mut identity = Identity {
        id: String::new(),
        name: cfg.name.clone(),
        platform: platform.clone(),
        ip: ip.to_string(),
        p2p_port: cfg.p2p_port,
    };
    let self_id = Arc::new(RwLock::new(String::new()));

    if cfg.announce {
        match mdns.announce(&identity) {
            Ok(fullname) => {
                identity.id = fullname.clone();
                *self_id.write().unwrap() = fullname.clone();
                log_bus(
                    &bus,
                    "ok",
                    format!("broadcasting “{}” as {fullname}", identity.name),
                );
            }
            Err(e) => log_bus(&bus, "error", format!("mDNS announce failed: {e}")),
        }
    } else {
        log_bus(&bus, "warn", "mDNS announce disabled (--no-announce)");
    }
    let self_identity = Arc::new(RwLock::new(identity));

    println!(
        "\n⇄ p2p-bridge — mDNS LAN control server\n\
         ─────────────────────────────────────────────────────\n\
         identity  : {name} [{platform}] @ {ip}\n\
         dashboard : http://{ip}:{http_port}\n\
         p2p tcp   : {ip}:{p2p_port}\n\
         mDNS      : {SERVICE_TYPE} ({announce})\n\n\
         Press Ctrl-C to shut down (sends mDNS goodbye).\n",
        name = cfg.name,
        http_port = cfg.http_port,
        p2p_port = cfg.p2p_port,
        announce = if cfg.announce { "on" } else { "off" },
    );

    // Discovery loop: mDNS browse → live device registry → WS broadcast.
    let registry = Registry::new(bus.clone(), self_id.clone());
    {
        let daemon = mdns.daemon.clone();
        let reg = registry.clone();
        tokio::spawn(async move {
            registry::run_discovery(daemon, reg).await;
        });
    }

    // P2P TCP listener.
    let p2p = P2pManager::new(bus.clone(), self_identity.clone(), cfg.received_dir.clone());
    if let Err(e) = p2p.spawn_listener(cfg.p2p_port) {
        log_bus(
            &bus,
            "error",
            format!("cannot bind P2P port {}: {e}", cfg.p2p_port),
        );
        return Err(e.into());
    }

    let state = Arc::new(AppState {
        cfg,
        bus,
        registry,
        mdns,
        self_identity,
        p2p,
        started: std::time::Instant::now(),
    });

    server::serve(state.clone()).await?;

    // Graceful shutdown: mDNS goodbye so peers drop us immediately.
    state.mdns.shutdown();
    println!("bye 👋 ({})", now_ms());
    Ok(())
}
