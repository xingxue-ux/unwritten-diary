#!/usr/bin/env python3
"""在最小编译/测试存在之前，先让文档、模板与共用场景不腐坏。

检查项：
1. Markdown 里的相对链接指向仓库内真实存在的路径（跳过代码块、行内代码、外链和锚点）。
2. `.github/` 下的 YAML 能被解析，issue 模板具备 GitHub issue form 必需的字段。
3. `tests/fixtures/scenarios/` 下的场景 JSON 能解析、必备字段齐全、id 唯一。
4. 计划文档与仓库模板的关键文件存在。

退出码 0 表示通过；任何一条失败会逐条打印并返回 1。
"""

from __future__ import annotations

import fnmatch
import json
import os
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

LINK_RE = re.compile(r"(?<!!)\[[^\]]*\]\(([^)]+)\)")
FENCE_RE = re.compile(r"^\s*(```|~~~)")
INLINE_CODE_RE = re.compile(r"`[^`]*`")
SKIP_SCHEMES = ("http://", "https://", "mailto:", "#")

REQUIRED_FILES = [
    "README.md",
    "LICENSE",
    "CONTRIBUTING.md",
    ".github/pull_request_template.md",
    ".github/ISSUE_TEMPLATE/task.yml",
    ".github/ISSUE_TEMPLATE/contract-change.yml",
    "docs/development/00-开发总览.md",
    "docs/development/01-共享接口契约.md",
    "docs/development/02-前端开发任务书.md",
    "docs/development/03-后端开发任务书.md",
    "docs/development/04-验收与测试计划.md",
    "docs/development/05-模型交接提示词.md",
    "tests/fixtures/README.md",
]

SCENARIO_FIELDS = ("id", "name", "goal", "setup", "steps", "expected")

problems: list[str] = []
notes: list[str] = []


# 遍历时要跳过的目录：构建产物与依赖缓存不在仓库里，但工作区里常有。
# 不跳过会在有大 target/ 的工作区上把 18 GB 构建目录也扫一遍——在 /mnt/e（drvfs）上
# 实测会抛 `OSError: [Errno 12] Cannot allocate memory`（2026-10-18 踩过）。
# 注意 CI 上没有这些目录，所以**绿了也不代表本地能过**，这一条只能靠本地跑。
SKIP_DIRS = frozenset({
    ".git",
    "target",
    "node_modules",
    "oh_modules",
    ".hvigor",
    "build",
    "dist",
    ".dart_tool",
    "Pods",
})


def repo_files(pattern: str) -> list[Path]:
    """在仓库里找匹配的文件，**遍历时就剪掉构建目录**。

    `Path.rglob` 会先把整棵树走完再让人过滤，所以在有 `target/` 的工作区上照样会
    进 18 GB 的构建目录——在 `/mnt/e`（drvfs）上实测抛
    `OSError: [Errno 12] Cannot allocate memory`（2026-10-18 踩过）。
    因此这里用 `os.walk` 并在 `dirs` 上原地剪枝。
    """
    name = Path(pattern).name
    found: list[Path] = []
    for root, dirs, files in os.walk(ROOT):
        dirs[:] = [d for d in dirs if d not in SKIP_DIRS]
        for f in files:
            if fnmatch.fnmatch(f, name):
                found.append(Path(root) / f)
    return sorted(found)


def strip_code(text: str) -> str:
    """去掉围栏代码块与行内代码，避免把示例里的链接当成真链接。"""
    out: list[str] = []
    in_fence = False
    for line in text.splitlines():
        if FENCE_RE.match(line):
            in_fence = not in_fence
            out.append("")
            continue
        out.append("" if in_fence else INLINE_CODE_RE.sub("", line))
    return "\n".join(out)


def check_required_files() -> None:
    for rel in REQUIRED_FILES:
        if not (ROOT / rel).is_file():
            problems.append(f"缺少关键文件：{rel}")


def check_markdown_links() -> None:
    for md in repo_files("*.md"):
        text = strip_code(md.read_text(encoding="utf-8"))
        for match in LINK_RE.finditer(text):
            raw = match.group(1).strip()
            if raw.startswith(SKIP_SCHEMES):
                continue
            target = raw.split("#", 1)[0].split("?", 1)[0].strip()
            if not target:
                continue
            if not (md.parent / target).exists():
                rel = md.relative_to(ROOT)
                problems.append(f"{rel}: 链接指向不存在的路径 -> {raw}")


def check_yaml() -> None:
    try:
        import yaml
    except ImportError:  # 本地可能没装；CI 上必须装上才算通过
        notes.append("未安装 PyYAML，跳过 YAML 校验")
        return

    templates = {
        p.relative_to(ROOT).as_posix(): p
        for p in repo_files("*.yml") + repo_files("*.yaml")
        if p.parts[len(ROOT.parts)] == ".github"
    }
    for rel, path in sorted(templates.items()):
        try:
            data = yaml.safe_load(path.read_text(encoding="utf-8"))
        except yaml.YAMLError as exc:
            problems.append(f"{rel}: YAML 解析失败 -> {exc}")
            continue
        if not isinstance(data, dict):
            problems.append(f"{rel}: 顶层不是映射")
            continue
        if ".github/ISSUE_TEMPLATE/" in f"/{rel}" and not rel.endswith("config.yml"):
            for field in ("name", "description", "body"):
                if field not in data:
                    problems.append(f"{rel}: issue 模板缺少字段 `{field}`")


def check_fixtures() -> None:
    """共用场景是两层的共同依据，格式坏了要比编译错误更早发现。"""
    scenario_dir = ROOT / "tests" / "fixtures" / "scenarios"
    if not scenario_dir.is_dir():
        notes.append("没有 tests/fixtures/scenarios 目录，跳过场景校验")
        return

    for path in sorted(scenario_dir.glob("*.json")):
        rel = path.relative_to(ROOT)
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except json.JSONDecodeError as exc:
            problems.append(f"{rel}: JSON 解析失败 -> {exc}")
            continue
        if not isinstance(data, dict):
            problems.append(f"{rel}: 顶层不是对象")
            continue
        scenarios = data.get("scenarios")
        if not isinstance(scenarios, list) or not scenarios:
            problems.append(f"{rel}: 缺少非空的 scenarios 数组")
            continue

        seen: set[str] = set()
        for index, scenario in enumerate(scenarios):
            where = f"{rel} 第 {index + 1} 条"
            if not isinstance(scenario, dict):
                problems.append(f"{where}: 不是对象")
                continue
            for field in SCENARIO_FIELDS:
                if field not in scenario:
                    problems.append(f"{where}: 缺少字段 `{field}`")
            sid = scenario.get("id")
            if isinstance(sid, str):
                if sid in seen:
                    problems.append(f"{where}: id 重复 `{sid}`")
                seen.add(sid)
                where = f"{rel} {sid}"


def main() -> int:
    check_required_files()
    check_markdown_links()
    check_yaml()
    check_fixtures()

    for note in notes:
        print(f"[跳过] {note}")
    for problem in problems:
        print(f"[失败] {problem}")

    if problems:
        print(f"\n共 {len(problems)} 处问题。")
        return 1

    print("[通过] 关键文件、Markdown 相对链接、.github YAML、共用场景 JSON 均正常。")
    return 0


if __name__ == "__main__":
    sys.exit(main())