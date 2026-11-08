# Single

**One room, one network.** Single connects every device in a room — laptops,
phones, desktops — into one shared space. Devices find each other
automatically, pair once with a PIN, and then every device's files are right
there on the others: browsable in the app, mounted as a **network drive in
"This PC"** on Windows, and usable by *any* other program as if the files
were local.

- **Developer:** [L. Malakar](https://github.com/L-Malakar)
- **Source code:** <https://github.com/L-Malakar/Single>
- **License:** [MIT](https://github.com/L-Malakar/Single/blob/main/LICENSE)
- **Platforms:** Windows · Android *(Linux & macOS: roadmap)*
- **Version:** 0.1.0

---

## What it does

1. **Finds devices automatically.** When Single launches it searches the
   local network (Wi-Fi and Ethernet — mDNS + broadcast beacons; Bluetooth is
   on the roadmap). If discovery is blocked, you can always add a device by
   IP.
2. **Pairs devices securely.** Pairing needs *both* a 6-digit PIN shown on
   the target device **and** a manual **Allow** on it. Nothing connects
   silently.
3. **Maps other devices as drives / folders.** On Windows every paired
   device can be mounted as a new network drive in *This PC*. On Android
   (rooted phones, experimental) per-device folders appear at `/single/<DeviceName>`.
4. **Share everything directly.** Browse, stream, open, upload, download,
   rename, delete — files and media flow device-to-device over the room's own
   network, never through a cloud.
5. **Works with your other apps.** Because paired devices are real OS-level
   drives (Windows WebDAV), *every* program — video players, Office, code
   editors, file managers — can open those files directly.

## How it works (architecture)

```text
┌───────────────────────────── Single app ──────────────────────────────┐
│  HTML / CSS / JS UI  (system webview)                                 │
│         │  Tauri IPC                                                  │
│  ┌──────▼────────────────────────────────────────────────────┐        │
│  │  Rust backend (single-core)                               │        │
│  │   • discovery:  mDNS + UDP beacons + manual IP            │        │
│  │   • pairing:    PIN + approval, cert-pinned TLS           │        │
│  │   • transfer:   HTTPS/WebDAV subset (TLS 1.2/1.3)         │        │
│  │   • platform:   Windows drive mapping / Android root dirs │        │
│  └──────┬────────────────────────────────────────────────────┘        │
└─────────┼─────────────────────────────────────────────────────────────┘
          │  TLS with certificate pinning (never a public CA)
   ┌──────▼────────┐            ┌───────────────┐
   │  Device A     │◀──────────▶│  Device B     │
   │  port 27500   │  encrypted │  port 27500   │
   └───────────────┘            └───────────────┘
```

Every device runs its own tiny certificate authority (stored in
`~/.lmalakar/Single`). The SHA-256 fingerprint of that key **is** the device
identity. Peers pin it after pairing, so a connection can only be made to a
device you have actually approved. File access is authorized with a shared
256-bit token derived from both fingerprints plus the pairing PIN.

The transfer layer is an HTTPS/WebDAV subset — that is deliberate: WebDAV is
what lets Windows mount a paired device as a **drive letter** (`net use S:
https://192.168.1.23:27500/dav ...`) that all other programs can use. Plain
FTP cannot be mounted as a drive; FTPS/FTP-client support may be added as an
extra compatibility layer later.

## Build it yourself

### Prerequisites (Windows)

| Tool | Why | Get it |
| --- | --- | --- |
| Rust (stable) | compiles the backend | <https://rustup.rs> |
| Node.js + npm *(optional)* | faster Tauri CLI install | <https://nodejs.org> |
| Inno Setup 6 *(optional)* | nicer `Single-Setup.exe` | <https://jrsoftware.org/isinfo.php> |
| Android Studio *(for the APK)* | SDK + NDK + JDK | <https://developer.android.com/studio> |

Without Node the script installs `tauri-cli` through Cargo (slower, one
time). Without Inno Setup the script falls back to Tauri's built-in NSIS
installer. Without Android Studio you still get all Windows artifacts.

### Build

```bat
:: clone / unzip the source, then:
Build.bat
```

The script does everything: refreshes icons from `logo.svg`, builds the
Windows exe, builds `Single-Setup-v0.1.0.exe`, packs the portable zip, and —
when an Android SDK is present — generates the Android project on first run
and builds the APK.

### Artifacts

```text
Single\
├── Windows\
│   ├── Single.exe                  portable, just run it
│   ├── Single-Setup-v0.1.0.exe     installer (Inno Setup or NSIS)
│   └── Single-v0.1.0.zip           portable zip
└── Android\
    └── Single-v0.1.0.apk           debug-signed, installs directly
```

### Use your own logo

Replace **`logo.svg`** (square artwork, transparent background works best)
and run `Build.bat` again — every app icon is regenerated from that file.

## Install & use

**Windows** — run `Single-Setup-v0.1.0.exe` (or just `Single.exe`). In
*Settings → Enable drive support* accept the one-time UAC prompt: it
installs Single's certificate, starts the WebClient service, lifts the
50 MB WebDAV cap, and opens the firewall. Then map paired devices as drives
from the *Devices* page.

**Android** — allow "install from unknown sources" for the APK. On rooted
phones enable *Settings → Expose per-device folders at /single* to create
`/single/<DeviceName>` (bind-mounted from the app's own storage). Without
root everything still works inside the app; the folders simply live in the
app's private storage.

**Everywhere** — the app's system folder is `~/.lmalakar/Single`:

```text
~/.lmalakar/Single/
├── config.json        settings
├── pairings.json      paired devices + shared tokens
├── ca.pem / ca_key.pem          device identity (keep private!)
├── server.pem / server_key.pem  TLS material
├── Share/             the folder other devices can see
└── Downloads/         landing zone for files pulled from peers
```

## Security model

- All traffic is TLS-encrypted; server certificates are **pinned** to the
  pairing record, not validated against public CAs.
- Pairing = **PIN + manual approval** on the target device, with the PIN
  verified cryptographically on both ends.
- File access requires a shared token derived from both device fingerprints
  and the PIN — a device that was never approved cannot read a byte.
- All paths are sandboxed inside the shared folder (`..`-traversal rejected).
- Unpairing revokes the token on both devices instantly.
- Known v0.1.0 limits: the WebDAV token travels in the URL-mapping (like any
  drive mapping), and drives must be re-mapped if a device's IP changes.

## Roadmap

- Bluetooth discovery + transfer
- Linux & macOS builds (the core is already cross-platform Rust)
- Android DocumentsProvider so remote devices appear in any file manager
- End-to-end resume for large transfers, per-folder sharing permissions

## Contributing

The interesting logic lives in [`crates/single-core`](crates/single-core) —
pure Rust, fully unit-tested (including a two-device end-to-end test). The
Tauri shell (`src-tauri`) is a thin adapter, and the UI is plain
HTML/CSS/JS in [`ui/`](ui). PRs at
<https://github.com/L-Malakar/Single> are welcome.

## License

MIT — © 2026 [L. Malakar](https://github.com/L-Malakar). See
[LICENSE](LICENSE).
