//! device-simulator — fake Android/Windows peers for local demos.
//!
//! Spawns five virtual devices that each:
//! - register a real mDNS service `_p2pbridge._tcp.local.` (staggered, so you
//!   can watch them pop into the dashboard one by one),
//! - accept the full P2P TCP handshake (HELLO / HELLO_ACK / ACK),
//! - answer chat with a canned "device personality", reply to latency pings,
//!   and receive file transfers (SHA-256 verified, saved under `received-sim/`).
//!
//! The last device ("OnePlus 9 (flaky)") drops off Wi-Fi every ~20 s and
//! rejoins ~8 s later to demonstrate mDNS goodbye / rediscovery / TTL reaping.
//!
//! Usage: device-simulator [--base-port 5100] [--no-flap] [--duration secs]

use base64::Engine;
use p2p_bridge::config::{MAGIC, PROTO_VER, SERVICE_TYPE};
use p2p_bridge::files;
use p2p_bridge::protocol::Wire;
use p2p_bridge::util::{human_bytes, nonce, now_ms};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

#[derive(Clone)]
struct SimDevice {
    name: &'static str,
    platform: &'static str,
    hostname: &'static str,
    port: u16,
    flaky: bool,
    replies: &'static [&'static str],
}

const ROSTER: &[SimDevice] = &[
    SimDevice {
        name: "Pixel 8",
        platform: "android",
        hostname: "pixel-8-sim.local.",
        port: 5100,
        flaky: false,
        replies: &[
            "hey from the Pixel 👋",
            "got it — Wi-Fi 6E is fast today",
            "📱 Pixel 8 acknowledges",
        ],
    },
    SimDevice {
        name: "Galaxy Tab S9",
        platform: "android",
        hostname: "galaxy-tab-s9-sim.local.",
        port: 5101,
        flaky: false,
        replies: &[
            "Galaxy Tab here, screen is huge 🖥",
            "received loud and clear",
            "📱 Tab S9: message stored locally",
        ],
    },
    SimDevice {
        name: "DESKTOP-WIN11-4F2A",
        platform: "windows",
        hostname: "desktop-win11-sim.local.",
        port: 5102,
        flaky: false,
        replies: &[
            "DESKTOP-WIN11 online. Windows Defender says hi 🪟",
            "processing… done.",
            "🪟 logged to C:\\p2p-bridge\\chat.txt",
        ],
    },
    SimDevice {
        name: "Surface Pro 11",
        platform: "windows",
        hostname: "surface-pro-11-sim.local.",
        port: 5103,
        flaky: false,
        replies: &[
            "Surface Pro 11 — tablet mode engaged ✏️",
            "copy that",
            "🪟 syncing via OneDrive… just kidding, P2P only 😄",
        ],
    },
    SimDevice {
        name: "OnePlus 9 (flaky)",
        platform: "android",
        hostname: "oneplus-9-sim.local.",
        port: 5104,
        flaky: true,
        replies: &[
            "one bar of Wi-Fi here… 📶",
            "still holding on!",
            "📱 OnePlus 9: might drop any second",
        ],
    },
];

fn stamp() -> String {
    let secs = now_ms() / 1000;
    format!("{:02}:{:02}:{:02}", (secs / 3600) % 24, (secs / 60) % 60, secs % 60)
}

fn log(msg: &str) {
    println!("[{}][sim] {msg}", stamp());
}

fn service_info(dev: &SimDevice, ip: IpAddr) -> mdns_sd::Result<ServiceInfo> {
    let txt: HashMap<String, String> = HashMap::from([
        ("platform".to_string(), dev.platform.to_string()),
        ("proto".to_string(), format!("{MAGIC}/{PROTO_VER}")),
    ]);
    ServiceInfo::new(SERVICE_TYPE, dev.name, dev.hostname, ip.to_string(), dev.port, txt)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut base_port = 5100u16;
    let mut flap = true;
    let mut duration = 0u64;
    let mut ip_override: Option<IpAddr> = None;
    let mut received_dir = PathBuf::from("received-sim");

    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--base-port" => {
                i += 1;
                if let Some(v) = args.get(i).and_then(|s| s.parse().ok()) {
                    base_port = v;
                }
            }
            "--duration" => {
                i += 1;
                if let Some(v) = args.get(i).and_then(|s| s.parse().ok()) {
                    duration = v;
                }
            }
            "--ip" => {
                i += 1;
                if let Some(v) = args.get(i).and_then(|s| s.parse().ok()) {
                    ip_override = Some(v);
                }
            }
            "--received-dir" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    received_dir = PathBuf::from(v.clone());
                }
            }
            "--no-flap" => flap = false,
            "--help" | "-h" => {
                println!("device-simulator [--base-port N] [--no-flap] [--duration secs] [--ip A.B.C.D] [--received-dir DIR]");
                return Ok(());
            }
            other => {
                eprintln!("unknown flag: {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }

    let ip = ip_override.unwrap_or_else(p2p_bridge::config::local_ip);
    let daemon = std::sync::Arc::new(ServiceDaemon::new()?);

    println!(
        "\n📡 device-simulator — faking {} devices on {ip}\n\
         ─────────────────────────────────────────────────────",
        ROSTER.len()
    );
    for (idx, dev) in ROSTER.iter().enumerate() {
        println!(
            "  {icon} {:<22} [{:<7}] → {}:{}", // icon platform port
            dev.name,
            dev.platform,
            ip,
            base_port + idx as u16
            , icon = platform_icon(dev.platform)
        );
    }
    println!("\nThey will appear in the p2p-bridge dashboard as {SERVICE_TYPE}\n");

    // Optional global duration → clean exit.
    if duration > 0 {
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(duration)).await;
            log("duration reached — exiting");
            std::process::exit(0);
        });
    }

    // Staggered registration + per-device TCP responder.
    for (idx, dev) in ROSTER.iter().enumerate() {
        let port = base_port + idx as u16;
        let daemon = daemon.clone();
        let flap = flap && dev.flaky;
        let dev = dev.clone();
        let dev_ip = ip;
        let received_dir = received_dir.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(idx as u64 * 1500)).await;
            let mut info = match service_info(&dev, dev_ip) {
                Ok(i) => i,
                Err(e) => {
                    log(&format!("service build failed for {}: {e}", dev.name));
                    return;
                }
            };
            let fullname = info.get_fullname().to_string();
            if let Err(e) = daemon.register(info) {
                log(&format!("register failed for {}: {e}", dev.name));
                return;
            }
            log(&format!("online: {fullname}"));

            // Flaky Wi-Fi: periodically unregister (goodbye) and rejoin.
            if flap {
                loop {
                    tokio::time::sleep(Duration::from_secs(20)).await;
                    let _ = daemon.unregister(&fullname);
                    log(&format!("Wi-Fi dropped: {} ✗", dev.name));
                    tokio::time::sleep(Duration::from_secs(8)).await;
                    info = match service_info(&dev, dev_ip) {
                        Ok(i) => i,
                        Err(_) => continue,
                    };
                    if daemon.register(info).is_ok() {
                        log(&format!("rejoined: {} ✓", dev.name));
                    }
                }
            }
            // Keep the TCP responder for this device alive.
            serve_device(dev, port, received_dir).await;
        });
    }

    // Block forever (or until --duration exits the process).
    futures_util::future::pending::<()>().await;
    Ok(())
}

fn platform_icon(p: &str) -> &'static str {
    match p {
        "android" => "🤖",
        "windows" => "🪟",
        "linux" => "🐧",
        "macos" => "🍎",
        _ => "❓",
    }
}

async fn serve_device(dev: SimDevice, port: u16, received_dir: PathBuf) {
    let listener = match TcpListener::bind(("0.0.0.0", port)).await {
        Ok(l) => l,
        Err(e) => {
            log(&format!("{} cannot bind :{port}: {e}", dev.name));
            return;
        }
    };
    loop {
        if let Ok((sock, addr)) = listener.accept().await {
            let dev = dev.clone();
            let dir = received_dir.clone();
            tokio::spawn(async move {
                if let Err(e) = respond(sock, addr, &dev, &dir).await {
                    if e.kind() != ErrorKind::UnexpectedEof {
                        log(&format!("{} session ended: {e}", dev.name));
                    }
                }
            });
        }
    }
}

/// Full P2P responder: handshake → chat / ping / file transfer.
async fn respond(
    sock: TcpStream,
    addr: std::net::SocketAddr,
    dev: &SimDevice,
    received_dir: &PathBuf,
) -> std::io::Result<()> {
    let (rd, mut wr) = sock.into_split();
    let mut lines = BufReader::new(rd).lines();

    // HELLO
    let first = match lines.next_line().await? {
        Some(l) => l,
        None => return Ok(()),
    };
    let hello = if first.starts_with("{\"type\":\"hello\"") {
        first
    } else {
        let _ = wr
            .write_all(b"{\"type\":\"err\",\"msg\":\"expected hello\"}\n")
            .await;
        return Ok(());
    };
    let hello: Wire = match serde_json::from_str(&hello) {
        Ok(Wire::Hello { magic, ver, nonce, name, .. }) if magic == MAGIC && ver == PROTO_VER => {
            Wire::Hello {
                magic,
                ver,
                nonce,
                name,
                id: String::new(),
                platform: String::new(),
                password: String::new(),
            }
        }
        _ => {
            let _ = wr
                .write_all(b"{\"type\":\"err\",\"msg\":\"bad hello\"}\n")
                .await;
            return Ok(());
        }
    };
    let Wire::Hello { nonce: peer_nonce, name: peer_name, .. } = hello else { return Ok(()) };

    // HELLO_ACK (SYN-ACK)
    let my_nonce = nonce();
    let ack_json = serde_json::json!({
        "type": "hello-ack",
        "ok": true,
        "id": format!("{}.{}", dev.name, SERVICE_TYPE),
        "name": dev.name,
        "platform": dev.platform,
        "echo_nonce": peer_nonce,
        "nonce": my_nonce,
    });
    wr.write_all(format!("{ack_json}\n").as_bytes()).await?;

    // ACK
    let ack = match tokio::time::timeout(Duration::from_secs(5), lines.next_line()).await {
        Ok(Ok(Some(l))) => l,
        _ => return Ok(()),
    };
    let ack: Wire = match serde_json::from_str(&ack) {
        Ok(w) => w,
        Err(_) => return Ok(()),
    };
    match ack {
        Wire::Ack { echo_nonce } if echo_nonce == my_nonce => {}
        _ => {
            log(&format!("{}: ACK nonce mismatch from {addr}", dev.name));
            return Ok(());
        }
    }
    log(&format!("🤝 {} ⇄ {peer_name} established ({addr})", dev.name));

    // Session loop
    let mut inbound: Option<files::InboundTransfer> = None;
    let mut reply_idx = 0usize;
    loop {
        let line = match lines.next_line().await {
            Ok(Some(l)) => l,
            Ok(None) => return Ok(()),
            Err(e) => return Err(e),
        };
        let wire: Wire = match serde_json::from_str(&line) {
            Ok(w) => w,
            Err(_) => continue,
        };
        match wire {
            Wire::Msg { text, .. } => {
                log(&format!("{} ⇠ {peer_name}: {text}", dev.name));
                tokio::time::sleep(Duration::from_millis(300 + (now_ms() % 900) as u64)).await;
                let reply = dev.replies[reply_idx % dev.replies.len()];
                reply_idx += 1;
                let out = serde_json::json!({
                    "type": "msg",
                    "text": format!("{reply} — “{text}”"),
                    "ts": now_ms(),
                });
                wr.write_all(format!("{out}\n").as_bytes()).await?;
            }
            Wire::Ping { t } => {
                let pong = serde_json::json!({ "type": "pong", "t": t });
                wr.write_all(format!("{pong}\n").as_bytes()).await?;
            }
            Wire::FileStart {
                tid,
                name,
                size,
                sha256,
            } => {
                log(&format!("{} ⇠ incoming file “{name}” ({})", dev.name, human_bytes(size)));
                inbound = Some(files::InboundTransfer {
                    tid,
                    name,
                    size,
                    sha256,
                    buf: Vec::new(),
                });
            }
            Wire::FileChunk { tid, seq, data } => {
                if let Some(tr) = inbound.as_mut() {
                    if tr.tid == tid {
                        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&data) {
                            tr.buf.extend_from_slice(&bytes);
                        }
                        let prog = serde_json::json!({
                            "type": "file-progress", "tid": tid, "seq": seq, "bytes": tr.received(),
                        });
                        wr.write_all(format!("{prog}\n").as_bytes()).await?;
                    }
                }
            }
            Wire::FileEnd { tid } => {
                if let Some(tr) = inbound.take() {
                    if tr.tid == tid {
                        let sha_ok = tr.sha256.is_empty()
                            || files::sha256_hex(&tr.buf) == tr.sha256;
                        let (ok, detail) = if !sha_ok {
                            (false, "sha256 mismatch".to_string())
                        } else {
                            match files::save(received_dir, &tr.name, &tr.buf) {
                                Ok(p) => (true, p.display().to_string()),
                                Err(e) => (false, e.to_string()),
                            }
                        };
                        log(&format!(
                            "{} {} file “{}” {} ({})",
                            dev.name,
                            if ok { "✓ stored" } else { "✗ rejected" },
                            tr.name,
                            human_bytes(tr.buf.len() as u64),
                            detail
                        ));
                        let done = serde_json::json!({
                            "type": "file-done", "tid": tid, "ok": ok, "detail": detail,
                        });
                        wr.write_all(format!("{done}\n").as_bytes()).await?;
                    }
                }
            }
            Wire::Bye { reason } => {
                log(&format!("{} ⇠ {peer_name} said bye ({reason})", dev.name));
                return Ok(());
            }
            _ => {}
        }
    }
}
