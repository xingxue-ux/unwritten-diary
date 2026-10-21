; 「不写日记」Windows 安装包（Inno Setup 6）
;
; 由 tools/release/build-windows.ps1 -Installer 调用，例如：
;   ISCC.exe /DAppVersion=0.1.0 tools\release\windows-installer.iss
;
; 设计约束（别随手改）：
;   1. AppId 一旦发布就不能变——它决定「升级」还是「并存安装」；变了等于装出一个新应用。
;   2. 安装目录只放程序，**用户数据不在 {app} 下**：数据在
;      %APPDATA%\Lingxing\不写日记\unwritten_diary\library.sqlite
;      （由 Runner.rc 的 CompanyName/ProductName 决定，见 docs/development/升级与迁移.md）。
;      卸载**不删**这份数据，也不提供「连数据一起删」的选项。
;   3. 默认按用户安装（不需要管理员），装到 %LOCALAPPDATA%\Programs\；
;      需要装到 Program Files 时安装器会自己请求提权。

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

#define AppName        "不写日记"
#define AppExeName     "unwritten_diary.exe"
#define AppPublisher   "Lingxing"
#define RepoUrl        "https://github.com/xingxue-ux/unwritten-diary"
#define StageDir       "..\..\dist\windows\unwritten-diary-" + AppVersion + "-windows-x64"

[Setup]
AppId={{B73EFDA3-EFE3-4A7C-A03B-C34F6B64AF34}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#RepoUrl}
AppSupportURL={#RepoUrl}/issues
DefaultDirName={autopf}\UnwrittenDiary
DisableProgramGroupPage=yes
DefaultGroupName={#AppName}
; 安装包产物落在这里，与便携包同目录
OutputDir=..\..\dist\windows
OutputBaseFilename=unwritten-diary-{#AppVersion}-windows-x64-setup
SetupIconFile=..\..\apps\diary_app\windows\runner\resources\app_icon.ico
UninstallDisplayIcon={app}\{#AppExeName}
UninstallDisplayName={#AppName}
LicenseFile=..\..\LICENSE
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; Windows 10 及以上、x64
MinVersion=10.0
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; 默认按用户安装；用户可在第一页选择「为所有用户安装」
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
; 覆盖安装时如果应用还在运行：让用户在安装器里关掉它（Restart Manager）
CloseApplications=yes
RestartApplications=no

[Languages]
; 默认英文界面。要用中文界面，把 Inno Setup 的 ChineseSimplified.isl
; （官方仓库的 Unofficial translations）放进编译器 Languages 目录，然后加一行：
;   Name: "chinesesimp"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
; 便携包里的所有内容：unwritten_diary.exe、diary_bridge.dll、flutter_windows.dll、
; data\（icudtl.dat / app.so / flutter_assets）、legal\、LICENSE
Source: "{#StageDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExeName}"
Name: "{group}\卸载 {#AppName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExeName}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExeName}"; Description: "启动 {#AppName}"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
; 只清安装目录里的可重建产物；用户数据不在这里，见文件头的约束 2。
Type: filesandordirs; Name: "{app}\data\logs"

[Code]
// 卸载完成后明确告诉用户：数据还在，重装能接着用。
// 这比「卸载后资料看起来没了」的惊吓重要——Windows 上换 CompanyName/ProductName
// 或换安装路径都可能让用户以为数据丢了（见 docs/development/升级与迁移.md）。
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  DataDir: String;
begin
  if CurUninstallStep = usPostUninstall then
  begin
    DataDir := ExpandConstant('{userappdata}\Lingxing\不写日记');
    MsgBox('程序已卸载。' + Chr(13) + Chr(10) + Chr(13) + Chr(10) +
           '你的日记数据没有被删除，仍然保存在：' + Chr(13) + Chr(10) +
           '  ' + DataDir + Chr(13) + Chr(10) + Chr(13) + Chr(10) +
           '重新安装后会自动接着使用这份数据。',
           mbInformation, MB_OK);
  end;
end;