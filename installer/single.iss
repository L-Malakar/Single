; Inno Setup script for Single
; Built by Build.bat -> Single\Windows\Single-Setup-v<version>.exe
;
; The installer:
;   * installs Single.exe into Program Files
;   * opens the Windows firewall for Single (inbound TCP, app-scoped)
;   * starts the WebClient service (needed for "net use" drive mapping)
;   * lifts the 50 MB WebDAV per-file cap to 4 GB
; Per-user things (pairings, the device certificate, drive letters) live in
; the app system folder ~/.lmalakar/Single and are NOT touched on uninstall.

#define MyAppName "Single"
#ifndef MyAppVersion
#define MyAppVersion "0.1.5"
#endif
#define MyAppPublisher "L. Malakar"
#define MyAppURL "https://github.com/L-Malakar/Single"
#define MyAppExeName "Single.exe"

[Setup]
AppId={{7021444F-96BB-466F-84C3-9664A4F1FD29}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}/issues
AppUpdatesURL={#MyAppURL}
DefaultDirName={autopf}\{#MyAppName}
DisableProgramGroupPage=yes
LicenseFile=..\LICENSE
OutputDir=..\Single\Windows
OutputBaseFilename=Single-Setup-v{#MyAppVersion}
SetupIconFile=..\src-tauri\icons\icon.ico
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesInstallIn64BitMode=x64compatible
ArchitecturesAllowed=x64compatible
PrivilegesRequired=admin
UninstallDisplayIcon={app}\{#MyAppExeName}
CloseApplications=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; \
    GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\src-tauri\target\release\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#MyAppName}}"; \
    Flags: nowait postinstall skipifsilent runasoriginaluser

; allow incoming connections to Single on any port it listens on
Filename: "netsh"; Parameters: "advfirewall firewall delete rule name=""Single (file sharing)"""; Flags: runhidden
Filename: "netsh"; Parameters: "advfirewall firewall add rule name=""Single (file sharing)"" dir=in action=allow program=""{app}\{#MyAppExeName}"" protocol=TCP"; Flags: runhidden

; WebDAV redirector requirements for mapping drives in "This PC"
Filename: "sc"; Parameters: "config WebClient start= demand"; Flags: runhidden
Filename: "net"; Parameters: "start WebClient"; Flags: runhidden
Filename: "reg"; Parameters: "add HKLM\SYSTEM\CurrentControlSet\Services\WebClient\Parameters /v FileSizeLimitInBytes /t REG_DWORD /d 4294967295 /f"; Flags: runhidden

[UninstallRun]
Filename: "netsh"; Parameters: "advfirewall firewall delete rule name=""Single (file sharing)"""; Flags: runhidden; RunOnceId: "DelFirewall"
