; The Windows installer of ognivo, which scripts/package-windows.sh builds: the window and
; ognivo for the user alone, with no administrator, in %LOCALAPPDATA%\Programs\ognivo; a
; Start menu entry for the window; and ognivo's bin on the user's PATH, for a terminal.
;
;   ISCC /DVersion=0.1.0 /DFileVersion=0.1.0 /DSource=<the window> /DOutput=<out> ognivo.iss

#ifndef Version
  #error Version is not defined: ISCC /DVersion=...
#endif

[Setup]
AppId={{6F1A4E3C-2B7D-4C59-9E21-7A3D5B8C0F42}
AppName=ognivo
AppVersion={#Version}
AppVerName=ognivo {#Version}
AppPublisher=Kristian Pilatovich
AppPublisherURL=https://github.com/Kristian5013/ognivo
AppSupportURL=https://github.com/Kristian5013/ognivo/issues
VersionInfoVersion={#FileVersion}
DefaultDirName={localappdata}\Programs\ognivo
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#Output}
OutputBaseFilename=ognivo-{#Version}-x86_64-windows-setup
SetupIconFile=ognivo.ico
UninstallDisplayIcon={app}\ognivo-window.exe
UninstallDisplayName=ognivo
LicenseFile=..\..\..\LICENSE
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; the core may still run from bin: Windows' Restart Manager closes it for the update
CloseApplications=yes
ChangesEnvironment=yes

[Files]
Source: "{#Source}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\ognivo"; Filename: "{app}\ognivo-window.exe"

[Run]
Filename: "{app}\ognivo-window.exe"; Description: "Start ognivo"; Flags: nowait postinstall skipifsilent

[Code]
const
  EnvKey = 'Environment';

{ The user's PATH has `Dir`, in any case. }
function OnPath(Paths, Dir: string): Boolean;
begin
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Paths) + ';') > 0;
end;

procedure AddPath(Dir: string);
var
  Paths: string;
begin
  if not RegQueryStringValue(HKEY_CURRENT_USER, EnvKey, 'Path', Paths) then
    Paths := '';
  if OnPath(Paths, Dir) then
    exit;
  if (Paths <> '') and (Copy(Paths, Length(Paths), 1) <> ';') then
    Paths := Paths + ';';
  RegWriteExpandStringValue(HKEY_CURRENT_USER, EnvKey, 'Path', Paths + Dir);
end;

{ The user's PATH without `Dir`, the rest as it was. }
procedure RemovePath(Dir: string);
var
  Paths, Rest, Part: string;
  At: Integer;
begin
  if not RegQueryStringValue(HKEY_CURRENT_USER, EnvKey, 'Path', Paths) then
    exit;
  if not OnPath(Paths, Dir) then
    exit;
  Rest := '';
  while Paths <> '' do
  begin
    At := Pos(';', Paths);
    if At = 0 then
    begin
      Part := Paths;
      Paths := '';
    end
    else
    begin
      Part := Copy(Paths, 1, At - 1);
      Delete(Paths, 1, At);
    end;
    if (Part <> '') and (CompareText(Part, Dir) <> 0) then
    begin
      if Rest <> '' then
        Rest := Rest + ';';
      Rest := Rest + Part;
    end;
  end;
  RegWriteExpandStringValue(HKEY_CURRENT_USER, EnvKey, 'Path', Rest);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
    AddPath(ExpandConstant('{app}\bin'));
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
    RemovePath(ExpandConstant('{app}\bin'));
end;
