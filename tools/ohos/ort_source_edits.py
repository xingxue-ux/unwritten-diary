#!/usr/bin/env python3
"""对 ONNX Runtime 1.29.1 源码做 OHOS/arm64 可移植性编辑（**幂等**）。

这些编辑不是「随手改上游」，每一条都对应一次实测过的编译/链接失败（见
`docs/architecture/m6-ORT-构建与缓存.md` 的「踩过的坑」）。之所以用字符串替换
而不是 `patch(1)`：

* 上游 ORT 的 FetchContent 补丁链在**重复进入同一构建目录**时会重复打补丁，
  GNU patch 会报 `Reversed (or previously applied) patch detected` 并让 configure
  直接失败（§24.5 的幂等坑）；
* 我们的编辑只落在 8 个文件上，用「先看标记，再替换」的写法天然幂等，
  第二次运行只会打印 `already applied`。

覆盖的 8 个上游文件（与探针阶段验证过的 8 个补丁一一对应）：

1. `cmake/adjust_global_compile_flags.cmake` —— OHOS 的 BF16/FP16 `-march` 与
   `-Wno-unused-command-line-argument`；
2. `cmake/external/{abseil-cpp,eigen,xnnpack,onnxruntime_external_deps}.cmake`
   —— 把 `patch --binary --ignore-whitespace` 换成 `-l`（鸿蒙电脑上的 toybox
   patch 不认长选项；Linux/GNU patch 两者都认，换掉只为与已验证配方一致）；
3. 同文件里的 flatbuffers：OHOS 上跳过 flatbuffers 的上游补丁；
4. `cmake/onnxruntime_mlas.cmake` —— 给整个 `onnxruntime_mlas` 目标加 BF16/FP16；
5. `onnxruntime/core/platform/posix/env.cc` —— OHOS 的 musl 没有
   `pthread_setaffinity_np`，用 `__MUSL__` 挡掉；
6. `onnxruntime/core/mlas/lib/mlasi.h` —— SBGEMM BF16 内核声明用
   `__ARM_FEATURE_BF16` 保护（否则没有 BF16 intrinsic 时声明与定义对不上）；
7. `onnxruntime/core/mlas/lib/qlutgemm.cpp` —— OHOS libc++ 15.0.4 没有 C++23 的
   `std::make_unique_for_overwrite`；
8. `onnxruntime/core/session/model_editor_c_api.cc` —— clang 15 不支持在 lambda 里
   捕获结构化绑定（P1091R3），改成两个普通局部变量。

另有一个**configure 之后**的编辑（FetchContent 把 cpuinfo 解到 `_deps/` 之后才能改）：
`--patch-cpuinfo <build-dir>`，把 cpuinfo 的 `Linux/Android` 判断补上 `OHOS`。

用法：
    python3 tools/ohos/ort_source_edits.py --root <ort-1.29.1 源码根>
    python3 tools/ohos/ort_source_edits.py --patch-cpuinfo <cmake 构建目录>

退出码：0 = 全部就绪（含「本来就已经改过」）；1 = 锚点对不上（说明源码版本
不是 1.29.1，需要重新 baseline）。
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

APPLIED: list[str] = []
ALREADY: list[str] = []


class EditError(RuntimeError):
    pass


def _read(path: Path) -> str:
    if not path.is_file():
        raise EditError(f"源文件不存在：{path}（源码版本不是 1.29.1？）")
    return path.read_text(encoding="utf-8")


def _write(path: Path, text: str) -> None:
    path.write_text(text, encoding="utf-8")


def _replace_once(path: Path, old: str, new: str, what: str) -> None:
    """old→new，恰好一处；已经换成 new 则视为幂等命中。"""
    text = _read(path)
    if new in text:
        ALREADY.append(f"{path.name}: {what}")
        return
    count = text.count(old)
    if count != 1:
        raise EditError(
            f"{path}:{what}: 期望恰好 1 处锚点，实际 {count} 处 —— "
            "源码版本或上游上下文已变化，需要重新 baseline")
    _write(path, text.replace(old, new, 1))
    APPLIED.append(f"{path.name}: {what}")


def edit_global_compile_flags(root: Path) -> None:
    path = root / "cmake" / "adjust_global_compile_flags.cmake"
    text = _read(path)
    if "-march=armv8.2-a+bf16+fp16" in text:
        ALREADY.append(f"{path.name}: OHOS BF16/FP16 -march")
        return
    anchor = (
        "# The flags set by this file are applied to 3rd-party libraries(e.g. protobuf) "
        "as well. If you want to set something that is only for ORT's code, please put it "
        "in the main CMakeLists.txt file.\n"
    )
    if text.count(anchor) != 1:
        raise EditError(f"{path}: 找不到文件头注释锚点")
    block = (
        "\n"
        "# OHOS platform: add march flags for BF16/FP16 support and suppress toolchain warnings\n"
        'if (CMAKE_SYSTEM_NAME STREQUAL "OHOS")\n'
        "  add_compile_options(-march=armv8.2-a+bf16+fp16)\n"
        "  add_compile_options(-Wno-unused-command-line-argument)\n"
        "endif()\n"
    )
    _write(path, text.replace(anchor, anchor + block, 1))
    APPLIED.append(f"{path.name}: OHOS BF16/FP16 -march")


def edit_patch_long_options(root: Path) -> None:
    """toybox patch 兼容：--binary --ignore-whitespace → -l（GNU patch 同样接受 -l）。"""
    files = [
        root / "cmake" / "external" / "abseil-cpp.cmake",
        root / "cmake" / "external" / "eigen.cmake",
        root / "cmake" / "external" / "xnnpack.cmake",
        root / "cmake" / "external" / "onnxruntime_external_deps.cmake",
    ]
    old = "--binary --ignore-whitespace"
    replaced_total = 0
    for path in files:
        text = _read(path)
        count = text.count(old)
        if count == 0:
            ALREADY.append(f"{path.name}: patch 长选项已替换")
            continue
        _write(path, text.replace(old, "-l"))
        replaced_total += count
        APPLIED.append(f"{path.name}: patch 长选项 ×{count}")
    if replaced_total:
        print(f"    patch(1) 长选项替换：{replaced_total} 处")


def edit_flatbuffers_skip(root: Path) -> None:
    path = root / "cmake" / "external" / "onnxruntime_external_deps.cmake"
    text = _read(path)
    if 'if(Patch_FOUND AND NOT CMAKE_SYSTEM_NAME STREQUAL "OHOS")' in text:
        ALREADY.append(f"{path.name}: OHOS 跳过 flatbuffers 补丁")
        return
    old = (
        "if(Patch_FOUND)\n"
        "  set(ONNXRUNTIME_FLATBUFFERS_PATCH_COMMAND"
    )
    new = (
        'if(Patch_FOUND AND NOT CMAKE_SYSTEM_NAME STREQUAL "OHOS")\n'
        "  set(ONNXRUNTIME_FLATBUFFERS_PATCH_COMMAND"
    )
    if text.count(old) != 1:
        raise EditError(f"{path}: 找不到 flatbuffers 补丁命令锚点")
    _write(path, text.replace(old, new, 1))
    APPLIED.append(f"{path.name}: OHOS 跳过 flatbuffers 补丁")


def edit_mlas_bf16(root: Path) -> None:
    path = root / "cmake" / "onnxruntime_mlas.cmake"
    text = _read(path)
    marker = "target_compile_options(onnxruntime_mlas PRIVATE -march=armv8.2-a+bf16+fp16)"
    if marker in text:
        ALREADY.append(f"{path.name}: MLAS 全目标 BF16/FP16")
        return
    anchor = (
        '          set_source_files_properties(${MLAS_SRC_DIR}/erf_neon_fp16.cpp '
        'PROPERTIES COMPILE_FLAGS " -march=armv8.2-a+fp16 ")\n'
        '          set_source_files_properties(${MLAS_SRC_DIR}/gelu_neon_fp16.cpp '
        'PROPERTIES COMPILE_FLAGS " -march=armv8.2-a+fp16 ")\n'
        "        endif()\n"
    )
    if text.count(anchor) != 1:
        raise EditError(f"{path}: 找不到 erf/gelu neon_fp16 锚点（1.29.1 的上下文变了？）")
    replacement = anchor.replace(
        "        endif()\n",
        "          # OHOS: enable BF16+FP16 for the whole MLAS target.  mlasi.h exposes\n"
        "          # bfloat16_t members whenever __ARM_FEATURE_BF16 is visible, so the\n"
        "          # per-file flags above are not sufficient for every translation unit.\n"
        f"          {marker}\n"
        "        endif()\n",
    )
    _write(path, text.replace(anchor, replacement, 1))
    APPLIED.append(f"{path.name}: MLAS 全目标 BF16/FP16")


def edit_musl_affinity(root: Path) -> None:
    path = root / "onnxruntime" / "core" / "platform" / "posix" / "env.cc"
    old = (
        "#if !defined(__APPLE__) && !defined(__ANDROID__) && !defined(__wasm__) "
        "&& !defined(_AIX)\n"
    )
    new = (
        "#if !defined(__APPLE__) && !defined(__ANDROID__) && !defined(__wasm__) "
        "&& !defined(_AIX) && !defined(__MUSL__)\n"
    )
    _replace_once(path, old, new, "musl 无 pthread_setaffinity_np")


def edit_mlasi_bf16_guard(root: Path) -> None:
    """只改 SBGEMM kernel 的**声明**两处（上游还有 3 处同名判断，故意不动）。

    这两处声明一旦在没有 BF16 intrinsic 的编译单元里可见，就会与不存在的
    `MlasSbgemmKernel*` 定义对不上（链接期 / 运行期才炸）。其余 3 处是线程
    复杂度常量与 override 结构体成员，不涉及 kernel 符号，保持上游原样。
    """
    path = root / "onnxruntime" / "core" / "mlas" / "lib" / "mlasi.h"
    text = _read(path)
    new_line = (
        "#if defined(__aarch64__) && defined(__linux__) "
        "&& defined(__ARM_FEATURE_BF16)\n"
    )
    sites = [
        (
            "#else\n\n"
            "#if defined(__aarch64__) && defined(__linux__)\n"
            "typedef size_t(MLASCALL MLAS_SBGEMM_FLOAT_KERNEL)(",
            "#else\n\n"
            f"{new_line}"
            "typedef size_t(MLASCALL MLAS_SBGEMM_FLOAT_KERNEL)(",
        ),
        (
            "    MLAS_GEMM_FLOAT_KERNEL MlasSgemmKernelAdd;\n"
            "#if defined(__aarch64__) && defined(__linux__)\n"
            "    MLAS_SBGEMM_FLOAT_KERNEL MlasSbgemmKernelZero;",
            "    MLAS_GEMM_FLOAT_KERNEL MlasSgemmKernelAdd;\n"
            f"{new_line}"
            "    MLAS_SBGEMM_FLOAT_KERNEL MlasSbgemmKernelZero;",
        ),
    ]
    applied = 0
    for old, new in sites:
        if new in text:
            continue
        if text.count(old) != 1:
            raise EditError(
                f"{path}: SBGEMM 声明锚点出现 {text.count(old)} 次（期望 1）")
        text = text.replace(old, new, 1)
        applied += 1
    if applied == 0:
        ALREADY.append(f"{path.name}: SBGEMM BF16 声明保护 ×2")
        return
    _write(path, text)
    APPLIED.append(f"{path.name}: SBGEMM BF16 声明保护 ×{applied}")


def edit_libcxx_fallback(root: Path) -> None:
    path = root / "onnxruntime" / "core" / "mlas" / "lib" / "qlutgemm.cpp"
    needle = (
        "    auto lut_buffer = "
        "std::make_unique_for_overwrite<int8_t[]>(lut_buffer_size);\n"
    )
    replacement = (
        "    // OHOS: std::make_unique_for_overwrite is C++23; libc++ 15.0.4 (OHOS SDK)\n"
        "    // does not provide it.  Same semantics: default-initialised array.\n"
        "    auto lut_buffer = std::unique_ptr<int8_t[]>(new int8_t[lut_buffer_size]);\n"
    )
    _replace_once(path, needle, replacement, "libc++ 15 无 make_unique_for_overwrite")


def edit_clang15_lambda(root: Path) -> None:
    path = root / "onnxruntime" / "core" / "session" / "model_editor_c_api.cc"
    sites = [
        (
            "  auto [ptr_it, ptr_inserted] = graph->initializer_ptrs.insert(tensor);\n",
            "  // OHOS: clang 15 lacks P1091R3, so `ptr_it` below cannot be captured by the\n"
            "  // [&] lambda in ORT_CATCH.  Bind the members to ordinary locals instead.\n"
            "  auto ptr_result = graph->initializer_ptrs.insert(tensor);\n"
            "  auto ptr_it = ptr_result.first;\n"
            "  const bool ptr_inserted = ptr_result.second;\n",
        ),
        (
            "  auto [ptr_it, ptr_inserted] = graph->node_ptrs.insert(node);\n",
            "  // OHOS: see AddInitializerToGraph -- clang 15 lacks P1091R3.\n"
            "  auto ptr_result = graph->node_ptrs.insert(node);\n"
            "  auto ptr_it = ptr_result.first;\n"
            "  const bool ptr_inserted = ptr_result.second;\n",
        ),
    ]
    text = _read(path)
    applied = 0
    for needle, replacement in sites:
        if replacement in text:
            continue
        if text.count(needle) != 1:
            raise EditError(
                f"{path}: 结构化绑定锚点出现 {text.count(needle)} 次（期望 1）")
        text = text.replace(needle, replacement, 1)
        applied += 1
    if applied == 0:
        ALREADY.append(f"{path.name}: clang15 结构化绑定捕获 ×2")
        return
    _write(path, text)
    APPLIED.append(f"{path.name}: clang15 结构化绑定捕获 ×{applied}")


def apply_source_edits(root: Path) -> None:
    edit_global_compile_flags(root)
    edit_patch_long_options(root)
    edit_flatbuffers_skip(root)
    edit_mlas_bf16(root)
    edit_musl_affinity(root)
    edit_mlasi_bf16_guard(root)
    edit_libcxx_fallback(root)
    edit_clang15_lambda(root)


def patch_cpuinfo(build_dir: Path) -> int:
    """configure 之后改 FetchContent 解出来的 cpuinfo；返回 0 表示成功或无需处理。"""
    candidates = sorted(build_dir.glob("_deps/*cpuinfo*/CMakeLists.txt"))
    if not candidates:
        print(
            "!! 没找到 _deps/*cpuinfo*/CMakeLists.txt（cpuinfo 未被 FetchContent 解出？）\n"
            "   跳过 cpuinfo 的 OHOS 补丁 —— 如果后面链接/编译报 sysfs 相关错误，先查这里。",
            file=sys.stderr,
        )
        return 0
    path = candidates[0]
    text = _read(path)
    if "OHOS" in text:
        print(f"    cpuinfo 已经是 OHOS 版本：{path}")
        return 0
    original = text
    text = text.replace(
        'STREQUAL "Linux" OR CMAKE_SYSTEM_NAME STREQUAL "Android"',
        'STREQUAL "Linux" OR CMAKE_SYSTEM_NAME STREQUAL "Android" '
        'OR CMAKE_SYSTEM_NAME STREQUAL "OHOS"',
    )
    text = text.replace(
        "^(Windows|WindowsStore|CYGWIN|MSYS|Darwin|Linux|Android|FreeBSD|Emscripten)$",
        "^(Windows|WindowsStore|CYGWIN|MSYS|Darwin|Linux|Android|FreeBSD|Emscripten|OHOS)$",
    )
    if text == original:
        print(
            f"!! cpuinfo 的两处平台判断都没命中：{path}\n"
            "   上游 cpuinfo 版本可能变了；本次继续构建，但请留意 cpuinfo 的平台分支。",
            file=sys.stderr,
        )
        return 0
    _write(path, text)
    print(f"    cpuinfo 已补 OHOS 平台分支：{path}")
    return 0


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, help="ORT 1.29.1 源码根目录")
    parser.add_argument(
        "--patch-cpuinfo", type=Path, metavar="BUILD_DIR",
        help="configure 之后：给 _deps 里的 cpuinfo 补 OHOS 平台分支")
    args = parser.parse_args(argv)

    if args.patch_cpuinfo:
        return patch_cpuinfo(args.patch_cpuinfo)

    if not args.root:
        parser.error("需要 --root <源码根> 或 --patch-cpuinfo <构建目录>")

    root: Path = args.root
    if not (root / "cmake" / "CMakeLists.txt").is_file():
        print(f"!! {root} 不像 ORT 源码根（缺 cmake/CMakeLists.txt）", file=sys.stderr)
        return 1

    try:
        apply_source_edits(root)
    except EditError as exc:
        print(f"!! 源码编辑失败：{exc}", file=sys.stderr)
        return 1

    for line in APPLIED:
        print(f"    applied       {line}")
    for line in ALREADY:
        print(f"    already       {line}")
    print(f"== 源码编辑就绪：{len(APPLIED)} 处新应用，{len(ALREADY)} 处已存在")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
