#!/usr/bin/env python3
"""跨端界面结构检查：5 个一级入口 + 共享文案 key。

检查两层：

1. **一级导航**：Flutter 的 `_navItem(...)` 调用与 ArkTS 的
   `TabContent() { ... }.tabBar('...')` 必须与产品决定一致，且两端顺序相同。
   产品决定见 `docs/product/界面结构契约.md` §2：
   `记录 / 日记 / 搜索 / 片段 / 我的`（任务中心是二级，不是 tab）。
2. **文案 key**：`packages/ui_strings/ui_strings.json` 必须存在，必须是
   `key -> 非空中文文案` 的扁平 JSON；契约定下的必需 key 一个都不能少。
   源码里已经按约定引用（`uiStrings.get('...')` / `uiStrings.t('...')` /
   `uiString('...')`）的 key 也必须在表里。

这个脚本**现在应当红**：`origin/main` 上 `Index.ets` 还是录音验证页，PR #150
的鸿蒙产品壳（4 入口）尚未合入。等 PR #150 改成 5 入口并合入后自然转绿。
**不要用 `|| true` / `continue-on-error` 把它伪装成绿**——红灯本身就是交付物。

用法：
    python3 tools/check_ui_parity.py
    python3 tools/check_ui_parity.py --json
    python3 tools/check_ui_parity.py --arkts /path/to/Index.ets
    python3 tools/check_ui_parity.py --expect 记录,日记,搜索,片段,我的

`--expect` 只改变比较基准（用于排障/演练），**不是放行开关**：不匹配仍然非零退出。

退出码：0 = 通过；1 = 发现漂移/缺 key/解析失败；2 = 命令行用法错误。
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

# 源码引用文案 key 的约定写法。当前两端都还没接表，所以引用数应当是 0；
# 接表之后这里会自动开始检查，防止代码用了表里没有的 key。
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


def find_matching_brace(text: str, open_index: int) -> int:
    """返回与 `open_index` 处 `{` 配对的 `}` 下标；跳过字符串/模板字符串。"""
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
            elif ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
                if depth == 0:
                    return i
        i += 1
    return -1


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
    """按 `TabContent() { ... }.tabBar('...')` 的顺序解析一级入口。"""
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
        flutter_text = read_text(flutter_path)
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
    nav_failures("flutter", flutter_entries, expected, failures)

    arkts_entries: list[Entry] = []
    arkts_error: str | None = None
    if not arkts_path.is_file():
        arkts_error = f"找不到 ArkTS 根页面：{arkts_path}"
        failures.append(Failure("arkts.missing_file", arkts_error))
    else:
        arkts_text = read_text(arkts_path)
        arkts_entries = parse_arkts(arkts_text)
        if not arkts_entries:
            arkts_error = (
                "没有解析到 TabContent/.tabBar 一级入口；"
                "PR #150 的鸿蒙产品壳还没合入时这是预期红灯"
            )
            failures.append(Failure("arkts.shell_missing", arkts_error))
        else:
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
    referenced_missing = sorted({ref["key"] for ref in references if ref["key"] not in strings})
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
    result = {
        "schema": "ui-parity/v1",
        "ok": ok,
        "root": str(root),
        "expected": {
            "labels": expected,
            "source": "--expect" if args.expect else "canonical",
        },
        "flutter": {
            "path": str(flutter_path),
            "exists": flutter_path.is_file(),
            "nav": [entry.as_dict() for entry in flutter_entries],
        },
        "arkts": {
            "path": str(arkts_path),
            "exists": arkts_path.is_file(),
            "nav": [entry.as_dict() for entry in arkts_entries],
            "error": arkts_error,
        },
        "strings": {
            "path": str(strings_path),
            "exists": strings_path.is_file(),
            "key_count": len(strings),
            "required_missing": required_missing,
            "referenced": references,
            "referenced_missing": referenced_missing,
        },
        "failures": [failure.as_dict() for failure in failures],
    }

    if args.json:
        print(json.dumps(result, ensure_ascii=False, indent=2))
    else:
        def labels(entries: list[Entry]) -> str:
            return " / ".join(entry.label for entry in entries) if entries else "（未解析到）"

        print("UI 结构检查（5 入口 + 共享文案 key）")
        print(f"  期望：{len(expected)} 个入口：{' / '.join(expected)}")
        print(
            f"  Flutter：{len(flutter_entries)} 个入口：{labels(flutter_entries)}"
            f"  [{flutter_path.relative_to(root) if flutter_path.is_relative_to(root) else flutter_path}]"
        )
        print(
            f"  ArkTS  ：{len(arkts_entries)} 个入口：{labels(arkts_entries)}"
            f"  [{arkts_path.relative_to(root) if arkts_path.is_relative_to(root) else arkts_path}]"
        )
        print(
            f"  文案表：{len(strings)} 个 key"
            f"  [{strings_path.relative_to(root) if strings_path.is_relative_to(root) else strings_path}]"
        )
        print(f"  源码引用：{len(references)} 个 key")
        if failures:
            print()
            for failure in failures:
                print(f"[FAIL] {failure.code}: {failure.message}")
            print()
            print(f"结果：红（{len(failures)} 项失败）。")
            print("这不是脚本坏了：入口/文案不一致就是这次要抓的漂移。")
            print("PR #150 把鸿蒙改成 5 入口并合入后，这一步会自然转绿。")
        else:
            print()
            print("结果：绿。两端一级入口与共享文案 key 一致。")

    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
