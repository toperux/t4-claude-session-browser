; Inno Setup script for csb - per-user, no admin required.
;
; Build:
;   ISCC.exe /DVersion=0.2.0 /DStageDir=<abs path to staged files> ^
;            /DIconFile=<abs path to csb.ico> /DOutDir=<abs path> installer\csb.iss
;
; StageDir must contain csb.exe, csb-gui.exe, README.md and LICENSE.
;
; Requires Inno Setup 6.3+ for the "x64compatible" architecture identifiers.
; On an older ISCC, change those two directives to "x64".
;
; NOTE ON THE OUTPUT NAME: it must not contain the target triple, nor both
; "x86_64" and "windows". The updater picks its download by searching asset
; names for those, and takes the first match in GitHub's name-sorted asset list
; - an installer called csb-setup-x86_64-pc-windows-msvc.exe would sort before
; csb-x86_64-pc-windows-msvc.zip and get downloaded instead of it, breaking
; `csb update` for every existing install. See src/update.rs.

#ifndef Version
  #define Version "0.0.0"
#endif
#ifndef StageDir
  #define StageDir "..\dist\stage"
#endif
#ifndef IconFile
  #define IconFile "..\assets\csb.ico"
#endif
#ifndef OutDir
  #define OutDir "..\dist"
#endif

#define AppName "Claude Session Browser"
#define AppExeName "csb.exe"
#define GuiExeName "csb-gui.exe"
#define AppPublisher "Christopher Montevirgen"
#define AppUrl "https://github.com/toperux/t4-claude-session-browser"

[Setup]
; Never change AppId: it is what lets a later version upgrade this one in place
; instead of installing a second copy alongside it.
AppId={{28B1C928-DD6B-48AA-A52F-46A05F83B179}
AppName={#AppName}
AppVersion={#Version}
VersionInfoVersion={#Version}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppUrl}
AppSupportURL={#AppUrl}/issues
AppUpdatesURL={#AppUrl}/releases

; Per-user install: no UAC prompt, and - the reason it actually matters -
; `csb update` can replace the binaries in place afterwards. A machine-wide
; install under Program Files would make every self-update fail on permissions,
; so elevation is deliberately not offered.
PrivilegesRequired=lowest
DefaultDirName={localappdata}\Programs\csb
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
DisableDirPage=auto

ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible

; The installer edits the per-user PATH, so Windows must be told to broadcast
; WM_SETTINGCHANGE. Already-open terminals still will not see it; new ones will.
ChangesEnvironment=yes

OutputDir={#OutDir}
OutputBaseFilename=csb-setup-{#Version}-x64
SetupIconFile={#IconFile}
UninstallDisplayIcon={app}\{#GuiExeName}
UninstallDisplayName={#AppName}
LicenseFile={#StageDir}\LICENSE
WizardStyle=modern
Compression=lzma2
SolidCompression=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "addtopath"; Description: "Add csb to my PATH (so ""csb"" works in any terminal)"; \
  GroupDescription: "Command line:"

[Files]
Source: "{#StageDir}\{#AppExeName}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\{#GuiExeName}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\README.md";     DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\LICENSE";       DestDir: "{app}"; Flags: ignoreversion

[Icons]
; Shortcuts point at the GUI-subsystem binary. Pointing them at csb.exe would
; open the app with a console window sitting behind it - the whole reason that
; second binary exists.
Name: "{group}\{#AppName}"; Filename: "{app}\{#GuiExeName}"
Name: "{group}\Uninstall {#AppName}"; Filename: "{uninstallexe}"

[Run]
Filename: "{app}\{#GuiExeName}"; Description: "Launch {#AppName}"; \
  Flags: nowait postinstall skipifsilent

[Code]
const
  EnvKey = 'Environment';

function CurrentPath(): String;
begin
  if not RegQueryStringValue(HKEY_CURRENT_USER, EnvKey, 'Path', Result) then
    Result := '';
end;

{ Split on ';'. Inno has no built-in string split, and doing this by hand is
  what keeps the add/remove pair symmetric and free of stray semicolons. }
function SplitPath(Value: String): TArrayOfString;
var
  Count, Start, I: Integer;
begin
  Count := 0;
  SetArrayLength(Result, Length(Value) + 1);
  Start := 1;
  for I := 1 to Length(Value) do
  begin
    if Value[I] = ';' then
    begin
      Result[Count] := Copy(Value, Start, I - Start);
      Count := Count + 1;
      Start := I + 1;
    end;
  end;
  Result[Count] := Copy(Value, Start, Length(Value) - Start + 1);
  Count := Count + 1;
  SetArrayLength(Result, Count);
end;

{ Compare two directory entries ignoring case and a trailing backslash. }
function SameDir(A, B: String): Boolean;
begin
  A := Trim(A);
  B := Trim(B);
  if (A <> '') and (A[Length(A)] = '\') then Delete(A, Length(A), 1);
  if (B <> '') and (B[Length(B)] = '\') then Delete(B, Length(B), 1);
  Result := (A <> '') and (CompareText(A, B) = 0);
end;

function PathContains(Dir: String): Boolean;
var
  Parts: TArrayOfString;
  I: Integer;
begin
  Result := False;
  Parts := SplitPath(CurrentPath());
  for I := 0 to GetArrayLength(Parts) - 1 do
    if SameDir(Parts[I], Dir) then
    begin
      Result := True;
      Exit;
    end;
end;

{ Append, done in code rather than with a [Registry] entry so that a profile
  with no Path value yet is handled - the olddata constant has nothing to
  expand there. }
procedure AddToPath(Dir: String);
var
  Existing, Updated: String;
begin
  if PathContains(Dir) then
    Exit;
  Existing := CurrentPath();
  if Existing = '' then
    Updated := Dir
  else if Existing[Length(Existing)] = ';' then
    Updated := Existing + Dir
  else
    Updated := Existing + ';' + Dir;
  RegWriteExpandStringValue(HKEY_CURRENT_USER, EnvKey, 'Path', Updated);
end;

{ Rebuild the list without our entry, rather than cutting a substring out of it. }
procedure RemoveFromPath(Dir: String);
var
  Parts: TArrayOfString;
  Rebuilt, Original: String;
  I: Integer;
begin
  Original := CurrentPath();
  if Original = '' then
    Exit;

  Parts := SplitPath(Original);
  Rebuilt := '';
  for I := 0 to GetArrayLength(Parts) - 1 do
  begin
    if (Trim(Parts[I]) <> '') and not SameDir(Parts[I], Dir) then
    begin
      if Rebuilt <> '' then
        Rebuilt := Rebuilt + ';';
      Rebuilt := Rebuilt + Parts[I];
    end;
  end;

  if Rebuilt <> Original then
  begin
    if Rebuilt = '' then
      RegDeleteValue(HKEY_CURRENT_USER, EnvKey, 'Path')
    else
      RegWriteExpandStringValue(HKEY_CURRENT_USER, EnvKey, 'Path', Rebuilt);
  end;
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
