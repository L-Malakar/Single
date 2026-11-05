//! Inbound file-transfer assembly: sanitisation, buffering, SHA-256 verify, save.

use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// Hard cap for in-memory assembly (256 MB).
pub const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;

pub struct InboundTransfer {
    pub tid: String,
    pub name: String,
    pub size: u64,
    pub sha256: String,
    pub buf: Vec<u8>,
}

impl InboundTransfer {
    pub fn received(&self) -> u64 {
        self.buf.len() as u64
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}

/// Strip directories / control characters so a malicious peer cannot escape
/// the receive directory via `../../` or weird names.
pub fn sanitize_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("file.bin");
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_control() || c.is_ascii() && (c as u8) < 32 {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').to_string();
    if trimmed.is_empty() {
        "file.bin".to_string()
    } else {
        trimmed.chars().take(96).collect()
    }
}

/// Save `bytes` under `received_dir`, de-duplicating (`photo.jpg` → `photo (1).jpg`).
pub fn save(received_dir: &std::path::Path, name: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(received_dir)?;
    let safe = sanitize_filename(name);
    let mut candidate = received_dir.join(&safe);
    let stem = {
        let p = std::path::Path::new(&safe);
        p.file_stem().and_then(|s| s.to_str()).unwrap_or("file").to_string()
    };
    let ext = {
        let p = std::path::Path::new(&safe);
        p.extension().and_then(|s| s.to_str()).map(|e| format!(".{e}")).unwrap_or_default()
    };
    let mut counter = 1;
    while candidate.exists() {
        candidate = received_dir.join(format!("{stem} ({counter}){ext}"));
        counter += 1;
    }
    std::fs::write(&candidate, bytes)?;
    Ok(candidate)
}
