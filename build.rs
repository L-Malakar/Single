//! Embed the app icon (static/logo.ico) into the Windows executables so
//! Single-vX.Y.Z.exe, the taskbar and the app window all show the logo.
//! Runs only when building ON Windows for Windows; other hosts skip it
//! (cross-checks from Linux/macOS stay green without the Windows SDK).

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") && cfg!(windows) {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("static/logo.ico");
        res.compile().expect("cannot embed static/logo.ico");
    }
    println!("cargo:rerun-if-changed=static/logo.ico");
}
