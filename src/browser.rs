//! Auto-open the dashboard as an app-style window once the server is up.
//!
//! This is what turns the server into a "double-click and it works" Windows
//! app: the user never types a localhost address. Window strategy:
//!
//! - **Windows** — launch Edge (present on every modern Windows install) with
//!   `--app=…`, which renders the dashboard in a clean standalone window:
//!   no tabs, no address bar, its own taskbar entry. Chrome is the fallback,
//!   then the system default browser via `cmd /C start`.
//! - **macOS / Linux** — `open` / `xdg-open` on the default browser.
//!
//! Everything here is best-effort: a failure to open a window is logged, it
//! never breaks the server.

#[cfg(not(target_os = "android"))]
use std::process::Command;

/// Wait (in the background) until the dashboard port accepts connections,
/// then open it in an app-style window. Never blocks or panics.
#[cfg(not(target_os = "android"))]
pub fn open_when_ready(port: u16) {
    let _ = std::thread::Builder::new()
        .name("open-dashboard".into())
        .spawn(move || {
            // Poll until axum has bound the HTTP listener (max ~15 s).
            for _ in 0..150 {
                if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            // Small grace period so the router is fully serving.
            std::thread::sleep(std::time::Duration::from_millis(300));
            let url = format!("http://127.0.0.1:{port}");
            if open_app_window(&url) {
                println!("  app window ready: {url}");
            } else {
                println!("  note: open the dashboard manually at {url}");
            }
        });
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn open_app_window(url: &str) -> bool {
    use std::path::Path;

    // Chromium browsers that support `--app` standalone windows, best first.
    const BROWSERS: [&str; 2] = [
        r"Microsoft\Edge\Application\msedge.exe",
        r"Google\Chrome\Application\chrome.exe",
    ];
    // Install roots, covering x64 / x86 / per-user installs.
    let roots = [
        std::env::var_os("ProgramFiles(x86)"),
        std::env::var_os("ProgramFiles"),
        std::env::var_os("LOCALAPPDATA"),
    ];

    for rel in BROWSERS {
        for root in roots.iter().flatten() {
            let exe = Path::new(root).join(rel);
            if exe.is_file() {
                let opened = Command::new(&exe)
                    .arg(format!("--app={url}"))
                    .arg("--window-size=1360,900")
                    .spawn()
                    .is_ok();
                if opened {
                    return true;
                }
            }
        }
    }

    // Final fallback: the system default browser, as a plain tab.
    Command::new("cmd.exe")
        .args(["/C", "start", "", url])
        .spawn()
        .is_ok()
}

// ---------------------------------------------------------------------------
// macOS / Linux / BSDs
// ---------------------------------------------------------------------------

#[cfg(all(unix, not(target_os = "android")))]
fn open_app_window(url: &str) -> bool {
    let prog = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    Command::new(prog).arg(url).spawn().is_ok()
}

#[cfg(not(any(windows, unix)))]
fn open_app_window(_url: &str) -> bool {
    false
}

// ---------------------------------------------------------------------------
// Android
// ---------------------------------------------------------------------------

/// On Android the dashboard is displayed by the wrapper app's WebView
/// (see `android/`), so there is nothing to auto-open from the server.
#[cfg(target_os = "android")]
pub fn open_when_ready(_port: u16) {}
