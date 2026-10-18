# M6 ORT 构建与缓存（OHOS / arm64-v8a）

> 状态（2026-10-18，分支 `feat/B8c-ort-ci`）：
> **缓存命中路径、前置检查、以及完整的 CMake configure 已在本机实测**；
> **`cmake --build`（~24 min 的编译）与 CI job 本身没有跑过**。
> 未验证项集中列在 [§8](#8-没做到的如实)，不要把它们当成已完成。
>
> 相关：[鸿蒙可行性探针](m6-鸿蒙可行性探针.md)（§4 / §23 / §24.5 / §25）、
> [`tools/ohos/build-ort.sh`](../../tools/ohos/build-ort.sh)、
> [`tools/ohos/ort_source_edits.py`](../../tools/ohos/ort_source_edits.py)、
> [`.github/workflows/check.yml`](../../.github/workflows/check.yml)。

## 0 一句话

`tools/ohos/build-ort.sh` 一条命令产出 `libonnxruntime.so`：**先查缓存**（manifest
sha256 + ELF 自检，命中约 0.1–0.3 s），**未命中才完整构建**（~33 min / 16 核）；
产物只落 `$HOME/dev/caches/ort-1291/`，**不进仓库**；CI 里是一个**可选的**
`ohos-ort` job，用 `actions/cache` 缓存产物。

## 1 为什么必须自编

| 事实 | 证据 |
|---|---|
| 产品用的 `ort 2.0.0-rc.13` / `ort-sys` 要求 **ORT C API ≥ 17**（即 ORT ≥ 1.17） | 探针 §23.3 |
| 生态里 OHOS 预编译的 ORT **只有 1.16.3**（sherpa-onnx OHOS 包），`GetApi(17)` 返回 `NULL` | 探针 §23.2 路线 A |
| 用 1.16.3 走产品路径是**干净的 Err**，不是崩溃：`expected version >= '1.17.x', but got '1.16.3'` | 探针 §23.3 |
| 把 `ort` 降到 1.x 不是改版本号，是跨大版本 API 迁移（`ORT_DYLIB_PATH` 环境变量、`Value`/`Session` 类型都变了），且永久锁死在 2023 年的运行时 | 探针 §23.5 |
| 自编 1.29.1 已在真机上跑通产品 `ort` crate 全路径（`init_from` → session → run → tensor） | 探针 §23.1 |

所以自编不是偏好，是**版本约束下的唯一低成本解**——代价只是「首次构建 + 缓存产物」。

## 2 怎么用

```bash
# 日常：命中缓存就秒级返回（本机实测 0.10–0.25 s）
tools/ohos/build-ort.sh

# 给 HAP 打包用：复制到 entry/libs/arm64-v8a/
tools/ohos/build-ort.sh --dest probes/ohos-bge-smoke/entry/libs/arm64-v8a

# 忽略缓存强制重建（~33 min / 16 核）
tools/ohos/build-ort.sh --force-build

# 只检查构建前置条件（不下载、不编译）——CI 与排障先跑这个
tools/ohos/build-ort.sh --check-only

# 分阶段验证（本机没时间跑全量时用）
tools/ohos/build-ort.sh --prepare-only     # 源码 + 补丁 + 依赖 + 宿主 protoc
tools/ohos/build-ort.sh --configure-only   # 再走到 CMake configure
```

| 环境变量 | 默认 | 说明 |
|---|---|---|
| `ORT_VERSION` | `1.29.1` | 只内置 1.29.1 的 URL/sha256；换版本必须同时给下面两项 |
| `ORT_SOURCE_URL` | GitHub `v1.29.1.tar.gz` | 源码包；失败自动换 codeload |
| `ORT_SOURCE_SHA256` | 内置 `aba6ff91…d0f0d` | **不匹配就拒绝使用** |
| `ORT_SOURCE_DIR` | 未设 | 复用已有源码树（离线/调试），给了就不下载 |
| `ORT_CACHE_DIR` | `$HOME/dev/caches/ort-1291` | 产物 + manifest |
| `ORT_BUILD_DIR` | `$HOME/dev/build/ort-1.29.1` | 源码、依赖镜像、protoc、构建树 |
| `NDK` | `~/command-line-tools/sdk/default/openharmony/native` | OHOS NDK native 目录 |
| `JOBS` | `nproc` | 并行度 |
| `ORT_FETCH_DEPS` | `1` | 预拉 12 个 FetchContent 依赖（断网/限流时可复用） |
| `ORT_GITHUB_PROXY` | `https://gh-proxy.com/` | 依赖下载的代理前缀 |
| `HOST_CC` / `HOST_CXX` | `cc` / `c++`（没有就用 NDK 里的宿主 clang） | 编宿主 protoc |
| `ORT_PROTOC` | 未设 | 直接指定宿主 protoc，跳过编译 |

退出码：`0` 成功；`2` 前置检查/参数错误；`3` 下载失败；`4` configure/build/自检失败。

**路径纪律**：脚本不用 `/tmp`。源码、依赖、构建树、产物全部在 `$HOME`；本机 `/tmp`
被清空过一次，19.6 MB 的成品当时是靠 conan 构建树里的副本救回来的（探针 §23.2）。

## 3 缓存判定与失效

### 3.1 命中条件

缓存目录里两个文件：`libonnxruntime.so` + `manifest.json`（脚本写）。命中要求全部满足：

1. 文件存在且体积 > 1 MB；
2. 有 manifest 时：`ort_version` 等于当前 `ORT_VERSION`，且**文件 sha256 与 manifest 一致**；
   没有 manifest（例如手工放进来的成品）时：先自检，通过就**收编**并补写 manifest；
3. ELF 自检通过：
   - `file` 认得出是 aarch64 ELF；
   - `NEEDED` **只允许** `libc.so` 与 `libc++_shared.so`，且 `libc.so` 必须在；
   - `SONAME` 为 `libonnxruntime.so.1`；
   - `nm -D` 里有 `OrtGetApiBase@@VERS_1.29.1`。

> **与任务描述的一处事实差异**：成品实测 `NEEDED` 是
> **`libc++_shared.so` + `libc.so` 两条**，不是「只有 libc.so」。ORT 是 C++ 写的，
> 探针 §24.1 已实测应用进程的 linker namespace 看不到系统那份 `libc++_shared.so`，
> 必须随 HAP 带。所以自检用的是**白名单**（这两条）而不是「只允许 libc.so」；
> 出现第三条依赖会直接判失败。

自检优先用 NDK 的 `llvm-readelf` / `llvm-nm` / `llvm-strip`；没有 NDK 时退回宿主的
`readelf` / `nm` / `strip`——所以 **CI 的 hosted runner 没有 NDK 也能验证缓存命中**。

### 3.2 失效条件

| 触发 | 行为 |
|---|---|
| `ORT_VERSION` 变了 | 缓存目录与 manifest 都变，必然重建 |
| 文件被截断/篡改/拷贝坏 | sha256 或体积/符号自检失败 ⇒ 视为未命中，走构建 |
| 依赖变化（换了 NDK / 工具链） | **本地不会自动失效**：产物仍然满足接口契约（NEEDED/符号/版本），继续复用是合理的；要重编请 `--force-build` |
| 构建脚本改了 | 本地不强制失效（同上）；**CI 的 cache key 含脚本 sha256**，脚本一改就重编一次以验证脚本本身 |
| ORT 版本升级 | 需要重新 baseline `ort_source_edits.py` 的锚点（它会**大声失败**而不是静默跳过） |

为什么不缓存构建树：一个构建树 `_deps` + 中间产物约 3 GB，而产物只有 19.6 MB；
Actions 缓存按仓库 10 GB 上限算，缓存构建树不划算。源码包与 12 个依赖在
`$ORT_BUILD_DIR` 里，本地重复构建不会重下（依赖按 sha1 校验后复用）。

## 4 配方：从零构建都做什么

以下参数与探针阶段**实际编出成品的那次**一致（`probes/ohos-core-smoke/evidence/ort-1.29.1-build.md`
的 configure 选项原文）。

1. **源码**：`onnxruntime v1.29.1` 归档，sha256 固定；解压时去掉顶层目录。
2. **依赖**：解析 ORT 自带 `cmake/deps.txt`，把 12 个真正用到的依赖预拉到
   `$SRC/mirror/github.com/…`，逐个校验 sha1；`FetchContent` 优先读本地 mirror
   （`onnxruntime_CMAKE_DEPS_MIRROR_DIR`）。12 个是：abseil、protobuf、onnx、re2、
   safeint、GSL、date、eigen、cpuinfo、mp11、json、flatbuffers。
3. **宿主 protoc**：用 protobuf **v21.12** 源码在宿主上编出 `protoc`。
   交叉编译**必须**给 `ONNX_CUSTOM_PROTOC_EXECUTABLE`，否则 configure 会试图运行刚编出来的
   aarch64 protoc。约 4 min。
4. **源码编辑**：8 处可移植性修改（§5），幂等。
5. **configure**：OHOS toolchain + 全算子集。关键项：
   `-DCMAKE_TOOLCHAIN_FILE=<NDK>/build/cmake/ohos.toolchain.cmake`、
   `-DOHOS_ARCH=arm64-v8a`、`-DCMAKE_CXX_STANDARD=20`、
   `-Donnxruntime_BUILD_SHARED_LIB=ON`、`-Donnxruntime_MINIMAL_BUILD=OFF`、
   `-Donnxruntime_DISABLE_EXCEPTIONS=OFF` / `RTTI=OFF→ON`（保持异常与 RTTI）、
   `-DFLATBUFFERS_BUILD_FLATC=OFF`、`-DONNX_CUSTOM_PROTOC_EXECUTABLE=<宿主 protoc>`、
   `-DCMAKE_AR/ RANLIB=<NDK>/llvm/bin/llvm-{ar,ranlib}`。
6. **cpuinfo**：configure 之后、build 之前给 `_deps/*cpuinfo*/CMakeLists.txt` 补 OHOS
   平台分支；`cmake --build` 会检测到 CMakeLists 变化并自动重跑 configure。
7. **build**：`cmake --build <build> -j$JOBS`（Unix Makefiles，与已验证那次一致）。
8. **strip + 落缓存**：`llvm-strip --strip-unneeded`，写 manifest，自检。

实测耗时（16 核，探针 §24.5）：下载 ~4 min、宿主 protoc ~4 min、configure ~1 min、
ORT 构建 **~24 min**，合计 **~33 min**。产物：未 strip 31,958,648 B → strip 后
**19,623,736 B**，`NEEDED: libc++_shared.so, libc.so`，`SONAME libonnxruntime.so.1`，
`OrtGetApiBase@@VERS_1.29.1`。

## 5 踩过的坑（8 处源码修改逐条）

全部实现在 [`ort_source_edits.py`](../../tools/ohos/ort_source_edits.py)：**先看标记再替换**，
第二次运行打印 `already applied`。用字符串编辑而不是 `patch(1)`，是为了绕开 §5.9 的幂等坑。

| # | 症状 | 修改 |
|---|---|---|
| 1 | 没有 BF16/FP16 的 `-march`，MLAS 的 BF16 内核与声明对不上 | `adjust_global_compile_flags.cmake`：OHOS 下加 `-march=armv8.2-a+bf16+fp16` 和 `-Wno-unused-command-line-argument` |
| 2 | 鸿蒙电脑上的 toybox `patch` 不认 `--binary --ignore-whitespace`（Linux 的 GNU patch 认） | 4 个 external cmake 里统一换成 `-l`（与已验证配方一致；GNU patch 同样接受） |
| 3 | flatbuffers 的上游补丁在 OHOS 构建里不需要，且会拖慢/打断 configure | `onnxruntime_external_deps.cmake`：OHOS 下跳过 flatbuffers patch step |
| 4 | 只给个别文件加 `-march` 不够：`mlasi.h` 里的 `bfloat16_t` 成员在其它 TU 里也要一致 | `onnxruntime_mlas.cmake`：给整个 `onnxruntime_mlas` 目标加 BF16/FP16 |
| 5 | OHOS 的 musl 没有 `pthread_setaffinity_np` | `posix/env.cc`：用 `!defined(__MUSL__)` 挡掉 |
| 6 | 没有 BF16 intrinsic 时，SBGEMM kernel 的声明仍然可见，和定义对不上 | `mlasi.h`：只给两处 **kernel 声明**加 `__ARM_FEATURE_BF16`（另外 3 处同名判断是常量/结构体成员，故意不动） |
| 7 | OHOS libc++ 15.0.4 没有 C++23 的 `std::make_unique_for_overwrite` | `qlutgemm.cpp`：换成语义等价的 `std::unique_ptr<int8_t[]>(new int8_t[n])` |
| 8 | clang 15 不支持在 lambda 里捕获结构化绑定（P1091R3） | `model_editor_c_api.cc`：两处 `auto [ptr_it, ptr_inserted] = ...` 改成普通局部变量 |

两个**环境级**坑：

### 5.9 FetchContent 的补丁 step 不幂等

第二次进入同一个构建目录时，`onnx-populate` 的 patch step 会重跑，
GNU patch 报 `Reversed (or previously applied) patch detected!`，退出码 1，configure 直接挂
（探针 §24.5 的原始输出）。

本仓库的解法：`build-ort.sh` 在 PATH 最前面放一个 **patch shim**：

```bash
# 先试 patch -R --dry-run：能反着打 ⇒ 补丁已经在了 ⇒ 直接 rc=0
if "$real" -t -R --dry-run "$@" < "$spool"; then exit 0; fi
exec "$real" "$@" < "$spool"
```

它不替换 `patch` 的实现，只在「已应用」时把失败变成成功；第一次构建仍然真打补丁。
比探针阶段手工把 patch 命令改成 `cmake -E echo` 更通用——那种改法会让**新**依赖
的补丁静默不生效。

### 5.10 宿主环境：clang / cmake 3.28 / llvm-ar / PATH

- ORT 的 FetchContent 会 `find_program(patch)`，patch shim 必须在 PATH 里；
- CMake 必须是 **3.28–3.x**：protobuf 21.12 的 `cmake_minimum_required` 是 3.5，
  CMake 4.x 直接拒绝。NDK 自带 `build-tools/cmake/bin/cmake` 是 3.28.2，脚本优先用它；
- `CMAKE_AR` / `CMAKE_RANLIB` 显式指向 `<NDK>/llvm/bin/llvm-ar` / `llvm-ranlib`；
- 宿主 protoc 用宿主编译器（`cc`/`c++`；CI 上没有就用 NDK 里的宿主 x86_64 clang）。

## 6 CI 集成

### 6.1 job 与触发条件

`.github/workflows/check.yml` 里新增两个 job：

| job | 作用 |
|---|---|
| `changes` | 几秒钟的 diff job，输出 `ohos=true/false` |
| `ohos-ort` | 需要 `changes.outputs.ohos == 'true'`，或 `workflow_dispatch` |

`ohos-ort` 只在改动下面这些路径时跑（或手动触发）：

- `probes/ohos-*/**`
- `apps/ohos-*/**`
- `tools/ohos/**`
- `.github/workflows/check.yml` 本身

> 为什么多一个 `changes` job：GitHub 的 `paths` 过滤只能作用在**触发事件**上，
> 不能只过滤某个 job。用 `git diff` 自己算，避免为了一个可选 job 引入第三方
> paths-filter action。

### 6.2 缓存 key 与路径

```yaml
path: ~/.cache/dsh-ort
key: ort-1.29.1-${{ runner.os }}-${{ hashFiles('tools/ohos/build-ort.sh', 'tools/ohos/ort_source_edits.py') }}-v1
```

- **ORT 版本** + **构建脚本内容哈希** + runner OS + 一个 schema 后缀 `v1`；
- 脚本改了 key 就变：旧产物虽然接口上还能用，但应该让 CI 重编一次，验证「新脚本确实
  还能从零编出来」——这是这个 job 存在的意义；
- 缓存里只有 `ort-1291/{libonnxruntime.so,manifest.json}`（约 20 MB），不缓存 3 GB 构建树；
- 命中时 job 只跑 `tools/ohos/build-ort.sh`（复制 + 自检，秒级，**不需要 NDK**）；
- 未命中时先准备 NDK，再 `--force-build`（~33 min）。

### 6.3 GitHub hosted runner 上没有 OHOS NDK 怎么办

按优先级：

1. **自托管 runner**：装好 Command Line Tools，NDK 在
   `~/command-line-tools/sdk/default/openharmony/native`，手动 dispatch 一次；
   产物进 Actions 缓存后，hosted runner 以后都能命中。
2. **仓库变量**：设 `OHOS_CLT_URL` 指向 Command Line Tools（linux-x64）压缩包，
   建议同时设 `OHOS_CLT_SHA256`。job 会下载、校验、解压并自动找 `*/openharmony/native`。
3. **不配也行**：job 会 **fail-fast** 并往 Step Summary 写上面这段指引。
   它**不是** main 的必需检查，红着不影响合入。

### 6.4 为什么不做成必需检查

- 缓存未命中 = **33 min**，比现有最长的 job（30 min）还长，不该卡所有人的 PR；
- hosted runner 天生没有 NDK，首次必然要人预热/配置——把「没预热」变成「阻塞合入」
  是把基础设施缺位变成开发者的红灯；
- 它的价值是**回归保护**（脚本/补丁改坏时能发现），而不是每次 PR 的必经门禁。
  需要时手动 dispatch，或等它命中缓存后自然变绿。

## 7 与探针 / 产品的关系

- `probes/ohos-bge-smoke/scripts/build-native.sh`：ORT 步骤改成调用
  `tools/ohos/build-ort.sh --dest entry/libs/arm64-v8a`，不再自己找/复制 ORT；
- `probes/ohos-core-smoke/scripts/build-native.sh`：默认也走自编 1.29.1；
  `ORT_FLAVOR=sherpa-1.16.3` 保留历史快路（只用于复现「1.16.3 也能 dlopen」），
  缓存从 `/tmp` 挪到 `$HOME/dev/caches/sherpa-ort-1.16.3`；
- HAP 里仍然必须同时带 `libc++_shared.so`（ORT 的 NEEDED 里有它）；
- 19.6 MB 的 `.so` **不进仓库**（`.gitignore` 已忽略 `entry/libs/`，缓存目录在 `$HOME`）。

## 8 没做到的（如实）

1. **没有在 CI 真跑过**：只验证了 YAML 能被 `tools/check_docs.py` 解析、job 里的命令
   在本机等价可跑（缓存命中路径）。`changes` 的 diff 逻辑、`actions/cache` 的 key
   命中行为、NDK 下载分支都**没有实跑**。
2. **没有从零全量跑完**：为避免占用机器 33 min（本机另有一条 cargo 重线在跑），
   本次验证到 **CMake configure 成功**为止：
   - 源码包下载（sha256 校验）→ 解压 → 编辑（幂等，第二次全部 `already`）；
   - 编辑结果与**已验证构建树**（`~/.conan2/p/b/onnxrb1d51c7f51392/b`）逐文件对照一致
     （受影响的 10 个文件；其中 4 个只是 `--binary --ignore-whitespace → -l` 的选项替换）；
   - 12 个 FetchContent 依赖从 mirror 复用 / 补齐（sha1 校验）；
   - 宿主 protoc 用系统 gcc 编出（`libprotoc 3.21.12`），第二次运行复用；
   - `cmake` configure 在全新构建目录里 **56 s 跑完**，`Patch found` 指向我们的
     shim，BF16/FP16 `-march` 已进入 `flags.make`，cpuinfo 的 OHOS 分支已打上。
   **`cmake --build`（真正的 24 min 编译 + 链接）没有执行**，所以
   「一条命令从零到 `.so`」仍未被完整证明——差的就是最后这一步。
3. **多 ABI 未做**：只有 `arm64-v8a`；`armeabi-v7a` / x86_64 模拟器都没试。
4. **没有哈希级可复现**：ELF 的 BuildID 与时间戳未做可复现构建处理，不同机器编出的
   sha256 不保证一致；缓存判定因此用「manifest 记录的 sha256」而不是写死一个期望值。
5. **升级 ORT 版本要重新 baseline**：`ort_source_edits.py` 的锚点只对 1.29.1 成立；
   换版本会给「锚点对不上」的硬失败（不会静默产出坏二进制），但需要人工重新对一遍。
6. **宿主 protoc 没在 arm64 宿主上验证**：x86_64 宿主（WSL / GitHub runner）是验证过的路径。
7. **依赖下载依赖 GitHub**：mirror 预拉支持代理前缀，但企业内网镜像没有接。

## 9 本机验收记录（可复现）

```console
$ tools/ohos/build-ort.sh
== 缓存没有 manifest（可能是手工放进去的成品）：先自检，通过就收编
   manifest    /home/qianxia/dev/caches/ort-1291/manifest.json
== 自检（缓存命中，未编译）：
   size_bytes  19623736
   sha256      a224007497059f8a2a433b6604a22e5898bac76d7b3332bb64582df6ed9991de
   file        ELF 64-bit LSB shared object, ARM aarch64, ... stripped
   NEEDED      libc++_shared.so libc.so   （白名单：libc.so libc++_shared.so）
   SONAME      libonnxruntime.so.1
   symbol      00000000005463e8 T OrtGetApiBase@@VERS_1.29.1
== 缓存命中，用时 0s
```

第二次运行（已有 manifest）：`== 缓存命中，用时 0s`，`time` 实测 **0.10–0.25 s**。

configure 级验证（不编译；用已准备好的源码树与依赖，从零 configure 到全新构建目录）：

```console
$ ORT_SOURCE_DIR=$HOME/dev/build/ort-1.29.1/src \
  ORT_BUILD_DIR=$HOME/dev/build/ort-1.29.1 JOBS=4 \
  tools/ohos/build-ort.sh --configure-only
== 复用外部源码树：/home/qianxia/dev/build/ort-1.29.1/src
== 源码编辑就绪：0 处新应用，11 处已存在
== 依赖镜像就绪：新下载 0 个，复用 12 个
== 复用宿主 protoc：…/host-protoc-build/protoc（libprotoc 3.21.12）
== 补丁 shim：…/hostbin/patch（real patch = /usr/bin/patch）
== CMake configure（OHOS toolchain，全算子集，非 MINIMAL_BUILD）
== configure 完成
    cpuinfo 已补 OHOS 平台分支：…/build/_deps/pytorch_cpuinfo-src/CMakeLists.txt
== --configure-only 完成（未编译），用时 56s
```

configure 之后的 CMakeCache 关键项（与探针阶段实际编出成品的那次一致）：
`CMAKE_TOOLCHAIN_FILE=…/ohos.toolchain.cmake`、`OHOS_ARCH=arm64-v8a`、
`CMAKE_CXX_STANDARD=20`、`onnxruntime_BUILD_SHARED_LIB=ON`、
`onnxruntime_MINIMAL_BUILD=OFF`、`ONNX_CUSTOM_PROTOC_EXECUTABLE=…/protoc`、
`CMAKE_AR/RANLIB=…/llvm-{ar,ranlib}`。

同一次 configure 里，FetchContent 的 patch step **真的执行了**（不是只打印计划）：

```text
# _deps/date-subbuild/CMakeFiles/date-populate.dir/build.make
cd .../date-src && .../hostbin/patch -l -p1 < .../cmake/patches/date/date.patch
# 打完后戳存在：_deps/date-subbuild/.../date-populate-patch
```

也就是说 patch shim 的「真打补丁」路径被实际走过；同时 flatbuffers 的构建规则显示
`No patch step for 'flatbuffers-populate'`——说明编辑 #3（OHOS 跳过 flatbuffers 补丁）
也生效了。

前置检查失败的样子（`NDK=/nonexistent/native`）：

```console
$ NDK=/nonexistent/native tools/ohos/build-ort.sh --check-only; echo $?
!! 找不到 OHOS NDK：/nonexistent/native
!!   期望目录：/nonexistent/native/llvm/bin/aarch64-unknown-linux-ohos-clang
!!   获取方式（任选其一）：
!!     1) DevEco Studio → SDK Manager → Native（…）
!!     2) 下载 Command Line Tools（linux-x64）解压到 ~/command-line-tools（…）
!!     3) 显式指定：NDK=/path/to/native tools/ohos/build-ort.sh --force-build
!!   只想用已有缓存（不需要 NDK）：去掉 --force-build 再跑一次
2
```

## 10 需要拍板的点

1. **CI 的「绿」语义**：接受「首次必须人工预热缓存（自托管 runner 或 `OHOS_CLT_URL`）」
   吗？如果不接受，就得把 NDK 固化成自托管 runner 镜像，这超出本任务范围。
2. **产物分发**：现在只有 Actions 缓存（可能被 7 天未使用清理）。要不要把
   `libonnxruntime.so` 作为 release 资产 / 内网镜像发布，让 CI 的冷启动不依赖 GitHub 缓存？
3. **core-smoke 的 sherpa 1.16.3 快路**：保留为 `ORT_FLAVOR=sherpa-1.16.3`（现状），
   还是彻底删掉、只留自编 1.29.1？
4. **ORT 升级节奏**：是否每年跟一次 minor，并把 `ort_source_edits.py` 的 re-baseline
   写进升级 runbook（现在只有硬失败提示）。
