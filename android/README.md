# Single for Android

The same Rust server (mDNS discovery + dashboard + P2P chat/files), packaged
as a real installable APK. The wrapper app is intentionally tiny:

```
android/
├── settings.gradle / build.gradle / gradle.properties
├── gradle/wrapper/gradle-wrapper.properties
└── app/
    ├── build.gradle                 no external dependencies — pure SDK APIs
    └── src/main/
        ├── AndroidManifest.xml      permissions: internet, multicast, FGS
        ├── java/app/single/bridge/
        │   ├── MainActivity.java    WebView pointing at http://127.0.0.1:3000
        │   └── ServerService.java   foreground service: spawns the server,
        │                            holds the Wi-Fi MulticastLock for mDNS
        └── res/                      icons, theme, strings
```

## Just want to try it on a phone? Use the prebuilt APK

The source project includes **`single-v0.1.3.apk`** (next to `build.bat`),
and `build.bat` copies it into the `Single\` folder on every run. No Android
SDK, no Android Studio, no gradle needed. It is signed with the project's
**stable keystore** (`android/single.keystore`), so every future build
installs right over it:

1. Copy `single-v0.1.3.apk` to the phone (USB cable, Drive, e-mail…).
2. Tap it in the Files app and allow "install unknown apps" when asked.
3. Open **Single**, keep the phone on the same Wi-Fi as your PC.

**Updating from v0.1.1/v0.1.2 or any earlier debug-signed build?** Those were
signed with a throwaway debug key, so Android refuses the direct update —
uninstall the old app **once**, then install v0.1.3. From v0.1.3 onwards,
updates install over each other without uninstalling (same stable key).

## What works on the phone

- Full dashboard UI (identical multi-page app — Home / Devices / Chats /
  Activity / Settings) with a **mobile layout**: top bar with a green live
  dot, bottom tab bar, safe-area aware, nothing hidden under the bars.
- **Reopen-safe**: the wrapper probes the local server and only loads the
  dashboard once it answers — the old "net::ERR_CONNECTION_REFUSED after
  closing and reopening" bug is gone (details below).
- mDNS discovery with other Single instances on the same Wi-Fi (the service
  holds a `MulticastLock`, which Android requires for multicast).
- P2P chat + file transfer. Files sent to the phone land in
  `Android/data/app.single.bridge/files/received/` (visible with any file
  manager or via a PC USB connection).
- Server output is mirrored to logcat: `adb logcat -s Single`.

## How the v0.1.3 wrapper fixed the reopen bug

Three defenses stack up in `MainActivity` / `ServerService`:

1. **Probe, then load.** The WebView first shows an inline splash; a
   background thread TCP-probes `127.0.0.1:3000` and only loads the URL when
   the server actually accepts connections. If the server dies mid-session,
   the main-frame error handler swaps back to the splash and re-probes — the
   Chromium error page is never the final state.
2. **Respawn on early exit.** A watcher restarts the server process with
   backoff if it dies within 15 s of spawning — the classic quick
   close→reopen race was the new server losing the port race against the old
   one's teardown ("address already in use" → exit → nothing listening).
3. **Clean teardown.** Stopping the service SIGTERMs the server, waits 2 s,
   then escalates to a hard kill, so the port is always free for the next
   launch. The service is `START_NOT_STICKY` (the activity owns its
   lifecycle), which also stops zombie restarts from the background.

The Rust core gained matching hardening for future from-source builds: both
listeners retry binding for up to 10 s, and SIGTERM now triggers the graceful
shutdown (mDNS goodbye) instead of being ignored.

## How build.bat makes the APK

1. **Prebuilt fast path** — `single-v<version>.apk` exists next to
   `build.bat` (it ships with the source zip): it is simply copied into the
   `Single\` folder. Nothing else runs.
2. **From-source path** (only when the prebuilt file is missing):
   `cargo build --release --target aarch64-linux-android` (+ `x86_64` for
   emulators) linked with the NDK, the binary lands in
   `app/src/main/jniLibs/<abi>/libsingle.so`, and `gradle assembleDebug`
   packages the APK. Android extracts `lib*.so` files to the app's
   `nativeLibraryDir` and marks them executable, which is how the server
   travels inside the APK.

Either way the APK is signed with the stable project keystore
**`android/single.keystore`** (store/key password `single-app`, alias
`single`, committed on purpose — this is a self-distributed app, and the
stable key is what lets each new version install right over the previous
one). Never delete or regenerate that file: Android refuses cross-signature
updates.

The dashboard UI ships inside the APK as `assets/web/*`; the service copies
it to the app's data folder on every start and serves it via `--static-dir`.
UI changes therefore don't need a Rust rebuild — but they do need a new APK
(scripts/build flow), or a from-source build.

## Building the APK from source yourself

Anything that can produce the gradle debug build works:

- **Android Studio** — open the `android/` folder, let it install SDK 34 +
  NDK, press Run (or `gradlew assembleDebug` from `android/`).
- **Command line, no Studio** — install a JDK 17, then:
  ```
  # one-time: SDK command-line tools -> sdkmanager "platforms;android-34"
  #           "build-tools;34.0.0" "ndk;27.2.12479018" "platform-tools"
  set ANDROID_HOME=%LOCALAPPDATA%\Android\Sdk
  rustup target add aarch64-linux-android x86_64-linux-android
  build.bat
  ```

Version note: the prebuilt APK always matches the version of the source zip
it ships with. If you bump `version` in `Cargo.toml`, delete the old
`single-v<old>.apk` so build.bat rebuilds fresh (it never mixes versions).

## Notes & limits

- **Both devices must be on the same Wi-Fi network.** Carrier data, guest
  networks and "AP/client isolation" hotspots block peer discovery.
- Some Android builds ship a system mDNS responder that can keep UDP 5353
  busy; if the phone cannot see or announce devices, reboot the phone once or
  switch Wi-Fi — the dashboard and outbound connections keep working.
- The app keeps a low-priority "Single is running" notification while the
  server is alive. Closing the app (Back out of it) stops the server
  cleanly; reopening starts it again and the dashboard waits for it via the
  probe loop — no more connection-refused error page.
- A self-signed APK shows the usual Play-Protect warning on first install —
  expected for self-built apps; the wrapper uses only framework APIs and
  bundles no third-party code.
