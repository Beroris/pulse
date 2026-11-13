; Pulse's installer: a normal Windows setup wizard, for the current user, with no
; admin rights.
;
; Build it with scripts/package-windows.ps1, which passes the version and the built exe:
;   ISCC.exe /DAppVersion=1.0.0 /DSourceExe=..\target\release\pulse.exe installer\pulse.iss

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceExe
  #define SourceExe "..\target\release\pulse.exe"
#endif

#define AppName "Pulse"
#define AppExe "pulse.exe"
#define FileBase "Pulse"

[Setup]
; Never change this: Windows knows the installed app by it, for upgrades and uninstalling.
AppId={{33E6D0C0-1277-4F01-9D80-759B7C8AAED9}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Beroris
AppPublisherURL=https://github.com/Beroris/pulse
AppSupportURL=https://github.com/Beroris/pulse/issues
AppComments=A fast, lightweight task manager.
VersionInfoVersion={#AppVersion}
VersionInfoDescription={#AppName} Setup
; Per user: no admin prompt.
PrivilegesRequired=lowest
DefaultDirName={localappdata}\Programs\{#AppName}
DisableDirPage=yes
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
DisableWelcomePage=no
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
SetupIconFile=..\assets\app.ico
WizardStyle=modern
WizardSizePercent=100
WizardSmallImageFile=..\assets\icon.png
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
Compression=lzma2/max
SolidCompression=yes
OutputDir=..\dist
OutputBaseFilename={#FileBase}-{#AppVersion}-Setup
; The app is closed by the code below, so Windows' "close these apps" page isn't needed.
CloseApplications=no
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "autostart"; Description: "Start {#AppName} when I sign in"; GroupDescription: "Options:"
Name: "desktopicon"; Description: "Put a shortcut on the desktop"; GroupDescription: "Options:"; Flags: unchecked

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "{#AppExe}"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"; Comment: "Task manager: see what your PC is doing"
Name: "{userdesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Comment: "Task manager: see what your PC is doing"; Tasks: desktopicon

[Registry]
; The same entry the app's own "Start when I sign in" switch writes, so the two agree.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "{#AppName}"; ValueData: """{app}\{#AppExe}"" --background"; Tasks: autostart; Flags: uninsdeletevalue

[Run]
Filename: "{app}\{#AppExe}"; Description: "Start {#AppName} now"; Flags: postinstall nowait skipifsilent

[UninstallDelete]
Type: dirifempty; Name: "{app}"

[Code]
const
  RunKey = 'Software\Microsoft\Windows\CurrentVersion\Run';
  ApprovedKey = 'Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run';

{ Asks a running copy to quit, so its exe can be replaced or removed. }
procedure QuitApp();
var
  Code: Integer;
  I: Integer;
  Exe: String;
begin
  Exe := ExpandConstant('{app}\{#AppExe}');
  if FileExists(Exe) then
    Exec(Exe, '--quit', '', SW_HIDE, ewWaitUntilTerminated, Code);
  { The app holds this until its process has gone, and with it its hold on the exe. }
  for I := 1 to 50 do
  begin
    if not CheckForMutexes('Local\Beroris.Pulse.Instance') then
      Break;
    Sleep(100);
  end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  QuitApp();
  Result := '';
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  { Unticking the sign-in option on an upgrade turns it off, as it says. }
  if (CurStep = ssPostInstall) and not WizardIsTaskSelected('autostart') then
    RegDeleteValue(HKCU, RunKey, '{#AppName}');
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Settings: String;
begin
  if CurUninstallStep = usUninstall then
  begin
    QuitApp();
    { Its sign-in entry, and the on/off flag Windows keeps for it. Changes made with the app to
      other apps' startup entries and to services are the user's own choices, and stay. }
    RegDeleteValue(HKCU, RunKey, '{#AppName}');
    RegDeleteValue(HKCU, ApprovedKey, '{#AppName}');
  end;
  Settings := ExpandConstant('{userappdata}\{#AppName}');
  if (CurUninstallStep = usPostUninstall) and DirExists(Settings) and not UninstallSilent() then
  begin
    if MsgBox('Also remove your {#AppName} settings?' #13#10#13#10 'Keep them if you might install it again.', mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
      DelTree(Settings, True, True, True);
  end;
end;
