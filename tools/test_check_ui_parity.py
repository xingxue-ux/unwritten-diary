#!/usr/bin/env python3
"""`check_ui_parity.py` 的自测：证明它真的会红/会绿，而不是只会读文件。

它用临时目录里的最小 Fixture 跑七种情况：

1. ArkTS 4 入口（PR #150 的 `今日/搜索/任务/设置`）→ 必须非零；
2. ArkTS 5 入口（契约的 `记录/日记/搜索/片段/我的`）→ 必须零；
3. 文案表删掉 `state.draft` → 必须非零；
4. ArkTS 没有 `TabContent` → 必须零，且 `nav.status=skipped`（不是 passed）；
5. 有 `TabContent` 但没有 `.tabBar` → 必须非零；
6. `--json` 可解析，字段完整；
7. 源码引用数按去重后的 key 统计（同 key 多处引用只算 1 个 key）。

这些用例不依赖 `origin/main` 上有没有鸿蒙主壳，所以真实检查跳过或红的时候，
自测仍然能证明检查器本身没有坏。
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
import check_ui_parity as cup  # noqa: E402


class UiParityCheckerTest(unittest.TestCase):
    def make_fixture(self, root: Path, arkts_labels: list[str]) -> None:
        root.mkdir(parents=True, exist_ok=True)
        flutter = "\n".join(
            f"        _navItem({i}, Icons.x, '{label}'),"
            for i, label in enumerate(["记录", "日记", "搜索", "片段", "我的"])
        )
        (root / "flutter.dart").write_text(
            "Widget _bottomNav() => Row(children: [\n" + flutter + "\n]);\n",
            encoding="utf-8",
        )
        tabs: list[str] = []
        for index, label in enumerate(arkts_labels):
            tabs.append(
                "        TabContent() {\n"
                f"          Page{index}()\n"
                "        }\n"
                f"        .tabBar('{label}')"
            )
        (root / "index.ets").write_text(
            "Tabs() {\n" + "\n\n".join(tabs) + "\n}\n", encoding="utf-8"
        )
        strings = {f"nav.{nav_id}": label for nav_id, label in cup.CANONICAL_NAV}
        strings.update(
            {
                key: "文案"
                for key in cup.REQUIRED_KEYS
                if key not in strings
            }
        )
        (root / "strings.json").write_text(
            json.dumps(strings, ensure_ascii=False), encoding="utf-8"
        )

    def run_checker(self, root: Path, *extra: str) -> tuple[int, str]:
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = cup.main(
                [
                    "--root",
                    str(root),
                    "--flutter",
                    "flutter.dart",
                    "--arkts",
                    "index.ets",
                    "--strings",
                    "strings.json",
                    *extra,
                ]
            )
        return code, out.getvalue()

    def test_four_entries_are_red(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.make_fixture(root, ["今日", "搜索", "任务", "设置"])
            code, output = self.run_checker(root)
            self.assertEqual(code, 1, output)
            self.assertIn("arkts.nav.count", output)
            self.assertIn("4 个入口", output)

    def test_five_entries_are_green(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.make_fixture(root, ["记录", "日记", "搜索", "片段", "我的"])
            code, output = self.run_checker(root)
            self.assertEqual(code, 0, output)
            self.assertIn("结果：绿", output)

    def test_missing_required_key_is_red(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.make_fixture(root, ["记录", "日记", "搜索", "片段", "我的"])
            strings_path = root / "strings.json"
            strings = json.loads(strings_path.read_text(encoding="utf-8"))
            del strings["state.draft"]
            strings_path.write_text(json.dumps(strings, ensure_ascii=False), encoding="utf-8")
            code, output = self.run_checker(root)
            self.assertEqual(code, 1, output)
            self.assertIn("strings.required_missing", output)
            self.assertIn("state.draft", output)

    def test_json_output_is_parseable(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.make_fixture(root, ["今日", "搜索", "任务", "设置"])
            code, output = self.run_checker(root, "--json")
            payload = json.loads(output)
            self.assertEqual(code, 1)
            self.assertFalse(payload["ok"])
            self.assertEqual(payload["schema"], "ui-parity/v2")
            self.assertEqual(payload["nav"]["status"], "checked")
            self.assertFalse(payload["nav"]["consistent"])
            self.assertEqual(len(payload["arkts"]["nav"]), 4)

    def test_reference_count_is_distinct_keys_not_call_sites(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.make_fixture(root, ["记录", "日记", "搜索", "片段", "我的"])
            app = root / "apps" / "demo" / "lib"
            app.mkdir(parents=True)
            (app / "copy.dart").write_text(
                "const a = uiStrings.t('state.draft');\n"
                "const b = uiStrings.t('state.draft');\n"
                "const c = uiStrings.t('job.queued');\n",
                encoding="utf-8",
            )
            code, output = self.run_checker(root, "--json")
            payload = json.loads(output)
            self.assertEqual(code, 0, output)
            # “25 个 key（44 处）”里的 key 数按去重后的 key 算，不按调用点算。
            self.assertEqual(payload["strings"]["referenced_key_count"], 2)
            self.assertEqual(len(payload["strings"]["referenced"]), 3)
            self.assertEqual(
                payload["strings"]["referenced_keys"],
                ["job.queued", "state.draft"],
            )

    def test_missing_shell_is_skipped_not_passed(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.make_fixture(root, [])
            code, output = self.run_checker(root)
            self.assertEqual(code, 0, output)
            self.assertIn("[跳过]", output)
            self.assertIn("结果：绿（入口检查跳过", output)
            self.assertNotIn("两端一级入口与共享文案 key 一致", output)

            code, output = self.run_checker(root, "--json")
            payload = json.loads(output)
            self.assertEqual(code, 0)
            self.assertTrue(payload["ok"])
            self.assertEqual(payload["nav"]["status"], "skipped")
            self.assertIsNone(payload["nav"]["consistent"])
            self.assertFalse(payload["arkts"]["shell_present"])
            self.assertEqual(payload["skipped"][0]["check"], "nav.cross_side")

    def test_tabcontent_without_tabbar_is_red(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.make_fixture(root, ["记录"])
            # 有 TabContent 但没有 .tabBar：这不是「没壳」，是壳坏了，必须红。
            (root / "index.ets").write_text(
                "Tabs() {\n  TabContent() {\n    Page0()\n  }\n}\n",
                encoding="utf-8",
            )
            code, output = self.run_checker(root)
            self.assertEqual(code, 1, output)
            self.assertIn("arkts.tabbar_unparsed", output)


if __name__ == "__main__":
    unittest.main()
