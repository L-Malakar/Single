//! Service constants, runtime configuration and host detection.

use std::net::IpAddr;
use std::path::PathBuf;

/// mDNS service type we broadcast and browse on the local Wi-Fi.
pub const SERVICE_TYPE: &str = "_p2pbridge._tcp.local.";

/// Product name shown in the console banner, help text and dashboard.
pub const APP_NAME: &str = "Single";
/// Release version — kept in sync with `version` in `Cargo.toml` automatically.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Magic string + protocol version exchanged during the P2P TCP handshake.
pub const MAGIC: &str = "P2PBRIDGE";
pub const PROTO_VER: u32 = 1;

/// How long a discovered device stays listed after its last mDNS refresh.
pub const DEVICE_TTL_MS: i64 = 45_000;

/// The mDNS browse cycle: mdns-sd retransmits with exponential back-off, so we
/// restart the browse periodically to keep resolutions fresh.
pub const BROWSE_CYCLE_SECS: u64 = 15;

#[derive(Debug, Clone)]
pub struct Config {
    /// Display name broadcast over mDNS (also editable from the dashboard).
    pub name: String,
    /// HTTP + WebSocket port serving the dashboard.
    pub http_port: u16,
    /// TCP port peers connect to for the P2P handshake.
    pub p2p_port: u16,
    /// Loopback WebDAV port (per-peer drive mapping on Windows).
    pub dav_port: u16,
    /// Where inbound file transfers are stored.
    pub received_dir: PathBuf,
    /// Folders shared with connected devices over the encrypted bridge.
    pub shares: Vec<crate::protocol::ShareRootDto>,
    /// `--no-share`: start with an empty share list.
    pub no_share: bool,
    /// Register our own mDNS service (disable with `--no-announce`).
    pub announce: bool,
    /// Also join the loopback interface for mDNS (containers / offline dev).
    pub mdns_loopback: bool,
    /// Serve the UI from disk instead of the embedded copy (live editing).
    pub static_dir: Option<PathBuf>,
    /// Open the dashboard in an app-style browser window once it is up.
    /// Disabled with `--no-open`.
    pub open_dashboard: bool,
}

impl Config {
    pub fn parse(args: Vec<String>) -> Config {
        let mut cfg = Config {
            name: default_device_name(),
            http_port: 3000,
            p2p_port: 5050,
            dav_port: 5051,
            received_dir: PathBuf::from("received"),
            shares: Vec::new(),
            no_share: false,
            announce: true,
            mdns_loopback: false,
            static_dir: None,
            open_dashboard: true,
        };
        let mut i = 1;
        while i < args.len() {
            match args[i].as_str() {
                "--name" | "-n" => cfg.take_str(&args, &mut i, |c, v| c.name = v),
                "--http-port" => cfg.take_val(&args, &mut i, |c, v| c.http_port = v),
                "--p2p-port" => cfg.take_val(&args, &mut i, |c, v| c.p2p_port = v),
                "--dav-port" => cfg.take_val(&args, &mut i, |c, v| c.dav_port = v),
                "--received-dir" => {
                    cfg.take_str(&args, &mut i, |c, v| c.received_dir = PathBuf::from(v))
                }
                "--share" => cfg.take_str(&args, &mut i, |c, v| {
                    let title = PathBuf::from(&v)
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "Share".into());
                    c.shares.push(crate::protocol::ShareRootDto { title, path: v });
                }),
                "--no-share" => {
                    cfg.no_share = true;
                    cfg.shares.clear();
                }
                "--static-dir" => {
                    cfg.take_str(&args, &mut i, |c, v| c.static_dir = Some(PathBuf::from(v)))
                }
                "--no-announce" => cfg.announce = false,
                "--mdns-loopback" => cfg.mdns_loopback = true,
                "--no-open" => cfg.open_dashboard = false,
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                other => {
                    eprintln!("unknown flag: {other} (see --help)");
                    std::process::exit(2);
                }
            }
            i += 1;
        }
        cfg
    }

    fn take_str(&mut self, args: &[String], i: &mut usize, set: impl FnOnce(&mut Config, String)) {
        if *i + 1 < args.len() {
            *i += 1;
            set(self, args[*i].clone());
        }
    }

    fn take_val(&mut self, args: &[String], i: &mut usize, set: impl FnOnce(&mut Config, u16)) {
        if *i + 1 < args.len() {
            *i += 1;
            if let Ok(v) = args[*i].parse() {
                set(self, v);
            }
        }
    }
}

fn print_help() {
    println!(
        "{APP_NAME} v{APP_VERSION} - mDNS LAN control server\n\n\
         USAGE: {APP_NAME} [FLAGS]\n\n\
         FLAGS:\n\
           --name <name>        display name broadcast over mDNS (default: hostname)\n\
           --http-port <port>   dashboard + WebSocket port   (default: 3000)\n\
           --p2p-port <port>    peer-to-peer TCP port        (default: 5050)\n\
           --dav-port <port>    loopback WebDAV port for drive mapping (default: 5051)\n\
           --share <dir>        share an extra folder over the encrypted bridge\n\
                                (repeatable; default: your home folder / phone storage)\n\
           --no-share           share nothing until configured on the Files page\n\
           --received-dir <dir> where inbound files are saved (default: ./received)\n\
           --static-dir <dir>   serve UI from disk instead of embedded assets\n\
           --no-open            do not auto-open the dashboard app window\n\
           --no-announce        do not broadcast our own mDNS service\n\
           --mdns-loopback      join multicast on loopback too (containers/offline dev)\n\
         "
    );
}

/// Best-effort platform string, mirrored into the mDNS TXT record.
pub fn detect_platform() -> String {
    #[cfg(target_os = "windows")]
    {
        "windows".to_string()
    }
    #[cfg(target_os = "macos")]
    {
        "macos".to_string()
    }
    #[cfg(target_os = "android")]
    {
        "android".to_string()
    }
    #[cfg(target_os = "linux")]
    {
        // Rust binaries cross-compiled for Android report `linux`; the
        // Android runtime always exports these env vars.
        if std::env::var_os("ANDROID_ROOT").is_some() || std::env::var_os("ANDROID_DATA").is_some()
        {
            "android".to_string()
        } else {
            "linux".to_string()
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "android", target_os = "linux")))]
    {
        "unknown".to_string()
    }
}

fn default_device_name() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "p2p-host".to_string())
}

/// Primary LAN IPv4 of this host. Uses a UDP `connect` (no packets are sent)
/// to ask the kernel which source address the default route would pick.
pub fn local_ip() -> IpAddr {
    for probe in ["8.8.8.8:80", "192.168.1.1:80", "1.1.1.1:53"] {
        if let Ok(sock) = std::net::UdpSocket::bind("0.0.0.0:0") {
            if sock.connect(probe).is_ok() {
                if let Ok(addr) = sock.local_addr() {
                    return addr.ip();
                }
            }
        }
    }
    IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
}
