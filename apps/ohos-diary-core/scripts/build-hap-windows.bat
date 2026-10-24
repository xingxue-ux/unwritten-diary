@echo off
REM 用 Windows 侧 DevEco 的 hvigor 构建**已签名**的 HAP。
REM 为什么需要这个：签名材料（.p12/.cer/.p7b 与两处加密口令）由 DevEco 的
REM “自动生成签名文件”写进 build-profile.json5 的 signingConfigs；
REM 从 WSL 用 Linux hvigor 构建时读不到 C:\ 的路径，所以签名构建走 Windows。
REM
REM 前置：
REM   1) 用 DevEco 打开本工程，Project Structure > Signing Configs 勾“自动生成签名文件”
REM      并点确定（会把 signingConfigs 写进 build-profile.json5，含 DevEco 加密口令）；
REM   2) **不要**把带 signingConfigs 的 build-profile.json5 提交进仓库（那是敏感材料）。
REM
REM 用法：把本文件放在工程根目录后双击，或在 cmd 里 `build-hap-windows.bat`。
chcp 65001 >nul
cd /d "%~dp0.."
set DEVECO=E:\huawei\DevEco Studio
set DEVECO_SDK_HOME=%DEVECO%\sdk
set JAVA_HOME=%DEVECO%\jbr
set PATH=%DEVECO%\tools\node;%PATH%
python ..\..\tools\stage_ohos_models.py --check
if errorlevel 1 exit /b 1
for %%L in (libdiary_capi.so libdiary_napi.so libc++_shared.so libsherpa-onnx-c-api.so libonnxruntime.so libonnxruntime-diary-1.29.1.so libdiary_llm.so libdiary_llm_dotprod.so) do (
  if not exist "entry\libs\arm64-v8a\%%L" (
    echo Missing native runtime: %%L. Run the native staging/build scripts first.
    exit /b 1
  )
)
call "%DEVECO%\tools\hvigor\bin\hvigorw.bat" --mode module -p product=default -p module=entry@default -p buildMode=debug assembleHap --no-daemon
echo EXITCODE=%ERRORLEVEL%
