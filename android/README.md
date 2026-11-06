# Single for Android

The same Rust server (mDNS discovery + dashboard + P2P chat/files), packaged
as a real installable APK. The wrapper app is intentionally tiny:

```
android/
├── settings.gradle / build.gradle / gradle.properties
├── single.keystore                 ONE stable signing key (see below)
├── gradle/wrapper/gradle-wrapper.properties
└── app/
    ├── build.gradle                 no external dependencies — pure SDK APIs
    └── src/main/
        ├── AndroidManifest.xml      permissions: internet, multicast, FGS
        ├── java/app/single/bridge/
        │   ├── MainActivity.java    WebView pointing at http://127.0.0.1:3000,
        │   │                        splash + wait-for-server startup
        │   └── ServerService.java   foreground service: spawns the server,
        │                            holds the Wi-Fi MulticastLock for mDNS
        └── res/                      icons, day/night splash theme, strings
```

## Just want to try it on a phone? Use the prebuilt APK

The source project includes **`single-v0.1.3.apk`** (next to `build.bat`),
and `build.bat` copies it into the `Single\` folder on every run. No Android
SDK, no Android Studio, no gradle needed:

1. Copy `single-v0.1.3.apk` to the phone (USB cable, Drive, e-mail…).
2. Tap it in the Files app and allow "install unknown apps" when asked.
3. Open **Single**, keep the phone on the same Wi-Fi as your PC.

## Updates & signing (important)

Every build of this project — the prebuilt APK, `build.bat`, Android Studio,
`gradle assembleDebug/Release` — signs with the **same stable key**:
`android/single.keystore` (store password `single-bridge`, alias `single`).

- A newer APK installs directly **over** an older one — no uninstall, your
  data (chats, name, protocol) survives in `single-state.json`.
- `versionCode` is derived from the version string (0.1.3 → 103), so it is
  always monotonic.
- The key is committed with the source **on purpose**: this is a self-built
  LAN app, not a Play Store release — one shared key is what makes updates
  work for everyone who builds it. If you ever publish to a store, replace
  it with your own private key (and uninstall-first, once, on every device).
- One-time exception: an install made from a *different* key — e.g. an APK
  built before v0.1.3 — cannot be updated in place. Uninstall the old app
  once, install this build, and every future APK updates normally.

## What works on the phone

- Full dashboard UI (identical multi-page app — Home / Devices / Chats /
  Activity / Settings).
- mDNS discovery with other Single instances on the same Wi-Fi (the service
  holds a `MulticastLock`, which Android requires for multicast).
- P2P chat + file transfer. Files sent to the phone land in
  `Android/data/app.single.bridge/files/received/` (visible with any file
  manager or via a PC USB connection).
- Server output is mirrored to logcat: `adb logcat -s Single`.

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
The APK files are `.gitignored` (`single-v*.apk`) — they ship in the source
zip, but are never pushed to the git repo.

## Notes & limits

- **Both devices must be on the same Wi-Fi network.** Carrier data, guest
  networks and "AP/client isolation" hotspots block peer discovery.
- Some Android builds ship a system mDNS responder that can keep UDP 5353
  busy; if the phone cannot see or announce devices, reboot the phone once or
  switch Wi-Fi — the dashboard and outbound connections keep working.
- The app keeps a low-priority "Single is running" notification while the
  server is alive. Swiping the app away stops the server (graceful mDNS
  goodbye).
- A debug-signed APK shows the usual Play-Protect warning on first install —
  expected for self-built apps; the wrapper uses only framework APIs and
  bundles no third-party code.
