; Inno Setup 6 script for Equalizerer.
; Built by scripts/build-installer.ps1, which passes AppVersion and RepoRoot on the command line.
; Relative paths in this file are relative to this script's folder (installer/).

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef RepoRoot
  #define RepoRoot ".."
#endif

#define AppExe "eq-app.exe"
#define VBCableSetup "VBCABLE_Setup_x64.exe"

[Setup]
AppName=Equalizerer
AppVersion={#AppVersion}
AppPublisher=Equalizerer
AppId={{207FD8C5-7EDA-47C4-A954-E4B50188B251}
DefaultDirName={autopf}\Equalizerer
DefaultGroupName=Equalizerer
DisableProgramGroupPage=yes
OutputDir=output
OutputBaseFilename=Equalizerer-Setup-{#AppVersion}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
; Installing the VB-CABLE driver needs administrator rights.
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Unprefixed names live in the session-local namespace, the same as the app's Local\equalizerer
; mutex, so setup and uninstall detect a running instance and ask the user to close it.
AppMutex=equalizerer
UninstallDisplayIcon={app}\{#AppExe}
LicenseFile=LICENSE.txt
InfoBeforeFile=THIRD_PARTY.txt

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Additional icons:"; Flags: unchecked
Name: "autostart"; Description: "Start Equalizerer with Windows"; GroupDescription: "Options:"; Flags: checkedonce

[Files]
Source: "{#RepoRoot}\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
; Copy of the VB-CABLE setup kept in the application folder, so the uninstaller can remove the
; driver later. Only when this setup installs VB-CABLE.
Source: "{#RepoRoot}\installer\vendor\vbcable\*"; DestDir: "{app}\vbcable"; Flags: ignoreversion; Check: NeedVBCable
; Working copy used by this setup only; deleted after installation.
Source: "{#RepoRoot}\installer\vendor\vbcable\*"; DestDir: "{tmp}\vbcable"; Flags: ignoreversion deleteafterinstall; Check: NeedVBCable

[Icons]
Name: "{group}\Equalizerer"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\Equalizerer"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
; Autostart entry, same value and command the app's "Start with Windows" checkbox writes.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Equalizerer"; ValueData: """{app}\{#AppExe}"" --minimized"; Flags: uninsdeletevalue; Tasks: autostart
; Records that this setup installed VB-CABLE, so the uninstaller may offer to remove it.
Root: HKLM; Subkey: "Software\Equalizerer"; ValueType: dword; ValueName: "InstalledVBCable"; ValueData: "1"; Flags: uninsdeletekey; Check: NeedVBCable

[Run]
Filename: "{tmp}\vbcable\{#VBCableSetup}"; Parameters: "-i -h"; StatusMsg: "Installing VB-CABLE virtual audio device…"; Flags: waituntilterminated; Check: NeedVBCable
Filename: "{app}\{#AppExe}"; Description: "Launch Equalizerer"; Flags: nowait postinstall skipifsilent runasoriginaluser

[UninstallRun]
; Runs before the files are removed: give the real output device back to the user first.
Filename: "{app}\{#AppExe}"; Parameters: "--restore"; Flags: runhidden waituntilterminated; RunOnceId: "RestoreDefault"
; Only when the user agreed in CurUninstallStepChanged.
Filename: "{app}\vbcable\{#VBCableSetup}"; Parameters: "-u -h"; Flags: runhidden waituntilterminated; RunOnceId: "RemoveVBCable"; Check: UninstallRemoveVBCable

[Code]
var
  // Decided once, before any file is copied, so the files, the registry flag and the
  // VB-CABLE run entry all agree.
  VBCableNeeded: Boolean;
  // Set by the uninstaller's question; read by the UninstallRun entry.
  RemoveVBCable: Boolean;

// Runs a hidden PowerShell check for the VB-CABLE capture endpoint. Returns True only when the
// endpoint is present; if PowerShell cannot be started, the endpoint counts as absent.
function VBCablePresent(): Boolean;
var
  ResultCode: Integer;
begin
  Result := False;
  if Exec(ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'),
    '-NoProfile -ExecutionPolicy Bypass -Command "if (Get-PnpDevice -PresentOnly -Class AudioEndpoint -ErrorAction SilentlyContinue | Where-Object { $_.FriendlyName -like ''*VB-Audio Virtual Cable*'' }) { exit 0 } else { exit 1 }"',
    '', SW_HIDE, ewWaitUntilTerminated, ResultCode) then
    Result := (ResultCode = 0);
end;

function NeedVBCable(): Boolean;
begin
  Result := VBCableNeeded;
end;

function UninstallRemoveVBCable(): Boolean;
begin
  Result := RemoveVBCable;
end;

// Polls for the endpoint after the VB-CABLE setup has finished, so Equalizerer does not start
// before its virtual device exists.
procedure WaitForVBCableEndpoint();
var
  Attempt: Integer;
  Found: Boolean;
begin
  Found := False;
  Attempt := 0;
  while (not Found) and (Attempt < 10) do
  begin
    Found := VBCablePresent();
    if not Found then
    begin
      Sleep(500);
      Attempt := Attempt + 1;
    end;
  end;
  if not Found then
    MsgBox('VB-CABLE was installed, but its audio device has not appeared yet. ' +
      'If Equalizerer reports that the virtual device is missing, restart Windows and try again.',
      mbInformation, MB_OK);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  // Before files are copied: the decision must not change while the files are installed.
  if CurStep = ssInstall then
    VBCableNeeded := not VBCablePresent();
  // After the VB-CABLE run entry (which runs during installation) and before the postinstall
  // Launch entry.
  if CurStep = ssPostInstall then
    if VBCableNeeded then
      WaitForVBCableEndpoint();
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Installed: Cardinal;
begin
  // The question comes before anything is removed, because the VB-CABLE setup is one of the
  // files the uninstaller deletes.
  if CurUninstallStep = usUninstall then
  begin
    RemoveVBCable := False;
    if RegQueryDWordValue(HKLM, 'Software\Equalizerer', 'InstalledVBCable', Installed) then
      if Installed = 1 then
        if not UninstallSilent() then
          RemoveVBCable := (MsgBox('Also remove the VB-CABLE virtual audio device? ' +
            'Keep it if other apps use it. (VB-CABLE is donationware by VB-Audio Software.)',
            mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES);
  end;
  if CurUninstallStep = usPostUninstall then
    DelTree(ExpandConstant('{app}\vbcable'), True, True, True);
end;
