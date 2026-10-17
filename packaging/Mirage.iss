#ifndef AppVersion
  #error AppVersion must be supplied by scripts/package.ps1
#endif
#ifndef StageDir
  #error StageDir must be supplied by scripts/package.ps1
#endif
#ifndef OutputDir
  #error OutputDir must be supplied by scripts/package.ps1
#endif

[Setup]
AppId={{598060A7-03BB-4780-AEF5-462FB65B6066}
AppName=Mirage
AppVersion={#AppVersion}
AppVerName=Mirage {#AppVersion}
DefaultDirName={localappdata}\Programs\Mirage
DefaultGroupName=Mirage
PrivilegesRequired=lowest
ArchitecturesAllowed=x64os
ArchitecturesInstallIn64BitMode=x64os
MinVersion=10.0.17763
WizardStyle=modern
DisableWelcomePage=no
DisableDirPage=auto
DisableProgramGroupPage=yes
UsePreviousAppDir=yes
InfoBeforeFile=install-guide.txt
InfoAfterFile=installed-guide.txt
UninstallDisplayIcon={app}\Mirage.exe
SetupIconFile={#StageDir}\app.ico
OutputDir={#OutputDir}
OutputBaseFilename=Mirage-{#AppVersion}-windows-x64-setup
Compression=lzma2
SolidCompression=yes
CloseApplications=no
RestartApplications=no
SetupLogging=yes
SetupMutex=MirageSetup-598060A7-03BB-4780-AEF5-462FB65B6066
VersionInfoVersion={#AppVersion}

[Languages]
Name: chinese; MessagesFile: ChineseSimplified.isl
Name: english; MessagesFile: compiler:Default.isl

[Tasks]
Name: desktopicon; Description: "创建桌面快捷方式"; Flags: unchecked

[Files]
Source: "{#StageDir}\Mirage.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\mirage-svc.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\core\*"; DestDir: "{app}\core"; Flags: ignoreversion
Source: "{#StageDir}\licenses\*"; DestDir: "{app}\licenses"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "{#StageDir}\THIRD-PARTY-NOTICES.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\payload-sha256.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\Mirage\Mirage"; Filename: "{app}\Mirage.exe"; WorkingDir: "{app}"
Name: "{userprograms}\Mirage\卸载 Mirage"; Filename: "{uninstallexe}"
Name: "{userdesktop}\Mirage"; Filename: "{app}\Mirage.exe"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\Mirage.exe"; WorkingDir: "{app}"; Description: "启动 Mirage"; Flags: nowait postinstall skipifsilent runasoriginaluser

[Code]
function StopInstalledApp(const Uninstalling: Boolean): Boolean;
var
  Client, Arguments: String;
  ResultCode: Integer;
begin
  Result := True;
  Client := ExpandConstant('{app}\Mirage.exe');
  if not FileExists(Client) then exit;
  Arguments := '--quit';
  if Uninstalling then Arguments := '--prepare-uninstall';
  Result := Exec(Client, Arguments, ExpandConstant('{app}'), SW_HIDE,
    ewWaitUntilTerminated, ResultCode);
  if Result then Result := ResultCode = 0;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  if not StopInstalledApp(False) then
    Result := '请从托盘退出 Mirage，再重试安装。程序需要先恢复代理设置并停止内核。';
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
    if not StopInstalledApp(True) then
      RaiseException('卸载准备失败或管理员授权被取消。请退出 Mirage，并重试；已安装 TUN 服务时需要管理员授权。');
end;
