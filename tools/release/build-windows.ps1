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

  It does not build an installer unless you pass -Installer, and it does not
  sign anything unless you pass -Sign (with -Thumbprint or -PfxPath). Both are
  opt-in so a plain run still works on a machine without a certificate or
  Inno Setup.

.PARAMETER DryRun
  Print the commands without executing the builds.

.PARAMETER SkipRust
  Reuse an existing target\x86_64-pc-windows-msvc\release\diary_bridge.dll.

.PARAMETER Installer
  Also build the Inno Setup installer (tools\release\windows-installer.iss).
  Requires Inno Setup 6 (ISCC.exe on PATH or in its default install dir).

.PARAMETER Sign
  Authenticode-sign unwritten_diary.exe, diary_bridge.dll and the installer,
  with an RFC3161 timestamp. Needs signtool.exe plus either -Thumbprint
  (certificate already in the cert store) or -PfxPath/-PfxPassword.

.PARAMETER Thumbprint
  Certificate thumbprint to sign with (spaces ignored).

.PARAMETER PfxPath
  Path to a .pfx file to sign with (CI style). Pair with -PfxPassword.

.PARAMETER TimestampUrl
  Timestamp server; defaults to DigiCert. A signature without a timestamp
  stops being trusted once the certificate expires.

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
  [switch]$SkipRust,
  # 构建 Inno Setup 安装包（需要 Inno Setup 6，ISCC.exe 在 PATH 或默认安装目录）
  [switch]$Installer,
  # 用 Authenticode 证书签名 exe / bridge DLL / 安装包（不签名会在别的机器上触发 SmartScreen）
  [switch]$Sign,
  # 签名方式二选一：证书指纹（证书已在证书存储里）
  [string]$Thumbprint,
  # 或 PFX 文件（CI 用，配合 -PfxPassword）
  [string]$PfxPath,
  [string]$PfxPassword,
  # 时间戳服务；签名不加时间戳会在证书过期后失效
  [string]$TimestampUrl = 'http://timestamp.digicert.com'
)

$ErrorActionPreference = 'Stop'
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$App = Join-Path $Root 'apps\diary_app'
$Dist = Join-Path $Root 'dist\windows'
# 仓库规范要求每个工作树用自己的 CARGO_TARGET_DIR（共享 target 曾导致 58 个幻影错误）。
# 设了它之后产物不在仓库根的 target\ 下，所以这里跟着 CARGO_TARGET_DIR 走；
# 没设才用默认位置。否则使用者得自己造 junction 才能跑通脚本（#172 第 1 条）。
$TargetRoot = if ($env:CARGO_TARGET_DIR) {
  if ([System.IO.Path]::IsPathRooted($env:CARGO_TARGET_DIR)) { $env:CARGO_TARGET_DIR }
  else { Join-Path $Root $env:CARGO_TARGET_DIR }
} else {
  Join-Path $Root 'target'
}
$Dll = Join-Path $TargetRoot 'x86_64-pc-windows-msvc\release\diary_bridge.dll'
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

function Find-SignTool {
  $cmd = Get-Command signtool.exe -ErrorAction SilentlyContinue
  if ($cmd) { return $cmd.Source }
  $roots = @(
    (Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'),
    (Join-Path ${env:ProgramFiles} 'Windows Kits\10\bin')
  ) | Where-Object { Test-Path $_ }
  foreach ($base in $roots) {
    $hit = Get-ChildItem -Path $base -Filter signtool.exe -Recurse -ErrorAction SilentlyContinue |
      Where-Object { $_.FullName -match '\\x64\\signtool\.exe$' } |
      Sort-Object FullName -Descending | Select-Object -First 1
    if ($hit) { return $hit.FullName }
  }
  return $null
}

function Find-Iscc {
  $cmd = Get-Command ISCC.exe -ErrorAction SilentlyContinue
  if ($cmd) { return $cmd.Source }
  $candidates = @(
    (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'),
    (Join-Path ${env:ProgramFiles} 'Inno Setup 6\ISCC.exe'),
    (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe')
  )
  foreach ($p in $candidates) { if (Test-Path $p) { return $p } }
  return $null
}

$script:SignTool = $null

function Sign-File([string]$Path) {
  if (-not $Sign) { return }
  if (-not (Test-Path $Path)) { Fail "签名前找不到文件：$Path" }
  $signArgs = @('sign', '/fd', 'SHA256', '/tr', $TimestampUrl, '/td', 'SHA256')
  if ($PfxPath) {
    if (-not (Test-Path $PfxPath)) { Fail "找不到 PFX：$PfxPath" }
    $signArgs += @('/f', $PfxPath)
    if ($PfxPassword) { $signArgs += @('/p', $PfxPassword) }
  } elseif ($Thumbprint) {
    $signArgs += @('/sha1', ($Thumbprint -replace '\s', ''))
  } else {
    Fail '开了 -Sign，但既没给 -Thumbprint 也没给 -PfxPath。'
  }
  $signArgs += $Path
  if ($DryRun) {
    Write-Host "[dry-run] signtool $($signArgs -join ' ')"
    return
  }
  & $script:SignTool @signArgs
  if ($LASTEXITCODE -ne 0) { Fail "签名失败（signtool 退出码 $LASTEXITCODE）：$Path" }
  $sig = Get-AuthenticodeSignature $Path
  if ($sig.Status -ne 'Valid') {
    Fail ("签名后校验不通过（{0}）：{1}" -f $sig.Status, $Path)
  }
  Write-Host ("已签名并校验：{0}（{1}）" -f (Split-Path $Path -Leaf), $sig.SignerCertificate.Subject)
}

Step '1/7 检查工具链'
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
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$vs = $null
if (Test-Path $vswhere) {
  # **必须带 -prerelease**：vswhere 默认不列预发布版本。装的是 VS Insiders 时
  # （例如 Community 2026 Insiders），不带这个开关会返回空，于是下面误报
  # 「Visual Studio 已安装但没有 MSVC」——而实际上 flutter build 能正常出包。
  $vs = (& $vswhere -latest -prerelease -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath) 2>$null
  if ($vs) { Write-Host "Visual Studio：$vs" }
  else { Fail 'Visual Studio 已安装但没有 MSVC x64 C++ 工具链（含预发布版本也已检查）。' }
} else {
  Write-Warning '没找到 vswhere，无法自动确认 MSVC 工具链；如果 flutter build 失败，请先装 Visual Studio C++ 工作负载。'
}

# cmake 优先用 PATH 上的；PATH 上没有时，VS 自带的 CMake/Ninja 通常在
# <VS安装目录>\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin 下，
# 直接 Fail 会让人以为“没装 CMake”，其实装了只是没进 PATH（#172 第 2 条）。
if (-not (Get-Command cmake -ErrorAction SilentlyContinue)) {
  $foundCmake = $null
  if ($vs) {
    $candidate = Join-Path $vs 'Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe'
    if (Test-Path $candidate) { $foundCmake = $candidate }
  }
  if ($foundCmake) {
    $env:PATH = (Split-Path $foundCmake) + ';' + $env:PATH
    Write-Host "cmake 不在 PATH，已用 Visual Studio 自带的：$foundCmake"
  } else {
    Fail '找不到 cmake。安装 Visual Studio 的“使用 C++ 的桌面开发”工作负载，或把 CMake 3.14+ 放进 PATH（VS 自带的在 <VS>\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin）。'
  }
}

if ($Sign) {
  $script:SignTool = Find-SignTool
  if (-not $script:SignTool) {
    Fail '开了 -Sign 但找不到 signtool.exe。装 Windows SDK（含签名工具），或把 signtool 放进 PATH。'
  }
  Write-Host "signtool：$script:SignTool"
  if ($PfxPath) { Write-Host "签名方式：PFX $PfxPath" }
  elseif ($Thumbprint) { Write-Host "签名方式：证书存储里的 $Thumbprint" }
  else { Fail '开了 -Sign，但既没给 -Thumbprint 也没给 -PfxPath。' }
}
if ($Installer -and -not (Find-Iscc)) {
  Fail '开了 -Installer 但找不到 ISCC.exe。装 Inno Setup 6（https://jrsoftware.org/isdl.php）后重试。'
}

Step '2/7 构建 Rust Windows DLL'
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

Step '3/7 flutter build windows --release'
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

Step '4/7 核对产物结构'
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
if ($Sign) {
  # 先签 exe 与 bridge DLL，再打包：这样 zip 和安装包里带的都是已签名文件。
  Sign-File $exe.FullName
  Sign-File (Join-Path $Bundle 'diary_bridge.dll')
} else {
  $signature = Get-AuthenticodeSignature $exe.FullName
  if ($signature.Status -ne 'Valid') {
    Write-Warning ("unwritten_diary.exe 未签名或签名无效（{0}）。分发到其他机器会触发 SmartScreen；用 -Sign -Thumbprint/-PfxPath，或手工签名并加时间戳。" -f $signature.Status)
  }
}

Step '5/7 收集便携包'
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

Step '6/7 Inno Setup 安装包'
$installerPath = $null
if (-not $Installer) {
  Write-Host '跳过安装包（没有 -Installer）。便携 zip 已生成，可直接分发或解压即用。'
} else {
  $iscc = Find-Iscc
  $iss = Join-Path $Root 'tools\release\windows-installer.iss'
  if (-not (Test-Path $iss)) { Fail "找不到安装包脚本：$iss" }
  $installerOut = Join-Path $Dist "unwritten-diary-$version-windows-x64-setup.exe"
  if (Test-Path $installerOut) { Remove-Item -Force $installerOut }
  Invoke-Step $iscc @("/DAppVersion=$version", $iss) $Root
  if (-not (Test-Path $installerOut)) { Fail "ISCC 跑完但没有产出 $installerOut" }
  $installerPath = $installerOut
  Write-Host ("安装包：{0}（{1:N1} MiB）" -f $installerPath, ((Get-Item $installerPath).Length / 1MB))
  if ($Sign) { Sign-File $installerPath }
  else {
    $isig = Get-AuthenticodeSignature $installerPath
    if ($isig.Status -ne 'Valid') {
      Write-Warning ("安装包未签名（{0}）——它比 exe 更容易被 SmartScreen 拦。" -f $isig.Status)
    }
  }
}

Step '7/7 输出'
Get-ChildItem $stage | Select-Object Name, Length
Get-ChildItem -Recurse $stage -Filter *.dll | Select-Object FullName, Length
Write-Host ("便携包：{0}（{1:N1} MiB）" -f $zip, ((Get-Item $zip).Length / 1MB))
Get-FileHash $zip -Algorithm SHA256 | Format-List
if ($installerPath) {
  Write-Host ("安装包：{0}" -f $installerPath)
  Get-FileHash $installerPath -Algorithm SHA256 | Format-List
}
Write-Host ''
Write-Host '安装包方案与签名要求见 docs/development/M5-打包与发布.md §3；'
Write-Host '装机验收步骤见 docs/development/Windows-验收清单.md。'
Write-Host '未签名 EXE 在别的机器上会触发 SmartScreen；这不是“杀毒误报”，是信誉/签名缺失。'
