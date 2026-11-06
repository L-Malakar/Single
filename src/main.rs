//! p2p-bridge-server: mDNS-discovered, WebSocket-controlled LAN P2P server.
//!
//! Boot order:
//! 1. detect host identity (name / platform / LAN IP)
//! 2. start the mDNS daemon — broadcast `_p2pbridge._tcp.local.` + browse peers
//! 3. bind the P2P TCP listener (handshake endpoint for peers)
//! 4. serve the embedded dashboard + WebSocket control channel

use p2p_bridge::access::AccessControl;
use p2p_bridge::browser;
use p2p_bridge::config::{self, Config, APP_NAME, APP_VERSION, SERVICE_TYPE};
use p2p_bridge::mdns::Mdns;
use p2p_bridge::p2p::P2pManager;
use p2p_bridge::persist::{Persist, STATE_FILE};
use p2p_bridge::protocol::Identity;
use p2p_bridge::registry::{self, Registry};
use p2p_bridge::server::{self, AppState};
use p2p_bridge::util::{log_bus, now_ms};
use std::sync::{Arc, RwLock};
use tokio::sync::broadcast;

type DynError = Box<dyn std::error::Error>;

#[tokio::main]
async fn main() -> Result<(), DynError> {
    #[cfg(windows)]
    setup_console();

    let cfg = Config::parse(std::env::args().collect());
    let ip = config::local_ip();
    let platform = config::detect_platform();

    // ---- persistence ------------------------------------------------------
    // single-state.json lives next to the received-files folder (so it sits
    // in the app folder on desktops and in the app's storage on Android).
    let state_path = cfg.received_dir.with_file_name(STATE_FILE);
    let persist = Persist::load(state_path.clone());
    let saved = persist.snapshot();

    let (bus, _guard) = broadcast::channel(512);
    persist.spawn(bus.clone());

    // A dashboard rename (persisted) wins over the --name default; the flag
    // only seeds the name on first boot.
    let effective_name = saved.name.clone().unwrap_or_else(|| cfg.name.clone());
    if saved.name.is_some() && saved.name.as_deref() != Some(cfg.name.as_str()) {
        println!(
            "  restored: device name “{}” (flag --name is only the first-boot default)",
            saved.name.clone().unwrap_or_default()
        );
    } else {
        // Seed the store so a restart without the flag keeps the name.
        persist.set_name(&effective_name);
    }

    let mdns = Arc::new(Mdns::new(cfg.mdns_loopback)?);

    let mut identity = Identity {
        id: String::new(),
        name: effective_name,
        platform: platform.clone(),
        ip: ip.to_string(),
        p2p_port: cfg.p2p_port,
    };
    let banner_name = identity.name.clone();
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
        "\n  {APP_NAME} v{APP_VERSION} - mDNS LAN control server\n\
         ─────────────────────────────────────────────────────\n\
         identity  : {name} [{platform}] @ {ip}\n\
         dashboard : http://{ip}:{http_port}\n\
         p2p tcp   : {ip}:{p2p_port}\n\
         access    : {access} (Public / Private / Protected - Home › Protocol)\n\
         mDNS      : {SERVICE_TYPE} ({announce})\n\
         state     : {state_file}\n\n\
         The app window opens by itself - keep this console open.\n\
         Press Ctrl-C to shut down (sends mDNS goodbye).\n",
        name = banner_name,
        http_port = cfg.http_port,
        p2p_port = cfg.p2p_port,
        access = saved.access_mode.clone().unwrap_or_else(|| "private".to_string()),
        announce = if cfg.announce { "on" } else { "off" },
        state_file = state_path.display(),
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

    // P2P TCP listener (inbound policy lives in AccessControl).
    let access = AccessControl::new(
        bus.clone(),
        persist.clone(),
        saved.access_mode.clone(),
        saved.access_password.clone(),
    );
    let p2p = P2pManager::new(
        bus.clone(),
        self_identity.clone(),
        cfg.received_dir.clone(),
        access.clone(),
        persist.clone(),
    );
    if let Err(e) = p2p.spawn_listener(cfg.p2p_port) {
        log_bus(
            &bus,
            "error",
            format!("cannot bind P2P port {}: {e}", cfg.p2p_port),
        );
        return Err(e.into());
    }
    // Bring yesterday's conversations back before the first dashboard
    // connects, so the initial sync includes them.
    p2p.restore_sessions(&saved);

    // Auto-open the dashboard as an app window once the listener is up
    // (unless --no-open). The helper thread polls the port, so it is safe
    // to start it before the server actually binds.
    if cfg.open_dashboard {
        browser::open_when_ready(cfg.http_port);
    }

    let state = Arc::new(AppState {
        cfg,
        bus,
        registry,
        mdns,
        self_identity,
        p2p,
        access,
        persist,
        started: std::time::Instant::now(),
    });

    server::serve(state.clone()).await?;

    // Graceful shutdown: flush persisted state, then mDNS goodbye so peers
    // drop us immediately.
    state.persist.flush_now();
    state.mdns.shutdown();
    println!(
        "bye - state saved to {} · mDNS goodbye sent ({})",
        state.persist.path().display(),
        now_ms()
    );
    Ok(())
}

/// Switch the Windows console to UTF-8 so the banner, mDNS names and chat
/// text render correctly on every code page / legacy console. Uses a child
/// `cmd` so no extra dependency is needed.
#[cfg(windows)]
fn setup_console() {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("cmd.exe")
        .raw_arg("/C chcp 65001 >NUL")
        .status();
}
