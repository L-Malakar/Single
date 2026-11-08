@echo off
rem ============================================================
rem  Single build script  (Windows)
rem  Produces:
rem    Single\Windows\Single.exe
rem    Single\Windows\Single-Setup-v0.1.0.exe   (Inno Setup, or NSIS fallback)
rem    Single\Windows\Single-v0.1.0.zip         (portable)
rem    Single\Android\Single-v0.1.0.apk         (if Android SDK is installed)
rem ============================================================
setlocal enabledelayedexpansion
cd /d "%~dp0"
set VERSION=0.1.0
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

rem ------------------------------------------------ icons from logo.svg
echo [2/6] Refreshing app icons from logo.svg...
copy /Y logo.svg ui\assets\logo.svg >nul 2>&1
call :tauri icon logo.svg
if errorlevel 1 echo [WARN] tauri icon failed - keeping the bundled placeholder icons.

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
    if defined JAVA_HOME echo        JAVA_HOME: %JAVA_HOME%

    set "NDK_DIR="
    for /d %%D in ("%SDK%\ndk\*") do set "NDK_DIR=%%D"
    if defined NDK_DIR (
        echo        NDK: %NDK_DIR%
        rustup target add aarch64-linux-android
        if not exist "src-tauri\gen\android" (
            echo        First run: generating the Android project...
            call :tauri android init
            if errorlevel 1 goto :android_fail
        )
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
echo        Scroll up for the exact error ^(usually a missing NDK or SDK license^).
echo.
pause
exit /b 1

:fail
echo.
echo [ERROR] Build failed. Scroll up for the exact error message.
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
