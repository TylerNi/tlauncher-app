[Setup]
AppId=Tlauncher
AppName=Tlauncher
AppVerName=Tlauncher
DefaultDirName={autopf}\Tlauncher
PrivilegesRequired=lowest
DisableDirPage=yes
DisableProgramGroupPage=yes
DisableReadyPage=yes
DisableFinishedPage=yes
OutputDir=.
OutputBaseFilename=TlauncherSetup
SetupIconFile=assets\icon.ico
UninstallDisplayName=Tlauncher
UninstallDisplayIcon={app}\tlauncher.exe
WizardStyle=modern

[Languages]
Name: "fr"; MessagesFile: "compiler:Languages\French.isl"

[Files]
Source: "tlauncher.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Tlauncher"; Filename: "{app}\tlauncher.exe"

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueName: "Tlauncher"; ValueType: none; Flags: uninsdeletevalue

[Run]
Filename: "{app}\tlauncher.exe"; Flags: nowait

[UninstallDelete]
Type: filesandordirs; Name: "{app}"
Type: filesandordirs; Name: "{localappdata}\Tlauncher"

[Code]
procedure Quit();
var
  Code: Integer;
begin
  if FileExists(ExpandConstant('{app}\tlauncher.exe')) then
    Exec(ExpandConstant('{app}\tlauncher.exe'), '--quit', '', SW_HIDE, ewWaitUntilTerminated, Code);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Quit();
  Result := '';
end;

function InitializeUninstall(): Boolean;
begin
  Quit();
  Result := True;
end;
