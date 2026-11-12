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
<<<<<<< HEAD
- **Version:** 0.1.0
=======
- **Version:** 0.1.2
>>>>>>> e87b726b12d4762f68ebf0cba47f2ef7fa1f21b4

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
<<<<<<< HEAD
| Node.js + npm *(optional)* | faster Tauri CLI install | <https://nodejs.org> |
=======
| Node.js + npm | Tauri CLI + **required for the APK** (the Android Gradle build runs the Tauri CLI through npm) | <https://nodejs.org> |
>>>>>>> e87b726b12d4762f68ebf0cba47f2ef7fa1f21b4
| Inno Setup 6 *(optional)* | nicer `Single-Setup.exe` | <https://jrsoftware.org/isinfo.php> |
| Android Studio *(for the APK)* | SDK + NDK + JDK | <https://developer.android.com/studio> |

Without Node the script installs `tauri-cli` through Cargo (slower, one
<<<<<<< HEAD
time). Without Inno Setup the script falls back to Tauri's built-in NSIS
=======
time) and Windows artifacts still build — but the **APK step needs Node.js**.
Without Inno Setup the script falls back to Tauri's built-in NSIS
>>>>>>> e87b726b12d4762f68ebf0cba47f2ef7fa1f21b4
installer. Without Android Studio you still get all Windows artifacts.

### Build

```bat
:: clone / unzip the source, then:
Build.bat
```

The script does everything: refreshes icons from `logo.svg`, builds the
<<<<<<< HEAD
Windows exe, builds `Single-Setup-v0.1.0.exe`, packs the portable zip, and —
=======
Windows exe, builds `Single-Setup-v0.1.2.exe`, packs the portable zip, and —
>>>>>>> e87b726b12d4762f68ebf0cba47f2ef7fa1f21b4
when an Android SDK is present — generates the Android project on first run
and builds the APK.

### Artifacts

```text
Single\
├── Windows\
│   ├── Single.exe                  portable, just run it
<<<<<<< HEAD
│   ├── Single-Setup-v0.1.0.exe     installer (Inno Setup or NSIS)
│   └── Single-v0.1.0.zip           portable zip
└── Android\
    └── Single-v0.1.0.apk           debug-signed, installs directly
=======
│   ├── Single-Setup-v0.1.2.exe     installer (Inno Setup or NSIS)
│   └── Single-v0.1.2.zip           portable zip
└── Android\
    └── Single-v0.1.2.apk           debug-signed, installs directly
>>>>>>> e87b726b12d4762f68ebf0cba47f2ef7fa1f21b4
```

### Use your own logo

<<<<<<< HEAD
Replace **`logo.svg`** (square artwork, transparent background works best)
and run `Build.bat` again — every app icon is regenerated from that file.

## Install & use

**Windows** — run `Single-Setup-v0.1.0.exe` (or just `Single.exe`). In
=======
All app icons are generated from **`logo.svg`** in the repo root (square
artwork, transparent background works best). Replace it and run `Build.bat`
again — every icon, including the Android launcher icons, is regenerated
from that file.

## Install & use

**Windows** — run `Single-Setup-v0.1.2.exe` (or just `Single.exe`). In
>>>>>>> e87b726b12d4762f68ebf0cba47f2ef7fa1f21b4
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
<<<<<<< HEAD
- Known v0.1.0 limits: the WebDAV token travels in the URL-mapping (like any
  drive mapping), and drives must be re-mapped if a device's IP changes.

=======
- Known v0.1.2 limits: the WebDAV token travels in the URL-mapping (like any
  drive mapping), and drives must be re-mapped if a device's IP changes.

## Troubleshooting

### `A problem occurred starting process 'command 'npm.bat''` (Android)

The Android build has two layers: `Build.bat` → Gradle → back into the
Tauri CLI. When the Android project was generated via `npx`, the generated
Gradle task (`src-tauri/gen/android/buildSrc/.../BuildTask.kt`) launches the
Tauri CLI **through npm**, so Gradle must be able to start `npm.bat`.
It fails when npm is missing from the PATH that the Gradle *daemon* sees:

1. **Install Node.js LTS** from <https://nodejs.org> (if `npm --version`
   fails in a fresh terminal).
2. **Open a NEW terminal** (or reboot) — running shells and Gradle daemons
   keep the PATH they were started with. `Build.bat` now runs
   `gradlew --stop` before building, which retires stale daemons
   automatically; rebooting does the same.
3. Verify: `where npm` must print a path, then run `Build.bat` again.
4. Still stuck? Delete `src-tauriin/android` → run `Build.bat`; the
4. Still stuck? Delete `src-tauri\gen\android` and run `Build.bat`; the

### `memory allocation of ... bytes failed` (rustc dies mid-build)

The compiler ran out of RAM — the `windows` crate that Tauri depends on is
gigantic and each rustc process can peak at several GB. Multiple rustc
processes run in parallel by default, so machines with 8 GB or less (or a
browser / Android Studio open at the same time) hit the wall mid-build.

- `Build.bat` detects your RAM and caps parallel compile jobs automatically
  (16 GB+ → no cap, 8 GB → 2 jobs, less → 1 job). It also intentionally does
  not enable `lto` / `codegen-units = 1`, which multiply peak memory.
- Free more RAM: close browsers, Android Studio, IDEs before building.
- Force the safest setting once and rebuild in the same terminal:
  `set CARGO_BUILD_JOBS=1` then `Build.bat`.
- Make sure Windows has a page file enabled (System → Advanced system
  settings → Performance → Virtual memory).

### Windows build is fine, only the APK fails

That is expected whenever a prerequisite of the Android toolchain is
missing (NDK, licenses, Node). All Windows artifacts under `Single\Windows\`
are already finished — fix the reported cause and re-run `Build.bat`.

>>>>>>> e87b726b12d4762f68ebf0cba47f2ef7fa1f21b4
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
