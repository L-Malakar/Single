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
use p2p_bridge::crypto::{self, SessionKeys};
use p2p_bridge::files;
use p2p_bridge::protocol::{FsEntryDto, Wire};
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
    let share_root = seed_demo_share(&dev, &received_dir);
    log(&format!("{} demo share: {}", dev.name, share_root.display()));
    loop {
        if let Ok((sock, addr)) = listener.accept().await {
            let dev = dev.clone();
            let dir = received_dir.clone();
            let share = share_root.clone();
            tokio::spawn(async move {
                if let Err(e) = respond(sock, addr, &dev, &dir, &share).await {
                    if e.kind() != ErrorKind::UnexpectedEof {
                        log(&format!("{} session ended: {e}", dev.name));
                    }
                }
            });
        }
    }
}

/// Full P2P responder: handshake (with E2EE) → chat / ping / file transfer
/// + a small demo file share so the encrypted bridge can be exercised
/// against fake devices too (they appear as drives / in the Files app just
/// like real ones).
async fn respond(
    sock: TcpStream,
    addr: std::net::SocketAddr,
    dev: &SimDevice,
    received_dir: &PathBuf,
    share_root: &PathBuf,
) -> std::io::Result<()> {
    let (rd, mut wr) = sock.into_split();
    let mut lines = BufReader::new(rd).lines();

    // HELLO
    let first = match lines.next_line().await? {
        Some(l) => l,
        None => return Ok(()),
    };
    let (peer_nonce, peer_name, peer_e2ee) = if first.starts_with("{\"type\":\"hello\"") {
        match serde_json::from_str::<Wire>(&first) {
            Ok(Wire::Hello { magic, ver, nonce, name, e2ee, .. })
                if magic == MAGIC && ver == PROTO_VER =>
            {
                (nonce, name, e2ee)
            }
            _ => {
                let _ = wr
                    .write_all(b"{\"type\":\"err\",\"msg\":\"bad hello\"}\n")
                    .await;
                return Ok(());
            }
        }
    } else {
        let _ = wr
            .write_all(b"{\"type\":\"err\",\"msg\":\"expected hello\"}\n")
            .await;
        return Ok(());
    };

    // HELLO_ACK (SYN-ACK) — with our ephemeral X25519 key for E2EE.
    let my_nonce = nonce();
    let e2ee_key = crypto::EphemeralKey::generate();
    let ack = Wire::HelloAck {
        ok: true,
        id: format!("{}.{}", dev.name, SERVICE_TYPE),
        name: dev.name.to_string(),
        platform: dev.platform.to_string(),
        echo_nonce: peer_nonce.clone(),
        nonce: my_nonce.clone(),
        reason: String::new(),
        e2ee: Some(e2ee_key.public_b64()),
    };
    write_line(&mut wr, &ack, None).await?;

    // ACK
    let ack_line = match tokio::time::timeout(Duration::from_secs(5), lines.next_line()).await {
        Ok(Ok(Some(l))) => l,
        _ => return Ok(()),
    };
    let ack: Wire = match serde_json::from_str(&ack_line) {
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

    // E2EE keys — identical schedule to the real server.
    let keys: Option<SessionKeys> = peer_e2ee
        .as_deref()
        .and_then(|pub_b64| {
            crypto::derive_keys(&e2ee_key, pub_b64, &peer_nonce, &my_nonce, false).ok()
        });
    log(&format!(
        "🤝 {} ⇄ {peer_name} established ({addr}){}",
        dev.name,
        if keys.is_some() { " · 🔒 E2EE" } else { "" }
    ));

    // Session loop
    let mut inbound: Option<files::InboundTransfer> = None;
    let mut reply_idx = 0usize;
    loop {
        let line = match lines.next_line().await {
            Ok(Some(l)) => l,
            Ok(None) => return Ok(()),
            Err(e) => return Err(e),
        };
        let wire = match parse_line(&line, keys.as_ref()) {
            Some(w) => w,
            None => continue,
        };
        match wire {
            Wire::Msg { text, .. } => {
                log(&format!("{} ⇠ {peer_name}: {text}", dev.name));
                tokio::time::sleep(Duration::from_millis(300 + (now_ms() % 900) as u64)).await;
                let reply = dev.replies[reply_idx % dev.replies.len()];
                reply_idx += 1;
                let out = Wire::Msg {
                    text: format!("{reply} — “{text}”"),
                    ts: now_ms(),
                };
                write_line(&mut wr, &out, keys.as_ref()).await?;
            }
            Wire::Ping { t } => {
                write_line(&mut wr, &Wire::Pong { t }, keys.as_ref()).await?;
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
                        let prog = Wire::FileProgress { tid, seq, bytes: tr.received() };
                        write_line(&mut wr, &prog, keys.as_ref()).await?;
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
                        let done = Wire::FileDone { tid, ok, detail };
                        write_line(&mut wr, &done, keys.as_ref()).await?;
                    }
                }
            }

            // ---- demo file bridge -------------------------------------
            Wire::FsListReq { rid, path } => {
                let res = sim_list(share_root, &path);
                let out = match res {
                    Ok(entries) => Wire::FsListRes { rid, ok: true, error: String::new(), entries },
                    Err(e) => Wire::FsListRes { rid, ok: false, error: e, entries: Vec::new() },
                };
                write_line(&mut wr, &out, keys.as_ref()).await?;
            }
            Wire::FsStatReq { rid, path } => {
                let out = match sim_stat(share_root, &path) {
                    Ok(entry) => Wire::FsStatRes { rid, ok: true, error: String::new(), entry: Some(entry) },
                    Err(e) => Wire::FsStatRes { rid, ok: false, error: e, entry: None },
                };
                write_line(&mut wr, &out, keys.as_ref()).await?;
            }
            Wire::FsReadReq { rid, path, offset, len } => {
                match sim_open(share_root, &path) {
                    Ok(mut file) => {
                        use std::io::{Read, Seek, SeekFrom};
                        let _ = file.seek(SeekFrom::Start(offset));
                        let mut remaining = if len == 0 { u64::MAX } else { len };
                        let mut seq = 0u64;
                        loop {
                            let want = remaining.min(256 * 1024) as usize;
                            let mut buf = vec![0u8; want];
                            match file.read(&mut buf) {
                                Ok(0) => break,
                                Ok(n) => {
                                    buf.truncate(n);
                                    remaining -= n as u64;
                                    let eof = remaining == 0 || n < want;
                                    let frame = Wire::FsData {
                                        rid: rid.clone(),
                                        seq,
                                        data: base64::engine::general_purpose::STANDARD.encode(&buf),
                                        eof,
                                    };
                                    write_line(&mut wr, &frame, keys.as_ref()).await?;
                                    if eof {
                                        break;
                                    }
                                    seq += 1;
                                }
                                Err(_) => break,
                            }
                        }
                    }
                    Err(e) => {
                        write_line(&mut wr, &Wire::FsErr { rid, error: e }, keys.as_ref()).await?;
                    }
                }
            }
            Wire::FsWriteStart { rid, path, size: _ } => {
                let out = match sim_write_target(share_root, &path) {
                    Ok(target) => {
                        std::fs::write(&target, b"").ok();
                        SIM_WRITES.with(|w| w.borrow_mut().insert(rid.clone(), target));
                        Wire::FsOk { rid, ok: true, error: String::new() }
                    }
                    Err(e) => Wire::FsOk { rid, ok: false, error: e },
                };
                write_line(&mut wr, &out, keys.as_ref()).await?;
            }
            Wire::FsData { rid, data, .. } => {
                if let Some(target) = SIM_WRITES.with(|w| w.borrow().get(&rid).cloned()) {
                    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&data) {
                        use std::io::Write;
                        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&target) {
                            f.write_all(&bytes).ok();
                        }
                    }
                }
            }
            Wire::FsWriteEnd { rid } => {
                SIM_WRITES.with(|w| w.borrow_mut().remove(&rid));
                let out = Wire::FsOk { rid, ok: true, error: String::new() };
                write_line(&mut wr, &out, keys.as_ref()).await?;
                log(&format!("{} ⇠ bridge upload finished", dev.name));
            }
            Wire::FsMkdirReq { rid, path } => {
                let out = match sim_write_target(share_root, &path) {
                    Ok(target) => {
                        let ok = std::fs::create_dir_all(&target).is_ok();
                        Wire::FsOk { rid, ok, error: String::new() }
                    }
                    Err(e) => Wire::FsOk { rid, ok: false, error: e },
                };
                write_line(&mut wr, &out, keys.as_ref()).await?;
            }
            Wire::FsDeleteReq { rid, path, recursive } => {
                let out = match sim_write_target(share_root, &path) {
                    Ok(target) => {
                        let ok = if recursive {
                            std::fs::remove_dir_all(&target).is_ok()
                        } else {
                            std::fs::remove_file(&target).is_ok()
                        };
                        Wire::FsOk { rid, ok, error: String::new() }
                    }
                    Err(e) => Wire::FsOk { rid, ok: false, error: e },
                };
                write_line(&mut wr, &out, keys.as_ref()).await?;
            }

            Wire::Bye { reason } => {
                log(&format!("{} ⇠ {peer_name} said bye ({reason})", dev.name));
                return Ok(());
            }
            _ => {}
        }
    }
}

thread_local! {
    /// rid → temp file for inbound bridge uploads (demo scale: one per conn).
    static SIM_WRITES: std::cell::RefCell<HashMap<String, PathBuf>> =
        std::cell::RefCell::new(HashMap::new());
}

async fn write_line(
    wr: &mut tokio::net::tcp::OwnedWriteHalf,
    wire: &Wire,
    keys: Option<&SessionKeys>,
) -> std::io::Result<()> {
    let mut line = match keys {
        Some(k) => {
            let plain = serde_json::to_vec(wire).map_err(|e| std::io::Error::other(e))?;
            let (n, c) = crypto::seal(&k.send, &plain);
            serde_json::to_string(&Wire::Sealed { n, c }).map_err(|e| std::io::Error::other(e))?
        }
        None => serde_json::to_string(wire).map_err(|e| std::io::Error::other(e))?,
    };
    line.push('\n');
    wr.write_all(line.as_bytes()).await
}

fn parse_line(line: &str, keys: Option<&SessionKeys>) -> Option<Wire> {
    let outer: Wire = serde_json::from_str(line).ok()?;
    match (outer, keys) {
        (Wire::Sealed { n, c }, Some(k)) => {
            let plain = crypto::open(&k.recv, &n, &c).ok()?;
            serde_json::from_slice(&plain).ok()
        }
        (Wire::Sealed { .. }, None) => None,
        (w, _) => Some(w),
    }
}

// --- tiny demo share: "/Storage/…" maps into a generated folder ----------

fn sim_resolve(share_root: &PathBuf, path: &str) -> Result<PathBuf, String> {
    let normalized = path.replace('\\', "/");
    let segs: Vec<&str> = normalized
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    if segs.is_empty() {
        return Ok(share_root.clone());
    }
    if segs[0] != "Storage" {
        return Err(format!("no shared folder named “{}”", segs[0]));
    }
    let mut p = share_root.clone();
    for seg in &segs[1..] {
        if *seg == ".." || seg.contains(':') {
            return Err("path escapes the share".into());
        }
        p.push(seg);
    }
    Ok(p)
}

fn sim_list(share_root: &PathBuf, path: &str) -> Result<Vec<FsEntryDto>, String> {
    let target = sim_resolve(share_root, path)?;
    if path == "/" || path.replace('\\', "/").trim_matches('/').is_empty() {
        return Ok(vec![FsEntryDto {
            kind: "dir".into(),
            name: "Storage".into(),
            size: 0,
            modified: 0,
        }]);
    }
    let mut out = Vec::new();
    for ent in std::fs::read_dir(&target).map_err(|e| e.to_string())?.flatten() {
        let Ok(meta) = ent.metadata() else { continue };
        out.push(FsEntryDto {
            kind: if meta.is_dir() { "dir".into() } else { "file".into() },
            name: ent.file_name().to_string_lossy().to_string(),
            size: meta.len(),
            modified: 0,
        });
    }
    out.sort_by(|a, b| (a.kind != "dir", a.name.to_lowercase()).cmp(&(b.kind != "dir", b.name.to_lowercase())));
    Ok(out)
}

fn sim_stat(share_root: &PathBuf, path: &str) -> Result<FsEntryDto, String> {
    let target = sim_resolve(share_root, path)?;
    let meta = std::fs::metadata(&target).map_err(|e| e.to_string())?;
    let name = target.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    Ok(FsEntryDto {
        kind: if meta.is_dir() { "dir".into() } else { "file".into() },
        name: if name.is_empty() { "Storage".into() } else { name },
        size: meta.len(),
        modified: 0,
    })
}

fn sim_open(share_root: &PathBuf, path: &str) -> Result<std::fs::File, String> {
    let target = sim_resolve(share_root, path)?;
    std::fs::File::open(&target).map_err(|e| e.to_string())
}

fn sim_write_target(share_root: &PathBuf, path: &str) -> Result<PathBuf, String> {
    sim_resolve(share_root, path)
}

/// Generate the demo share once per device: a Photos folder, a Documents
/// folder and a README so drives from fake devices are not empty.
fn seed_demo_share(dev: &SimDevice, base: &PathBuf) -> PathBuf {
    let root = base.join(format!("sim-share-{}", dev.name.replace([' ', '(', ')'], "-")));
    let _ = std::fs::create_dir_all(root.join("Photos"));
    let _ = std::fs::create_dir_all(root.join("Documents"));
    let readme = root.join("README.txt");
    if !readme.exists() {
        let text = format!(
            "This folder is shared by the simulated device [{}].\nFiles you drop here are served to every connected peer over\nthe encrypted bridge - exactly like a real device.\n",
            dev.name
        );
        let _ = std::fs::write(&readme, text);
    }
    let notes = root.join("Documents/notes.txt");
    if !notes.exists() {
        let _ = std::fs::write(&notes, "shopping list\n- coffee\n- more Wi-Fi\n");
    }
    for i in 1..=3 {
        let photo = root.join(format!("Photos/IMG_{i:04}.jpg"));
        if !photo.exists() {
            let mut buf: Vec<u8> = Vec::new();
            buf.extend_from_slice(
                format!("/* fake jpeg payload from {} — photo {i} */\n", dev.name).as_bytes(),
            );
            buf.resize(48 * 1024 + i as usize * 1024, b'x');
            let _ = std::fs::write(&photo, buf);
        }
    }
    root
}
