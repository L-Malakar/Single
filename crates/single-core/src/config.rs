//! Configuration and on-disk layout.
//!
//! Single keeps *all* of its state under one "app system folder":
//!
//! ```text
//! Windows:  C:\Users\<you>\.lmalakar\Single\
//! Linux:    /home/<you>/.lmalakar/Single/
//! macOS:    /Users/<you>/.lmalakar/Single/
//! Android:  <app-private>/files/.lmalakar/Single/
//! ```
//!
//! Layout inside that folder:
//! ```text
//! config.json      user settings
//! pairings.json    paired devices + shared tokens
//! ca.pem / ca_key.pem        device-local certificate authority (the device identity)
//! server.pem / server_key.pem  TLS leaf key (persisted) + freshly signed leaf cert
//! Share/           default folder shared with the room
//! Downloads/       landing zone for files pulled from other devices
//! ```

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Development namespace folder used by apps made by L. Malakar.
pub const APP_VENDOR_DIR: &str = ".lmalakar";
pub const APP_NAME: &str = "Single";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// TCP port for the encrypted HTTPS/WebDAV server.
pub const DEFAULT_TCP_PORT: u16 = 27500;
/// UDP port used by the LAN discovery beacons.
pub const BEACON_PORT: u16 = 27501;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Human readable name shown on other devices.
    pub device_name: String,
    /// TCP port of the HTTPS/WebDAV server.
    pub tcp_port: u16,
    /// Folder shared with the room. `None` = use `<base>/Share`.
    pub share_dir: Option<PathBuf>,
    /// UI theme: "dark" or "light".
    pub theme: String,
    /// Windows: automatically try to map a network drive for every paired device.
    pub auto_map_drives: bool,
    /// Android (rooted phones only): expose per-device folders at /single/<name>.
    pub root_mode: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            device_name: default_device_name(),
            tcp_port: DEFAULT_TCP_PORT,
            share_dir: None,
            theme: "dark".to_string(),
            auto_map_drives: true,
            root_mode: false,
        }
    }
}

fn default_device_name() -> String {
    if let Ok(n) = std::env::var("COMPUTERNAME") {
        if !n.trim().is_empty() {
            return n.trim().to_string();
        }
    }
    if let Ok(n) = std::env::var("HOSTNAME") {
        if !n.trim().is_empty() {
            return n.trim().to_string();
        }
    }
    let mut buf = [0u8; 2];
    getrandom_fill(&mut buf);
    format!("Single-{}", hex::encode(buf))
}

fn getrandom_fill(buf: &mut [u8]) {
    use rand::RngCore;
    rand::rngs::OsRng.fill_bytes(buf);
}

/// Where every piece of Single's state lives.
#[derive(Debug, Clone)]
pub struct Paths {
    /// e.g. `C:\Users\me\.lmalakar\Single`
    pub base: PathBuf,
}

impl Paths {
    /// Resolve the default base folder.
    ///
    /// * Desktop: `<home>/.lmalakar/Single`
    /// * Android (no `$HOME`): `<app_data>/.lmalakar/Single` where `app_data`
    ///   is provided by the Tauri shell.
    pub fn default_base(app_data_override: Option<PathBuf>) -> PathBuf {
        if let Some(dir) = std::env::var_os("SINGLE_DATA_DIR") {
            return PathBuf::from(dir).join(APP_VENDOR_DIR).join(APP_NAME);
        }
        match (dirs_home(), app_data_override) {
            (Some(home), _) => home.join(APP_VENDOR_DIR).join(APP_NAME),
            (None, Some(app_data)) => app_data.join(APP_VENDOR_DIR).join(APP_NAME),
            (None, None) => std::env::temp_dir()
                .join(APP_VENDOR_DIR)
                .join(APP_NAME),
        }
    }

    pub fn new(base: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&base)
            .with_context(|| format!("failed to create app folder {}", base.display()))?;
        Ok(Paths { base })
    }

    pub fn config_file(&self) -> PathBuf {
        self.base.join("config.json")
    }
    pub fn pairings_file(&self) -> PathBuf {
        self.base.join("pairings.json")
    }
    pub fn ca_cert(&self) -> PathBuf {
        self.base.join("ca.pem")
    }
    pub fn ca_key(&self) -> PathBuf {
        self.base.join("ca_key.pem")
    }
    pub fn server_cert(&self) -> PathBuf {
        self.base.join("server.pem")
    }
    pub fn server_key(&self) -> PathBuf {
        self.base.join("server_key.pem")
    }
    /// Default folder shared with other devices.
    pub fn share_root(&self) -> PathBuf {
        self.base.join("Share")
    }
    /// Where files downloaded from peers are placed.
    pub fn downloads(&self) -> PathBuf {
        self.base.join("Downloads")
    }
    /// Android: app-visible per-device folders (also bind-mounted to /single in root mode).
    pub fn device_folders(&self) -> PathBuf {
        self.base.join("Devices")
    }

    pub fn load_config(&self) -> Config {
        match std::fs::read(self.config_file()) {
            Ok(bytes) => serde_json::from_slice::<Config>(&bytes).unwrap_or_default(),
            Err(_) => Config::default(),
        }
    }

    pub fn save_config(&self, cfg: &Config) -> Result<()> {
        let json = serde_json::to_vec_pretty(cfg)?;
        std::fs::write(self.config_file(), json)
            .with_context(|| format!("failed to write {}", self.config_file().display()))
    }

    /// Ensure the default Share/ and Downloads/ folders exist.
    pub fn ensure_layout(&self) -> Result<PathBuf> {
        let share = self.share_root();
        if !share.exists() {
            std::fs::create_dir_all(&share)?;
        }
        let dl = self.downloads();
        if !dl.exists() {
            std::fs::create_dir_all(&dl)?;
        }
        Ok(share)
    }
}

#[cfg(target_os = "android")]
fn dirs_home() -> Option<PathBuf> {
    None
}

#[cfg(not(target_os = "android"))]
fn dirs_home() -> Option<PathBuf> {
    dirs::home_dir()
}

/// Sanitize a peer/device name so it can be used as a folder name.
pub fn safe_folder_name(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        let ok = ch.is_alphanumeric() || matches!(ch, ' ' | '-' | '_' | '.' | '(' | ')');
        if ok {
            out.push(ch);
        } else {
            out.push('_');
        }
        if out.len() >= 48 {
            break;
        }
    }
    let trimmed = out.trim().to_string();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        "Device".to_string()
    } else {
        trimmed
    }
}

/// Verify that `candidate` exists and is a directory.
pub fn ensure_dir(p: &Path) -> Result<()> {
    if !p.is_dir() {
        std::fs::create_dir_all(p)
            .with_context(|| format!("failed to create folder {}", p.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_names_are_safe() {
        assert_eq!(safe_folder_name("My PC"), "My PC");
        assert_eq!(safe_folder_name("a/b\\c"), "a_b_c");
        assert_eq!(safe_folder_name(".."), "Device");
        assert_eq!(safe_folder_name(""), "Device");
    }

    #[test]
    fn config_roundtrip() {
        let dir = std::env::temp_dir().join(format!("single-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let paths = Paths::new(dir.clone()).unwrap();
        let mut cfg = Config::default();
        cfg.device_name = "Test Box".into();
        paths.save_config(&cfg).unwrap();
        let loaded = paths.load_config();
        assert_eq!(loaded.device_name, "Test Box");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
