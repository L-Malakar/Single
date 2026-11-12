//! Platform integration.
//!
//! * **Windows** — map paired devices as network drives in "This PC" via the
//!   built-in WebDAV redirector (`net use X: https://ip:port/dav`), plus the
//!   one-time elevated helper (install our CA into the machine trust store,
//!   start the WebClient service, raise the 50 MB transfer cap, open the
//!   firewall for port 27500).
//! * **Android (rooted)** — create `/single/<DeviceName>` folders at the
//!   system root and bind-mount the app's per-device folders onto them, so
//!   other apps can see remote devices like local folders. Without root the
//!   folders live in the app's own storage instead.

use anyhow::{anyhow, Result};

pub fn platform_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "android") {
        "android"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "other"
    }
}

// ------------------------------------------------------------------ Windows

#[cfg(windows)]
pub mod win {
    use super::*;
    use std::path::Path;
    use std::process::Command;

    /// Drive letters we are allowed to use (never A/B).
    const LETTERS: &[u8] = b"ZYXWVUTSRQPONMLKJIHGFED";

    pub fn find_free_letter() -> Option<String> {
        for &c in LETTERS {
            let letter = (c as char).to_string();
            let root = format!("{}:\\", letter);
            if std::fs::metadata(&root).is_err() {
                return Some(letter);
            }
        }
        None
    }

    fn net_use_args(letter: &str, url: &str, token: &str) -> Vec<String> {
        vec![
            "use".into(),
            format!("{}:", letter),
            url.into(),
            "/user:single".into(),
            token.into(),
            "/persistent:yes".into(),
        ]
    }

    /// Map a paired device as a drive letter. Returns the letter used.
    pub async fn map_drive(ip: &str, port: u16, token: &str) -> Result<String> {
        ensure_webclient_best_effort().await;
        let letter = find_free_letter().ok_or_else(|| anyhow!("no free drive letter"))?;
        let url = format!("https://{}:{}/dav", ip, port);
        let out = Command::new("net")
            .args(net_use_args(&letter, &url, token))
            .output()
            .map_err(|e| anyhow!("failed to run net use: {}", e))?;
        if !out.status.success() {
            let msg = String::from_utf8_lossy(&out.stderr).to_string();
            return Err(anyhow!(
                "net use failed: {} (hint: run \"Enable drive support\" once, and make sure the device is online)",
                msg.trim()
            ));
        }
        Ok(letter)
    }

    pub async fn unmap_drive(letter: &str) -> Result<()> {
        let out = Command::new("net")
            .args(["use", &format!("{}:", letter), "/delete", "/y"])
            .output()
            .map_err(|e| anyhow!("failed to run net use: {}", e))?;
        if !out.status.success() {
            return Err(anyhow!(
                "net use /delete failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(())
    }

    /// (letter, remote url) of currently mapped WebDAV drives.
    pub fn list_mapped() -> Vec<(String, String)> {
        let mut out = Vec::new();
        if let Ok(txt) = Command::new("net").arg("use").output() {
            let text = String::from_utf8_lossy(&txt.stdout);
            for line in text.lines() {
                let tokens: Vec<&str> = line.split_whitespace().collect();
                if tokens.len() < 3 {
                    continue;
                }
                // find "X:" and an https:// URL on the same line
                if let Some(pos) = tokens.iter().position(|t| t.len() == 2 && t.ends_with(':') && t.as_bytes()[0].is_ascii_uppercase()) {
                    if let Some(remote) = tokens.iter().find(|t| t.starts_with("http")) {
                        out.push((tokens[pos].trim_end_matches(':').to_string(), remote.to_string()));
                    }
                }
            }
        }
        out
    }

    /// Best effort: try to start WebClient without elevation.
    async fn ensure_webclient_best_effort() {
        let _ = tokio::task::spawn_blocking(|| {
            let _ = Command::new("net").args(["start", "WebClient"]).output();
        })
        .await;
    }

    /// One-time elevated setup (UAC prompt):
    /// * install the device CA into the machine Root store
    /// * start WebClient and set it to manual start
    /// * raise the WebDAV 50 MB per-file cap to 4 GB
    /// * open the firewall for inbound TCP 27500
    pub fn enable_drive_support(ca_pem_path: &Path, port: u16) -> Result<()> {
        let script_path = std::env::temp_dir().join("single-elevate.ps1");
        let ca = ca_pem_path
            .to_str()
            .ok_or_else(|| anyhow!("ca path not unicode"))?
            .to_string();
        let inner = format!(
            "$ErrorActionPreference='SilentlyContinue'; \
             certutil -addstore -f Root '{ca}'; \
             Set-Service WebClient -StartupType Manual; \
             Start-Service WebClient; \
             New-ItemProperty -Path 'HKLM:\\SYSTEM\\CurrentControlSet\\Services\\WebClient\\Parameters' -Name FileSizeLimitInBytes -Value 4294967295 -PropertyType DWord -Force | Out-Null; \
             netsh advfirewall firewall delete rule name='Single (file sharing)' | Out-Null; \
             netsh advfirewall firewall add rule name='Single (file sharing)' dir=in action=allow protocol=TCP localport={port} | Out-Null",
            ca = ca,
            port = port
        );
        std::fs::write(&script_path, inner).map_err(|e| anyhow!("write temp script: {}", e))?;
        let ps1 = script_path
            .to_str()
            .ok_or_else(|| anyhow!("temp path not unicode"))?
            .to_string();
        let outer = format!(
            "Start-Process powershell -Verb RunAs -Wait -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','{}'",
            ps1
        );
        let out = Command::new("powershell")
            .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &outer])
            .output()
            .map_err(|e| anyhow!("failed to launch elevated helper: {}", e))?;
        let _ = std::fs::remove_file(&script_path);
        if !out.status.success() {
            return Err(anyhow!(
                "elevated helper failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(())
    }

    /// Per-user CA install (no UAC) — helps apps that resolve through the
    /// user's store. The elevated helper handles the machine store.
    pub fn install_ca_user(ca_pem_path: &Path) -> Result<()> {
        let out = Command::new("certutil")
            .args(["-user", "-addstore", "Root"])
            .arg(ca_pem_path)
            .output()
            .map_err(|e| anyhow!("certutil failed: {}", e))?;
        if !out.status.success() {
            return Err(anyhow!(
                "certutil: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(())
    }
}

#[cfg(not(windows))]
pub mod win {
    use super::*;
    use std::path::Path;

    pub fn find_free_letter() -> Option<String> {
        None
    }
    pub async fn map_drive(_ip: &str, _port: u16, _token: &str) -> Result<String> {
        Err(anyhow!("drive mapping is Windows-only"))
    }
    pub async fn unmap_drive(_letter: &str) -> Result<()> {
        Err(anyhow!("drive mapping is Windows-only"))
    }
    pub fn list_mapped() -> Vec<(String, String)> {
        Vec::new()
    }
    pub fn enable_drive_support(_ca: &Path, _port: u16) -> Result<()> {
        Err(anyhow!("drive mapping is Windows-only"))
    }
    pub fn install_ca_user(_ca: &Path) -> Result<()> {
        Err(anyhow!("drive mapping is Windows-only"))
    }
}

// ------------------------------------------------------------------ Android

/// Per-device folder report.
pub struct RootReport {
    pub lines: Vec<String>,
    pub used_root: bool,
}

#[cfg(target_os = "android")]
pub mod android {
    use super::*;
    use std::path::Path;
    use std::process::Command;

    pub fn su_available() -> bool {
        Command::new("su")
            .args(["-c", "id"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Create `/single/<name>` at the system root and bind-mount the
    /// app-visible per-device folder onto it. Falls back gracefully.
    pub fn setup_root_folders(
        app_devices_dir: &Path,
        peers: &[(String, String)], // (folder_name, unique_suffix)
    ) -> Result<RootReport> {
        let mut lines = Vec::new();
        if !su_available() {
            return Ok(RootReport {
                lines: vec!["root (su) not available — using app folders".into()],
                used_root: false,
            });
        }
        // one big shell command = one root prompt
        let mut script = String::from("mkdir -p /single;");
        for (name, suffix) in peers {
            let folder = format!("{} ({})", crate::config::safe_folder_name(name), &suffix[..6]);
            let src = app_devices_dir.join(&folder);
            let _ = std::fs::create_dir_all(&src);
            let src = src.to_str().unwrap_or("");
            script.push_str(&format!(
                "mkdir -p '/single/{folder}'; grep -q ' /single/{folder} ' /proc/mounts || mount --bind '{src}' '/single/{folder}'; chmod 777 '/single/{folder}'; ",
                folder = folder,
                src = src
            ));
            lines.push(format!("/single/{} -> {}", folder, src));
        }
        let out = Command::new("su")
            .args(["-c", &script])
            .output()
            .map_err(|e| anyhow!("su failed: {}", e))?;
        if !out.status.success() {
            return Err(anyhow!(
                "root setup failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        lines.insert(0, "system-root folders ready at /single".into());
        Ok(RootReport {
            lines,
            used_root: true,
        })
    }
}

#[cfg(not(target_os = "android"))]
pub mod android {
    use super::*;
    use std::path::Path;

    pub fn su_available() -> bool {
        false
    }
    pub fn setup_root_folders(_dir: &Path, _peers: &[(String, String)]) -> Result<RootReport> {
        Err(anyhow!("Android only"))
    }
}
