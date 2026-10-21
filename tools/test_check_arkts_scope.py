#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""`tools/check_arkts_scope.py` 的自测：证明它**该红的时候会红**。

其他检查器都有同样的自测（`test_check_docs.py`、`test_check_ui_parity.py`）：
README 与 CONTRIBUTING 都写着「先跑自测」，因为一个永远为绿的检查比没有检查更糟。

这里用临时目录构造两份仓库，跑真检查器（不是 mock）：

1. **干净的树** → 必须 exit 0；
2. **少了闭合 `}` 的类** → 必须报「语法错误」并 exit 1；
3. **重复代码块引用内层变量** → 必须报 `TS2304` 并 exit 1。

第 2、3 条正是 2026-10-21 当天真实发生过的两次事故（`DiaryCore.ets` 少括号、
`VoiceRecorder.ets` 重复 finalize 块），都在 `pages/` 之外，所以本检查覆盖得到
（`pages/`、`ui/` 用的是 ArkUI 尾随闭包语法，`tsc` 解析不了，仍需本地 hvigorw）。

需要 `tsc`（CI 里由 `npm install -g typescript@5` 提供）。找不到时**跳过并 exit 0**，
与检查器本身的降级行为一致 —— 但 CI 里一定装得上，所以这不是逃生口。
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

    def test_dsl_directories_are_skipped(self) -> None:
        # pages/ 用 ArkUI 尾随闭包，tsc 解析不了 —— 必须在收集阶段就排除，
        # 否则 CI 会对每个 UI 页面报一片假红。
        proc = self.run_checker({"pages/Ui.ets": MISSING_BRACE})
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertIn("[跳过]", proc.stdout)


if __name__ == "__main__":
    unittest.main(verbosity=2)