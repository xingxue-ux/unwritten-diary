#!/usr/bin/env python3
"""`check_docs.repo_files` 的回归测试。

起因（2026-10-18）：`repo_files` 原来用 `ROOT.rglob(pattern)`，会先把整棵树走完，
于是在有 `target/`（18 GB）的工作区上把构建产物也扫一遍；在 `/mnt/e`（drvfs）上
直接抛 `OSError: [Errno 12] Cannot allocate memory`。CI 上没有这些目录，**全绿也
测不到**，所以这条只能靠本地/单测兜住。
"""

from __future__ import annotations

import contextlib
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import check_docs  # noqa: E402

_MISSING = object()
_ISSUE_TEMPLATE_YAML = "name: 任务\ndescription: 测试模板\nbody: []\n"


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


class YamlHonestyTest(unittest.TestCase):
    """缺 PyYAML 时不能一边跳过一边打印「.github YAML 均正常」。

    起因（2026-10-22）：`check_yaml` 把 ImportError 降级成一条 note，
    `main()` 却**无条件**打印「.github YAML 均正常」。CI 的 docs job 又没装
    PyYAML，于是 YAML 从来没被校验过、日志却一直说正常。
    """

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        for rel in check_docs.REQUIRED_FILES:
            path = self.root / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(
                _ISSUE_TEMPLATE_YAML if rel.endswith(".yml") else "",
                encoding="utf-8",
            )
        scenario_dir = self.root / "tests" / "fixtures" / "scenarios"
        scenario_dir.mkdir(parents=True)
        (scenario_dir / "base.json").write_text(
            json.dumps(
                {
                    "scenarios": [
                        {
                            "id": "s1",
                            "name": "n",
                            "goal": "g",
                            "setup": "s",
                            "steps": [],
                            "expected": [],
                        }
                    ]
                },
                ensure_ascii=False,
            ),
            encoding="utf-8",
        )
        self._old_root = check_docs.ROOT
        check_docs.ROOT = self.root
        self._saved_yaml = sys.modules.get("yaml", _MISSING)
        self._out = io.StringIO()
        self._redirect = contextlib.redirect_stdout(self._out)
        self._redirect.__enter__()

    def tearDown(self) -> None:
        self._redirect.__exit__(None, None, None)
        check_docs.ROOT = self._old_root
        if self._saved_yaml is _MISSING:
            sys.modules.pop("yaml", None)
        else:
            sys.modules["yaml"] = self._saved_yaml
        self._tmp.cleanup()

    def hide_yaml(self) -> None:
        # `sys.modules['yaml'] = None` 会让 `import yaml` 抛 ImportError，
        # 正好模拟「本机没装 PyYAML」。
        sys.modules["yaml"] = None

    def run_main(self, *argv: str) -> tuple[int, str]:
        self._out.seek(0)
        self._out.truncate()
        code = check_docs.main(list(argv))
        return code, self._out.getvalue()

    def test_missing_pyyaml_does_not_claim_yaml_is_normal(self) -> None:
        self.hide_yaml()
        code, output = self.run_main()
        self.assertEqual(code, 0, output)
        self.assertIn("[跳过]", output)
        self.assertIn("PyYAML", output)
        self.assertIn("未验证", output)
        self.assertNotIn(".github YAML", output.split("[通过]")[-1])

    def test_require_yaml_fails_without_pyyaml(self) -> None:
        self.hide_yaml()
        code, output = self.run_main("--require-yaml")
        self.assertEqual(code, 1, output)
        self.assertIn("[失败]", output)
        self.assertIn("PyYAML", output)
        self.assertIn("--require-yaml", output)
        self.assertNotIn(".github YAML 均正常", output)

    def test_with_pyyaml_the_pass_line_covers_yaml(self) -> None:
        if self._saved_yaml is _MISSING:
            sys.modules.pop("yaml", None)
        try:
            import yaml  # noqa: F401
        except ImportError:
            self.skipTest("本机没有 PyYAML")
        code, output = self.run_main()
        self.assertEqual(code, 0, output)
        self.assertIn(".github YAML", output)
        self.assertIn("共用场景 JSON", output)
        self.assertNotIn("[跳过]", output)

    def test_missing_scenario_dir_does_not_claim_json_is_normal(self) -> None:
        # 同一类假绿：没有场景目录时，成功文案里也不能写「共用场景 JSON 正常」。
        self.hide_yaml()
        scenario_dir = self.root / "tests" / "fixtures" / "scenarios"
        for path in scenario_dir.glob("*"):
            path.unlink()
        scenario_dir.rmdir()
        code, output = self.run_main()
        self.assertEqual(code, 0, output)
        self.assertIn("未验证", output)
        self.assertNotIn("共用场景 JSON", output.split("[通过]")[-1])


if __name__ == "__main__":
    unittest.main(verbosity=2)
