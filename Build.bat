@echo off
rem ============================================================
rem  Single build script  (Windows)
rem  Produces:
rem    Single\Windows\Single.exe
rem    Single\Windows\Single-Setup-v0.1.5.exe   (Inno Setup, or NSIS fallback)
rem    Single\Windows\Single-v0.1.5.zip         (portable)
rem    Single\Android\Single-v0.1.5.apk         (needs Android Studio + Node.js)
rem ============================================================
setlocal enabledelayedexpansion
cd /d "%~dp0"
set VERSION=0.1.5
set OUT=Single
set TAURI_ARGS=

echo.
echo  ==============================================
echo   Single v%VERSION% - build started
echo  ==============================================
echo.

rem ------------------------------------------------ check Rust
where cargo >nul 2>&1
if errorlevel 1 (
    echo [ERROR] Rust was not found. Install it from https://rustup.rs
    echo         then open a NEW terminal and run Build.bat again.
    goto :fail
)

rem ------------------------------------------------ protect low-RAM machines
rem      Compiling big crates (windows, tauri, tokio) peaks at several GB
rem      per rustc process. Running many at once exhausts RAM and kills the
rem      build with: memory allocation of N bytes failed. Cap the number of
rem      parallel compile jobs to what the machine can take. Preset the
rem      CARGO_BUILD_JOBS environment variable to override this.
if defined CARGO_BUILD_JOBS (
    echo        Compile jobs: %CARGO_BUILD_JOBS% ^(preset by user^)
    goto :jobs_done
)
set "RAMGB="
for /f "usebackq delims=" %%R in (`powershell -NoProfile -Command "[int][math]::Round((Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory/1GB)"`) do set "RAMGB=%%R"
if not defined RAMGB goto :jobs_done
if !RAMGB! GEQ 16 goto :jobs_done
if !RAMGB! GEQ 8 (
    set "CARGO_BUILD_JOBS=2"
    echo        RAM: !RAMGB! GB - capping compile jobs at 2
    goto :jobs_done
)
set "CARGO_BUILD_JOBS=1"
echo        RAM: !RAMGB! GB - capping compile jobs at 1
:jobs_done

rem ------------------------------------------------ resolve Tauri CLI (node or cargo)
set TAURI_CMD=cargo
where node >nul 2>&1
if not errorlevel 1 set TAURI_CMD=npx
echo [1/6] Tauri CLI: %TAURI_CMD%
if "%TAURI_CMD%"=="npx" (
    call npx --yes @tauri-apps/cli@2 --version >nul 2>&1
    if errorlevel 1 (
        echo [WARN] npx could not fetch the Tauri CLI, falling back to cargo.
        set TAURI_CMD=cargo
    )
)
if "%TAURI_CMD%"=="cargo" (
    cargo tauri --version >nul 2>&1
    if errorlevel 1 (
        echo        Installing tauri-cli via cargo - this can take a few minutes...
        cargo install tauri-cli --version "^2" --locked
        if errorlevel 1 goto :fail
    )
)

rem ------------------------------------------------ local Tauri CLI for Gradle's callback
rem      The generated Android Gradle project calls the Tauri CLI back with
rem      "npm run tauri ..." (src-tauri\gen\android\buildSrc\...\BuildTask.kt).
rem      That needs the root package.json "tauri" script plus the CLI inside
rem      node_modules - "npm install" provides both (also makes npx use the
rem      pinned local CLI instead of re-fetching).
if "%TAURI_CMD%"=="npx" (
    if not exist "node_modules\.bin\tauri.cmd" (
        echo        Installing the Tauri CLI locally ^(npm install^)...
        call npm install --no-audit --no-fund
        if errorlevel 1 (
            echo [WARN] npm install failed - see the Android step notes below.
        )
    )
)

rem ------------------------------------------------ icons from logo.svg
echo [2/6] Refreshing app icons from logo.svg...
copy /Y logo.svg ui\assets\logo.svg >nul 2>&1
call :tauri icon logo.svg
if errorlevel 1 echo [WARN] tauri icon failed - keeping the bundled icons.

rem ------------------------------------------------ Windows build
echo [3/6] Building Windows binary ^(this is the long step^)...
call :tauri build --no-bundle
if errorlevel 1 goto :fail

if not exist "src-tauri\target\release\Single.exe" (
    echo [ERROR] Build finished but Single.exe is missing.
    goto :fail
)

if not exist "%OUT%\Windows" mkdir "%OUT%\Windows"
copy /Y "src-tauri\target\release\Single.exe" "%OUT%\Windows\Single.exe" >nul
echo        Copied Single.exe -^> %OUT%\Windows\

rem ------------------------------------------------ installer: Inno Setup (preferred) or NSIS fallback
set "ISCC="
where iscc >nul 2>&1 && set "ISCC=iscc"
if not defined ISCC (
    if exist "%ProgramFiles(x86)%\Inno Setup 6\ISCC.exe" set "ISCC=%ProgramFiles(x86)%\Inno Setup 6\ISCC.exe"
)
if not defined ISCC (
    if exist "%ProgramFiles%\Inno Setup 6\ISCC.exe" set "ISCC=%ProgramFiles%\Inno Setup 6\ISCC.exe"
)
echo [4/6] Building Single-Setup-v%VERSION%.exe...
if defined ISCC (
    echo        Using Inno Setup: %ISCC%
    "%ISCC%" /DMyAppVersion=%VERSION% installer\single.iss
    if errorlevel 1 goto :fail
) else (
    echo        Inno Setup 6 not found - using the built-in NSIS bundler instead.
    echo        ^(Install Inno Setup from https://jrsoftware.org/isinfo.php for the richer installer.^)
    call :tauri build --bundles nsis
    if errorlevel 1 goto :fail
    copy /Y "src-tauri\target\release\bundle\nsis\Single_%VERSION%_x64-setup.exe" "%OUT%\Windows\Single-Setup-v%VERSION%.exe" >nul
)
if exist "%OUT%\Windows\Single-Setup-v%VERSION%.exe" (
    echo        Copied Single-Setup-v%VERSION%.exe -^> %OUT%\Windows\
) else (
    echo [WARN] Setup exe was not produced.
)

rem ------------------------------------------------ portable zip
echo        Packing portable Single-v%VERSION%.zip...
powershell -NoProfile -Command "Compress-Archive -Force -Path 'src-tauri\target\release\Single.exe','LICENSE' -DestinationPath '%OUT%\Windows\Single-v%VERSION%.zip'"
if exist "%OUT%\Windows\Single-v%VERSION%.zip" echo        Copied Single-v%VERSION%.zip -^> %OUT%\Windows\

rem ------------------------------------------------ Android build
echo [5/6] Android...
set "SDK="
if defined ANDROID_HOME if exist "%ANDROID_HOME%\platform-tools" set "SDK=%ANDROID_HOME%"
if not defined SDK if defined ANDROID_SDK_ROOT if exist "%ANDROID_SDK_ROOT%\platform-tools" set "SDK=%ANDROID_SDK_ROOT%"
if not defined SDK if exist "%LOCALAPPDATA%\Android\Sdk\platform-tools" set "SDK=%LOCALAPPDATA%\Android\Sdk"

if defined SDK (
    echo        Android SDK: %SDK%
    if not defined JAVA_HOME (
        if exist "%ProgramFiles%\Android\Android Studio\jbr" set "JAVA_HOME=%ProgramFiles%\Android\Android Studio\jbr"
    )
    if defined JAVA_HOME echo        JAVA_HOME: !JAVA_HOME!

    set "NDK_DIR="
    for /d %%D in ("%SDK%\ndk\*") do set "NDK_DIR=%%D"
    if defined NDK_DIR (
        echo        NDK: !NDK_DIR!

        rem ---- If the Android project was created via npx, the generated
        rem      Gradle build runs the Tauri CLI through npm, so Node.js
        rem      must be on PATH or Gradle cannot start npm.bat.
        set "BUILDTASK=src-tauri\gen\android\buildSrc\src\main\kotlin\BuildTask.kt"
        if exist "!BUILDTASK!" (
            findstr /i "npm" "!BUILDTASK!" >nul 2>&1
            if not errorlevel 1 (
                where npm >nul 2>&1
                if errorlevel 1 (
                    echo [WARN] npm is not on PATH - skipping the APK.
                    echo        This Gradle project invokes the Tauri CLI through npm,
                    echo        so the Android build needs Node.js installed.
                    echo        Fix: install the LTS from https://nodejs.org, open a
                    echo        NEW terminal, run Build.bat again.
                    goto :skip_android
                )
            )
        )

        rem ---- The Gradle callback runs "npm run tauri" from the project
        rem      root, which resolves the CLI from node_modules. If the
        rem      install above failed, skip the APK with a clear message
        rem      instead of dying on "npm error Missing script: tauri".
        if "%TAURI_CMD%"=="npx" if not exist "node_modules\.bin\tauri.cmd" (
            echo [WARN] node_modules\.bin\tauri.cmd is missing - skipping the APK.
            echo        The Android build calls "npm run tauri" in the project root,
            echo        which needs the root package.json and a successful
            echo        "npm install". Fix: run "npm install" in this folder
            echo        ^(internet + Node.js LTS required^), then Build.bat again.
            goto :skip_android
        )

        rem ---- Stop stale Gradle daemons: one started before Node.js was
        rem      installed keeps the old PATH and cannot launch npm.bat,
        rem      which surfaces as: A problem occurred starting process.
        if exist "src-tauri\gen\android\gradlew.bat" (
            echo        Stopping stale Gradle daemons...
            pushd "src-tauri\gen\android"
            call gradlew.bat --stop >nul 2>&1
            popd
        )

        rustup target add aarch64-linux-android
        if not exist "src-tauri\gen\android" (
            echo        First run: generating the Android project...
            call :tauri android init
            if errorlevel 1 goto :android_fail
            echo        Refreshing the Android launcher icons from logo.svg...
            call :tauri icon logo.svg >nul 2>&1
        )
        rem ---- Disable Kotlin incremental compilation. The Tauri Android
        rem      library sources live inside the Cargo registry (usually C:\)
        rem      while the project can sit on another drive (e.g. D:\). When
        rem      both are on different drives Kotlin's incremental compiler
        rem      dies with "this and base files have different roots" /
        rem      "Could not close incremental caches" (tauri issue #10876).
        set "GPROPS=src-tauri\gen\android\gradle.properties"
        if exist "!GPROPS!" (
            findstr /i /c:"kotlin.incremental=false" "!GPROPS!" >nul 2>&1
            if errorlevel 1 (
                echo        Disabling Kotlin incremental compilation ^(cross-drive fix^)...
                >>"!GPROPS!" echo.
                >>"!GPROPS!" echo kotlin.incremental=false
            )
            findstr /i /c:"kotlin.compiler.execution.strategy=in-process" "!GPROPS!" >nul 2>&1
            if errorlevel 1 >>"!GPROPS!" echo kotlin.compiler.execution.strategy=in-process
        )

        rem ---- Remove stale Kotlin caches next to the Tauri Android sources
        rem      in the Cargo registry (leftovers from earlier failed builds).
        for /d %%D in ("%USERPROFILE%\.cargo\registry\src\*\tauri-2*") do (
            if exist "%%D\mobile\android\build" rmdir /s /q "%%D\mobile\android\build"
        )

        rem ---- Export the detected SDK/NDK so the Tauri CLI does not have to
        rem      guess ^(silences "ANDROID_HOME not set" and makes Gradle and
        rem      the CLI see exactly the same tools^).
        set "ANDROID_HOME=!SDK!"
        set "ANDROID_SDK_ROOT=!SDK!"
        set "NDK_HOME=!NDK_DIR!"

        echo        Signing release APK with the debug key for easy installs...
        powershell -NoProfile -ExecutionPolicy Bypass -File "scripts\sign-android-release.ps1"
        echo        Building APK ^(aarch64^)...
        call :tauri android build --apk --target aarch64
        if errorlevel 1 goto :android_fail
        if not exist "%OUT%\Android" mkdir "%OUT%\Android"
        set /a APK_COUNT=0
        for /r "src-tauri\gen\android\app\build\outputs" %%F in (app-*.apk) do (
            copy /Y "%%F" "%OUT%\Android\Single-v%VERSION%.apk" >nul
            set /a APK_COUNT+=1
        )
        if !APK_COUNT! GTR 0 (
            echo        Copied Single-v%VERSION%.apk -^> %OUT%\Android\
        ) else (
            echo [WARN] APK output not found - check the build log above.
        )
    ) else (
        echo [WARN] No Android NDK in %SDK%\ndk - skipping the APK.
        echo        Install it with: sdkmanager "ndk;26.3.11579264" ^(or via Android Studio^)
        goto :skip_android
    )
) else (
    echo [WARN] Android SDK not found - skipping the APK.
    echo        Install Android Studio ^(or the command-line tools^) and set ANDROID_HOME.
    goto :skip_android
)

:android_ok
echo.
echo [6/6] Done! Build artifacts:
echo.
tree /f "%OUT%"
echo.
echo   Upload-ready: zip the folder ^(without target\ and gen\^) or push this
echo   repository to https://github.com/L-Malakar/Single
echo.
pause
exit /b 0

:skip_android
echo.
echo [6/6] Done ^(Windows only^). Artifacts:
echo.
tree /f "%OUT%"
echo.
pause
exit /b 0

:android_fail
echo.
echo [WARN] The Android build failed. Everything Windows-side is still fine.
echo.
echo        How to fix the most common causes:
echo.
echo        1. "A problem occurred starting process 'npm.bat'"
echo           Gradle could not launch npm. Fix:
echo             a. Install the Node.js LTS from https://nodejs.org
echo             b. Open a NEW terminal, or reboot, so the new PATH is seen
echo             c. Run Build.bat again - it stops stale Gradle daemons itself
echo.
echo        2. Missing NDK or unaccepted SDK licenses
echo           Open Android Studio ^> SDK Manager, install an NDK and accept
echo           the license agreements, then run Build.bat again.
echo.
echo        3. "npm error Missing script: tauri"
echo           The Gradle callback needs the root package.json "tauri" script
echo           and the CLI inside node_modules. Build.bat runs "npm install"
echo           for you. Fix: check internet + Node.js, run "npm install" in
echo           the project root, verify with "npm run tauri --version",
echo           then run Build.bat again.
echo.
echo        4. Kotlin daemon crash, "different roots", "Could not close
echo           incremental caches"
echo           Your project and the Cargo cache sit on different drives
echo           ^(e.g. project on D:, Cargo on C:^). Build.bat disables Kotlin
echo           incremental compilation automatically. If it still fails, add
echo           "kotlin.incremental=false" to src-tauri\gen\android\gradle.properties
echo           by hand, delete src-tauri\gen\android\app\build and retry.
echo.
echo        5. Still failing
echo           Delete the folder src-tauri\gen\android and run Build.bat again;
echo           the whole Android project is regenerated from scratch.
echo.
pause
exit /b 1

:fail
echo.
echo [ERROR] Build failed. Scroll up for the exact error message.
echo.
echo        If the log shows "memory allocation of ... bytes failed" the PC
echo        ran out of RAM while compiling. Fix:
echo          - close browsers, Android Studio and other apps, then retry
echo          - force single-file compiles:  set CARGO_BUILD_JOBS=1
echo            then run Build.bat again in the SAME terminal
echo.
pause
exit /b 1

rem -------------------------------------------- helper: invoke tauri CLI
:tauri
if "%TAURI_CMD%"=="npx" (
    call npx --yes @tauri-apps/cli@2 %*
) else (
    call cargo tauri %*
)
exit /b %errorlevel%
