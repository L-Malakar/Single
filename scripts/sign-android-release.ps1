# Signs the release APK with the debug keystore so it installs directly
# on phones without a release signing setup. Called by Build.bat after
# `tauri android init`.
$ErrorActionPreference = "Stop"
$p = Join-Path $PSScriptRoot "..\src-tauri\gen\android\app\build.gradle.kts"
if (-not (Test-Path $p)) {
    Write-Host "[sign] build.gradle.kts not found - was 'tauri android init' run?"
    exit 0
}
$c = Get-Content $p -Raw
if ($c -match 'signingConfig\s*=\s*signingConfigs\.getByName\("debug"\)') {
    Write-Host "[sign] already patched"
    exit 0
}
# insert into the release build type
$patched = $c -replace '(?m)(getByName\("release"\)\s*\{)', ('$1' + "`r`n            signingConfig = signingConfigs.getByName(`"debug`")")
Set-Content -Path $p -Value $patched -Encoding UTF8
Write-Host "[sign] release APK now signed with the debug keystore"
