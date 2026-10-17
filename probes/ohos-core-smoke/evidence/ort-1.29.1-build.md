# 自编 ORT 1.29.1（OHOS/arm64-v8a）关键证据

配方：/tmp/requests-wt/archives/o/onnxruntime/1.29.1（conan create -pr:h=ohos-aarch64-wsl -pr:b=default -c tools.build:jobs=16）
构建日志：/tmp/ort-build/build5.log（原始 2700+ 行，下面是关键片段）

## 产物
total 19172
drwxr-xr-x 2 qianxia qianxia     4096 Oct 16 18:56 .
drwxr-xr-x 7 qianxia qianxia     4096 Oct 16 19:01 ..
-rwxr-xr-x 1 qianxia qianxia 19623560 Oct 16 18:56 libonnxruntime.so

```
ELF 64-bit LSB shared object, ARM aarch64, version 1 (SYSV), dynamically linked, BuildID[sha1]=68fd9cbde88093cf586adcd5ad907d0ff36cbb0d, stripped
  0x0000000000000001 (NEEDED)       Shared library: [libc++_shared.so]
  0x0000000000000001 (NEEDED)       Shared library: [libc.so]
  0x000000000000000e (SONAME)       Library soname: [libonnxruntime.so.1]
   382: 00000000005463e8    12 FUNC    GLOBAL DEFAULT    17 OrtGetApiBase@@VERS_1.29.1
```

## configure 选项（日志原文，取自 conan 的调用行）

```
-DCMAKE_TOOLCHAIN_FILE=$NDK/build/cmake/ohos.toolchain.cmake
-DOHOS_ARCH=arm64-v8a   -DCMAKE_CXX_STANDARD=20
-Donnxruntime_BUILD_SHARED_LIB=ON      -Donnxruntime_BUILD_UNIT_TESTS=OFF
-Donnxruntime_ENABLE_PYTHON=OFF        -Donnxruntime_MINIMAL_BUILD=OFF
-Donnxruntime_DISABLE_EXCEPTIONS=OFF   -Donnxruntime_DISABLE_RTTI=OFF
-Donnxruntime_DISABLE_CONTRIB_OPS=OFF  -Donnxruntime_USE_NNAPI_BUILTIN=OFF
-Donnxruntime_BUILD_JAVA=OFF -Donnxruntime_BUILD_CSHARP=OFF -Donnxruntime_BUILD_OBJC=OFF
-DFLATBUFFERS_BUILD_FLATC=OFF
-DONNX_CUSTOM_PROTOC_EXECUTABLE=<宿主 protoc（protobuf v21.12 现场编）>
-Donnxruntime_CMAKE_DEPS_MIRROR_DIR=<mirror>
-DCMAKE_AR=<NDK>/llvm/bin/llvm-ar      -DCMAKE_RANLIB=<NDK>/llvm/bin/llvm-ranlib
```

## 构建收尾
[100%] Linking CXX shared library libonnxruntime.so
[100%] Built target onnxruntime

## 环境差异（本次手工补齐，见文档 §24.5）
① 宿主 clang/clang++（用 OHOS SDK 自带的宿主 x86_64 clang）
② 宿主 cmake 3.28（SDK 自带 build-tools/cmake）
③ PATH 里要有 llvm-ar / llvm-ranlib
④ _get_ohos_toolchain() 需要 OHOS_NDK_TOOLCHAIN_FILE 兜底
⑤ onnx-populate 的 patch step 幂等性坑（GNU patch 报 already applied）
