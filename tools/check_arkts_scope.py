#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""用 tsc 对「不依赖 ArkUI DSL」的 ArkTS 文件做语法与作用域检查。

## 为什么需要它

`apps/ohos-diary-core` 的 ArkTS 代码**无法在 CI 里用 hvigorw 编译**：鸿蒙 SDK 是
闭源的、约 6 GB、要华为账号与许可协议，拿不到 GitHub runner 上。于是这类错误
**谁都不拦**，只能靠人在真机/本机编译时撞见：

- `core/DiaryCore.ets` 少一个 `}` —— 后面的方法被解析进上一个方法体，
  **整个 HAP 编译失败**（2026-10-21 真实发生，评审靠逐字符数括号才发现）；
- `recording/VoiceRecorder.ets` 里一段重复的 finalize 代码引用了**内层作用域**的
  变量 —— `ArkTS` 报 4 个错，**main 上一个 HAP 都编不出来**（同日，我自己造成的）。

而 `check_arkts_capi_contract.py` 只核对方法名/条数/错误码，`cargo test` 与
`flutter test` 完全不碰 ArkTS ⇒ 三者全绿也拦不住上面两个。

## 怎么做

ArkTS 是 TypeScript 的超集（`struct`、`$r()`、尾随闭包等 DSL 之外的部分一致），
所以把 `.ets` 当 `.ts` 丢给 `tsc --noEmit` 就能查出**语法错误**与**作用域错误**。

**只检查不需要 ArkUI DSL 的目录**。`pages/` 与 `ui/` 用了 ArkUI 的尾随闭包写法
（`Column({...}) { ... }`），那是 ArkTS 特有的语法糖，`tsc` 解析不了（会报一片
TS1005/TS1128），所以排除掉 —— 它们仍需本地 `hvigorw` 把关（见 CONTRIBUTING）。

覆盖范围：`core/`、`recording/`、`entryability/`、`verify/`、`service/`、`data/`
以及根下其它非 UI 目录。**今天两个真实事故都落在这个范围内。**

## 忽略哪些诊断码

| 码 | 原因 |
|---|---|
| `TS2307` | 找不到 `@kit.*` / `@ohos.*` 模块 —— 鸿蒙内置模块没有 `.d.ts` 可解析 |
| `TS2339` | 属性不存在 —— 上一条的连带（基类来自解析不到的内置模块） |
| `TS1206` | 装饰器不能用在顶层函数 —— ArkTS 的 `@Concurrent` 允许，`tsc` 不允许 |

**不忽略** `TS2304`（找不到名字）：那正是「变量出了作用域还在用」的信号。

## 为什么分两轮判

`tsc` 在**存在语法错误时会跳过语义检查**。所以先看有没有语法类诊断（`TS1xxx`）：

- 有 ⇒ 报「语法错误」，此时 `TS2304` 之类的语义问题可能还没被算出来；
- 没有 ⇒ 再看剩下的语义诊断。

两个阶段任一有货就 exit 1。这样「少括号」与「变量出作用域」两类都能被抓到，
不会因为前者把后者盖住。
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
# 自测（tools/test_check_arkts_scope.py）用这个环境变量指向临时目录，
# 以便在不改动工作树的前提下证明「注入这两类错误时检查会红」。
ETS_ROOT = Path(os.environ["ARKTS_ETS_ROOT"]) if os.environ.get("ARKTS_ETS_ROOT") \
    else ROOT / "apps" / "ohos-diary-core" / "entry" / "src" / "main" / "ets"

# 用 ArkUI 尾随闭包语法的目录，tsc 解析不了（详见模块文档）。
DSL_DIRS = {"pages", "ui"}

# 见模块文档里的「忽略哪些诊断码」。
IGNORED_CODES = {
    "TS2307",  # Cannot find module '@kit.*' / '@ohos.*'
    "TS2339",  # Property does not exist（基类来自解析不到的内置模块）
    "TS1206",  # Decorators are not valid here（ArkTS 的 @Concurrent）
}

DIAG_RE = re.compile(r"^(?P<file>[^(]+)\((?P<line>\d+),(?P<col>\d+)\): error (?P<code>TS\d+): (?P<msg>.*)$")


def collect_sources() -> list[Path]:
    """挑出 tsc 能解析的 .ets 文件（排除 ArkUI DSL 目录）。"""
    if not ETS_ROOT.is_dir():
        return []
    out: list[Path] = []
    for path in sorted(ETS_ROOT.rglob("*.ets")):
        rel = path.relative_to(ETS_ROOT)
        if rel.parts and rel.parts[0] in DSL_DIRS:
            continue
        out.append(path)
    return out


def run_tsc(workdir: Path, sources: list[Path]) -> tuple[int, str]:
    """把 .ets 复制成 .ts（保留相对结构，import 才找得到同目录文件）后跑 tsc。"""
    copied = 0
    for src in sources:
        rel = src.relative_to(ETS_ROOT).with_suffix(".ts")
        dst = workdir / rel
        dst.parent.mkdir(parents=True, exist_ok=True)
        # 同时补一份 .ets，避免 ArkTS 里 `import ... from './Foo'` 解析差异；
        # tsc 默认按 .ts 找，两个都留没有副作用。
        dst.write_text(src.read_text(encoding="utf-8"), encoding="utf-8")
        copied += 1

    tsconfig = {
        "compilerOptions": {
            "target": "es2020",
            "module": "esnext",
            "moduleResolution": "node",
            "noEmit": True,
            "skipLibCheck": True,
            "experimentalDecorators": True,
            "noImplicitAny": False,
            "strict": False,
            "types": [],
        },
        "include": ["**/*.ts"],
    }
    (workdir / "tsconfig.json").write_text(json.dumps(tsconfig, indent=2), encoding="utf-8")

    tsc = shutil.which("tsc")
    if tsc is None:
        # 本地可能只装了 npx；CI 里用 `npm install -g typescript` 放 path。
        tsc = shutil.which("npx")
        if tsc is None:
            print("[跳过] 找不到 tsc / npx：本检查需要 Node 与 TypeScript（CI 里装 typescript@5）。", file=sys.stderr)
            return 0, ""
        cmd = [tsc, "--yes", "typescript@5", "tsc", "-p", "tsconfig.json"]
    else:
        cmd = [tsc, "-p", "tsconfig.json"]

    proc = subprocess.run(cmd, cwd=workdir, capture_output=True, text=True, timeout=600)
    return proc.returncode, proc.stdout + proc.stderr


def main() -> int:
    sources = collect_sources()
    if not sources:
        print("[跳过] 没找到可检查的 ArkTS 文件。")
        return 0

    with tempfile.TemporaryDirectory(prefix="arkts-scope-") as tmp:
        workdir = Path(tmp)
        _, output = run_tsc(workdir, sources)

        syntax: list[str] = []
        semantic: list[str] = []
        for line in output.splitlines():
            m = DIAG_RE.match(line.strip())
            if not m:
                continue
            code = m.group("code")
            if code in IGNORED_CODES:
                continue
            # 还原成仓库里的真实路径与行号（临时目录不暴露给使用者）。
            rel = Path(m.group("file")).relative_to(workdir) if m.group("file").startswith(str(workdir)) else Path(m.group("file"))
            rel_display = str(rel).replace(os.sep, "/")
            if rel_display.endswith(".ts"):
                rel_display = rel_display[:-3] + ".ets"
            pretty = f"  apps/ohos-diary-core/entry/src/main/ets/{rel_display}" \
                     f":{m.group('line')}:{m.group('col')}  {code}  {m.group('msg')}"
            (syntax if code.startswith("TS1") else semantic).append(pretty)

    if syntax:
        print("[失败] ArkTS 语法错误（编译期就会挂，HAP 编不出来）：", file=sys.stderr)
        print("\n".join(syntax), file=sys.stderr)
        return 1

    if semantic:
        print("[失败] ArkTS 作用域/类型错误：", file=sys.stderr)
        print("\n".join(semantic), file=sys.stderr)
        print(
            "\n提示：TS2304「找不到名字」通常是变量被写在别的作用域里（例如重复粘贴的代码块）。",
            file=sys.stderr,
        )
        return 1

    checked = len(sources)
    print(f"[通过] {checked} 个 ArkTS 文件语法与作用域检查通过（pages/ 与 ui/ 需要 hvigorw 编译，见 CONTRIBUTING）。")
    return 0


if __name__ == "__main__":
    sys.exit(main())