#!/usr/bin/env python3
"""跨端界面结构检查：5 个一级入口 + 共享文案 key。

检查两层：

1. **一级导航**：Flutter 的 `_navItem(...)` 调用与 ArkTS 的一级导航必须与产品决定
   一致，且两端顺序相同。产品决定见 `docs/product/界面结构契约.md` §2：
   `记录 / 日记 / 搜索 / 片段 / 我的`（任务中心是二级，不是 tab）。
   ArkTS 支持两种写法，**哪种存在就解析哪种**：

   - **底部页签（旧壳）**：`TabContent() { ... }.tabBar('记录')`；
   - **顶部导航条（新壳，PR #188 起）**：`this.NavItem('记录', 0)` 这样的组件调用，
     或者名字里带 `nav` / `tab` / `entry` 的 `string[]` / `[('记录', 0), ...]`
     元组数组常量。

   解析是**结构化**的：按组件调用、括号配对与数组元素取标签和顺序，不看缩进、
   不看行号，所以重排缩进、改换行、插注释都不会让它失明。解析前会先剥掉
   `//` 与 `/* */` 注释（替换成等长空格），注释里贴的示例入口不会被算成入口。两种结构同时存在但
   入口清单不一致 ⇒ 红（说明有一份是废弃的，得删掉）。

2. **文案 key**：`packages/ui_strings/ui_strings.json` 必须存在，必须是
   `key -> 非空中文文案` 的扁平 JSON；契约定下的必需 key 一个都不能少。
   源码里已经按约定引用（`uiStrings.get('...')` / `uiStrings.t('...')` /
   `uiString('...')`）的 key 也必须在表里。

**跳过条件收得很窄**：只有「`Index.ets` 不存在」或「底部页签、顶部导航两种结构都
没找到」才允许跳过。跳过时仍然 **exit 0**，但人类输出会打 `[跳过]` 和醒目原因，
JSON 的 `nav.status` 是 `skipped`（不是 `passed`）——按 `CONTRIBUTING.md` §10.1.1
的例子 3，「跳过要如实说没跑，而不是显示绿」。
一旦找到导航结构，**数量 / 集合 / 顺序不符就是 exit 1**；找到结构却取不到字面量
标签（例如有 `TabContent` 却没有 `.tabBar`、`NavItem(SOME_VAR, i)` 用了变量）
同样是 exit 1，不是跳过。
**不要用 `|| true` / `continue-on-error` 把它伪装成绿**，也不要把「跳过」说成「通过」。

用法：
    python3 tools/check_ui_parity.py
    python3 tools/check_ui_parity.py --json
    python3 tools/check_ui_parity.py --arkts /path/to/Index.ets

`--expect` **只给自测/排障用，CI 永远不传**：它只改变比较基准，不是放行开关；
不匹配仍然非零退出。默认基准是产品决定的 5 入口，不依赖这个参数。

退出码：0 = 通过或「确实没有可比对的导航结构」；1 = 发现漂移/缺 key/解析失败；
2 = 命令行用法错误。
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# 遍历源码时剪掉构建产物/依赖缓存：本地工作区可能有巨大的 oh_modules/build，
# 不剪枝既慢又可能在 drvfs 上抛内存错误（与 tools/check_docs.py 同样的坑）。
SKIP_DIRS = frozenset(
    {
        ".git",
        ".dart_tool",
        ".hvigor",
        "build",
        "dist",
        "node_modules",
        "oh_modules",
        "target",
    }
)

DEFAULT_FLUTTER = "apps/diary_app/lib/src/experience_shell.dart"
DEFAULT_ARKTS = "apps/ohos-diary-core/entry/src/main/ets/pages/Index.ets"
DEFAULT_STRINGS = "packages/ui_strings/ui_strings.json"

# 产品负责人 2026-10-19 的决定：以 Windows 的 5 入口为准，任务中心降为二级。
# id 用于 ui_strings.json 的 `nav.<id>`；label 是两端必须逐字一致的可见文案。
CANONICAL_NAV: tuple[tuple[str, str], ...] = (
    ("record", "记录"),
    ("diary", "日记"),
    ("search", "搜索"),
    ("snippets", "片段"),
    ("profile", "我的"),
)

# 语义层必需 key。前 5 个是导航（检查器用来把入口和文案表钉在一起）；
# 其余来自任务书与 `docs/development/01-共享接口契约.md` §7/§8。
REQUIRED_KEYS: tuple[str, ...] = (
    "nav.record",
    "nav.diary",
    "nav.search",
    "nav.snippets",
    "nav.profile",
    "state.draft",
    "state.committed",
    "state.trashed",
    "job.queued",
    "job.running",
    "job.failed",
    "job.needs_attention",
    "search.partial_coverage",
    "error.revision_conflict",
    "error.invalid_state",
    "export.contains_private_data",
    "backup.not_encrypted",
    "privacy.local_only",
)

# 源码引用文案 key 的约定写法。Flutter 已接表，ArkTS 尚未接；
# 这里只校验「引用的 key 在表里存在」，不要求两端引用数相同。
KEY_REFERENCE_RE = re.compile(
    r"""(?:uiStrings\.(?:get|t)|uiString)\s*\(\s*(['"])([a-z][a-z0-9_.-]*)\1""",
    re.IGNORECASE,
)

# Flutter：`_navItem(0, Icons.edit_outlined, '记录')`。
# 只匹配字面量 label；索引必须是整数，icon 表达式里没有逗号。
FLUTTER_NAV_RE = re.compile(
    r"""_navItem\s*\(\s*(\d+)\s*,\s*[^,\n]+?\s*,\s*'([^']*)'""",
)
# Flutter 桌面顶栏是另一份硬编码：`for (final (index, label) in ... [(0, '记录'), ...])`。
FLUTTER_DESKTOP_BLOCK_RE = re.compile(
    r"""for\s*\(\s*final\s*\(\s*index\s*,\s*label\s*\)\s*in\s*<\(int,\s*String\)>\s*\[(.*?)\]\s*\)""",
    re.DOTALL,
)
FLUTTER_DESKTOP_ENTRY_RE = re.compile(r"""\(\s*(\d+)\s*,\s*'([^']*)'\s*\)""")

# ── ArkTS 顶部导航（PR #188 起的主结构）────────────────────────────────────
# 组件调用：`this.NavItem('记录', 0)`。@Builder 定义 `NavItem(label: string, ...)`
# 的第一个参数是参数名、不是字面量，所以不会命中这里（下面另有调用标记专门排除定义）。
ARKTS_NAV_ITEM_RE = re.compile(
    r"""(?:\bthis\s*\.\s*)?\bNavItem\s*\(\s*(['"])([^'"]*)\1""",
)
# 对象参数：`NavItem({ label: '记录' })`。
ARKTS_NAV_ITEM_OBJECT_RE = re.compile(
    r"""\bNavItem\s*\(\s*\{\s*(?:label|title|text)\s*:\s*(['"])([^'"]*)\1""",
)
# 调用的存在性标记：`(?!label\s*:)` 把 `NavItem(label: string, index: number)` 定义排除掉。
ARKTS_NAV_ITEM_CALL_RE = re.compile(r"""\bNavItem\s*\(\s*(?!label\s*:)""")
# 入口数组常量：`private readonly navItems: Array<[string, number]> = [...]`。
# 只认名字里带 nav/tab/entry/menu 的（按 camelCase / snake_case 分词），
# 避免把普通文案数组、`pathStack` 之类的东西当成导航清单。
ARKTS_ARRAY_HEAD_RE = re.compile(
    r"""\b([A-Za-z_][A-Za-z0-9_]*)\s*(?::\s*[^=;\n{}]+?)?\s*=\s*\[""",
)
ARKTS_NAV_WORDS = frozenset(
    {"nav", "navigation", "tab", "tabs", "entry", "entries", "menu", "menuitem"}
)
ARKTS_TUPLE_ENTRY_RE = re.compile(
    r"""[\[(]\s*(['"])([^'"]*)\1\s*,\s*-?\d+\s*[\])]""",
)
ARKTS_STRING_RE = re.compile(r"""(['"])((?:\\.|[^'"\\])*)\1""")
# 顶部导航的内容区：`Column() { RecordPage(...) } ... .visibility(this.currentTab === 0 ...)`。
# `page` 只写进 JSON 当上下文，不参与通过/失败判定。
ARKTS_TAB_PAGE_RE = re.compile(
    r"""\b([A-Z][A-Za-z0-9_]*Page)\s*\([^()]*\)[\s\S]{0,400}?currentTab\s*===\s*(\d+)""",
)

STRING_ESCAPES = {"n": "\n", "t": "\t", "r": "\r", "\\": "\\", "'": "'", '"': '"'}


@dataclass
class Entry:
    index: int
    label: str
    line: int
    page: str | None = None

    def as_dict(self) -> dict[str, object]:
        out: dict[str, object] = {
            "index": self.index,
            "label": self.label,
            "line": self.line,
        }
        if self.page is not None:
            out["page"] = self.page
        return out


@dataclass
class Failure:
    code: str
    message: str

    def as_dict(self) -> dict[str, str]:
        return {"code": self.code, "message": self.message}


def line_of(text: str, offset: int) -> int:
    return text.count("\n", 0, offset) + 1


def read_text(path: Path) -> str:
    # 两端源码都是 UTF-8；bom 会破坏第一行的正则匹配，顺手去掉。
    return path.read_text(encoding="utf-8-sig")


def strip_comments(text: str) -> str:
    """把 `//` 与 `/* */` 注释替换成等长空格（保留换行），再交给解析器。

    注释里的示例代码不是 UI：文档里写一句 `NavItem('<文案>', <索引>)` 不能变成
    第 6 个入口。替换时保持长度与行号不变，所以 `line_of` 仍然指向真实行。
    """
    out: list[str] = []
    i = 0
    quote: str | None = None
    while i < len(text):
        ch = text[i]
        if quote is not None:
            out.append(ch)
            if ch == "\\" and i + 1 < len(text):
                out.append(text[i + 1])
                i += 2
                continue
            if ch == quote:
                quote = None
            i += 1
            continue
        if ch in ("'", '"', "`"):
            quote = ch
            out.append(ch)
            i += 1
            continue
        if ch == "/" and i + 1 < len(text):
            nxt = text[i + 1]
            if nxt == "/":
                end = text.find("\n", i)
                if end < 0:
                    end = len(text)
                out.append(" " * (end - i))
                i = end
                continue
            if nxt == "*":
                end = text.find("*/", i + 2)
                stop = len(text) if end < 0 else end + 2
                out.append("".join("\n" if c == "\n" else " " for c in text[i:stop]))
                i = stop
                continue
        out.append(ch)
        i += 1
    return "".join(out)


def find_matching(text: str, open_index: int, open_ch: str, close_ch: str) -> int:
    """返回与 `open_index` 处 `open_ch` 配对的 `close_ch` 下标；跳过字符串/模板字符串。"""
    depth = 0
    quote: str | None = None
    escaped = False
    i = open_index
    while i < len(text):
        ch = text[i]
        if quote is not None:
            if escaped:
                escaped = False
            elif ch == "\\":
                escaped = True
            elif ch == quote:
                quote = None
        else:
            if ch in ("'", '"', "`"):
                quote = ch
            elif ch == open_ch:
                depth += 1
            elif ch == close_ch:
                depth -= 1
                if depth == 0:
                    return i
        i += 1
    return -1


def find_matching_brace(text: str, open_index: int) -> int:
    return find_matching(text, open_index, "{", "}")


def unescape_single(value: str) -> str:
    out: list[str] = []
    i = 0
    while i < len(value):
        ch = value[i]
        if ch == "\\" and i + 1 < len(value):
            nxt = value[i + 1]
            out.append(STRING_ESCAPES.get(nxt, nxt))
            i += 2
        else:
            out.append(ch)
            i += 1
    return "".join(out)


def parse_tab_bar(rest: str) -> str | None:
    """从 `.tabBar(...)` 的参数里取字面量；支持字符串与 `{ text: '...' }`。"""
    match = re.search(
        r"""\.tabBar\s*\(\s*(?:\{[^{}]*?\btext\s*:\s*)?(['"])(.*?)\1""",
        rest,
        re.DOTALL,
    )
    if not match:
        return None
    return unescape_single(match.group(2))


def parse_flutter(text: str) -> list[Entry]:
    entries: list[Entry] = []
    for match in FLUTTER_NAV_RE.finditer(text):
        entries.append(
            Entry(
                index=int(match.group(1)),
                label=match.group(2),
                line=line_of(text, match.start()),
            )
        )
    return entries


def parse_flutter_desktop(text: str) -> list[Entry] | None:
    block = FLUTTER_DESKTOP_BLOCK_RE.search(text)
    if block is None:
        return None
    entries: list[Entry] = []
    for match in FLUTTER_DESKTOP_ENTRY_RE.finditer(block.group(1)):
        entries.append(
            Entry(
                index=int(match.group(1)),
                label=match.group(2),
                line=line_of(text, block.start(1) + match.start()),
            )
        )
    return entries


def parse_arkts(text: str) -> list[Entry]:
    """按 `TabContent() { ... }.tabBar('...')` 的顺序解析一级入口（底部页签）。"""
    entries: list[Entry] = []
    cursor = 0
    while True:
        hit = re.search(r"\bTabContent\s*\(\s*\)\s*\{", text[cursor:])
        if hit is None:
            break
        absolute = cursor + hit.start()
        open_brace = cursor + hit.end() - 1
        close_brace = find_matching_brace(text, open_brace)
        if close_brace < 0:
            # 括号不配对时不要猜；交给调用方按「没解析到」处理。
            break
        body = text[open_brace + 1 : close_brace]
        rest = text[close_brace + 1 : close_brace + 600]
        label = parse_tab_bar(rest)
        if label is not None:
            page_match = re.search(r"\b([A-Za-z_][A-Za-z0-9_]*)\s*\(", body)
            entries.append(
                Entry(
                    index=len(entries),
                    label=label,
                    line=line_of(text, absolute),
                    page=page_match.group(1) if page_match else None,
                )
            )
        cursor = close_brace + 1
    return entries


def is_nav_name(name: str) -> bool:
    """判断常量名是不是「导航清单」：按 camelCase / snake_case 分词后看词根。"""
    words = re.findall(r"[A-Z]+(?=[A-Z][a-z])|[A-Z]?[a-z]+|[A-Z]+|\d+", name)
    return any(word.lower() in ARKTS_NAV_WORDS for word in words)


def parse_nav_items(text: str) -> list[Entry]:
    """按出现顺序解析 `this.NavItem('记录', 0)` / `NavItem({ label: '记录' })`。"""
    hits: list[tuple[int, str]] = []
    for pattern in (ARKTS_NAV_ITEM_RE, ARKTS_NAV_ITEM_OBJECT_RE):
        hits.extend((match.start(), match.group(2)) for match in pattern.finditer(text))
    hits.sort(key=lambda hit: hit[0])
    return [
        Entry(index=index, label=unescape_single(label), line=line_of(text, offset))
        for index, (offset, label) in enumerate(hits)
    ]


def parse_nav_array(text: str) -> tuple[list[Entry], str | None]:
    """解析导航项数组常量；返回 (入口, 常量名)。

    支持 `['记录', ...]` 与 `[('记录', 0), ...]` / `[['记录', 0], ...]`。
    顺序按数组元素顺序——UI 的 `ForEach` 就是这么渲染的，所以交换元素必须红。
    """
    for match in ARKTS_ARRAY_HEAD_RE.finditer(text):
        name = match.group(1)
        if not is_nav_name(name):
            continue
        open_index = match.end() - 1
        close_index = find_matching(text, open_index, "[", "]")
        if close_index < 0:
            continue
        body_start = open_index + 1
        body = text[body_start:close_index]
        hits = [
            (found.start(), found.group(2))
            for found in ARKTS_TUPLE_ENTRY_RE.finditer(body)
        ]
        if len(hits) < 2:
            hits = [
                (found.start(), found.group(2))
                for found in ARKTS_STRING_RE.finditer(body)
            ]
        if len(hits) < 2:
            continue
        return (
            [
                Entry(
                    index=index,
                    label=unescape_single(label),
                    line=line_of(text, body_start + offset),
                )
                for index, (offset, label) in enumerate(hits)
            ],
            name,
        )
    return [], None


def parse_top_pages(text: str) -> dict[int, str]:
    """从 `currentTab === N` 的显隐条件里取每个入口对应的页面（只进 JSON）。"""
    pages: dict[int, str] = {}
    for match in ARKTS_TAB_PAGE_RE.finditer(text):
        pages.setdefault(int(match.group(2)), match.group(1))
    return pages


def has_top_nav_hint(text: str) -> bool:
    """有没有顶部导航的「痕迹」——哪怕取不到字面量标签也算，不能当没看见。"""
    if ARKTS_NAV_ITEM_CALL_RE.search(text):
        return True
    return any(
        is_nav_name(match.group(1)) for match in ARKTS_ARRAY_HEAD_RE.finditer(text)
    )


def parse_top_nav(text: str) -> tuple[list[Entry], str | None, bool]:
    """解析顶部导航；返回 (入口, 结构名, 是否看到顶部导航的痕迹)。"""
    entries = parse_nav_items(text)
    if entries:
        return entries, "nav-item", True
    entries, name = parse_nav_array(text)
    if entries:
        return entries, f"array:{name}", True
    return [], None, has_top_nav_hint(text)


def describe_structure(structure: str | None) -> str:
    if structure == "tabs":
        return "底部页签"
    if structure == "nav-item":
        return "顶部导航"
    if structure and structure.startswith("array:"):
        return f"顶部导航数组 {structure.split(':', 1)[1]}"
    return structure or ""


def parse_expected(raw: str) -> list[str]:
    # 同时接受 ASCII 逗号与中文逗号，避免手输 --expect 时得到一条奇怪的单标签。
    labels = [part.strip() for part in raw.replace("，", ",").split(",")]
    return [label for label in labels if label]


def load_strings(path: Path, failures: list[Failure]) -> dict[str, str]:
    if not path.is_file():
        failures.append(Failure("strings.missing_file", f"找不到文案表：{path}"))
        return {}
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        failures.append(Failure("strings.invalid_json", f"文案表不是合法 JSON：{error}"))
        return {}
    if not isinstance(data, dict):
        failures.append(Failure("strings.not_object", "文案表顶层必须是 key -> 中文文案 的对象"))
        return {}
    strings: dict[str, str] = {}
    for key, value in data.items():
        if not isinstance(key, str) or not key.strip():
            failures.append(Failure("strings.bad_key", f"文案表有非字符串/空 key：{key!r}"))
            continue
        if not isinstance(value, str) or not value.strip():
            failures.append(Failure("strings.bad_value", f"文案 key {key!r} 的值不是非空字符串"))
            continue
        strings[key] = value
    return strings


def scan_key_references(root: Path) -> list[dict[str, object]]:
    references: list[dict[str, object]] = []
    for base in (root / "apps", root / "packages"):
        if not base.is_dir():
            continue
        for current, dirs, files in os.walk(base):
            dirs[:] = [name for name in dirs if name not in SKIP_DIRS]
            for name in files:
                path = Path(current) / name
                if path.suffix not in (".dart", ".ets"):
                    continue
                try:
                    text = read_text(path)
                except (OSError, UnicodeDecodeError):
                    continue
                for match in KEY_REFERENCE_RE.finditer(text):
                    key = match.group(2)
                    references.append(
                        {
                            "key": key,
                            "path": str(path.relative_to(root)),
                            "line": line_of(text, match.start()),
                        }
                    )
    return references


def nav_failures(
    side: str,
    entries: list[Entry],
    expected: list[str],
    failures: list[Failure],
) -> None:
    labels = [entry.label for entry in entries]
    if not entries:
        failures.append(
            Failure(
                f"{side}.nav.empty",
                f"{side} 没有解析到一级入口（期望 {len(expected)} 个：{' / '.join(expected)}）",
            )
        )
        return
    if labels != expected:
        if len(labels) != len(expected):
            failures.append(
                Failure(
                    f"{side}.nav.count",
                    f"{side} 有 {len(labels)} 个入口，期望 {len(expected)} 个："
                    f"{' / '.join(labels)} != {' / '.join(expected)}",
                )
            )
        missing = [label for label in expected if label not in labels]
        extra = [label for label in labels if label not in expected]
        if missing or extra:
            detail: list[str] = []
            if missing:
                detail.append(f"缺：{' / '.join(missing)}")
            if extra:
                detail.append(f"多：{' / '.join(extra)}")
            failures.append(
                Failure(
                    f"{side}.nav.set",
                    f"{side} 入口集合与契约不一致（{'；'.join(detail)}）",
                )
            )
        if len(labels) == len(expected) and labels != expected:
            for index, (actual, wanted) in enumerate(zip(labels, expected)):
                if actual != wanted:
                    failures.append(
                        Failure(
                            f"{side}.nav.order[{index}]",
                            f"{side} 第 {index + 1} 个入口是 {actual!r}，期望 {wanted!r}",
                        )
                    )
                    break
        # 顺序相同但数量不同时，单独指出多出来的位置。
        if len(labels) != len(expected) and labels[: min(len(labels), len(expected))] != expected[: min(len(labels), len(expected))]:
            for index, (actual, wanted) in enumerate(zip(labels, expected)):
                if actual != wanted:
                    failures.append(
                        Failure(
                            f"{side}.nav.order[{index}]",
                            f"{side} 第 {index + 1} 个入口是 {actual!r}，期望 {wanted!r}",
                        )
                    )
                    break


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="校验 Flutter / ArkTS 的一级入口与共享文案 key。红就是红，不用 --force 掩盖。",
    )
    parser.add_argument("--root", default=str(ROOT), help="仓库根目录（默认：脚本上一级）")
    parser.add_argument("--flutter", default=DEFAULT_FLUTTER, help="Flutter 壳文件（相对 --root）")
    parser.add_argument("--arkts", default=DEFAULT_ARKTS, help="ArkTS 根页面（相对 --root）")
    parser.add_argument("--strings", default=DEFAULT_STRINGS, help="共享文案表（相对 --root）")
    parser.add_argument(
        "--expect",
        default=None,
        help="逗号分隔的期望入口标签；默认用产品决定的 5 入口。不是放行开关。",
    )
    parser.add_argument("--json", action="store_true", help="只输出机器可读 JSON")
    args = parser.parse_args(argv)

    root = Path(args.root).resolve()
    flutter_path = (root / args.flutter).resolve() if not Path(args.flutter).is_absolute() else Path(args.flutter)
    arkts_path = (root / args.arkts).resolve() if not Path(args.arkts).is_absolute() else Path(args.arkts)
    strings_path = (root / args.strings).resolve() if not Path(args.strings).is_absolute() else Path(args.strings)

    expected = parse_expected(args.expect) if args.expect else [label for _, label in CANONICAL_NAV]
    if not expected:
        parser.error("--expect 不能是空列表")

    failures: list[Failure] = []

    if not flutter_path.is_file():
        failures.append(Failure("flutter.missing_file", f"找不到 Flutter 壳：{flutter_path}"))
        flutter_entries: list[Entry] = []
    else:
        flutter_text = strip_comments(read_text(flutter_path))
        flutter_entries = parse_flutter(flutter_text)
        desktop_entries = parse_flutter_desktop(flutter_text)
        if desktop_entries is not None and [e.label for e in desktop_entries] != [
            e.label for e in flutter_entries
        ]:
            failures.append(
                Failure(
                    "flutter.desktop_nav",
                    "Flutter 桌面顶栏与底部/移动导航的入口不一致："
                    f"桌面 {' / '.join(e.label for e in desktop_entries)} != "
                    f"_navItem {' / '.join(e.label for e in flutter_entries)}",
                )
            )

    arkts_entries: list[Entry] = []
    arkts_error: str | None = None
    arkts_shell_present = False
    arkts_structure: str | None = None
    arkts_structures = {"tabs": False, "top_bar": False}
    nav_status = "error"
    nav_reason: str | None = None
    if not arkts_path.is_file():
        arkts_error = f"找不到 ArkTS 根页面：{arkts_path}"
        nav_reason = arkts_error
        failures.append(Failure("arkts.missing_file", arkts_error))
    else:
        # 先去掉注释：注释里贴的示例入口不是 UI，不能算进清单。
        arkts_text = strip_comments(read_text(arkts_path))
        # 两种结构各自解析；谁存在就核对谁。两个都不存在才允许跳过。
        tabs_present = re.search(r"\bTabContent\s*\(", arkts_text) is not None
        tab_entries = parse_arkts(arkts_text) if tabs_present else []
        top_entries, top_structure, top_hint = parse_top_nav(arkts_text)
        arkts_structures = {"tabs": tabs_present, "top_bar": top_hint}
        arkts_shell_present = tabs_present or top_hint
        if top_entries:
            pages = parse_top_pages(arkts_text)
            for entry in top_entries:
                entry.page = pages.get(entry.index)
        if top_entries:
            arkts_entries = top_entries
            arkts_structure = top_structure
        elif tab_entries:
            arkts_entries = tab_entries
            arkts_structure = "tabs"

        # 两份结构同时存在：入口清单必须一致；不一致说明有一份是废弃的，不能猜哪份算数。
        if tab_entries and top_entries:
            tab_labels = [entry.label for entry in tab_entries]
            top_labels = [entry.label for entry in top_entries]
            if tab_labels != top_labels:
                failures.append(
                    Failure(
                        "arkts.nav.structures",
                        "Index.ets 同时有底部页签与顶部导航，但入口不一致："
                        f"底部 {' / '.join(tab_labels)} != 顶部 {' / '.join(top_labels)}；"
                        "删掉废弃的那一份，只留一份可核对的清单",
                    )
                )

        if top_hint and not top_entries:
            # 看到了顶部导航的写法却取不到字面量标签：不能假装没看见，按解析失败处理。
            nav_status = "error"
            nav_reason = (
                "Index.ets 有顶部导航的写法（NavItem(...) 调用或 nav/tab 命名的数组），"
                "但取不到字面量入口标签；检查器无法核对顺序，按解析失败处理"
            )
            arkts_error = nav_reason
            failures.append(Failure("arkts.nav_unparsed", nav_reason))
        elif not arkts_entries:
            if tabs_present:
                nav_status = "error"
                nav_reason = "Index.ets 有 TabContent，但没有解析到任何 .tabBar(...) 入口"
                arkts_error = nav_reason
                failures.append(Failure("arkts.tabbar_unparsed", nav_reason))
            else:
                # 比较一个不存在的东西没有意义。跳过而不是失败，但必须留下可审计的原因：
                # 人类输出打 [跳过]，JSON 的 nav.status 是 skipped（不是 passed）。
                nav_status = "skipped"
                nav_reason = (
                    "Index.ets 里既没有底部页签（TabContent().tabBar(...)），也没有顶部导航"
                    "（NavItem('标签', 索引) 调用或名字带 nav/tab/entry 的数组常量）"
                    "——没有可比对的入口清单"
                )
                arkts_error = nav_reason
        else:
            nav_status = "checked"

    if nav_status == "checked":
        nav_failures("flutter", flutter_entries, expected, failures)
        nav_failures("arkts", arkts_entries, expected, failures)

    strings = load_strings(strings_path, failures)
    required_missing = [key for key in REQUIRED_KEYS if key not in strings]
    if required_missing:
        failures.append(
            Failure(
                "strings.required_missing",
                f"文案表缺必需 key：{', '.join(required_missing)}",
            )
        )
    canonical_missing: list[str] = []
    for nav_id, label in CANONICAL_NAV:
        key = f"nav.{nav_id}"
        if key not in strings:
            canonical_missing.append(key)
            continue
        if strings[key] != label:
            failures.append(
                Failure(
                    "strings.nav_value",
                    f"{key} 是 {strings[key]!r}，契约期望 {label!r}",
                )
            )
    if canonical_missing:
        failures.append(
            Failure("strings.nav_missing", f"文案表缺导航 key：{', '.join(canonical_missing)}")
        )

    references = scan_key_references(root)
    referenced_keys = sorted({ref["key"] for ref in references})
    referenced_missing = [key for key in referenced_keys if key not in strings]
    if referenced_missing:
        failures.append(
            Failure(
                "strings.referenced_missing",
                f"源码引用了文案表里不存在的 key：{', '.join(referenced_missing)}",
            )
        )

    # 两端都解析到入口时，再给出一个集合/顺序的交叉结论，方便读输出。
    for side, entries in (("flutter", flutter_entries), ("arkts", arkts_entries)):
        if not entries:
            continue
        labels = [entry.label for entry in entries]
        if len(labels) != len(set(labels)):
            failures.append(
                Failure(f"{side}.nav.duplicate", f"{side} 一级入口有重复：{' / '.join(labels)}")
            )

    ok = not failures
    nav_consistent: bool | None = None
    if nav_status == "checked":
        nav_consistent = not any(
            failure.code.startswith(("flutter.nav", "arkts.nav")) for failure in failures
        )
    result = {
        "schema": "ui-parity/v2",
        "ok": ok,
        "root": str(root),
        "expected": {
            "labels": expected,
            "source": "--expect（仅诊断，CI 不传）" if args.expect else "canonical",
        },
        "nav": {
            "status": nav_status,
            "reason": nav_reason,
            "consistent": nav_consistent,
        },
        "skipped": (
            [
                {
                    "check": "nav.cross_side",
                    "reason": nav_reason,
                    "effect": "exit 0；不声称入口一致；找到任一种导航结构后自动恢复检查",
                }
            ]
            if nav_status == "skipped"
            else []
        ),
        "flutter": {
            "path": str(flutter_path),
            "exists": flutter_path.is_file(),
            "nav": [entry.as_dict() for entry in flutter_entries],
        },
        "arkts": {
            "path": str(arkts_path),
            "exists": arkts_path.is_file(),
            "shell_present": arkts_shell_present,
            "nav_structure": arkts_structure,
            "structures": arkts_structures,
            "nav": [entry.as_dict() for entry in arkts_entries],
            "error": arkts_error,
        },
        "strings": {
            "path": str(strings_path),
            "exists": strings_path.is_file(),
            "key_count": len(strings),
            "required_missing": required_missing,
            "referenced": references,
            "referenced_key_count": len(referenced_keys),
            "referenced_keys": referenced_keys,
            "referenced_missing": referenced_missing,
        },
        "failures": [failure.as_dict() for failure in failures],
    }

    if args.json:
        print(json.dumps(result, ensure_ascii=False, indent=2))
    else:
        def labels(entries: list[Entry]) -> str:
            return " / ".join(entry.label for entry in entries) if entries else "（未解析到）"

        def rel(path: Path) -> str:
            return str(path.relative_to(root)) if path.is_relative_to(root) else str(path)

        print("UI 结构检查（5 入口 + 共享文案 key）")
        print(f"  期望：{len(expected)} 个入口：{' / '.join(expected)}")
        print(
            f"  Flutter：{len(flutter_entries)} 个入口：{labels(flutter_entries)}"
            f"  [{rel(flutter_path)}]"
        )
        if nav_status == "skipped":
            arkts_nav_label = "（没有可比对的一级导航，跳过）"
            structure_note = ""
        else:
            arkts_nav_label = labels(arkts_entries)
            structure_note = (
                f"（{describe_structure(arkts_structure)}）" if arkts_structure else ""
            )
        print(
            f"  ArkTS  ：{len(arkts_entries)} 个入口{structure_note}：{arkts_nav_label}"
            f"  [{rel(arkts_path)}]"
        )
        print(f"  文案表：{len(strings)} 个 key  [{rel(strings_path)}]")
        print(f"  源码引用：{len(referenced_keys)} 个 key（{len(references)} 处）")
        if args.expect:
            print("  [诊断] 使用了 --expect：只改比较基准，不是放行开关；CI 永不传这个参数。")
        if nav_status == "skipped":
            print()
            print(f"[跳过] {nav_reason}")
            print(
                "  跳过不等于通过：JSON 的 nav.status 是 skipped，consistent 是 null；"
                "这只表示没有可比对的导航结构。"
            )
        if failures:
            print()
            for failure in failures:
                print(f"[FAIL] {failure.code}: {failure.message}")
            print()
            suffix = "；入口检查已跳过" if nav_status == "skipped" else ""
            print(f"结果：红（{len(failures)} 项失败{suffix}）。")
            print("这不是脚本坏了：入口/文案不一致就是这次要抓的漂移。")
        elif nav_status == "skipped":
            print()
            print("结果：绿（入口检查跳过；文案表检查通过）。")
            print("跳过原因见上：没有可比对的导航结构，不是「入口一致」。")
        else:
            print()
            print("结果：绿。两端一级入口与共享文案 key 一致。")

    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())