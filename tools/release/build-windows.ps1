<#
.SYNOPSIS
  Build the Windows x64 release bundle for 「不写日记」.

.DESCRIPTION
  Run this from Windows PowerShell (not WSL). It:
    1. checks Flutter, Rust, the MSVC target and Visual Studio;
    2. builds crates/diary_bridge as diary_bridge.dll;
    3. runs `flutter build windows --release` (CMake installs the DLL next to
       unwritten_diary.exe);
    4. validates the bundle layout and stages a portable zip under dist\windows.

  It intentionally does not build an installer. MSIX / Inno Setup / NSIS are
  compared in docs/development/M5-打包与发布.md; choosing one is a product
  decision, not something this script should silently make.

.PARAMETER DryRun
  Print the commands without executing the builds.

.PARAMETER SkipRust
  Reuse an existing target\x86_64-pc-windows-msvc\release\diary_bridge.dll.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File tools\release\build-windows.ps1

.NOTES
  Unsigned executables will trigger Windows SmartScreen ("Windows protected
  your PC") on other machines. Sign with an Authenticode certificate and
  timestamp the signature before distributing. See the packaging doc.
#>
[CmdletBinding()]
param(
  [switch]$DryRun,
  [switch]$SkipRust
)

$ErrorActionPreference = 'Stop'
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$App = Join-Path $Root 'apps\diary_app'
$Dist = Join-Path $Root 'dist\windows'
$Dll = Join-Path $Root 'target\x86_64-pc-windows-msvc\release\diary_bridge.dll'
$Bundle = Join-Path $App 'build\windows\x64\runner\Release'

function Fail([string]$Message) {
  Write-Host "[build-windows][失败] $Message" -ForegroundColor Red
  exit 2
}

function Step([string]$Message) {
  Write-Host ""
  Write-Host "==> $Message"
}

function Invoke-Step([string]$Exe, [string[]]$Arguments, [string]$WorkDir = $Root) {
  $pretty = "$Exe $($Arguments -join ' ')"
  if ($DryRun) {
    Write-Host "[dry-run] (cd $WorkDir; $pretty)"
    return
  }
  Push-Location $WorkDir
  try {
    & $Exe @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$pretty 退出码 $LASTEXITCODE" }
  } finally {
    Pop-Location
  }
}

Step '1/6 检查工具链'
if (-not (Get-Command flutter -ErrorAction SilentlyContinue)) {
  Fail '找不到 flutter。把 Flutter SDK\bin 加进 PATH。'
}
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
  Fail '找不到 cargo。安装 Rust 并确保 %USERPROFILE%\.cargo\bin 在 PATH 上。'
}
if (-not (Get-Command rustup -ErrorAction SilentlyContinue)) {
  Fail '找不到 rustup（安装 rustup 或用 rustup-init 管理工具链）。'
}
$flutterVersion = (& flutter --version | Select-Object -First 1)
Write-Host "flutter：$flutterVersion"
$installedTargets = (& rustup target list --installed) -join ' '
if ($installedTargets -notmatch 'x86_64-pc-windows-msvc') {
  Fail '缺少 Rust target x86_64-pc-windows-msvc（rustup target add x86_64-pc-windows-msvc）。'
}
if (-not (Get-Command cmake -ErrorAction SilentlyContinue)) {
  Fail '找不到 cmake。安装 Visual Studio 2022 的“使用 C++ 的桌面开发”工作负载，或在 PATH 上提供 CMake 3.14+。'
}
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (Test-Path $vswhere) {
  $vs = (& $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath) 2>$null
  if (-not $vs) { Fail 'Visual Studio 已安装但没有 MSVC x64 C++ 工具链。' }
  Write-Host "Visual Studio：$vs"
} else {
  Write-Warning '没找到 vswhere，无法自动确认 MSVC 工具链；如果 flutter build 失败，请先装 Visual Studio C++ 工作负载。'
}

Step '2/6 构建 Rust Windows DLL'
if ($SkipRust) {
  Write-Host '跳过 cargo（-SkipRust）'
} else {
  Invoke-Step 'cargo' @('build', '--release', '--target', 'x86_64-pc-windows-msvc', '-p', 'diary_bridge')
}
if (-not $DryRun) {
  if (-not (Test-Path $Dll)) { Fail "没有产出 $Dll" }
  $dllInfo = Get-Item $Dll
  Write-Host ("diary_bridge.dll bytes={0}" -f $dllInfo.Length)
}

Step '3/6 flutter build windows --release'
# The CMake project reads this exact path:
#   target/x86_64-pc-windows-msvc/release/diary_bridge.dll
# and installs it next to unwritten_diary.exe. If CMake cannot find it, the
# build fails with a FATAL_ERROR instead of producing a broken bundle.
Invoke-Step 'flutter' @('build', 'windows', '--release') $App

if ($DryRun) {
  Write-Host ''
  Write-Host '[dry-run] 不会检查产物；去掉 -DryRun 后会在以下目录检查：'
  Write-Host "  $Bundle"
  exit 0
}

Step '4/6 核对产物结构'
$required = @(
  'unwritten_diary.exe',
  'diary_bridge.dll',
  'flutter_windows.dll',
  'data\icudtl.dat',
  'data\app.so',
  'data\flutter_assets\AssetManifest.bin'
)
foreach ($relative in $required) {
  $path = Join-Path $Bundle $relative
  if (-not (Test-Path $path)) { Fail "发布目录缺少 $relative（在 $Bundle）" }
}

$exe = Get-Item (Join-Path $Bundle 'unwritten_diary.exe')
$signature = Get-AuthenticodeSignature $exe.FullName
if ($signature.Status -ne 'Valid') {
  Write-Warning ("unwritten_diary.exe 未签名或签名无效（{0}）。分发到其他机器会触发 SmartScreen；先做 Authenticode 签名并时间戳。" -f $signature.Status)
}

Step '5/6 收集便携包'
New-Item -ItemType Directory -Force -Path $Dist | Out-Null
$versionMatch = Select-String -Path (Join-Path $App 'pubspec.yaml') -Pattern '^version:\s*([^+\s]+)' | Select-Object -First 1
$version = if ($versionMatch) { $versionMatch.Matches.Groups[1].Value } else { '0.0.0' }
$stage = Join-Path $Dist "unwritten-diary-$version-windows-x64"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force -Path $stage | Out-Null
Copy-Item -Path (Join-Path $Bundle '*') -Destination $stage -Recurse -Force
$legalSource = Join-Path $App 'assets\legal'
if (Test-Path $legalSource) {
  Copy-Item -Recurse -Force $legalSource (Join-Path $stage 'legal')
}
Copy-Item -Force (Join-Path $Root 'LICENSE') (Join-Path $stage 'LICENSE')

$zip = "$stage.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip -CompressionLevel Optimal

Step '6/6 输出'
Get-ChildItem $stage | Select-Object Name, Length
Get-ChildItem -Recurse $stage -Filter *.dll | Select-Object FullName, Length
Write-Host ("便携包：{0}（{1:N1} MiB）" -f $zip, ((Get-Item $zip).Length / 1MB))
Get-FileHash $zip -Algorithm SHA256 | Format-List
Write-Host ''
Write-Host '未做安装包。三种方案的取舍见 docs/development/M5-打包与发布.md：'
Write-Host '  MSIX：系统原生、可签名/自动更新，但证书与打包链最重；'
Write-Host '  Inno Setup：脚本简单、Windows 用户熟悉，需单独维护卸载/升级；'
Write-Host '  NSIS：体积小、可定制，但脚本与安全审查成本更高。'
Write-Host '未签名 EXE 在别的机器上会触发 SmartScreen；这不是“杀毒误报”，是信誉/签名缺失。'
