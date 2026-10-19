# M6 ORT 构建与缓存（OHOS / arm64-v8a）

> 状态（2026-10-18，分支 `feat/B8c-ort-ci`，评审 #126 **第四轮**修复后）：
> **缓存命中路径、前置检查、以及完整的 CMake configure 已在本机实测**；
> **冷缓存的全量 `cmake --build`（~24 min 编译）仍没有跑过**。
>
> 第四轮修复（1 个阻塞 + 6 个小问题）：**B2 自愈删除的作用域**收窄到当前 ref / PR base /
> 默认分支，逐 ref `gh cache list --ref` + 删除前记录 `ref + key + id`，**绝不删其它
> `refs/pull/*/merge`**（§6.5 / §9.3）；`--check-cache` 区分 exit 2（用法/工具错误，不删）
> 与 exit 4（缓存损坏）；`cache_ok` 输出删除；`labeled/unlabeled` 只认 `ci:ohos-ort` 本身；
> CLT 的 cmake smoke 降为 warning（不是所有 CLT 都带 cmake）；修掉重复的 `## 10` 标题。
>
> 第三轮（已闭合）：① **B1** zip 改用 `unzip`/`bsdtar` 解压（`python3 -m
> zipfile` 不恢复可执行位/符号链接），解压后 smoke clang，`build-ort.sh` 的 preflight
> 从 `-e` 改成 `-x`；② **B2** 新增 `build-ort.sh --check-cache`（只验不编），缓存 key 在
> gate 里自算并传给 restore/delete，`cache-heal` 是唯一有 `actions: write` 的 job；
> ③ **B3** 压缩包 magic 从 `-N4` 改成 `-N6`，xz 分支能命中。
>
> 第二轮（已闭合）：`run_build` dispatch 输入、`timeout-minutes: 240`、拆出独立 workflow
> [`ohos-ort.yml`](../../.github/workflows/ohos-ort.yml)（`check.yml` 不再有 ORT job）。
>
> 本轮**有真实 CI run**：skip 与 fail-fast 两条路径都实跑过（run URL 见 §9.2）；
> 但**成功的冷缓存全量构建仍未在 CI 跑过**（见 [§8](#8-没做到的如实)）。
>
> 相关：[鸿蒙可行性探针](m6-鸿蒙可行性探针.md)（§4 / §23 / §24.5 / §25）、
> [`tools/ohos/build-ort.sh`](../../tools/ohos/build-ort.sh)、
> [`tools/ohos/ort_source_edits.py`](../../tools/ohos/ort_source_edits.py)、
> [`.github/workflows/check.yml`](../../.github/workflows/check.yml)、
> [`.github/workflows/ohos-ort.yml`](../../.github/workflows/ohos-ort.yml)。

## 0 一句话

`tools/ohos/build-ort.sh` 一条命令产出 `libonnxruntime.so`：**先查缓存**（manifest
sha256 + ELF 自检，命中约 0.1–0.3 s），**未命中才完整构建**（~33 min / 16 核，
4 核约 1.5–2 h）。产物只落 `$HOME/dev/caches/ort-1291/`，**不进仓库**。CI 里是一个
**独立的可选 workflow** [`ohos-ort.yml`](../../.github/workflows/ohos-ort.yml)：
`ohos-ort-cache` 是秒级 gate，`ohos-ort` 才构建；**未配置 `vars.OHOS_CLT_URL` 时，
普通 PR 与不勾 `run_build` 的 dispatch 都是 skipped，不会报红**，gate 会留 notice
说明它没跑（§6）。

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

# 只验缓存（不下载、不编译）；坏缓存以 4 退出，CI 的缓存自检用它
tools/ohos/build-ort.sh --check-cache

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
| `ORT_FORCE_BUILD` | `0` | `1` = 忽略缓存强制重建（等价 `--force-build`；CI 的 `force_build` 输入也传 `--force-build`） |

退出码：`0` 成功；`2` 前置检查/参数错误；`3` 下载失败；`4` configure/build/自检失败
（`--check-cache` 时也表示缓存不存在/不可用）。

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
   - `SONAME` 为 `libonnxruntime.so.1`（**脚本会断言**，不只是打印；评审 #126 建议项 5）；
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
| 想刷新 CI 缓存 | **Actions 缓存不可变**：同 key 不会覆盖。bump `vars.ORT_CACHE_REV` 让 key 变掉再重跑（§6.4）；旧缓存用 `gh cache delete` 清 |
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

### 6.1 为什么是独立 workflow（评审 #126 第二轮阻塞 3）

ORT 构建是长任务（16 核 33 min，4 核 1.5–2 h）。上一版把它放在 `check.yml` 里，与
`check` 的 **run 级**并发策略共享同一个组：

```yaml
concurrency: {group: check-${{ github.ref }}, cancel-in-progress: true}
```

后果：同一 PR 每 push 一次就取消上一次整个 run；`push: main` 时一次 merge 也会取消
main 上正在跑的手动预热。`actions/cache` 的 post-step 在 **cancelled** 的 run 里不会
保存 ⇒ 配了 `OHOS_CLT_URL` 之后，活跃 PR 上的 `ohos-ort` 会长期 `cancelled`，缓存永远
存不下来。这比 skipped 更糟：它看起来像失败。

实测佐证：`push main` 的 run **39135398152** 结论就是 **cancelled**
（<https://github.com/xingxue-ux/unwritten-diary/actions/runs/39135398152>）。

**job 级 `concurrency` 修不了这个问题**（run 级取消在 job 开始前就生效），所以拆成独立
workflow [`.github/workflows/ohos-ort.yml`](../../.github/workflows/ohos-ort.yml)：

```yaml
concurrency:
  group: ohos-ort-${{ github.ref }}
  cancel-in-progress: false
```

- 与 `check-*` 完全分开，长构建不会再被 PR push / merge main 连坐取消；
- 同一 PR 连续 push 是**排队**（`cancel-in-progress: false`），不是互相取消；
- `check.yml` 里**不再有** `ohos-ort` / `ohos_ort_cache` 两个 job（只留 check 的常规检查）。

| workflow | job | 作用 | 必需检查 |
|---|---|---|---|
| `check.yml` | docs / dart / flutter / rust / bridge / probe / platform-host | 常规门禁 | 是 |
| `ohos-ort.yml` | `ohos-ort-cache` | 轻量 gate：checkout 只为算脚本 sha；判定跑不跑，留 notice + Summary | 否 |
| `ohos-ort.yml` | `ohos-ort-cache-check` | 恢复产物缓存 + `build-ort.sh --check-cache`（**只验不编**，不需要 NDK） | 否 |
| `ohos-ort.yml` | `cache-heal` | 只在缓存损坏时运行；**唯一** `actions: write` 的 job，只删缓存、不 checkout | 否 |
| `ohos-ort.yml` | `ohos-ort` | 真正构建/复用 ORT 产物 | **否，不要勾进 required checks** |

权限：workflow 级只有 `contents: read`；`actions: write` 单独开在 `cache-heal`
（建议项 1），所有 checkout 都带 `persist-credentials: false`。

### 6.2 触发条件与三态

```yaml
enabled      := vars.OHOS_CLT_URL != '' || inputs.run_build == true
should_build := enabled && (非 PR || 同仓库 PR && 打了 ci:ohos-ort label)
```

- `run_build` 是 `workflow_dispatch` 的 boolean 输入、**默认 false**：普通 dispatch（不勾）
  在未配置变量时 ⇒ **skipped**；勾了 ⇒ 真跑（失败也只给指引，不阻塞合入）。
- 普通 PR：`pull_request` 只在 `tools/ohos/**`、`probes/ohos-*/**`、
  `.github/workflows/ohos-ort.yml` 变更时触发；未配置变量时 gate 绿 + notice、`ohos-ort`
  skipped。
- PR 还要 `ci:ohos-ort` label：把「能自举」与「每个 PR 都跑」拆开（建议项 6）；fork PR
  一律不跑（`head.repo.full_name == github.repository`）。label 先建一次：
  `gh label create ci:ohos-ort --description "在 CI 上真跑 ORT 构建（1.5–2 h）"`.
- **push main 不触发本 workflow**：main 的缓存预热靠手动 dispatch（见 §6.5）。
- `pull_request.types` 含 `labeled` 与 `unlabeled`，但 gate **只认 `ci:ohos-ort` 本身**
  （`github.event.label.name == 'ci:ohos-ort'`）：打/撤 `ci:ohos-ort` 都会留一条记录；
  给 PR 打/撤别的 label 不会把整条流水线（含 1.5–2 h 构建）再跑一遍。撤 `ci:ohos-ort`
  时是 skip，不会取消已经开始的构建——构建的取消由 run 级 concurrency 管。

逐场景推演（三种结果都不阻塞合入）：

| # | 事件 | `OHOS_CLT_URL` | `run_build` | `ci:ohos-ort` | gate | `ohos-ort` | 用户看到 |
|---|---|---|---|---|---|---|---|
| 1 | 普通 PR（命中 paths） | 空 | — | 无 | false | skipped | gate 绿 + notice「未配置 URL，跳过」 |
| 2 | 普通 PR（未命中 paths） | 任意 | — | — | — | 不触发 | PR 上没有这个 workflow |
| 3 | 普通 PR + label | 空 | — | 有 | false | skipped | gate 绿 + notice（label 单独不放宽开关） |
| 4 | 普通 PR + label | 有 | — | 有 | true | 跑 | 命中缓存秒级；未命中下载 NDK 后 1.5–2 h |
| 5 | 普通 PR | 有 | — | 无 | true | skipped | gate 绿 + notice「需要 ci:ohos-ort label」 |
| 6 | fork PR | 有 | — | 有 | true | skipped | gate 绿 + notice「fork PR 不跑」 |
| 7 | `workflow_dispatch` 不勾 | 空 | false | — | false | skipped | gate 绿 + notice（**评审 #126 要的证据**） |
| 8 | `workflow_dispatch` 勾 run_build | 空 | true | — | true | 跑 | 无 NDK/URL/缓存 ⇒ fail-fast 红 + 三条自举指引 |
| 9 | `workflow_dispatch` | 有 | 任意 | — | true | 跑 | 同上；有缓存则秒级 |
| 10 | 缓存命中但自检失败 | 任意 | — | — | true | 跑（自愈） | `cache-check` 只验不编 ⇒ `cache-heal` 删该 key ⇒ 构建 job restore miss ⇒ 重建；无 NDK ⇒ 报「缓存损坏」而不是「没有 NDK」 |
| 11 | 构建/下载/自检失败 | 任意 | — | — | true | 红 | 失败；不是必需检查，不阻塞合入 |

> 第 3 行是有意的：label 是「同意在 CI 上花 1.5–2 h」的开关，但**不能**代替
> `OHOS_CLT_URL` / `run_build`——否则一个 label 就能在没有 NDK 的 runner 上稳定报红。

### 6.3 timeout 与 runner 规格（评审 #126 第二轮阻塞 2）

- `ohos-ort` 的 `timeout-minutes` 是 **240**（上一版 90 不够）；
- **4 核 `ubuntu-latest` 冷缓存约 1.5–2 h**（按 16 核实测 33 min 的核数粗算，**没有实测**）；
- 因此：**不推荐在 4 核 hosted 上自举**；**≥8 核（或带 NDK 的自托管 runner）才建议跑全量**。
  这句话同时写进了 gate 的 Step Summary（每次运行都能看到）；
- 16 核实测 33 min 的拆解见 §4；240 min 留的是 4 核慢路径的余量，不是正常耗时。

### 6.4 仓库变量

| 变量 | 默认 | 作用 |
|---|---|---|
| `vars.OHOS_CLT_URL` | 空 | NDK 下载地址。**非空 = 允许 `ohos-ort` 运行**（PR 仍需 label）；空 = 只有勾 `run_build` 才跑 |
| `vars.OHOS_CLT_SHA256` | 空 | 下载包的 sha256。**配了 URL 就必须配它**：为空直接 fail-fast，不再 warning 后解压（建议项 1）；同时是缓存 key 的 NDK 维度 |
| `vars.OHOS_RUNNER` | `ubuntu-latest` | `ohos-ort` 的 runner label；有 NDK 的自托管 runner 填 `self-hosted` |
| `vars.OHOS_NDK` | 脚本默认路径 | 自托管 runner 上 OHOS NDK 的 `native` 目录（与 `OHOS_NDK_REV` 一起构成缓存 key 的 NDK 维度） |
| `vars.OHOS_NDK_REV` | `v1` | 自托管 NDK 的版本标识。默认路径下光看路径是常量，原地升级 NDK 时 bump 它让 key 变掉（建议项 3） |
| `vars.ORT_CACHE_REV` | `v1` | 缓存代次；bump 它让 key 变掉，用于刷新/作废旧缓存（§6.5） |

> 为什么 SHA256 是强制的：压缩包里的 clang（以及包内可能自带的 cmake）会在 runner 上
> **执行**，产物还会写进 Actions 缓存。只 warning 等于把「供应链校验」降级成装饰（建议项 1）。
> 为什么 URL 不进日志：仓库变量不是 secret，带 token 的地址会进公开日志；工作流里
> `::add-mask::` 之后只打印 host（建议项 2）。

### 6.5 缓存 key、自愈与预热

key 不再用 `hashFiles()`，而是在 `ohos-ort-cache` 里**算一次**、通过 job 输出同时传给
`actions/cache` 的 restore 和 `gh cache delete`（两边必须完全一致；评审 #126 B2）：

```text
cache_key = ort-${ORT_VERSION}-${RUNNER_OS}-${build_sha}-ndk${ndk_id}-${ORT_CACHE_REV:-v1}
build_sha = sha256(cat tools/ohos/build-ort.sh tools/ohos/ort_source_edits.py)
ndk_id    = sha256(vars.OHOS_CLT_SHA256 或 "视 OHOS_NDK/OHOS_NDK_REV 而定")[:12]
```

- **ORT 版本** + **构建脚本内容哈希** + runner OS + **NDK 维度** + **缓存代次**；
- NDK 维度 `ndk_id` = `vars.OHOS_CLT_SHA256`（优先）；自托管用
  `"${vars.OHOS_NDK}:${vars.OHOS_NDK_REV}"` 的短 sha256：换 CLT / 换 NDK 路径 / bump
  `OHOS_NDK_REV` 都会换 key（建议项 3）；
- `ORT_CACHE_REV` 未设置时为 `v1`；**Actions 缓存不可变**，要刷新产物必须 bump 它；
- 缓存里只有 `ort-1291/{libonnxruntime.so,manifest.json}`（约 20 MB），不缓存 3 GB 构建树；
- 命中时只跑 `build-ort.sh --check-cache`（复制 + 自检，秒级，不需要 NDK）——**不编译**；
  这是 B2 的关键：普通 full 模式在坏缓存 + 有 NDK 时会直接重建并 `exit 0`，自愈永远不触发；
- **缓存损坏自愈**（B2）：`ohos-ort-cache-check` 用 `--check-cache` 判定并输出
  `cache_broken`——**只有 exit 4（缓存不存在/不可用）才算**；exit 2（参数/工具链错误）
  或其它退出码不是缓存损坏，`cache-heal` 不会运行（cache-check 直接红），避免误删。
- **删除的作用域**（评审 #126 第四轮 B2）：`cache-heal`（唯一 `actions: write`）只列/删
  三个 ref，去重后：
  1. 当前 run 的 `$GITHUB_REF`（PR 是 `refs/pull/<N>/merge`，dispatch 是 `refs/heads/<ref>`）；
  2. PR 的 base：`refs/heads/$GITHUB_BASE_REF`（非 PR 不列）；
  3. 默认分支：`refs/heads/<default_branch>`。

  对每个 ref 执行 `gh cache list --ref <ref> --key <key> --json id,key,ref`，客户端再按
  `key` 与 `ref` **精确**过滤（`gh --key` 是前缀匹配），**删除前把 `ref + key + id` 打进
  日志**，再 `gh cache delete <id>`。**不会**不带 `--ref` 全仓库列 key，也**绝不**删其它
  `refs/pull/*/merge`：一个 PR 只可能恢复 base/默认分支和它自己的 merge ref，别的 PR 的
  ref 不可能造成这次失败；删了只会让它们下次 miss（配了 CLT URL 白跑 1.5–2 h，没配就
  fail-fast）。`gh cache list` 失败时**不猜、不删**；三个 ref 里都没有该 key 时只 warning。
  随后构建 job 重新 restore ⇒ miss ⇒ 重建 ⇒ post-step 把新产物写回同名 key。
  没有 `actions: write` 或删不掉时会 warning，提示 bump `ORT_CACHE_REV`；脚本侧也会把
  「缓存损坏」与「没有 NDK」分开报（`build-ort.sh` 的 `CACHE_UNUSABLE_REASON`）；
- **CLT 下载/解压缓存**（建议项 4）：`~/ohos-clt` 按 `ohos-clt-v2-<os>-<OHOS_CLT_SHA256>`
  缓存（v2 = 用 unzip/bsdtar 解压，避免复用旧的坏缓存）。命中后仍要 smoke clang
  （cmake 只 warning：不是所有 CLT 都自带 `build-tools/cmake`，`build-ort.sh` 会退回
  PATH 上的 cmake；clang smoke 失败才删掉重解压）；托管 runner 上省一次 1–2 GB 下载；
- **zip 的坑**（B1）：DevEco 的 linux-x64 CLT 多为 `.zip`。`python3 -m zipfile` 解压
  **不恢复可执行位、不建符号链接**，`llvm/bin/*`、`build-tools/cmake/bin/*` 会变成 0644，
  CMake 调 clang 才 `Permission denied`。现在一律用 `unzip`（退回 `bsdtar`），解压后
  跑一次 `clang --version` + `cmake --version`；`build-ort.sh` 的 preflight 也从 `-e`
  改成 `-x`，在构建前就报「存在但不可执行」；
- 压缩包按 magic 分派（`od -N6`）：zip / tar.gz / tar.xz 都支持（B3：xz 的 magic 是
  6 字节 `fd377a585a00`，`-N4` 会永远落到「未知格式」）。

**在自托管 runner / 本机预热缓存并上传**

`gh cache` 只能 `list` / `delete`，没有 `upload`：缓存只能由一次成功的 workflow run 在
`actions/cache` 的 post-step 里保存。

**A. 有 NDK 的自托管 runner（推荐）**

1. 注册 runner、打 label（如 `self-hosted`）；装 Command Line Tools，NDK 在
   `~/command-line-tools/sdk/default/openharmony/native`（或用 `vars.OHOS_NDK` 指定）。
2. 仓库变量 `vars.OHOS_RUNNER=self-hosted`。
3. Actions → **`ohos-ort`** → Run workflow：**勾 `run_build`**（没有 URL 时这是唯一开关），
   分支选 `main`（缓存才对 PR 可见）；等价的 CLI：
   `gh workflow run ohos-ort.yml --ref main -f run_build=true`。
4. 缓存未命中 → 33 min（16 核）构建 → post-step 保存 `~/.cache/dsh-ort`。

**B. 没有自托管 runner：hosted + `OHOS_CLT_URL`**

1. 设 `vars.OHOS_CLT_URL` 指向 Command Line Tools（linux-x64）压缩包，**同时**设
   `vars.OHOS_CLT_SHA256`。
2. Actions → `ohos-ort` → Run workflow：有 URL 时不用勾 `run_build`；分支选 `main`
   （`gh workflow run ohos-ort.yml --ref main`）。
3. 首次未命中：下载、校验、按 magic 解压 NDK → 构建 → 保存缓存。
4. 4 核 hosted 约 1.5–2 h，**不推荐**；≥8 核 runner 才建议自举。

**C. 本机已编好，搬到自托管 runner 再上传**

1. 本机 `tools/ohos/build-ort.sh --force-build`（或命中本机缓存），产物在
   `$HOME/dev/caches/ort-1291/`。
2. 拷到自托管 runner 的 `~/.cache/dsh-ort/ort-1291/`。
3. dispatch（勾 `run_build` 或用 URL）：restore miss 不会清掉已有文件，脚本直接命中，
   post-step 上传。

**刷新 / 作废旧缓存**

1. bump `vars.ORT_CACHE_REV`（如 `v1` → `v2`）；
2. 可选：dispatch 勾 `force_build`（未配 `OHOS_CLT_URL` 时需同时勾 `run_build`；即使 key
   意外命中，也强制重编）；
3. 新 key 的 run 成功后，旧缓存用 `gh cache list` / `gh cache delete <id|key>` 清理。

> 只勾 `force_build` 而不 bump `ORT_CACHE_REV` 不会更新缓存（key 已存在、缓存不可变）。
> 它是「验证脚本能重建」和配合新 key 刷新的开关，不是单独的刷新开关。

### 6.6 为什么不做成必需检查

- 缓存未命中 = 33 min（16 核）/ 1.5–2 h（4 核），比现有最长的 job（30 min）还长；
- hosted runner 天生没有 NDK，首次必须人预热/配置——把「没预热」变成「阻塞合入」
  是把基础设施缺位变成开发者的红灯；
- 未配置时是 **skipped**（不是绿也不是红）；不要把它勾进 required checks，否则一个
  「本来就不跑」的 job 会变成分支保护里的噪声；
- 它的价值是**回归保护**（脚本/补丁改坏时能发现），而不是每次 PR 的必经门禁。

## 7 与探针 / 产品的关系

- `probes/ohos-bge-smoke/scripts/build-native.sh`：ORT 步骤改成调用
  `tools/ohos/build-ort.sh --dest entry/libs/arm64-v8a`，不再自己找/复制 ORT；
- `probes/ohos-core-smoke/scripts/build-native.sh`：默认也走自编 1.29.1；
  `ORT_FLAVOR=sherpa-1.16.3` 保留历史快路（只用于复现「1.16.3 也能 dlopen」），
  缓存从 `/tmp` 挪到 `$HOME/dev/caches/sherpa-ort-1.16.3`；
- HAP 里仍然必须同时带 `libc++_shared.so`（ORT 的 NEEDED 里有它）；
- 19.6 MB 的 `.so` **不进仓库**（`.gitignore` 已忽略 `entry/libs/`，缓存目录在 `$HOME`）。

## 8 没做到的（如实）

1. **没有在 CI 上跑过冷缓存全量构建，也没有实跑「缓存命中 → 损坏 → 自愈」**：
   本轮实跑了 skip 与 fail-fast 两条路径（run URL 见 §9.2），但**没有一次「从零到 `.so`」
   的成功构建**：4 核 hosted 约 1.5–2 h，仓库没有 `OHOS_CLT_URL`，也没有 ≥8 核自托管 runner。
   因此这些仍然**没有在 GitHub 上实跑**：
   `actions/cache` 命中后的**保存**、CLT 解压缓存命中/保存、CI 里的 NDK 下载与 zip/tar.gz
   解压分支、`cache-heal` 的 `gh cache delete`、`vars.OHOS_RUNNER` 选 runner。
   `cache-heal` 的**作用域逻辑**（三个 ref、只删该删的）用假 `gh` + 真实 YAML 里的
   run 块在本地跑过三种事件（§9.3），但**没有**在 runner 上真的删过一次 Actions 缓存。
   已实跑的：`skipped` 呈现、`::notice`、gate Step Summary、`needs` 输出传递、
   `ohos-ort-cache-check` 的 cache miss 路径、fail-fast 的报错与指引（见 §9.2）。
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

前置检查失败的样子（`NDK=/nonexistent/native`，并把 `ORT_CACHE_DIR` 指到不存在的目录，
确保是「无 NDK 无缓存」）：

```console
$ NDK=/nonexistent/native ORT_CACHE_DIR=$PWD/.scratch-no-cache tools/ohos/build-ort.sh --check-only; echo $?
!! 找不到 OHOS NDK：/nonexistent/native
!!   期望目录：/nonexistent/native/llvm/bin/aarch64-unknown-linux-ohos-clang
!!   获取方式（任选其一）：
!!     1) DevEco Studio → SDK Manager → Native（…）
!!     2) 下载 Command Line Tools（linux-x64）解压到 ~/command-line-tools（…）
!!     3) 显式指定：NDK=/path/to/native tools/ohos/build-ort.sh --force-build
!!   只想用已有缓存（不需要 NDK）：去掉 --force-build 再跑一次
2
```

### 9.1 评审 #126 第三轮的本地验证（非 CI）

```console
$ python3 tools/check_docs.py
[通过] 关键文件、Markdown 相对链接、.github YAML、共用场景 JSON 均正常。
$ # 两个 workflow 的所有 step.run 交给 bash -n：40 个 run 块，0 失败
$ tools/ohos/build-ort.sh --check-cache          # 好缓存：exit 0
== 缓存检查通过（--check-cache，未编译），用时 0s
$ ORT_CACHE_DIR=<截断的缓存> tools/ohos/build-ort.sh --check-cache; echo $?
== 缓存 sha256 与 manifest 不一致（…）→ 视为未命中
!! 缓存不可用（--check-cache）：文件 sha256 与 manifest 不一致（缓存损坏）；未编译
4
```

gate 脚本 8 种输入、`cache_key` 计算（脚本 sha + ndk_id + rev）在本机 bash 里逐条跑过；
`cache_key` 在同一组输入下稳定，改 `OHOS_NDK_REV` 会让 `ndk_id`/key 变化。

**B1：zip 可执行位（能红/能绿）**

- 造一个 zip，**里面记录的是 0755**（`zip -r`）；分别用两种方式解压：
  - `python3 -m zipfile -e` → `llvm/bin/aarch64-unknown-linux-ohos-clang` 是 **0644**；
    用它跑 `build-ort.sh --check-only`（新的 `-x` 检查）：
    `!! NDK 里的文件存在但不可执行：…/llvm/bin/aarch64-unknown-linux-ohos-clang`，
    `!! NDK 不完整（见上）`，**exit 2**（修复前 `-e` 会放行，随后 CMake 才 Permission denied）；
  - `unzip -q` → **0755**；`--check-only`：`== 前置检查通过`，**exit 0**。
- 再造一个 zip，**里面记录的就是 0644**（坏包）：`unzip` 解压后 smoke 直接报
  `clang 不存在或不可执行 …` + `::error::Command Line Tools 解压后 clang 不可执行`，
  **exit 1**——不等到 CMake。
- 第四轮把 cmake 从硬失败降为 warning 后补测：zip 里有可执行 clang、**没有**
  `build-tools/cmake/bin/cmake` ⇒ 只有 `::warning::CLT 包内没有可用的 cmake …`，
  `clang 自检：…`，**exit 0**（`build-ort.sh` 会退回 PATH 上的 cmake）；把 clang 也去掉
  ⇒ 仍 `::error::…clang 不可执行`，**exit 1**。
- 用 `file://` 跑完整的「准备 NDK」步骤（下载 → sha256 → unzip → smoke → 写 `NDK=`）：
  zip 路径 exit 0；再跑一次命中 `~/ohos-clt` 解压缓存：`复用 CLT 解压缓存`，exit 0。

**B2：`--check-cache`（能红）**

见上面的 console：好缓存 exit 0；截断 `.so` 后 exit **4**，报「缓存不可用（--check-cache）：
文件 sha256 与 manifest 不一致（缓存损坏）；未编译」——**不是**「没有 NDK」。普通 full 模式
在坏缓存 + 有 NDK 时会重建并 exit 0（自愈不触发），所以 CI 的 verify 必须用 `--check-cache`。

**B3：xz magic（能红/能绿）**

```text
$ od -An -tx1 -N4 clt.tar.xz   → fd377a58      → 旧 case 落到「未知格式」（缺陷）
$ od -An -tx1 -N6 clt.tar.xz   → fd377a585a00  → 命中 tar.xz 分支
```

再用 `file://` 跑完整「准备 NDK」步骤走 tar.xz：解压、smoke、`NDK=` 都通过（exit 0）。

**其他本地检查**

- `host` 打印剥离 `user:pass@`：`https://user:s3cret@example.com:8443/...` → `host=example.com:8443`；
- `cache-heal` 的 ref 作用域（当前 ref / PR base / 默认分支）用假 `gh` + 真实 run 块
  跑过 PR、push main、dispatch 与「假 gh 无视 --ref」四组，见 §9.3；没有真实缓存可删。

> 这验证的是 gate/key 逻辑、YAML/bash 语法、zip 权限与 smoke、xz 分支、`--check-cache`
> 语义；`actions/cache` 实际命中/保存、CLT 缓存、`gh cache delete` 仍没有实跑（§8.1）。

### 9.2 CI 实跑证据（2026-10-18，第三轮 commit 985648c）

| run | 事件 | 结论 | 实际看到什么 |
|---|---|---|---|
| <https://github.com/xingxue-ux/unwritten-diary/actions/runs/39141196132> | `pull_request`（本 PR 命中 paths） | success | `ohos-ort-cache` 绿；`ohos-ort-cache-check` / `cache-heal` / `ohos-ort` 全部 **skipped**（未配置变量，自动跳过） |
| <https://github.com/xingxue-ux/unwritten-diary/actions/runs/39141226334> | `workflow_dispatch`，**不勾** `run_build` | success | `ohos-ort-cache` 绿 + notice「未配置 vars.OHOS_CLT_URL 且没有勾 run_build ⇒ ohos-ort skipped」；后三个 job **skipped** |
| <https://github.com/xingxue-ux/unwritten-diary/actions/runs/39141323075> | `workflow_dispatch`，**勾** `run_build=true` | failure（**预期**） | `ohos-ort-cache` 绿；`ohos-ort-cache-check` 走完 **cache miss** 路径（`--check-cache` 未执行）；`cache-heal` skipped；`ohos-ort` 在「准备 NDK」fail-fast：`##[error] … 1.5–2 h 的构建跑不了` |

dispatch 用的是 `gh workflow run ohos-ort.yml --ref feat/B8c-ort-ci -f run_build=<bool>`。
`workflow_dispatch` 通常要求 workflow 文件在默认分支上才有 UI 按钮；本次是 PR 的
`pull_request` run 先把 workflow 注册进去之后，CLI 才能按分支 dispatch。合并到 main
之后 UI 的 "Run workflow" 会出现。

> 缓存命中后的 `--check-cache` 成功路径、`cache-heal` 删除、CLT 缓存、冷缓存全量构建
> 仍然**没有**在 CI 上实跑；见 §8.1。

### 9.3 评审 #126 第四轮：cache-heal 的 ref 作用域（本地，假 gh）

用 PyYAML 读出 `.github/workflows/ohos-ort.yml` 里 `cache-heal` 的**真实 `run` 块**执行，
PATH 上放一个假 `gh`（记录每次参数；数据集故意包含另一个 PR 的 `refs/pull/84/merge`、
`refs/heads/other-branch`，以及前缀 key `...-old`）。三种事件 + 一组对抗：

```console
=== PR #82（base=main） ===
会 list 的 ref：refs/pull/82/merge | refs/heads/main
每条 list 都带 --ref：是
会删的 id：101 102 204
删除日志：
  删除缓存：ref=refs/pull/82/merge key=ort-1.29.1-…-v1 id=101
  删除缓存：ref=refs/heads/main key=ort-1.29.1-…-v1 id=102
  删除缓存：ref=refs/heads/main key=ort-1.29.1-…-v1 id=204

=== push main ===
会 list 的 ref：refs/heads/main
每条 list 都带 --ref：是
会删的 id：102 204

=== workflow_dispatch（分支 feat/B8c-ort-ci） ===
会 list 的 ref：refs/heads/feat/B8c-ort-ci | refs/heads/main
每条 list 都带 --ref：是
会删的 id：103 102 204

=== 对抗：假 gh 无视 --ref 返回全部 ===
会 list 的 ref：refs/pull/82/merge | refs/heads/main
会删的 id：101 102 204
```

`refs/pull/84/merge`（另一个 PR）从未进入 `gh cache list --ref` 的参数，id=201 从未被删除；
`refs/heads/other-branch` 与 `...-old`（id=203）同样没有进入删除。对抗组证明：即使服务端
忽略 `--ref`，客户端按 `key + ref` 的精确过滤仍只会删本 run / base / 默认分支的条目。

**没验证的**：真实的 `gh cache list --ref` / `gh cache delete <id>` 没有在 runner 上跑过
（仓库里没有可删的真实坏缓存）；验证的是参数、日志与作用域逻辑，不是 GitHub API 行为。

## 10 需要拍板的点

1. **CI 的「绿」语义**：已按评审 #126 改成「未配置就 **skipped**，由 `ohos-ort-cache`
   留 notice」，不再把「没预热」变成普通 PR 的红灯；`run_build` 让手动验证可以显式真跑。
   还需要拍板的是：要不要配 `OHOS_CLT_URL` + `OHOS_CLT_SHA256` 并在 main 上预热一次
   （缓存才对 PR 可见），让打了 `ci:ohos-ort` label 的 PR 能秒级验证；4 核自举 1.5–2 h
   不推荐。
2. **产物分发**：现在只有 Actions 缓存（可能被 7 天未使用清理）。要不要把
   `libonnxruntime.so` 作为 release 资产 / 内网镜像发布，让 CI 的冷启动不依赖 GitHub 缓存？
3. **core-smoke 的 sherpa 1.16.3 快路**：保留为 `ORT_FLAVOR=sherpa-1.16.3`（现状），
   还是彻底删掉、只留自编 1.29.1？
4. **ORT 升级节奏**：是否每年跟一次 minor，并把 `ort_source_edits.py` 的 re-baseline
   写进升级 runbook（现在只有硬失败提示）。
