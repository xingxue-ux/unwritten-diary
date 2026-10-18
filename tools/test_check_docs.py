#!/usr/bin/env python3
"""`check_docs.repo_files` 的回归测试。

起因（2026-10-18）：`repo_files` 原来用 `ROOT.rglob(pattern)`，会先把整棵树走完，
于是在有 `target/`（18 GB）的工作区上把构建产物也扫一遍；在 `/mnt/e`（drvfs）上
直接抛 `OSError: [Errno 12] Cannot allocate memory`。CI 上没有这些目录，**全绿也
测不到**，所以这条只能靠本地/单测兜住。
"""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import check_docs  # noqa: E402


class RepoFilesSkipsBuildDirsTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        (self.root / "docs").mkdir()
        (self.root / "docs" / "real.md").write_text("x", encoding="utf-8")
        # 模拟构建产物与依赖缓存：里面也放同名 .md，必须被跳过
        for junk in ("target/debug", "node_modules/pkg", ".hvigor/cache", "build/out", "dist"):
            d = self.root / junk
            d.mkdir(parents=True)
            (d / "junk.md").write_text("x", encoding="utf-8")
        self._old_root = check_docs.ROOT
        check_docs.ROOT = self.root

    def tearDown(self) -> None:
        check_docs.ROOT = self._old_root
        self._tmp.cleanup()

    def test_skips_build_and_dependency_dirs(self) -> None:
        found = {p.relative_to(self.root).as_posix() for p in check_docs.repo_files("*.md")}
        self.assertEqual(found, {"docs/real.md"}, "构建目录里的文件不该被扫到")

    def test_skip_dirs_cover_the_ones_we_hit(self) -> None:
        # 这条防止有人把 target/ 从 SKIP_DIRS 里删掉
        for name in ("target", "node_modules", "oh_modules", ".hvigor", "build"):
            self.assertIn(name, check_docs.SKIP_DIRS)


if __name__ == "__main__":
    unittest.main(verbosity=2)
