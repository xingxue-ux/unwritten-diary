#!/usr/bin/env python3
"""核对 ArkTS 包装与 C ABI 是否**真的对得上**（不需要跑真机，也不需要 ArkTS 编译器）。

这一层最容易腐坏的不是代码，而是「两份手写的名字表」。逐个静态核对：

1. `DiaryCore.ets` 调用的方法名 == `crates/diary_capi/src/methods.rs` 的 `WIRED_METHODS`；
2. `DiaryError.ets` 的 `CORE_ERROR_CODES` == `crates/diary_core/src/error.rs` 里
   `ErrorCode::wire()` 的 21 个码（顺序也一致）；
3. `napi_shim.cpp` 里 `dlsym` 的符号 == `crates/diary_capi/src/lib.rs` 导出的 6 个符号；
4. `index.d.ts` 声明的函数 == shim `Init()` 里 `napi_define_properties` 注册的函数；
5. 模块名三处一致：`nm_modname` / `lib<name>.so` / types 包的 `"name"`；
6. `entry/oh-package.json5` 的 `file:` 依赖指向 types 目录，且那目录里
   `index.d.ts` + `oh-package.json5`（`types` 字段）都在（探针 §24.3 的三件套）；
7. 设计口径：JS 面**不导出** `stringFree`（C 字符串在 shim 内部复制并释放），
   `native_unavailable` 不是核心码；
8. `m6-核心-C-ABI.md` §6.1 标题里写的方法数 == `WIRED_METHODS` 的条数（文档漂移守卫：
   曾经文档写 14、实际 19，评审只能靠数表格发现）。

退出码 0 表示全部对得上；任何一条不符会逐条打印并返回 1。
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PROJECT = ROOT / "apps" / "ohos-diary-core"
CAPI = ROOT / "crates" / "diary_capi"
CORE = ROOT / "crates" / "diary_core"
SHIM = PROJECT / "entry" / "src" / "main" / "cpp" / "napi_shim.cpp"
DTS = PROJECT / "entry" / "src" / "main" / "cpp" / "types" / "libdiary_napi" / "index.d.ts"
TYPES_PKG = PROJECT / "entry" / "src" / "main" / "cpp" / "types" / "libdiary_napi" / "oh-package.json5"
ENTRY_PKG = PROJECT / "entry" / "oh-package.json5"
DIARY_CORE_ETS = PROJECT / "entry" / "src" / "main" / "ets" / "core" / "DiaryCore.ets"
DIARY_ERROR_ETS = PROJECT / "entry" / "src" / "main" / "ets" / "core" / "DiaryError.ets"
CAPI_DOC = ROOT / "docs" / "architecture" / "m6-核心-C-ABI.md"

problems: list[str] = []
notes: list[str] = []


def read(path: Path) -> str:
    if not path.is_file():
        problems.append(f"缺少文件：{path.relative_to(ROOT)}")
        return ""
    return path.read_text(encoding="utf-8")


def diff(label: str, expected: list[str], actual: list[str]) -> None:
    missing = [item for item in expected if item not in actual]
    extra = [item for item in actual if item not in expected]
    if missing:
        problems.append(f"{label}：ArkTS/shim 缺少 {missing}")
    if extra:
        problems.append(f"{label}：ArkTS/shim 多出（C ABI 没有）{extra}")
    if not missing and not extra:
        notes.append(f"{label}：{len(expected)} 项一致")


def wired_methods() -> list[str]:
    text = read(CAPI / "src" / "methods.rs")
    match = re.search(r"WIRED_METHODS:\s*&\[&str\]\s*=\s*&\[(.*?)\];", text, re.S)
    if not match:
        problems.append("methods.rs：找不到 WIRED_METHODS")
        return []
    return re.findall(r'"([\w.]+)"', match.group(1))


def core_error_codes() -> list[str]:
    text = read(CORE / "src" / "error.rs")
    match = re.search(r"impl ErrorCode \{(.*?)\n\}", text, re.S)
    if not match:
        problems.append("error.rs：找不到 impl ErrorCode")
        return []
    return re.findall(r"Self::\w+ => \"([a-z_]+)\"", match.group(1))


def capi_symbols() -> list[str]:
    text = read(CAPI / "src" / "lib.rs")
    return sorted(set(re.findall(r'pub (?:unsafe )?extern "C" fn (diary_\w+)', text)))


def shim_dlsym_symbols() -> list[str]:
    text = read(SHIM)
    return sorted(set(re.findall(r'dlsym\(h, "(diary_\w+)"\)', text)))


def shim_js_exports() -> list[str]:
    text = read(SHIM)
    match = re.search(r"napi_property_descriptor desc\[\] = \{(.*?)\n    \};", text, re.S)
    if not match:
        problems.append("napi_shim.cpp：找不到 Init() 的属性表")
        return []
    return re.findall(r'\{"(\w+)", nullptr, \w+Js', match.group(1))


def dts_exports() -> list[str]:
    text = read(DTS)
    match = re.search(r"export interface DiaryNativeModule \{(.*?)\n\}", text, re.S)
    if not match:
        problems.append("index.d.ts：找不到 DiaryNativeModule 接口")
        return []
    return re.findall(r"^\s{2}(\w+)\(", match.group(1), re.M)


def arkts_calls() -> list[str]:
    """DiaryCore.ets 里真的调了哪些方法。

    类型化包装走 `invokeData('...')`；返回 `null` 的（`providers.remove`）走
    `rawCall('...')`——两者都是「ArkTS 侧接了这个方法」，都要算，否则会把
    「故意没有返回值的 write 方法」误判成没接。
    """
    text = read(DIARY_CORE_ETS)
    return sorted(set(re.findall(r"(?:invokeData|rawCall)\('([\w.]+)'", text)))


def arkts_error_codes() -> list[str]:
    text = read(DIARY_ERROR_ETS)
    match = re.search(r"CORE_ERROR_CODES: string\[\] = \[(.*?)\];", text, re.S)
    if not match:
        problems.append("DiaryError.ets：找不到 CORE_ERROR_CODES")
        return []
    return re.findall(r"'([a-z_]+)'", match.group(1))


def documented_method_count() -> int | None:
    """`m6-核心-C-ABI.md` §6.1 标题里声明的方法数。

    这是防「文档写 14、实际 19」那类漂移的：方法表本身可以逐行增删，但标题里的
    数字必须与 `WIRED_METHODS` 一致。标题形状由文档自定，这里只认 `已接（N 条`。
    """
    text = read(CAPI_DOC)
    match = re.search(r"### 6\.1 已接（(\d+) 条", text)
    if not match:
        problems.append("m6-核心-C-ABI.md：§6.1 标题里找不到「已接（N 条」")
        return None
    return int(match.group(1))


def check_module_name() -> None:
    shim = read(SHIM)
    match = re.search(r'\.nm_modname = "([\w]+)"', shim)
    if not match:
        problems.append("napi_shim.cpp：找不到 nm_modname")
        return
    modname = match.group(1)
    if modname != "diary_napi":
        problems.append(f"nm_modname 是 `{modname}`，但打包文件名约定是 libdiary_napi.so")
    types = read(TYPES_PKG)
    try:
        types_json = json.loads(types)
    except json.JSONDecodeError as exc:
        problems.append(f"{TYPES_PKG.relative_to(ROOT)}：不是合法 JSON5 -> {exc}")
        return
    expected_name = f"lib{modname}.so"
    if types_json.get("name") != expected_name:
        problems.append(f"types 包的 name 是 `{types_json.get('name')}`，应为 `{expected_name}`")
    if types_json.get("types") != "./index.d.ts":
        problems.append("types 包的 types 字段应为 ./index.d.ts")
    if not DTS.is_file():
        problems.append(f"缺少 {DTS.relative_to(ROOT)}")

    entry = read(ENTRY_PKG)
    dep = re.search(r'"([\w.]+\.so)"\s*:\s*"file:([^"]+)"', entry)
    if not dep:
        problems.append("entry/oh-package.json5：找不到 .so 的 file: 依赖")
        return
    dep_name, dep_path = dep.group(1), dep.group(2)
    if dep_name != expected_name:
        problems.append(f"entry/oh-package.json5 依赖名是 `{dep_name}`，应为 `{expected_name}`")
    resolved = (ENTRY_PKG.parent / dep_path).resolve()
    if resolved != TYPES_PKG.parent.resolve():
        problems.append(f"file: 依赖指向 {resolved}，不是 types 目录 {TYPES_PKG.parent}")
    notes.append(f"三件套：{expected_name} → {dep_path}（ohpm install 后 ArkTS 才有类型）")


def main() -> int:
    methods = wired_methods()
    calls = arkts_calls()
    diff("契约方法（DiaryCore.ets ↔ WIRED_METHODS）", methods, calls)

    documented = documented_method_count()
    if documented is not None:
        if documented == len(methods):
            notes.append(f"C ABI 文档 §6.1 的方法数：{documented} 条（与 WIRED_METHODS 一致）")
        else:
            problems.append(
                f"m6-核心-C-ABI.md §6.1 写的是 {documented} 条，"
                f"WIRED_METHODS 实际是 {len(methods)} 条"
            )

    codes = core_error_codes()
    arkts_codes = arkts_error_codes()
    diff("错误码（DiaryError.ets ↔ ErrorCode::wire）", codes, arkts_codes)
    if codes and arkts_codes and codes != arkts_codes:
        problems.append("错误码顺序不一致：ArkTS 的 CORE_ERROR_CODES 应与 ErrorCode::wire 同序")

    symbols = capi_symbols()
    dl = shim_dlsym_symbols()
    diff("C 符号（napi_shim.cpp dlsym ↔ lib.rs 导出）", symbols, dl)
    if len(symbols) != 6:
        problems.append(f"C ABI 导出符号数是 {len(symbols)}，文档写的是 6 个")

    js = shim_js_exports()
    dts = dts_exports()
    diff("JS 函数（index.d.ts ↔ napi_define_properties）", js, dts)
    if "stringFree" in dts:
        problems.append("index.d.ts 不该导出 stringFree：C 字符串在 shim 内部复制并释放")

    error_text = read(DIARY_ERROR_ETS)
    if "native_unavailable" in codes:
        problems.append("native_unavailable 不应出现在核心错误码表里（它是宿主码）")
    if "NATIVE_UNAVAILABLE: string = 'native_unavailable'" not in error_text:
        problems.append("DiaryError.ets：宿主要码 NATIVE_UNAVAILABLE 定义缺失或不一致")

    check_module_name()

    for note in notes:
        print(f"[核对] {note}")
    for problem in problems:
        print(f"[失败] {problem}")
    if problems:
        print(f"\n共 {len(problems)} 处问题。")
        return 1
    print("[通过] ArkTS 包装与 C ABI 的方法名、错误码、符号、类型声明、打包三件套一致。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
