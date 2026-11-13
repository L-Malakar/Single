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
- **Version:** 0.1.5

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
| Node.js + npm | Tauri CLI + **required for the APK** (the Android Gradle build runs the Tauri CLI through npm) | <https://nodejs.org> |
| Inno Setup 6 *(optional)* | nicer `Single-Setup.exe` | <https://jrsoftware.org/isinfo.php> |
| Android Studio *(for the APK)* | SDK + NDK + JDK | <https://developer.android.com/studio> |

Without Node the script installs `tauri-cli` through Cargo (slower, one
time) and Windows artifacts still build — but the **APK step needs Node.js**.
Without Inno Setup the script falls back to Tauri's built-in NSIS
installer. Without Android Studio you still get all Windows artifacts.

The repo root contains a small `package.json` with a `"tauri"` script —
that is what the Android Gradle build calls back into (`npm run tauri …`),
and `Build.bat` runs `npm install` for you so the CLI lands in
`node_modules`. Keep the file next to `Build.bat`.

### Build

```bat
:: clone / unzip the source, then:
Build.bat
```

The script does everything: installs the local Tauri CLI (`npm install`),
refreshes icons from `logo.svg`, builds the Windows exe, builds
`Single-Setup-v0.1.5.exe`, packs the portable zip, and — when an Android SDK
is present — generates the Android project on first run and builds the APK.

### Artifacts

```text
Single\
├── Windows\
│   ├── Single.exe                  portable, just run it
│   ├── Single-Setup-v0.1.5.exe     installer (Inno Setup or NSIS)
│   └── Single-v0.1.5.zip           portable zip
└── Android\
    └── Single-v0.1.5.apk           debug-signed, installs directly
```

## Where the logo appears

Every logo is generated from **`logo.svg`** in the repo root — replace the
file and run `Build.bat` again to refresh all of them:

- desktop + taskbar icon (embedded into `Single.exe`),
- the **startup splash screen** (shown instantly, closes when the UI is
  ready — Windows only; see `ui/splash.html` and `tauri.windows.conf.json`),
- the NSIS installer **and uninstaller** executables plus the wizard header
  (`icons/header.bmp`, regenerated each build),
- the Inno Setup installer (setup icon + Add/Remove-Programs icon),
- the Android launcher icons (all densities).

## One app, two experiences

Single ships one codebase with a UI that adapts to the device it runs on —
no compromise layout shared between mouse and thumb:

**Windows / desktop (window ≥ 761 px wide)**

- Sidebar navigation with crisp SVG icons (no emojis that render differently
  across platforms).
- Double-click folders in *Files*; hover states everywhere.
- Consistent icon buttons (download / rename / delete) with tooltips.

**Android / mobile (narrow or touch screen)**

- A dedicated **top bar** (logo + this device's name) and a **bottom tab
  bar** with five large touch targets — thumb-friendly navigation.
- **Single tap** opens folders; the date column hides; row actions become
  icon buttons sized for fingers.
- Confirmation dialogs and the rename / new-folder prompts slide up as
  **bottom sheets** instead of native browser popups (which are unreliable
  inside the Android WebView anyway).
- Safe-area aware (edge-to-edge phones), status-bar color follows the
  dark/light theme, and the accent color matches the logo blue.

**Long-press freeze fixed.** Holding an image (or any content) used to pop
up the Android WebView's native callout/drag preview on top of the app and
freeze the screen. v0.1.4 disables image dragging, the touch callout and
text selection UI-wide (typing and the pairing PIN still work as before).

**Mobile is a first-class citizen since v0.1.5.** The Android file picker
returns `content://` streams that cannot be opened as file paths, so uploads
from a phone now go through the WebView's own file input and a chunked
staging pipeline (`upload_begin` / `upload_chunk` / `upload_end`) — with the
same progress view and failure handling as desktop uploads. Downloads try
the public `Download/Single` folder first (when Android lets the app write
it) so files actually show up in file managers and galleries, and fall back
to app storage otherwise. The hardware **back button** closes the open
dialog first and returns to *Devices* before exiting, and bottom sheets
ride above the on-screen keyboard instead of hiding under it.

## What changed in 0.1.5 (bug hunt)

A full audit of the codebase fixed, among others:

- **Windows: all writes were rejected.** `canonicalize()` returns verbatim
  `\\?\C:\...` paths on Windows, so the share-root containment check failed
  for every upload / folder creation / delete / rename. Both sides of the
  comparison are now canonicalized and prefix-normalized.
- **Unpair left the network drive mapped** on Windows (the drive letter was
  looked up *after* the pairing record was deleted).
- **Locked files could not be deleted** by their lock owner for up to an
  hour (the lock check dropped the request's `If:` token).
- **Chunked uploads could desync keep-alive connections** (the trailer
  parser left bytes in the stream).
- **File names containing `&amp;`-like text** were corrupted by a double
  XML-unescape in the directory listing parser.
- A stalled peer can no longer freeze a transfer forever (120 s read
  timeout), one slow offline device no longer delays everyone's
  online/offline status (concurrent heartbeats), the transfer list no
  longer grows without bound, and several panic paths (string slicing on
  short IDs, port retry near 65535) were removed.

## Install & use

**Windows** — run `Single-Setup-v0.1.5.exe` (or just `Single.exe`). In
*Settings → Enable drive support* accept the one-time UAC prompt: it
installs Single's certificate, starts the WebClient service, lifts the
50 MB WebDAV cap, and opens the firewall. Then map paired devices as drives
from the *Devices* page.

**Android** — allow "install from unknown sources" for the APK. Uploads
use the system file picker directly (any file, any app, no storage
permission needed); downloads land in `Download/Single` when the OS allows
it, otherwise in the app folder shown in *About*. On rooted
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
- All paths are sandboxed inside the shared folder (`..`-traversal
  rejected, symlink escapes blocked since v0.1.5).
- Unpairing revokes the token on both devices instantly.
- Known v0.1.5 limits: the WebDAV token travels in the URL-mapping (like any
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
   keep the PATH they were started with. `Build.bat` runs `gradlew --stop`
   before building, which retires stale daemons automatically.
3. Verify: `where npm` must print a path, then run `Build.bat` again.
4. Still stuck? Delete `src-tauri\gen\android` and run `Build.bat`; the
   whole Android project is regenerated from scratch.

### `npm error Missing script: "tauri"` (Android)

Gradle calls the Tauri CLI with `npm run tauri …` **in the project root**.
That requires a root `package.json` containing the `"tauri"` script and the
CLI installed in `node_modules`. This repo ships both; `Build.bat` runs
`npm install` automatically before the Android step.

1. Make sure `package.json` (repo root, next to `Build.bat`) is present.
2. Run `npm install` in the project root — internet + Node.js LTS needed.
3. Verify: `npm run tauri --version` must print a version.
4. Run `Build.bat` again. If it still fails, delete `src-tauri\gen\android`
   so Gradle regenerates the project.

### Kotlin daemon crash — `different roots` / `Could not close incremental caches` (Android)

The Tauri Android library sources live inside the Cargo registry (usually
`C:\Users\<you>\.cargo\registry\...`) while the project sits on another
drive (for example `D:\Webproject\Single-main`). Kotlin's *incremental*
compiler cannot store relative paths across two Windows drive letters and
the whole Kotlin daemon dies.

`Build.bat` prevents this automatically: it appends
`kotlin.incremental=false` to `src-tauri\gen\android\gradle.properties`,
compiles Kotlin in-process, and clears stale caches next to the Tauri
sources. Manual fix, if you build Gradle yourself:

1. Add `kotlin.incremental=false` to `src-tauri\gen\android\gradle.properties`.
2. Delete `C:\Users\<you>\.cargo\registry\src\...\tauri-<version>\mobile\android\build`.
3. Run the build again. (Alternatively keep the project and Cargo on the
   same drive — see [tauri issue #10876](https://github.com/tauri-apps/tauri/issues/10876).)

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
