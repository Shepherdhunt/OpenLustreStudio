; Inno Setup script for OpenLustre Studio — the standard Windows install
; experience: setup wizard, Start Menu + optional Desktop shortcut, and an
; uninstaller. The shortcuts run `openlustre.exe studio launch`, which starts
; the embedded Studio server and opens the default browser on the user's
; welcome project (created on first run at %USERPROFILE%\OpenLustre).
;
; Build (after `cargo build --release`):
;   ISCC.exe /DAppVersion=0.1.0 packaging\windows\openlustre.iss
; or run packaging\windows\build-installer.ps1 which does both steps.

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif

[Setup]
AppId={{7E1B0A4C-9C1D-4A53-B45E-0F4B8B6F3A21}
AppName=OpenLustre Studio
AppVersion={#AppVersion}
AppPublisher=OpenLustre Studio contributors
AppPublisherURL=https://github.com/openlustre/openlustre-studio
DefaultDirName={autopf}\OpenLustre Studio
DefaultGroupName=OpenLustre Studio
DisableProgramGroupPage=yes
LicenseFile=..\..\LICENSE
OutputDir=dist
OutputBaseFilename=OpenLustreStudio-{#AppVersion}-Setup
Compression=lzma2
SolidCompression=yes
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequiredOverridesAllowed=dialog
UninstallDisplayName=OpenLustre Studio

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; \
  GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "..\..\target\release\openlustre.exe"; DestDir: "{app}"; \
  Flags: ignoreversion
Source: "..\..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
; The shortcut every common Windows app has: double-click -> app opens.
Name: "{group}\OpenLustre Studio"; Filename: "{app}\openlustre.exe"; \
  Parameters: "studio launch"; WorkingDir: "{app}"; \
  Comment: "Graphical Lustre/CoCoSpec modeling IDE"
Name: "{group}\OpenLustre Studio (CLI here)"; Filename: "{cmd}"; \
  Parameters: "/K cd /D ""%USERPROFILE%\OpenLustre"" && ""{app}\openlustre.exe"" --help"; \
  Comment: "Command prompt with the openlustre CLI"
Name: "{autodesktop}\OpenLustre Studio"; Filename: "{app}\openlustre.exe"; \
  Parameters: "studio launch"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\openlustre.exe"; Parameters: "studio launch"; \
  Description: "Launch OpenLustre Studio now"; \
  Flags: nowait postinstall skipifsilent
