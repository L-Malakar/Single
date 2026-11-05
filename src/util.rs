//! Small shared helpers: time, nonces, human formatting, bus logging.

use std::time::{SystemTime, UNIX_EPOCH};

/// Current UNIX time in milliseconds.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Short random hex nonce, used as an application-level "sequence number"
/// during the TCP handshake (echo/ack validation).
pub fn nonce() -> String {
    let a = (rand::random::<u32>() & 0xFFFF) as u16;
    let b = (rand::random::<u32>() & 0xFFFF) as u16;
    format!("{a:04x}{b:04x}")
}

/// 1.2 KB / 3.4 MB style human formatting.
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < 4 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.1} {}", UNITS[i])
}

/// UTC HH:MM:SS for server-side log lines (the browser formats its own local time).
pub fn ts_hms(ts_ms: i64) -> String {
    let secs = ts_ms / 1000;
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    format!("{h:02}:{m:02}:{s:02}")
}

/// Print a log line to stdout and (optionally) fan it out to every
/// connected dashboard over the WebSocket broadcast bus.
pub fn log_bus(
    bus: &tokio::sync::broadcast::Sender<crate::protocol::ServerMsg>,
    level: &str,
    msg: impl AsRef<str>,
) {
    let msg = msg.as_ref();
    println!("[{}][{level}] {msg}", ts_hms(now_ms()));
    let _ = bus.send(crate::protocol::ServerMsg::Log {
        level: level.to_string(),
        msg: msg.to_string(),
        ts: now_ms(),
    });
}
