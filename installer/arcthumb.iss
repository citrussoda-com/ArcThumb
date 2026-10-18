; ArcThumb installer script (Inno Setup 6.x)
;
; Build with:
;   cargo build --release
;   iscc installer\arcthumb.iss
;
; Output: target\installer\ArcThumb-Setup.exe
;
; The installer supports BOTH per-user and per-machine modes via the
; standard Inno "auto" install mode. The mode is picked from one of
; three signals (PrivilegesRequiredOverridesAllowed=dialog commandline):
;
;   - Interactive run, normal: dialog asks; default per-user install
;     to %LOCALAPPDATA%\Programs\ArcThumb (HKCU).
;   - Interactive run, "Run as administrator" or accepted UAC: dialog
;     asks; default per-machine install to %ProgramFiles%\ArcThumb
;     (HKLM).
;   - Silent run with /CURRENTUSER -> per-user. Used by `winget install
;     CitrusSoda.ArcThumb` (Scope: user is the default).
;   - Silent run with /ALLUSERS    -> per-machine, requires elevation.
;     Used by `winget install --scope machine`. Required when Explorer
;     runs at High Mandatory Integrity (Windows Sandbox, some
;     enterprise lockdowns) because that Explorer ignores HKCU CLSIDs
;     by Microsoft's design.
;
; Post-install: silently calls `arcthumb-config.exe --install` from
; [Code] so its exit code can be checked; a failed registration is
; reported instead of ending in a setup that looks successful. The
; install mode is passed as `--scope user|machine`, so install dir and
; registry hive stay aligned in every mode above, including setup
; started elevated and then told to install for the current user only.
; The Finish page offers a checkbox to launch the configuration GUI,
; and below it a short "support development" note with one link to
; the /sponsor URL on citrussoda.com, in the language the installer
; runs in. That URL is a redirect the site controls, so where it lands
; can change without a new release (the config GUI's Help menu uses
; the same URL). See CLAUDE.md, "Support links". The
; link is a plain label: nothing is checked by default and nothing
; opens unless clicked.
;
; Pre-uninstall: silently calls `arcthumb-config.exe --uninstall`,
; which best-effort cleans both HKCU and HKLM, then removes files.

#define MyAppName       "ArcThumb"
#define MyAppVersion    "0.7.2"
#define MyAppPublisher  "citrussoda-com"
#define MyAppURL        "https://github.com/citrussoda-com/ArcThumb"
#define MyAppExeName    "arcthumb-config.exe"

[Setup]
; AppId — never change. Identifies upgrades vs new installs.
AppId={{DFE16BE0-6554-4F21-BB11-51601FD3FEC8}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}/issues
AppUpdatesURL={#MyAppURL}/releases
; {autopf} resolves to %ProgramFiles% for admin installs and
; %LocalAppData%\Programs for per-user installs (Inno Setup 6 auto
; install mode). The choice is made by PrivilegesRequired below plus
; the elevation dialog from PrivilegesRequiredOverridesAllowed.
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
LicenseFile=..\LICENSE-MIT
OutputDir=..\target\installer
OutputBaseFilename=ArcThumb-Setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
; Default to per-user (no UAC). Users who want per-machine can pick
; that mode via the elevation dialog enabled by the next setting,
; by right-clicking the installer and "Run as administrator", or by
; passing /ALLUSERS on the command line. winget uses the command-line
; switches when invoked with --scope machine.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog commandline
; 64-bit Explorer needs a 64-bit shell extension DLL.
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Show the config exe as the icon in Apps & Features. The exe's
; embedded icon (resource ID 1, set up by `resources/arcthumb-config.rc`)
; is what Apps & Features actually displays.
UninstallDisplayIcon={app}\{#MyAppExeName}
UninstallDisplayName={#MyAppName} {#MyAppVersion}
; Icon for the installer .exe itself (`ArcThumb-Setup.exe`).
SetupIconFile=..\assets\icon.ico

[Languages]
Name: "english";  MessagesFile: "compiler:Default.isl"
Name: "japanese"; MessagesFile: "compiler:Languages\Japanese.isl"

[CustomMessages]
; Finish-page support note. Keep the wording in step with the
; "Support" section of README.md. The URL is per language, like the
; `support_url` string in ui/main.slint and lang/ja/LC_MESSAGES/arcthumb.po.
english.SupportLead=ArcThumb is free and maintained in my spare time. If it saved you some clicking around, a small tip helps me keep at it.
japanese.SupportLead=ArcThumb は無料で、空き時間に開発しています。役に立ったと思ったら、少額の支援をいただけると続ける励みになります。
english.SupportLink=Support ArcThumb development
japanese.SupportLink=ArcThumb の開発を支援する
english.SupportUrl=https://citrussoda.com/en/arcthumb/sponsor
japanese.SupportUrl=https://citrussoda.com/arcthumb/sponsor

[Files]
; Shell extension DLL — the actual thumbnail provider.
Source: "..\target\release\arcthumb.dll";        DestDir: "{app}"; Flags: ignoreversion
; Configuration GUI + CLI installer/uninstaller helper.
Source: "..\target\release\arcthumb-config.exe"; DestDir: "{app}"; Flags: ignoreversion
; Dual-license text files.
Source: "..\LICENSE-MIT";                        DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE-APACHE";                     DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#MyAppName} Configuration"; Filename: "{app}\{#MyAppExeName}"
Name: "{autoprograms}\Uninstall {#MyAppName}";     Filename: "{uninstallexe}"

[Run]
; Finish-page checkbox. Launches the GUI if the user wants it.
Filename: "{app}\{#MyAppExeName}"; \
    Description: "Launch {#MyAppName} Configuration"; \
    Flags: postinstall nowait skipifsilent

[UninstallRun]
; Remove all shell-extension registry entries before files vanish.
; RunOnceId guards against the entry firing twice if the user
; cancels mid-uninstall and retries.
Filename: "{app}\{#MyAppExeName}"; Parameters: "--uninstall"; \
    RunOnceId: "ArcThumbUnregister"; \
    Flags: runhidden waituntilterminated

[Code]
var
  SupportNoteBuilt: Boolean;

// Open the support page in the user's default browser. "AsOriginalUser"
// matters for per-machine installs: setup runs elevated there, and we
// do not want the browser inheriting that token.
procedure SupportLinkClick(Sender: TObject);
var
  Url: String;
  ErrorCode: Integer;
begin
  Url := CustomMessage('SupportUrl');
  if not ShellExecAsOriginalUser('open', Url, '', '', SW_SHOWNORMAL, ewNoWait, ErrorCode) then
    Log(Format('Could not open %s: %s', [Url, SysErrorMessage(ErrorCode)]));
end;

// Lay out the support note at the bottom of the Finish page, under
// the "Launch Configuration" checkbox. Done once, the first time the
// page is shown, because the RunList is only positioned by then.
procedure BuildSupportNote;
var
  Lead, Link: TNewStaticText;
  Left, Width, Top, LinkTop: Integer;
begin
  if SupportNoteBuilt then
    Exit;
  SupportNoteBuilt := True;

  Left := WizardForm.FinishedLabel.Left;
  Width := WizardForm.FinishedLabel.Width;

  Lead := TNewStaticText.Create(WizardForm);
  Lead.Parent := WizardForm.FinishedPage;
  Lead.Caption := CustomMessage('SupportLead');
  Lead.AutoSize := False;
  Lead.WordWrap := True;
  Lead.Left := Left;
  Lead.Width := Width;
  Lead.AdjustHeight;

  // Anchor the block to the bottom of the page: lead text, then the
  // link on its own row beneath it.
  LinkTop := WizardForm.FinishedPage.ClientHeight - ScaleY(16) - ScaleY(14);
  Top := LinkTop - ScaleY(6) - Lead.Height;
  Lead.Top := Top;

  Link := TNewStaticText.Create(WizardForm);
  Link.Parent := WizardForm.FinishedPage;
  Link.Caption := CustomMessage('SupportLink');
  Link.Left := Left;
  Link.Top := LinkTop;
  Link.Cursor := crHand;
  Link.Font.Color := clBlue;
  Link.Font.Style := [fsUnderline];
  Link.OnClick := @SupportLinkClick;

  // Keep the checkbox list from running underneath the note.
  if WizardForm.RunList.Visible and
     (WizardForm.RunList.Top + WizardForm.RunList.Height > Top) then
    WizardForm.RunList.Height := Top - ScaleY(8) - WizardForm.RunList.Top;
end;

procedure CurPageChanged(CurPageID: Integer);
begin
  if CurPageID = wpFinished then
    BuildSupportNote;
end;

// Register the shell extension into the hive that matches the install
// mode chosen above (HKLM for all users, HKCU for the current user).
// The DLL was just placed in {app} so `--install` finds it via
// `current_exe()`'s neighbour.
//
// This lives here instead of in [Run] because [Run] ignores exit
// codes. The exit codes are documented in src/bin/arcthumb-config/cli.rs.
procedure CurStepChanged(CurStep: TSetupStep);
var
  ResultCode: Integer;
  Started: Boolean;
  Params: String;
begin
  if CurStep <> ssPostInstall then
    Exit;
  if IsAdminInstallMode then
    Params := '--install --scope machine'
  else
    Params := '--install --scope user';
  WizardForm.StatusLabel.Caption := 'Registering shell extension...';
  Started := Exec(ExpandConstant('{app}\{#MyAppExeName}'), Params, '',
    SW_HIDE, ewWaitUntilTerminated, ResultCode);
  if Started and (ResultCode = 0) then
    Exit;
  if not Started then
    Log('arcthumb-config --install could not be started: ' + SysErrorMessage(ResultCode))
  else
    Log(Format('arcthumb-config --install exited with code %d', [ResultCode]));
  SuppressibleMsgBox(
    'ArcThumb was copied, but registering the shell extension failed (code ' +
    IntToStr(ResultCode) + '). Thumbnails will not appear until it is registered.' + #13#10 + #13#10 +
    'Run "' + ExpandConstant('{app}\{#MyAppExeName}') + ' ' + Params + '" from a command prompt to try again.',
    mbError, MB_OK, IDOK);
end;
