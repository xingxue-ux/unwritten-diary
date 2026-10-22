#!/usr/bin/env python3
"""`check_ui_parity.py` 的自测：证明它真的会红/会绿，而不是只会读文件。

ArkTS 的两套一级导航结构**各有一组夹具**，不是只测老结构：

1. 底部页签（旧壳）：4 入口 → 非零；5 入口 → 零；
2. 顶部导航（PR #188 的写法，`Row` 里 `Text('不写日记')` + `NavItem` 文字按钮）：
   5 入口 → 零；4 入口 → 非零；5 个但顺序调换 → 非零；
3. 顶部导航把清单写成数组常量（`private readonly navItems: Array<[string, number]>`）：
   5 项 → 零；4 项 → 非零；
4. 两种结构都没有 → 零，且 `nav.status=skipped`（不是 passed）；
   有 `TabContent` 但没有 `.tabBar` → 非零；`NavItem` 用了变量而不是字面量 → 非零；
   两份结构同时存在但入口不一致 → 非零（说明有一份该删了）；
5. 文案表删掉 `state.draft` → 非零；
6. `--json` 可解析、字段完整；
7. 源码引用数按去重后的 key 统计（同 key 多处引用只算 1 个 key）。

这些用例不依赖 `origin/main` 上的鸿蒙壳长什么样，所以真实检查红或跳过时，
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
    FLUTTER_LABELS = ["记录", "日记", "搜索", "片段", "我的"]

    # ── Fixture 生成 ──────────────────────────────────────────────────────

    @classmethod
    def tabs_source(cls, labels: list[str]) -> str:
        """旧壳：`Tabs() { TabContent() {...}.tabBar('...') }`。"""
        tabs: list[str] = []
        for index, label in enumerate(labels):
            tabs.append(
                "        TabContent() {\n"
                f"          Page{index}()\n"
                "        }\n"
                f"        .tabBar('{label}')"
            )
        return "Tabs() {\n" + "\n\n".join(tabs) + "\n}\n"

    @classmethod
    def top_bar_source(cls, labels: list[str]) -> str:
        """新壳：与 PR #188 的 `Index.ets` 同构。

        `Row` 里 `Text('不写日记')` + 5 个 `NavItem` 文字按钮 + `Blank()` + 标语 + 任务；
        内容区是 `Stack` 里按 `currentTab` 显隐的页面。`@Builder NavItem(...)` 的定义
        也在文件里——解析器不能把定义当成一次调用。
        """
        items = "\n".join(
            f"          this.NavItem('{label}', {index})"
            for index, label in enumerate(labels)
        )
        pages = "\n\n".join(
            "          Column() {\n"
            f"            Page{index}({{ pathStack: this.pathStack }})\n"
            "          }\n"
            f"          .visibility(this.currentTab === {index} ? Visibility.Visible : Visibility.None)"
            for index in range(len(labels))
        )
        return (
            "@Builder\n"
            "NavItem(label: string, index: number) {\n"
            "  Text(label)\n"
            "}\n\n"
            "@Entry\n"
            "@Component\n"
            "struct Index {\n"
            "  @State currentTab: number = 0;\n\n"
            "  build() {\n"
            "    Navigation(this.pathStack) {\n"
            "      Column() {\n"
            "        Row() {\n"
            "          Text('不写日记')\n"
            f"{items}\n"
            "          Blank()\n"
            "          Text('给今天留一页')\n"
            "          Text('任务')\n"
            "        }\n\n"
            "        Stack({ alignContent: Alignment.TopStart }) {\n"
            f"{pages}\n"
            "        }\n"
            "      }\n"
            "    }\n"
            "  }\n"
            "}\n"
        )

    @classmethod
    def nav_array_source(cls, labels: list[str]) -> str:
        """顶部导航的另一种写法：入口清单是一个 `[('记录', 0), ...]` 常量。"""
        entries = "\n".join(
            f"    ['{label}', {index}]," for index, label in enumerate(labels)
        )
        return (
            "@Entry\n"
            "@Component\n"
            "struct Index {\n"
            "  private readonly navItems: Array<[string, number]> = [\n"
            f"{entries}\n"
            "  ];\n"
            "  @State currentTab: number = 0;\n\n"
            "  build() {\n"
            "    Row() {\n"
            "      ForEach(this.navItems, (item: [string, number]) => {\n"
            "        Text(item[0])\n"
            "      })\n"
            "    }\n"
            "  }\n"
            "}\n"
        )

    @classmethod
    def mixed_source(cls, tab_labels: list[str], nav_labels: list[str]) -> str:
        """一份文件里同时留着两套结构（迁移期），用来验证「两份必须一致」。"""
        items = "\n".join(
            f"        this.NavItem('{label}', {index})"
            for index, label in enumerate(nav_labels)
        )
        tabs = "\n".join(
            "        TabContent() {\n"
            f"          Page{index}()\n"
            "        }\n"
            f"        .tabBar('{label}')"
            for index, label in enumerate(tab_labels)
        )
        return (
            "@Builder\n"
            "NavItem(label: string, index: number) {\n"
            "  Text(label)\n"
            "}\n\n"
            "@Entry\n"
            "@Component\n"
            "struct Index {\n"
            "  build() {\n"
            "    Column() {\n"
            "      Row() {\n"
            f"{items}\n"
            "      }\n"
            "      Tabs() {\n"
            f"{tabs}\n"
            "      }\n"
            "    }\n"
            "  }\n"
            "}\n"
        )

    @classmethod
    def arkts_source(cls, labels: list[str], structure: str) -> str:
        if structure == "tabs":
            return cls.tabs_source(labels)
        if structure == "top-bar":
            return cls.top_bar_source(labels)
        if structure == "array":
            return cls.nav_array_source(labels)
        if structure == "none":
            return "Tabs() {\n}\n"
        raise AssertionError(f"未知夹具结构：{structure}")

    def make_fixture(
        self, root: Path, arkts_labels: list[str], structure: str = "tabs"
    ) -> None:
        root.mkdir(parents=True, exist_ok=True)
        flutter = "\n".join(
            f"        _navItem({i}, Icons.x, '{label}'),"
            for i, label in enumerate(self.FLUTTER_LABELS)
        )
        (root / "flutter.dart").write_text(
            "Widget _bottomNav() => Row(children: [\n" + flutter + "\n]);\n",
            encoding="utf-8",
        )
        (root / "index.ets").write_text(
            self.arkts_source(arkts_labels, structure), encoding="utf-8"
        )
        # 夹具用检查器固化的契约 §4.8 原文，默认就是「逐字一致」的合法文案表；
        # 要测漂移的用例再针对性改值。
        (root / "strings.json").write_text(
            json.dumps(cup.CONTRACT_STRINGS, ensure_ascii=False), encoding="utf-8"
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

    # ── 底部页签（旧结构，不能因为加了新夹具就丢掉）──────────────────────

    def test_four_entries_are_red(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.make_fixture(root, ["今日", "搜索", "任务", "设置"], "tabs")
            code, output = self.run_checker(root)
            self.assertEqual(code, 1, output)
            self.assertIn("arkts.nav.count", output)
            self.assertIn("4 个入口", output)

    def test_five_entries_are_green(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.make_fixture(root, self.FLUTTER_LABELS, "tabs")
            code, output = self.run_checker(root)
            self.assertEqual(code, 0, output)
            self.assertIn("结果：绿", output)
            self.assertIn("底部页签", output)

    # ── 顶部导航（PR #188 的结构）────────────────────────────────────────

    def test_top_bar_five_entries_are_green(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.make_fixture(root, self.FLUTTER_LABELS, "top-bar")
            code, output = self.run_checker(root)
            self.assertEqual(code, 0, output)
            self.assertIn("结果：绿", output)
            self.assertIn("顶部导航", output)
            self.assertIn("记录 / 日记 / 搜索 / 片段 / 我的", output)

            code, output = self.run_checker(root, "--json")
            payload = json.loads(output)
            self.assertEqual(code, 0, output)
            self.assertEqual(payload["nav"]["status"], "checked")
            self.assertTrue(payload["nav"]["consistent"])
            self.assertEqual(payload["arkts"]["nav_structure"], "nav-item")
            self.assertEqual(
                payload["arkts"]["structures"], {"tabs": False, "top_bar": True}
            )
            self.assertEqual(
                [entry["label"] for entry in payload["arkts"]["nav"]],
                self.FLUTTER_LABELS,
            )

    def test_top_bar_four_entries_are_red(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            # 少一个「我的」：顶部导航只有 4 个入口，必须红，不能跳过。
            self.make_fixture(root, ["记录", "日记", "搜索", "片段"], "top-bar")
            code, output = self.run_checker(root)
            self.assertEqual(code, 1, output)
            self.assertIn("arkts.nav.count", output)
            self.assertIn("4 个入口", output)
            self.assertIn("顶部导航", output)
            self.assertNotIn("[跳过]", output)

    def test_top_bar_wrong_order_is_red(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            # 5 个都在，但前两个调换：数量对不代表顺序对。
            self.make_fixture(root, ["日记", "记录", "搜索", "片段", "我的"], "top-bar")
            code, output = self.run_checker(root)
            self.assertEqual(code, 1, output)
            self.assertIn("arkts.nav.order[0]", output)
            self.assertIn("第 1 个入口是 '日记'，期望 '记录'", output)
            self.assertNotIn("[跳过]", output)

    def test_top_bar_array_constant_is_green(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.make_fixture(root, self.FLUTTER_LABELS, "array")
            code, output = self.run_checker(root)
            self.assertEqual(code, 0, output)
            self.assertIn("顶部导航数组 navItems", output)

    def test_top_bar_array_four_entries_are_red(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.make_fixture(root, ["记录", "日记", "搜索", "片段"], "array")
            code, output = self.run_checker(root)
            self.assertEqual(code, 1, output)
            self.assertIn("arkts.nav.count", output)

    def test_nav_item_with_variable_label_is_red(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.make_fixture(root, self.FLUTTER_LABELS, "top-bar")
            # 把字面量换成变量：结构还在、但检查器取不到标签，不能当没看见。
            (root / "index.ets").write_text(
                "@Builder\n"
                "NavItem(label: string, index: number) {\n"
                "  Text(label)\n"
                "}\n\n"
                "struct Index {\n"
                "  build() {\n"
                "    Row() {\n"
                "      this.NavItem(this.labelAt(0), 0)\n"
                "    }\n"
                "  }\n"
                "}\n",
                encoding="utf-8",
            )
            code, output = self.run_checker(root)
            self.assertEqual(code, 1, output)
            self.assertIn("arkts.nav_unparsed", output)
            self.assertNotIn("[跳过]", output)

    def test_both_structures_must_agree(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.make_fixture(root, self.FLUTTER_LABELS, "tabs")
            # 迁移期留了两份：底部 5 个、顶部 4 个。哪份算数说不清，必须红。
            (root / "index.ets").write_text(
                self.mixed_source(self.FLUTTER_LABELS, ["记录", "日记", "搜索", "片段"]),
                encoding="utf-8",
            )
            code, output = self.run_checker(root)
            self.assertEqual(code, 1, output)
            self.assertIn("arkts.nav.structures", output)

    def test_commented_out_nav_items_are_ignored(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.make_fixture(root, self.FLUTTER_LABELS, "top-bar")
            index = root / "index.ets"
            text = index.read_text(encoding="utf-8")
            # 注释里贴的示例入口不是 UI：行注释与块注释都不能变成第 6 个入口。
            text = text.replace(
                "          this.NavItem('记录', 0)",
                "          // 旧写法：this.NavItem('假入口', 9)\n"
                "          /* 也不要：this.NavItem('另一个假入口', 8) */\n"
                "          this.NavItem('记录', 0)",
                1,
            )
            index.write_text(text, encoding="utf-8")
            code, output = self.run_checker(root)
            self.assertEqual(code, 0, output)
            self.assertNotIn("假入口", output)
            self.assertIn("记录 / 日记 / 搜索 / 片段 / 我的", output)

    # ── 跳过 / 解析失败 / 文案表 ─────────────────────────────────────────

    def test_missing_shell_is_skipped_not_passed(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            # 两种结构都没有：这是唯一允许跳过的情况。
            self.make_fixture(root, [], "none")
            code, output = self.run_checker(root)
            self.assertEqual(code, 0, output)
            self.assertIn("[跳过]", output)
            self.assertIn("结果：绿（ArkTS 入口检查跳过", output)
            self.assertIn("仅 ArkTS 侧跳过", output)
            self.assertIn("顶部导航", output)  # 原因里要写清两种结构都没找到
            self.assertNotIn("两端一级入口与共享文案 key 一致", output)

            code, output = self.run_checker(root, "--json")
            payload = json.loads(output)
            self.assertEqual(code, 0)
            self.assertTrue(payload["ok"])
            self.assertEqual(payload["nav"]["status"], "skipped")
            self.assertIsNone(payload["nav"]["consistent"])
            self.assertFalse(payload["arkts"]["shell_present"])
            self.assertEqual(payload["skipped"][0]["check"], "nav.cross_side")

    def test_flutter_four_entries_is_red_even_when_arkts_skipped(self) -> None:
        # 回归：两端比对曾一起挂在 nav_status == "checked" 上。ArkTS 还没接导航时，
        # Flutter 只有 4 入口会被打印成「Flutter：4 个入口 + [跳过] + 绿」。
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.make_fixture(root, [], "none")
            flutter = "\n".join(
                f"        _navItem({i}, Icons.x, '{label}'),"
                for i, label in enumerate(self.FLUTTER_LABELS[:4])
            )
            (root / "flutter.dart").write_text(
                "Widget _bottomNav() => Row(children: [\n" + flutter + "\n]);\n",
                encoding="utf-8",
            )
            code, output = self.run_checker(root)
            self.assertEqual(code, 1, output)
            self.assertIn("flutter.nav.count", output)
            self.assertIn("Flutter：4 个入口", output)
            self.assertIn("[跳过]", output)  # 跳过的只有 ArkTS 那一侧
            self.assertIn("仅 ArkTS 入口检查跳过", output)
            self.assertNotIn("结果：绿", output)

            code, output = self.run_checker(root, "--json")
            payload = json.loads(output)
            self.assertEqual(code, 1)
            self.assertFalse(payload["ok"])
            self.assertEqual(payload["nav"]["status"], "skipped")
            self.assertEqual(len(payload["flutter"]["nav"]), 4)

    def test_contract_non_nav_text_drift_is_red(self) -> None:
        # 契约 §4.8 的 30 个 key 必须逐字一致，不只是 nav.*。
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.make_fixture(root, self.FLUTTER_LABELS, "top-bar")
            strings_path = root / "strings.json"
            strings = json.loads(strings_path.read_text(encoding="utf-8"))
            strings["search.partial_coverage"] = "没有结果"
            strings["state.committed"] = "未提交"
            strings["danger.trash_confirm"] = "删除？"
            strings_path.write_text(json.dumps(strings, ensure_ascii=False), encoding="utf-8")
            code, output = self.run_checker(root)
            self.assertEqual(code, 1, output)
            self.assertIn("strings.contract_value", output)
            self.assertIn("search.partial_coverage", output)
            self.assertIn("state.committed", output)
            self.assertIn("danger.trash_confirm", output)
            self.assertIn("契约 §4.8", output)

    def test_contract_doc_table_drift_is_red(self) -> None:
        # 固化在检查器里的 30 条文案还必须和契约文档 §4.8 的表一致；
        # 只改文档不改检查器（或反过来）都要红。
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.make_fixture(root, self.FLUTTER_LABELS, "top-bar")
            repo_root = Path(cup.__file__).resolve().parent.parent
            doc = (repo_root / cup.CONTRACT_DOC).read_text(encoding="utf-8")
            drifted = doc.replace(
                "| `state.committed` | 已提交 |",
                "| `state.committed` | 未提交 |",
            )
            self.assertNotEqual(doc, drifted, "契约 §4.8 的行格式变了，测试需要同步更新")
            doc_path = root / cup.CONTRACT_DOC
            doc_path.parent.mkdir(parents=True)
            doc_path.write_text(drifted, encoding="utf-8")
            code, output = self.run_checker(root)
            self.assertEqual(code, 1, output)
            self.assertIn("strings.contract_doc_value", output)
            self.assertIn("state.committed", output)

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

    def test_missing_required_key_is_red(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.make_fixture(root, self.FLUTTER_LABELS, "top-bar")
            strings_path = root / "strings.json"
            strings = json.loads(strings_path.read_text(encoding="utf-8"))
            del strings["state.draft"]
            strings_path.write_text(json.dumps(strings, ensure_ascii=False), encoding="utf-8")
            code, output = self.run_checker(root)
            self.assertEqual(code, 1, output)
            self.assertIn("strings.required_missing", output)
            self.assertIn("state.draft", output)

    # ── JSON / 引用统计 ─────────────────────────────────────────────────

    def test_json_output_is_parseable(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.make_fixture(root, ["今日", "搜索", "任务", "设置"], "tabs")
            code, output = self.run_checker(root, "--json")
            payload = json.loads(output)
            self.assertEqual(code, 1)
            self.assertFalse(payload["ok"])
            self.assertEqual(payload["schema"], "ui-parity/v2")
            self.assertEqual(payload["nav"]["status"], "checked")
            self.assertFalse(payload["nav"]["consistent"])
            self.assertEqual(payload["arkts"]["nav_structure"], "tabs")
            self.assertEqual(len(payload["arkts"]["nav"]), 4)

    def test_reference_count_is_distinct_keys_not_call_sites(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.make_fixture(root, self.FLUTTER_LABELS, "top-bar")
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


if __name__ == "__main__":
    unittest.main()