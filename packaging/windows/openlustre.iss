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
; Brand the wizard, the Add/Remove Programs entry, and (with ChangesAssociations)
; refresh the shell so associated files pick up the icon immediately.
SetupIconFile=openlustre.ico
UninstallDisplayIcon={app}\openlustre.ico
ChangesAssociations=yes

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
; Documents the optional dependencies (compiler, Kind 2, Docker) and what each
; unlocks; `openlustre doctor` (shortcut below) detects them at runtime.
Source: "..\..\DEPENDENCIES.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "openlustre.ico"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
; The shortcut every common Windows app has: double-click -> app opens.
Name: "{group}\OpenLustre Studio"; Filename: "{app}\openlustre.exe"; \
  Parameters: "studio launch"; WorkingDir: "{app}"; \
  IconFilename: "{app}\openlustre.ico"; \
  Comment: "Graphical Lustre/CoCoSpec modeling IDE"
Name: "{group}\OpenLustre Studio (CLI here)"; Filename: "{cmd}"; \
  Parameters: "/K cd /D ""%USERPROFILE%\OpenLustre"" && ""{app}\openlustre.exe"" --help"; \
  Comment: "Command prompt with the openlustre CLI"
Name: "{group}\Check Environment (dependencies)"; Filename: "{cmd}"; \
  Parameters: "/K ""{app}\openlustre.exe"" doctor"; \
  IconFilename: "{app}\openlustre.ico"; \
  Comment: "Which optional dependencies (compiler, Kind 2, Docker) are installed and what each enables"
Name: "{autodesktop}\OpenLustre Studio"; Filename: "{app}\openlustre.exe"; \
  Parameters: "studio launch"; WorkingDir: "{app}"; \
  IconFilename: "{app}\openlustre.ico"; Tasks: desktopicon

[Registry]
; Associate OpenLustre model files with the Studio: double-click a `.wksc`
; workspace or a `.ols` model and it opens in the Studio (resolve_workspace
; serves a file path directly). `.lus` is intentionally not claimed (it is an
; import-only format shared with other Lustre tooling) and `.json` is too
; generic to hijack. HKA = per-machine on an admin install, per-user otherwise.
; The extension keys delete only our own value on uninstall; the ProgID key is
; removed whole.
Root: HKA; Subkey: "Software\Classes\.wksc"; ValueType: string; ValueName: ""; \
  ValueData: "OpenLustreStudio.Model"; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\.ols"; ValueType: string; ValueName: ""; \
  ValueData: "OpenLustreStudio.Model"; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\OpenLustreStudio.Model"; ValueType: string; \
  ValueName: ""; ValueData: "OpenLustre Studio model"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\OpenLustreStudio.Model\DefaultIcon"; \
  ValueType: string; ValueName: ""; ValueData: "{app}\openlustre.ico,0"
Root: HKA; Subkey: "Software\Classes\OpenLustreStudio.Model\shell\open\command"; \
  ValueType: string; ValueName: ""; \
  ValueData: """{app}\openlustre.exe"" studio launch ""%1"""

[Run]
Filename: "{app}\openlustre.exe"; Parameters: "studio launch"; \
  Description: "Launch OpenLustre Studio now"; \
  Flags: nowait postinstall skipifsilent
