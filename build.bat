@echo off
setlocal EnableExtensions
title Single - build ^& package

rem ===========================================================================
rem  build.bat  -  builds Single and packages it as a double-clickable app
rem
rem  WHAT IT DOES
rem    1. compiles the release binaries with cargo
rem    2. creates a fresh folder  Single\
rem    3. fills it with everything the app needs:
rem         Single-v0.1.3.exe        the app - double-click to run
rem         Launch Single.bat        friendly starter
rem         enable-firewall.bat      one-time LAN visibility fix (admin)
rem         README.txt               end-user notes
rem         received\                incoming files land here
rem         tools\Device-Simulator.exe   optional 5-fake-device demo
rem    4. zips the folder to Single-v0.1.3.zip for sharing
rem    5. puts the Android app next to the folder:
rem         single-v0.1.3.apk  install on a phone and test for real
rem       The prebuilt APK that ships with the source zip is used as-is;
rem       when it is missing, a fresh one is built from source.
rem       NOTE: the APK is .gitignored - git never pushes the binary itself.
rem
rem  REQUIREMENTS
rem    Rust installed on Windows  -  https://rustup.rs  (rustup default stable)
rem    Android APK: none - a ready-made APK ships with the source project.
rem    To rebuild it from source: Android Studio (SDK platform 34 + NDK),
rem    or the command-line setup in android\README.md.
rem
rem  The version number is read from Cargo.toml automatically: bump the
rem  version there, re-run this script, done.
rem ===========================================================================

rem ---- configuration --------------------------------------------------------
set "APP_NAME=Single"
set "APP_VERSION=0.1.3"

rem ---- always run from the folder this script lives in ----------------------
cd /d "%~dp0"
if not exist "Cargo.toml" (
    echo [ERROR] Cargo.toml not found next to build.bat.
    echo         Put build.bat in the p2p-bridge project root and retry.
    goto :fail
)

rem ---- keep APP_VERSION in sync with Cargo.toml -----------------------------
set "TOML_VERSION="
for /f "tokens=2 delims==" %%a in ('findstr /b /c:"version " Cargo.toml') do set "TOML_VERSION=%%a"
if defined TOML_VERSION set TOML_VERSION=%TOML_VERSION: =%
if defined TOML_VERSION set APP_VERSION=%TOML_VERSION:"=%
echo(%APP_VERSION%|findstr /r "^[0-9]" >nul || set "APP_VERSION=0.1.3"

set "SERVER_BIN=p2p-bridge-server"
set "DIST=%APP_NAME%"
set "EXE_NAME=%APP_NAME%-v%APP_VERSION%.exe"
set "ZIP_NAME=%APP_NAME%-v%APP_VERSION%.zip"
set "APK_NAME=single-v%APP_VERSION%.apk"

echo.
echo ======================================================================
echo   Building %APP_NAME% v%APP_VERSION%
echo ======================================================================
echo.

rem ---- toolchain check -------------------------------------------------------
where cargo >nul 2>nul
if errorlevel 1 (
    echo [ERROR] Rust/Cargo was not found in PATH.
    echo         Install it from https://rustup.rs , open a NEW terminal, retry.
    goto :fail
)

rem ---- 1/6 compile -----------------------------------------------------------
echo [1/6] Compiling release binaries - first build can take a few minutes...
cargo build --release --bin %SERVER_BIN% --bin device-simulator
if errorlevel 1 goto :fail

rem ---- 2/6 fresh app folder ---------------------------------------------------
echo [2/6] Creating the %DIST% folder ...
if exist "%DIST%\" rmdir /s /q "%DIST%"
if exist "%ZIP_NAME%" del /f /q "%ZIP_NAME%"
mkdir "%DIST%" "%DIST%\tools" "%DIST%\received"
if errorlevel 1 goto :fail

rem ---- 3/6 the app ------------------------------------------------------------
echo [3/6] Copying %EXE_NAME% ...
if not exist "target\release\%SERVER_BIN%.exe" (
    echo [ERROR] target\release\%SERVER_BIN%.exe missing - did the build fail?
    goto :fail
)
copy /y "target\release\%SERVER_BIN%.exe"     "%DIST%\%EXE_NAME%" >nul
copy /y "target\release\device-simulator.exe" "%DIST%\tools\Device-Simulator.exe" >nul
if not exist "%DIST%\%EXE_NAME%" goto :fail
if not exist "%DIST%\tools\Device-Simulator.exe" goto :fail

rem ---- 4/6 helper files -------------------------------------------------------
echo [4/6] Writing launcher, firewall helper and README ...

rem  -- Launch Single.bat -------------------------------------------------------
>  "%DIST%\Launch %APP_NAME%.bat" echo @echo off
>> "%DIST%\Launch %APP_NAME%.bat" echo rem Double-click start for %APP_NAME%. The console window that opens IS the server.
>> "%DIST%\Launch %APP_NAME%.bat" echo rem The app window appears by itself.
>> "%DIST%\Launch %APP_NAME%.bat" echo cd /d "%%~dp0"
>> "%DIST%\Launch %APP_NAME%.bat" echo start "" "%EXE_NAME%" %%*

rem  -- enable-firewall.bat -----------------------------------------------------
>  "%DIST%\enable-firewall.bat" echo @echo off
>> "%DIST%\enable-firewall.bat" echo rem ONE-TIME setup: lets other devices discover this PC over Wi-Fi.
>> "%DIST%\enable-firewall.bat" echo rem Needs admin. Just double-click and accept the UAC prompt.
>> "%DIST%\enable-firewall.bat" echo net session ^>nul 2^>^&1
>> "%DIST%\enable-firewall.bat" echo if errorlevel 1 ^(
>> "%DIST%\enable-firewall.bat" echo     powershell -NoProfile -Command "Start-Process -FilePath '%%~f0' -Verb RunAs"
>> "%DIST%\enable-firewall.bat" echo     exit /b
>> "%DIST%\enable-firewall.bat" echo ^)
>> "%DIST%\enable-firewall.bat" echo powershell -NoProfile -Command "New-NetFirewallRule -DisplayName '%APP_NAME% mDNS' -Direction Inbound -Protocol UDP -LocalPort 5353 -Action Allow -ErrorAction SilentlyContinue"
>> "%DIST%\enable-firewall.bat" echo powershell -NoProfile -Command "New-NetFirewallRule -DisplayName '%APP_NAME% dashboard' -Direction Inbound -Protocol TCP -LocalPort 3000 -Action Allow -ErrorAction SilentlyContinue"
>> "%DIST%\enable-firewall.bat" echo powershell -NoProfile -Command "New-NetFirewallRule -DisplayName '%APP_NAME% P2P' -Direction Inbound -Protocol TCP -LocalPort 5050 -Action Allow -ErrorAction SilentlyContinue"
>> "%DIST%\enable-firewall.bat" echo echo.
>> "%DIST%\enable-firewall.bat" echo echo Firewall rules added. You can close this window.
>> "%DIST%\enable-firewall.bat" echo pause

rem  -- README.txt ---------------------------------------------------------------
>  "%DIST%\README.txt" echo ==================================================================
>> "%DIST%\README.txt" echo   %APP_NAME% v%APP_VERSION% - LAN control server
>> "%DIST%\README.txt" echo   mDNS auto-discovery - P2P chat and file transfer over Wi-Fi
>> "%DIST%\README.txt" echo ==================================================================
>> "%DIST%\README.txt" echo.
>> "%DIST%\README.txt" echo QUICK START
>> "%DIST%\README.txt" echo   1. Double-click "%EXE_NAME%".
>> "%DIST%\README.txt" echo   2. A console window appears - KEEP IT OPEN, it is the server.
>> "%DIST%\README.txt" echo   3. The %APP_NAME% app window opens by itself after a moment.
>> "%DIST%\README.txt" echo      No browser address to type, nothing else to start.
>> "%DIST%\README.txt" echo   4. Quit: focus the console window and press Ctrl+C, or close it.
>> "%DIST%\README.txt" echo.
>> "%DIST%\README.txt" echo THE APP
>> "%DIST%\README.txt" echo   Devices    nearby devices on this Wi-Fi - click Connect to reach one
>> "%DIST%\README.txt" echo   Chats      conversations, file transfers and latency per device
>> "%DIST%\README.txt" echo   Activity   live server log and stats
>> "%DIST%\README.txt" echo   Settings   your name, theme, network info and firewall help
>> "%DIST%\README.txt" echo.
>> "%DIST%\README.txt" echo FILES IN THIS FOLDER
>> "%DIST%\README.txt" echo   %EXE_NAME%      the app
>> "%DIST%\README.txt" echo   Launch %APP_NAME%.bat      same thing, friendly starter
>> "%DIST%\README.txt" echo   enable-firewall.bat  run ONCE so other devices can find you
>> "%DIST%\README.txt" echo                        needs admin - accept the UAC prompt
>> "%DIST%\README.txt" echo   received\             files sent TO you are saved here
>> "%DIST%\README.txt" echo   tools\Device-Simulator.exe   optional demo: 5 fake devices
>> "%DIST%\README.txt" echo.
>> "%DIST%\README.txt" echo OTHER DEVICES
>> "%DIST%\README.txt" echo   Copy this whole folder to any Windows PC on the same Wi-Fi
>> "%DIST%\README.txt" echo   and run the exe there. Every running %APP_NAME% appears
>> "%DIST%\README.txt" echo   automatically in every dashboard - no pairing, no config.
>> "%DIST%\README.txt" echo.
>> "%DIST%\README.txt" echo   Phones: install single-v%APP_VERSION%.apk from this folder -
>> "%DIST%\README.txt" echo   copy it to the phone and tap it (allow "install unknown
>> "%DIST%\README.txt" echo   apps" when asked) - or open http://YOUR-PC-IP:3000 in the
>> "%DIST%\README.txt" echo   mobile browser.
>> "%DIST%\README.txt" echo.
>> "%DIST%\README.txt" echo IF DEVICES DO NOT DISCOVER EACH OTHER
>> "%DIST%\README.txt" echo   - Run enable-firewall.bat once, as administrator.
>> "%DIST%\README.txt" echo   - Guest Wi-Fi often blocks mDNS - use a phone hotspot instead.
>> "%DIST%\README.txt" echo   - Ports used: UDP 5353, TCP 3000, TCP 5050.
>> "%DIST%\README.txt" echo.
>> "%DIST%\README.txt" echo COMMAND LINE FLAGS - open cmd here for more control
>> "%DIST%\README.txt" echo   %EXE_NAME% --name my-laptop
>> "%DIST%\README.txt" echo   --name name          display name others see, default: PC name
>> "%DIST%\README.txt" echo   --http-port port     dashboard port, default 3000
>> "%DIST%\README.txt" echo   --p2p-port port      peer-to-peer port, default 5050
>> "%DIST%\README.txt" echo   --received-dir dir   where incoming files are saved
>> "%DIST%\README.txt" echo   --no-open            do not auto-open the app window
>> "%DIST%\README.txt" echo   --no-announce        stay hidden from other devices
>> "%DIST%\README.txt" echo   --mdns-loopback      two instances on the same PC
>> "%DIST%\README.txt" echo   --help               full flag list
>> "%DIST%\README.txt" echo.
>> "%DIST%\README.txt" echo %APP_NAME% v%APP_VERSION% - MIT license. See LICENSE in the source project.

rem ---- 5/6 zip for sharing -----------------------------------------------------
echo [5/6] Zipping %ZIP_NAME% ...
powershell -NoProfile -ExecutionPolicy Bypass -Command "Compress-Archive -Path '%DIST%' -DestinationPath '%ZIP_NAME%' -Force" >nul 2>nul
if errorlevel 1 (
    echo        NOTE: zip step skipped - the %DIST% folder itself is complete.
)

rem ---- 6/6 Android APK ----------------------------------------------------------
echo [6/6] Android APK (%APK_NAME%) ...
set "APK_BUILT="

rem  -- fast path: the source project ships a ready-made APK --------------------
if exist "%APK_NAME%" (
    set "APK_BUILT=1"
    echo        using the prebuilt APK that ships with the project - done.
    goto :apk_ready
)

call :build_apk
if defined APK_BUILT (
    echo        done.
) else (
    echo        SKIPPED - no prebuilt %APK_NAME% here and no Android SDK on
    echo        this PC. The Windows app above is NOT affected.
    echo        Tip: the source zip includes %APK_NAME% - keep it next to
    echo        build.bat and this step always succeeds with no SDK at all.
    echo        To build the APK from source: android\README.md
)

:apk_ready
rem  -- always park the APK inside the Single folder so it is easy to find ------
if defined APK_BUILT if exist "%APK_NAME%" copy /y "%APK_NAME%" "%DIST%\" >nul

rem ---- summary -------------------------------------------------------------------
echo.
echo ======================================================================
echo   BUILD OK - %APP_NAME% v%APP_VERSION%
echo ======================================================================
echo.
for %%F in ("%DIST%\%EXE_NAME%") do echo   app    : %%~fF   [%%~zF bytes]
if exist "%ZIP_NAME%" for %%F in ("%ZIP_NAME%") do echo   zip    : %%~fF   [%%~zF bytes]
if defined APK_BUILT for %%F in ("%DIST%\%APK_NAME%") do echo   apk    : %%~fF   [%%~zF bytes]
for %%F in ("%DIST%") do echo   folder : %%~fF
echo.
echo   RUN IT : double-click   %DIST%\%EXE_NAME%
echo             the app window opens by itself - no localhost address needed.
if defined APK_BUILT echo   PHONE  : copy Single\%APK_NAME% to the phone and tap it to install.
echo.
goto :done

rem ===========================================================================
rem  :build_apk  -  cross-compile the server for Android and package the APK.
rem  Returns with APK_BUILT=1 on success; every failure path just returns
rem  (the Windows build is already complete at this point).
rem ===========================================================================
:build_apk

rem -- 1. locate the Android SDK -----------------------------------------------
set "SDK="
if defined ANDROID_HOME if exist "%ANDROID_HOME%" set "SDK=%ANDROID_HOME%"
if not defined SDK if defined ANDROID_SDK_ROOT if exist "%ANDROID_SDK_ROOT%" set "SDK=%ANDROID_SDK_ROOT%"
if not defined SDK if exist "%LOCALAPPDATA%\Android\Sdk" set "SDK=%LOCALAPPDATA%\Android\Sdk"
if not defined SDK (
    echo        [apk] Android SDK not found - looked at ANDROID_HOME,
    echo        ANDROID_SDK_ROOT and %%LOCALAPPDATA%%\Android\Sdk.
    goto :eof
)

rem -- 2. locate the newest installed NDK --------------------------------------
set "NDK_DIR="
for /f "delims=" %%d in ('dir /b /ad /o-n "%SDK%\ndk" 2^>nul') do (
    if not defined NDK_DIR if exist "%SDK%\ndk\%%d\toolchains\llvm\prebuilt\windows-x86_64\bin\aarch64-linux-android24-clang.cmd" set "NDK_DIR=%SDK%\ndk\%%d"
)
if not defined NDK_DIR (
    echo        [apk] No Android NDK under %SDK%\ndk - install it via
    echo        Android Studio ^> SDK Manager ^> SDK Tools ^> NDK ^(side by side^).
    goto :eof
)

rem -- 3. make sure the rust android targets are installed ----------------------
echo        [apk] checking rust targets ...
rustup target list --installed 2>nul | findstr /c:"aarch64-linux-android" >nul
if errorlevel 1 rustup target add aarch64-linux-android
if errorlevel 1 goto :eof
rustup target list --installed 2>nul | findstr /c:"x86_64-linux-android" >nul
if errorlevel 1 rustup target add x86_64-linux-android
if errorlevel 1 goto :eof

rem -- 4. cross-compile the server for arm64 (phones) + x86_64 (emulators) ------
set "LDR=%NDK_DIR%\toolchains\llvm\prebuilt\windows-x86_64\bin"
set "CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=%LDR%\aarch64-linux-android24-clang.cmd"
set "CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER=%LDR%\x86_64-linux-android24-clang.cmd"
echo        [apk] cargo build for aarch64-linux-android ...
cargo build --release --target aarch64-linux-android --bin %SERVER_BIN%
if errorlevel 1 goto :apk_failed
echo        [apk] cargo build for x86_64-linux-android ...
cargo build --release --target x86_64-linux-android --bin %SERVER_BIN%
if errorlevel 1 goto :apk_failed

if not exist "android\app\src\main\jniLibs\arm64-v8a" mkdir "android\app\src\main\jniLibs\arm64-v8a"
if not exist "android\app\src\main\jniLibs\x86_64"   mkdir "android\app\src\main\jniLibs\x86_64"
rem NOTE: android cross-builds have NO .exe extension, even on Windows.
copy /y "target\aarch64-linux-android\release\%SERVER_BIN%" "android\app\src\main\jniLibs\arm64-v8a\libsingle.so" >nul
copy /y "target\x86_64-linux-android\release\%SERVER_BIN%"   "android\app\src\main\jniLibs\x86_64\libsingle.so" >nul
if not exist "android\app\src\main\jniLibs\arm64-v8a\libsingle.so" goto :apk_failed

rem -- 5. run gradle to package the APK -----------------------------------------
set "GRADLE_CMD="
if exist "android\gradlew.bat" set "GRADLE_CMD=android\gradlew.bat"
if not defined GRADLE_CMD (
    where gradle >nul 2>nul
    if not errorlevel 1 set "GRADLE_CMD=gradle"
)
if not defined GRADLE_CMD (
    echo        [apk] gradle was not found. Either add it to PATH or open the
    echo        android\ folder in Android Studio and press Run.
    goto :eof
)
if defined APK_BUILT set "APK_BUILT="
echo        [apk] gradle assembleDebug - first run downloads gradle plugins ...
call %GRADLE_CMD% -p android -PsingleVersion=%APP_VERSION% assembleDebug --no-daemon
if errorlevel 1 goto :apk_failed
if not exist "android\app\build\outputs\apk\debug\app-debug.apk" goto :apk_failed

copy /y "android\app\build\outputs\apk\debug\app-debug.apk" "%APK_NAME%" >nul
if not exist "%APK_NAME%" goto :apk_failed
set "APK_BUILT=1"
goto :eof

:apk_failed
echo.
echo        *** ANDROID BUILD FAILED - messages above. Windows build is fine. ***
echo.
goto :eof

:done
echo Press any key to close this window ...
pause >nul
exit /b 0

:fail
echo.
echo *** BUILD FAILED - see the messages above. ***
echo.
echo Press any key to close this window ...
pause >nul
exit /b 1
