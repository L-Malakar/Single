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
rem         Single-v0.1.1.exe        the app - double-click to run
rem         Launch Single.bat        friendly starter
rem         enable-firewall.bat      one-time LAN visibility fix (admin)
rem         README.txt               end-user notes
rem         received\                incoming files land here
rem         tools\Device-Simulator.exe   optional 5-fake-device demo
rem    4. zips the folder to Single-v0.1.1.zip for sharing
rem
rem  REQUIREMENTS
rem    Rust installed on Windows  -  https://rustup.rs  (rustup default stable)
rem
rem  The version number is read from Cargo.toml automatically: bump the
rem  version there, re-run this script, done.
rem ===========================================================================

rem ---- configuration --------------------------------------------------------
set "APP_NAME=Single"
set "APP_VERSION=0.1.1"

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
echo(%APP_VERSION%|findstr /r "^[0-9]" >nul || set "APP_VERSION=0.1.1"

set "SERVER_BIN=p2p-bridge-server"
set "DIST=%APP_NAME%"
set "EXE_NAME=%APP_NAME%-v%APP_VERSION%.exe"
set "ZIP_NAME=%APP_NAME%-v%APP_VERSION%.zip"

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

rem ---- 1/5 compile -----------------------------------------------------------
echo [1/5] Compiling release binaries - first build can take a few minutes...
cargo build --release --bin %SERVER_BIN% --bin device-simulator
if errorlevel 1 goto :fail

rem ---- 2/5 fresh app folder ---------------------------------------------------
echo [2/5] Creating the %DIST% folder ...
if exist "%DIST%\" rmdir /s /q "%DIST%"
if exist "%ZIP_NAME%" del /f /q "%ZIP_NAME%"
mkdir "%DIST%" "%DIST%\tools" "%DIST%\received"
if errorlevel 1 goto :fail

rem ---- 3/5 the app ------------------------------------------------------------
echo [3/5] Copying %EXE_NAME% ...
if not exist "target\release\%SERVER_BIN%.exe" (
    echo [ERROR] target\release\%SERVER_BIN%.exe missing - did the build fail?
    goto :fail
)
copy /y "target\release\%SERVER_BIN%.exe"     "%DIST%\%EXE_NAME%" >nul
copy /y "target\release\device-simulator.exe" "%DIST%\tools\Device-Simulator.exe" >nul
if not exist "%DIST%\%EXE_NAME%" goto :fail
if not exist "%DIST%\tools\Device-Simulator.exe" goto :fail

rem ---- 4/5 helper files -------------------------------------------------------
echo [4/5] Writing launcher, firewall helper and README ...

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
>> "%DIST%\README.txt" echo   Phones: open http://YOUR-PC-IP:3000 in the mobile browser.
>> "%DIST%\README.txt" echo   YOUR-PC-IP is shown in the console window, e.g. 192.168.1.42
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
>> "%DIST%\README.txt" echo %APP_NAME% v%APP_VERSION% - MIT license. Built from the p2p-bridge Rust project.

rem ---- 5/5 zip for sharing -----------------------------------------------------
echo [5/5] Zipping %ZIP_NAME% ...
powershell -NoProfile -ExecutionPolicy Bypass -Command "Compress-Archive -Path '%DIST%' -DestinationPath '%ZIP_NAME%' -Force" >nul 2>nul
if errorlevel 1 (
    echo        NOTE: zip step skipped - the %DIST% folder itself is complete.
)

rem ---- summary -------------------------------------------------------------------
echo.
echo ======================================================================
echo   BUILD OK - %APP_NAME% v%APP_VERSION%
echo ======================================================================
echo.
for %%F in ("%DIST%\%EXE_NAME%") do echo   app    : %%~fF   [%%~zF bytes]
if exist "%ZIP_NAME%" for %%F in ("%ZIP_NAME%") do echo   zip    : %%~fF   [%%~zF bytes]
for %%F in ("%DIST%") do echo   folder : %%~fF
echo.
echo   RUN IT : double-click   %DIST%\%EXE_NAME%
echo             the app window opens by itself - no localhost address needed.
echo.
goto :done

:fail
echo.
echo *** BUILD FAILED - see the messages above. ***
echo.
echo Press any key to close this window ...
pause >nul
exit /b 1

:done
echo Press any key to close this window ...
pause >nul
exit /b 0

