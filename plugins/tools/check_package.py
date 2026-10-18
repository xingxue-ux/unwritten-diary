#!/usr/bin/env python3
"""插件包静态检查（不执行包内容，不联网）。

用法：
    python3 plugins/tools/check_package.py <插件目录或 zip>

只做打包阶段能独立验证的事：清单字段形状、路径安全、哈希覆盖、entryFile 存在、
Wasm 魔数。核心安装路径才是权威（还会检查平台/API 版本/权限/模板占位符）。
"""

from __future__ import annotations

import hashlib
import json
import re
import sys
import zipfile
from pathlib import Path, PurePosixPath

MAX_FILES = 128
MAX_FILE = 8 * 1024 * 1024
MAX_TOTAL = 16 * 1024 * 1024
ID = re.compile(r"^[a-z0-9][a-z0-9._-]{1,63}$")
VERSION = re.compile(r"^\d+\.\d+\.\d+$")
HEX = re.compile(r"^(sha256:)?[0-9a-f]{64}$")


def safe_path(name: str) -> bool:
    if not name or len(name) > 160 or "\\" in name or "\0" in name or ":" in name:
        return False
    if name.startswith("/"):
        return False
    return all(part not in ("", ".", "..") for part in name.split("/"))


def read_package(path: Path) -> dict[str, bytes]:
    if path.is_dir():
        files: dict[str, bytes] = {}
        for item in sorted(path.rglob("*")):
            if item.is_symlink():
                raise SystemExit(f"不允许符号链接：{item}")
            if item.is_file():
                rel = item.relative_to(path).as_posix()
                if not safe_path(rel):
                    raise SystemExit(f"不安全路径：{rel}")
                files[rel] = item.read_bytes()
        return files
    if path.is_file() and zipfile.is_zipfile(path):
        files = {}
        with zipfile.ZipFile(path) as archive:
            for info in archive.infolist():
                if info.is_dir():
                    continue
                name = info.filename
                if not safe_path(name):
                    raise SystemExit(f"zip 里有不安全路径：{name}")
                if info.file_size > MAX_FILE:
                    raise SystemExit(f"文件过大：{name}")
                files[name] = archive.read(info)
        return files
    raise SystemExit("插件包必须是目录或 zip")


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit(__doc__.strip())
    root = Path(sys.argv[1])
    files = read_package(root)
    if len(files) > MAX_FILES:
        raise SystemExit(f"文件数超过 {MAX_FILES}")
    total = sum(len(data) for data in files.values())
    if total > MAX_TOTAL:
        raise SystemExit(f"解压后超过 {MAX_TOTAL} 字节")
    if "manifest.json" not in files:
        raise SystemExit("缺少 manifest.json")
    manifest = json.loads(files.pop("manifest.json"))
    errors: list[str] = []

    def need(name: str, cond: bool, why: str) -> None:
        if not cond:
            errors.append(f"{name}: {why}")

    need("manifestVersion", manifest.get("manifestVersion") == 1, "必须是 1")
    need("id", bool(ID.match(str(manifest.get("id", "")))), "不合法")
    need("version", bool(VERSION.match(str(manifest.get("version", "")))), "必须是三段数字")
    for key in ("name", "author", "license", "apiRange"):
        need(key, isinstance(manifest.get(key), str) and bool(manifest[key].strip()), "不能为空")
    platforms = manifest.get("supportedPlatforms")
    need("supportedPlatforms", isinstance(platforms, list) and bool(platforms), "不能为空")
    entry_kind = manifest.get("entryKind")
    need("entryKind", entry_kind in ("wasm", "declarative"), "只能是 wasm / declarative")
    entry = manifest.get("entryFile")
    need("entryFile", isinstance(entry, str) and safe_path(entry), "必须是相对路径")
    if isinstance(entry, str):
        need("entryFile", entry in files, "包里没有这个文件")
        if entry_kind == "wasm":
            need("entryFile", entry.endswith(".wasm"), "wasm 入口必须是 .wasm")
        if entry_kind == "declarative":
            need("entryFile", entry.endswith(".json"), "declarative 入口必须是 .json")
    events = manifest.get("activationEvents")
    need("activationEvents", isinstance(events, list) and bool(events), "至少要一个激活点")
    if isinstance(events, list):
        for event in events:
            need("activationEvents", isinstance(event, str), "必须是字符串")
            need("activationEvents", str(event) != "startup", "不支持启动激活点")
    hashes = manifest.get("packageHashes")
    need("packageHashes", isinstance(hashes, dict) and bool(hashes), "不能为空")
    if isinstance(hashes, dict):
        if set(hashes) != set(files):
            missing = sorted(set(files) - set(hashes))
            extra = sorted(set(hashes) - set(files))
            errors.append(f"packageHashes 与文件不一致：未列出 {missing}，多列 {extra}")
        for name, expected in hashes.items():
            if not isinstance(expected, str) or not HEX.match(expected):
                errors.append(f"哈希格式不对：{name}")
                continue
            actual = hashlib.sha256(files[name]).hexdigest() if name in files else ""
            if expected.removeprefix("sha256:").lower() != actual:
                errors.append(f"哈希不符：{name}")
    if isinstance(entry, str) and entry in files and entry_kind == "wasm":
        data = files[entry]
        need("entryFile", data[:8] == b"\0asm\x01\0\0\0", "不是 WebAssembly 1.0 模块")
    if errors:
        print("✗ 检查失败：")
        for error in errors:
            print("  -", error)
        raise SystemExit(1)
    print(f"✓ {manifest['id']} {manifest['version']}：{len(files)} 个文件，清单/哈希/入口一致（未执行包内容）")


if __name__ == "__main__":
    main()
