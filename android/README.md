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
        ├── jniLibs/<abi>/libsingle.so   ← build.bat copies the cross-compiled
        │                                  Rust server here before gradle runs
        └── res/                      icons, theme, strings
```

## How a build works (all automatic via `..\build.bat`)

1. `cargo build --release --target aarch64-linux-android` (and `x86_64` for
   emulators) — linked with the NDK.
2. The produced binary is copied to `app/src/main/jniLibs/<abi>/libsingle.so`.
   Android extracts `lib*.so` files to the app's `nativeLibraryDir` and marks
   them executable, which is how the server travels inside the APK.
3. `gradle assembleDebug` packages the APK; build.bat copies it to the
   project root as `single-v<version>.apk`.

## Install on a phone

- USB: enable USB debugging → `adb install single-v0.1.1.apk`
- No computer: copy the APK to the phone (Drive / cable / e-mail), tap it in
  the Files app, allow "install unknown apps" when asked.

## What works on the phone

- Full dashboard UI (identical multi-page app — Home / Devices / Chats /
  Activity / Settings).
- mDNS discovery with other Single instances on the same Wi-Fi (the service
  holds a `MulticastLock`, which Android requires for multicast).
- P2P chat + file transfer. Files sent to the phone land in
  `Android/data/app.single.bridge/files/received/` (visible with any file
  manager or via a PC USB connection).
- Server output is mirrored to logcat: `adb logcat -s Single`.

## Notes & limits

- **Both devices must be on the same Wi-Fi network.** Carrier data, guest
  networks and "AP/client isolation" hotspots block peer discovery.
- Some Android builds ship a system mDNS responder that can keep UDP 5353
  busy; if the phone cannot see or announce devices, reboot the phone once or
  switch Wi-Fi — the dashboard and outbound connections keep working.
- The app keeps a low-priority "Single is running" notification while the
  server is alive. Swiping the app away stops the server (graceful mDNS
  goodbye).
- Building the APK locally needs: Rust (`rustup target add
  aarch64-linux-android x86_64-linux-android` happens automatically), the
  **Android SDK** (platform 34) and an **NDK** — i.e. a stock Android Studio
  install. Gradle itself must be on PATH (or use Android Studio: open the
  `android/` folder and press Run).
