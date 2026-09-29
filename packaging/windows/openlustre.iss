; Inno Setup script for OpenLustre Studio on Windows: a setup wizard that
; installs for the current user (no administrator rights) or for everyone,
; Start Menu shortcuts (the Studio, the PMS sample, a command prompt), an
; optional Desktop shortcut, the `openlustre` command on PATH, and an
; uninstaller that removes all of it. The shortcuts run `openlustre.exe
; studio launch`, which starts the Studio (its log in a console window;
; close it to stop the Studio) and opens the default browser.
;
; Built by packaging\windows\build-installer.ps1, which stages the files:
;   ISCC.exe /DAppVersion=<v> /DStageDir=<staged folder> /DOutputDir=<dir> openlustre.iss

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#ifndef StageDir
  #define StageDir "..\..\dist\stage\openlustre-studio-" + AppVersion + "-windows-x86_64"
#endif
#ifndef OutputDir
  #define OutputDir "..\..\dist"
#endif

[Setup]
AppId={{7E1B0A4C-9C1D-4A53-B45E-0F4B8B6F3A21}
AppName=OpenLustre Studio
AppVersion={#AppVersion}
AppVerName=OpenLustre Studio {#AppVersion}
AppPublisher=OpenLustre Studio contributors
AppPublisherURL=https://github.com/Shepherdhunt/OpenLustreStudio
AppSupportURL=https://github.com/Shepherdhunt/OpenLustreStudio/issues
DefaultDirName={autopf}\OpenLustre Studio
DefaultGroupName=OpenLustre Studio
DisableProgramGroupPage=yes
LicenseFile=..\..\LICENSE
OutputDir={#OutputDir}
OutputBaseFilename=OpenLustreStudio-{#AppVersion}-windows-x86_64-Setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Per-user by default (no administrator rights); the wizard offers "for
; everyone", and /ALLUSERS or /CURRENTUSER choose on the command line.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog commandline
ChangesEnvironment=yes
UninstallDisplayName=OpenLustre Studio
UninstallDisplayIcon={app}\openlustre.exe

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "addtopath"; Description: "Add the openlustre command to PATH"; GroupDescription: "Command line:"
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "{#StageDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\OpenLustre Studio"; Filename: "{app}\openlustre.exe"; Parameters: "studio launch"; \
  WorkingDir: "{app}"; Comment: "Graphical Lustre / CoCoSpec modelling workbench"
Name: "{group}\OpenLustre Studio - PMS sample"; Filename: "{app}\openlustre.exe"; \
  Parameters: "studio launch --sample pms"; WorkingDir: "{app}"; \
  Comment: "Open the Payload Management System sample"
Name: "{group}\OpenLustre command prompt"; Filename: "{cmd}"; \
  Parameters: "/K ""set ""PATH={app};%PATH%"" && openlustre --help"""; WorkingDir: "{%USERPROFILE}"; \
  Comment: "A command prompt with the openlustre command"
Name: "{group}\Read me (Windows)"; Filename: "{app}\README-windows.txt"
Name: "{group}\Uninstall OpenLustre Studio"; Filename: "{uninstallexe}"
Name: "{autodesktop}\OpenLustre Studio"; Filename: "{app}\openlustre.exe"; Parameters: "studio launch"; \
  WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\openlustre.exe"; Parameters: "studio launch --sample pms"; \
  Description: "Open the PMS sample in OpenLustre Studio now"; Flags: nowait postinstall skipifsilent

[Code]
{ The command on PATH: the user's PATH for a per-user install, the system's
  for an install for everyone; removed again on uninstall. }
function EnvRoot: Integer;
begin
  if IsAdminInstallMode then Result := HKEY_LOCAL_MACHINE else Result := HKEY_CURRENT_USER;
end;

function EnvKey: String;
begin
  if IsAdminInstallMode then
    Result := 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment'
  else
    Result := 'Environment';
end;

procedure AddToPath(Dir: String);
var
  Paths: String;
begin
  if not RegQueryStringValue(EnvRoot, EnvKey, 'Path', Paths) then Paths := '';
  if Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Paths) + ';') > 0 then exit;
  if (Paths <> '') and (Paths[Length(Paths)] <> ';') then Paths := Paths + ';';
  RegWriteExpandStringValue(EnvRoot, EnvKey, 'Path', Paths + Dir);
end;

procedure RemoveFromPath(Dir: String);
var
  Paths: String;
  P: Integer;
begin
  if not RegQueryStringValue(EnvRoot, EnvKey, 'Path', Paths) then exit;
  { In ';' + Paths + ';' the match starts at the ';' before the entry, so the
    entry itself starts at P in Paths. }
  P := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Paths) + ';');
  if P = 0 then exit;
  Delete(Paths, P, Length(Dir));
  if (P > 1) and (Paths[P - 1] = ';') then
    Delete(Paths, P - 1, 1)
  else if (P <= Length(Paths)) and (Paths[P] = ';') then
    Delete(Paths, P, 1);
  RegWriteExpandStringValue(EnvRoot, EnvKey, 'Path', Paths);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if (CurStep = ssPostInstall) and WizardIsTaskSelected('addtopath') then
    AddToPath(ExpandConstant('{app}'));
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
    RemoveFromPath(ExpandConstant('{app}'));
end;
