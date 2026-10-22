#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""`tools/check_arkts_scope.py` 的自测：证明它**该红的时候会红**。

其他检查器都有同样的自测（`test_check_docs.py`、`test_check_ui_parity.py`）：
README 与 CONTRIBUTING 都写着「先跑自测」，因为一个永远为绿的检查比没有检查更糟。

这里用临时目录构造两份仓库，跑真检查器（不是 mock）：

1. **干净的树** → 必须 exit 0；
2. **少了闭合 `}` 的类** → 必须报「语法错误」并 exit 1；
3. **重复代码块引用内层变量** → 必须报 `TS2304` 并 exit 1；
4. **本地类型上的属性拼错**（`counter.total`）→ 必须报 `TS2339` 并 exit 1
   （这一条以前被整类忽略，本地真实错误会静默放行）；
5. **假 tsc**（exit 1 且输出 `error TS5099: ...` 这种没有 `file(line,col)` 形状的
   文本）→ 检查器**不能打印通过**，必须 exit 1；
6. **PATH 里没有 tsc / npx**：默认跳过但明确写「未验证」且不打印 `[通过]`；
   加 `--require-tsc`（CI 用法）必须 exit 1。

第 2、3 条正是 2026-10-21 当天真实发生过的两次事故（`DiaryCore.ets` 少括号、
`VoiceRecorder.ets` 重复 finalize 块），都在 `pages/` 之外，所以本检查覆盖得到
（`pages/`、`ui/` 用的是 ArkUI 尾随闭包语法，`tsc` 解析不了，仍需本地 hvigorw）。

前 4 条需要真 `tsc`（CI 里由 `npm install -g typescript@5` 提供），找不到时跳过；
第 5、6 条用假 tsc / 空 PATH，**任何机器上都跑**。
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CHECKER = ROOT / "tools" / "check_arkts_scope.py"

CLEAN_CLASS = """\
export class Counter {
  private value: number = 0;

  increment(): number {
    this.value += 1;
    return this.value;
  }
}
"""

# 少一个闭合 `}`：后面的方法会被解析进上一个方法体 —— 真实事故 #1。
MISSING_BRACE = """\
export class Broken {
  ping(): string {
    return 'pong';
  // 这里本该有一个 }

  pong(): string {
    return 'ping';
  }
}
"""

# 重复粘贴的代码块引用内层作用域变量 —— 真实事故 #2。
DUP_BLOCK = """\
export class Dup {
  run(reason: string): string {
    try {
      const effectiveReason = reason;
      return effectiveReason;
    } catch (e) {
      throw e as Error;
    }
    const again = effectiveReason;
    return again;
  }
}
"""

# 完全本地定义的类型上拼错属性：真实 tsc 只报 TS2339，和 `@kit.*` 无关。
LOCAL_TYPE_TYPO = """\
export class Counter {
  private value: number = 0;

  increment(): number {
    this.value += 1;
    return this.value;
  }
}

export function readTotal(counter: Counter): number {
  return counter.total;
}
"""

# 只缺内置模块（TS2307）：这类诊断必须继续忽略，否则整个 ArkTS 目录会一片假红。
MISSING_KIT_MODULE = """\
import { NativeThing } from '@kit.SomeKit';

export class Use {
  describe(x: NativeThing): string {
    return JSON.stringify(x);
  }
}
"""


class FakeTscTest(unittest.TestCase):
    """不依赖真 TypeScript：证明「tsc 挂了」不会变成「检查通过」。"""

    def run_with_fake_tsc(
        self, script: str, *extra: str, path: str | None = None
    ) -> subprocess.CompletedProcess[str]:
        tmp = tempfile.mkdtemp(prefix="arkts-fake-tsc-")
        self.addCleanup(shutil.rmtree, tmp, True)
        base = Path(tmp)
        ets = base / "ets"
        (ets / "core").mkdir(parents=True)
        (ets / "core" / "Clean.ets").write_text(CLEAN_CLASS, encoding="utf-8")
        fakebin = base / "bin"
        fakebin.mkdir()
        fake_tsc = fakebin / "tsc"
        fake_tsc.write_text(script, encoding="utf-8")
        fake_tsc.chmod(0o755)
        env = dict(
            os.environ,
            ARKTS_ETS_ROOT=str(ets),
            PATH=path if path is not None else f"{fakebin}{os.pathsep}{os.environ.get('PATH', '')}",
        )
        return subprocess.run(
            [sys.executable, str(CHECKER), *extra],
            capture_output=True,
            text=True,
            env=env,
            timeout=600,
        )

    def test_tsc_crash_with_unrecognized_output_is_red(self) -> None:
        proc = self.run_with_fake_tsc(
            "#!/bin/sh\n"
            "echo 'error TS5099: The tsconfig is broken' >&2\n"
            "exit 1\n"
        )
        self.assertEqual(proc.returncode, 1, proc.stdout + proc.stderr)
        self.assertNotIn("[通过]", proc.stdout)
        self.assertIn("没有可解析的诊断", proc.stderr)
        self.assertIn("TS5099", proc.stderr)

    def test_npx_fallback_error_is_red(self) -> None:
        # 旧 npx 回退（`npx --yes typescript@5 tsc`）的真实输出形状：
        # npm 报错、exit 1，旧代码把它当「无诊断 ⇒ 通过」吞掉了。
        proc = self.run_with_fake_tsc(
            "#!/bin/sh\n"
            "echo 'npm error could not determine executable to run' >&2\n"
            "exit 1\n"
        )
        self.assertEqual(proc.returncode, 1, proc.stdout + proc.stderr)
        self.assertNotIn("[通过]", proc.stdout)

    def test_missing_tsc_without_require_warns_but_does_not_pass(self) -> None:
        tmp = tempfile.mkdtemp(prefix="arkts-no-tsc-")
        self.addCleanup(shutil.rmtree, tmp, True)
        empty_path = str(Path(tmp) / "empty")
        Path(empty_path).mkdir()
        proc = self.run_with_fake_tsc("#!/bin/sh\nexit 0\n", path=empty_path)
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertNotIn("[通过]", proc.stdout)
        self.assertIn("[跳过]", proc.stderr)
        self.assertIn("未验证", proc.stderr)

    def test_missing_tsc_with_require_tsc_fails(self) -> None:
        tmp = tempfile.mkdtemp(prefix="arkts-no-tsc-")
        self.addCleanup(shutil.rmtree, tmp, True)
        empty_path = str(Path(tmp) / "empty")
        Path(empty_path).mkdir()
        proc = self.run_with_fake_tsc("#!/bin/sh\nexit 0\n", "--require-tsc", path=empty_path)
        self.assertEqual(proc.returncode, 1, proc.stdout + proc.stderr)
        self.assertNotIn("[通过]", proc.stdout)
        self.assertIn("--require-tsc", proc.stderr)


def has_tsc() -> bool:
    return shutil.which("tsc") is not None or shutil.which("npx") is not None


class ArktsScopeCheckTest(unittest.TestCase):
    def setUp(self) -> None:
        if not has_tsc():
            self.skipTest("没有 tsc / npx：本自测需要 TypeScript")

    def run_checker(self, files: dict[str, str]) -> subprocess.CompletedProcess[str]:
        tmp = tempfile.mkdtemp(prefix="arkts-selftest-")
        self.addCleanup(shutil.rmtree, tmp, True)
        root = Path(tmp)
        for name, body in files.items():
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(body, encoding="utf-8")
        env = dict(os.environ, ARKTS_ETS_ROOT=str(root))
        return subprocess.run(
            [sys.executable, str(CHECKER)],
            capture_output=True,
            text=True,
            env=env,
            timeout=600,
        )

    def test_clean_tree_passes(self) -> None:
        proc = self.run_checker({"core/Counter.ets": CLEAN_CLASS})
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertIn("[通过]", proc.stdout)

    def test_missing_closing_brace_fails(self) -> None:
        proc = self.run_checker({"core/Broken.ets": MISSING_BRACE})
        self.assertEqual(proc.returncode, 1, proc.stdout + proc.stderr)
        self.assertIn("语法错误", proc.stderr)
        self.assertIn("Broken.ets:", proc.stderr)

    def test_duplicated_block_scope_error_fails(self) -> None:
        proc = self.run_checker({"recording/Dup.ets": DUP_BLOCK})
        self.assertEqual(proc.returncode, 1, proc.stdout + proc.stderr)
        self.assertIn("TS2304", proc.stderr)
        self.assertIn("effectiveReason", proc.stderr)

    def test_local_type_property_typo_fails(self) -> None:
        # 整类忽略 TS2339 时，这条会打印「通过」——本地真实错误被放走。
        proc = self.run_checker({"core/Typo.ets": LOCAL_TYPE_TYPO})
        self.assertEqual(proc.returncode, 1, proc.stdout + proc.stderr)
        self.assertIn("TS2339", proc.stderr)
        self.assertIn("total", proc.stderr)
        self.assertNotIn("[通过]", proc.stdout)

    def test_missing_kit_module_alone_is_still_ignored(self) -> None:
        # 仓库里普遍 import 解析不到的 `@kit.*`：只忽略 TS2307 时不能把整目录变红。
        proc = self.run_checker({"core/Kit.ets": MISSING_KIT_MODULE})
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertIn("[通过]", proc.stdout)

    def test_dsl_directories_are_skipped(self) -> None:
        # pages/ 用 ArkUI 尾随闭包，tsc 解析不了 —— 必须在收集阶段就排除，
        # 否则 CI 会对每个 UI 页面报一片假红。
        proc = self.run_checker({"pages/Ui.ets": MISSING_BRACE})
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertIn("[跳过]", proc.stdout)


if __name__ == "__main__":
    unittest.main(verbosity=2)