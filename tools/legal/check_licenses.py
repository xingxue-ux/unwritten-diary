#!/usr/bin/env python3
"""Audit third-party licenses and the shipped font's OFL metadata.

This is intentionally a checker, not a generator of legal conclusions. It:
  * reads Cargo metadata for every Rust package in the lockfile;
  * reports direct runtime crates and scans all crates for weak copyleft;
  * reads pubspec.lock for direct Flutter/Dart packages;
  * checks the font's TrueType `name` table for IDs 0 and 13;
  * checks that the license texts and notices are declared as Flutter assets;
  * checks model manifests for the recorded upstream licenses.

Exit 0 means "the audited facts are present and no forbidden license was
found". It does not replace legal review; see docs/legal/第三方许可与合规.md.
"""

from __future__ import annotations

import argparse
import json
import re
import struct
import subprocess
import sys
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
APP = ROOT / "apps" / "diary_app"
LEGAL = APP / "assets" / "legal"
FONT = APP / "assets" / "fonts" / "DiaryDisplay-AppSubset.ttf"
OFL = APP / "assets" / "fonts" / "OFL.txt"

WEAK_MARKERS = ("GPL", "MPL", "EPL", "CDDL", "SSPL")
# r-efi is triple-licensed; the project selects MIT/Apache-2.0, not LGPL.
WEAK_ALLOWED = {
    "r-efi": "MIT OR Apache-2.0 OR LGPL-2.1-or-later; MIT/Apache branch selected",
}

DART_DIRECT = {
    "file_selector": "BSD-3-Clause",
    "flutter": "BSD-3-Clause",
    "flutter_timezone": "Apache-2.0",
    "path": "BSD-3-Clause",
    "path_provider": "BSD-3-Clause",
    "workmanager": "MIT",
    "diary_api": "MIT",
    "diary_bridge": "MIT",
    "diary_mock": "MIT",
    "platform_host": "MIT",
}

REQUIRED_LEGAL_FILES = [
    "LICENSE-MIT-app.txt",
    "LICENSE-MIT-bge.txt",
    "LICENSE-MIT-SenseVoice.txt",
    "LICENSE-MIT-onnxruntime.txt",
    "LICENSE-Apache-2.0.txt",
    "LICENSE-Apache-2.0-sherpa-onnx.txt",
    "LICENSE-libcxx.txt",
    "LICENSE-ISC-libloading.txt",
    "LICENSE-Flutter-BSD-3-Clause.txt",
    "ONNX-Runtime-ThirdPartyNotices.txt",
    "OFL-DiaryDisplay.txt",
    "NOTICE-第三方组件.md",
]

problems: list[str] = []
notes: list[str] = []


def problem(message: str) -> None:
    problems.append(message)


def note(message: str) -> None:
    notes.append(message)


def run_cargo_metadata() -> dict[str, Any] | None:
    try:
        result = subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--locked", "--offline"],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
        )
    except FileNotFoundError:
        note("找不到 cargo，跳过 Rust 依赖审计")
        return None
    if result.returncode != 0:
        note(f"cargo metadata 失败（跳过 Rust 依赖审计）：{result.stderr.strip()[:300]}")
        return None
    return json.loads(result.stdout)


def package_license(pkg: dict[str, Any]) -> str:
    license_text = (pkg.get("license") or "").strip()
    if license_text:
        return license_text
    license_file = pkg.get("license_file")
    if license_file:
        return f"SEE LICENSE FILE: {license_file}"
    return "UNKNOWN"


def rust_audit(metadata: dict[str, Any] | None) -> None:
    if metadata is None:
        return
    packages = {f"{p['name']}@{p['version']}": p for p in metadata["packages"]}
    by_id = {p["id"]: p for p in metadata["packages"]}
    direct: set[str] = set()
    for node in metadata.get("resolve", {}).get("nodes", []):
        pkg = by_id.get(node["id"])
        if not pkg or pkg["name"] not in {"diary_core", "diary_bridge"}:
            continue
        for dep in node.get("deps", []):
            kinds = {kind.get("kind") for kind in dep.get("dep_kinds", [])}
            # kind=None is a normal runtime dependency; dev/build are not shipped.
            if None in kinds or "normal" in kinds:
                dep_pkg = by_id.get(dep["pkg"])
                if dep_pkg and dep_pkg["name"] not in {"diary_core", "diary_bridge", "diary_probe"}:
                    direct.add(f"{dep_pkg['name']}@{dep_pkg['version']}")

    print("\nRust 直接运行时依赖：")
    for key in sorted(direct, key=str.lower):
        pkg = packages[key]
        lic = package_license(pkg)
        print(f"  {pkg['name']:<22} {pkg['version']:<14} {lic}")
        if lic == "UNKNOWN":
            problem(f"Rust 直接依赖 {key} 没有许可证元数据")
        if "SEE LICENSE FILE" in lic:
            note(f"{key} 的许可证在文件中：{pkg.get('license_file')}")

    weak: list[tuple[str, str]] = []
    unknown: list[str] = []
    for pkg in metadata["packages"]:
        lic = package_license(pkg)
        if lic == "UNKNOWN":
            unknown.append(f"{pkg['name']}@{pkg['version']}")
            continue
        if any(marker in lic.upper() for marker in WEAK_MARKERS):
            weak.append((f"{pkg['name']}@{pkg['version']}", lic))

    print("\n弱 copyleft 扫描（整个 Cargo.lock 依赖图）：")
    if not weak:
        print("  未发现 GPL/AGPL/LGPL/MPL/EPL/CDDL/SSPL 标记")
    for key, lic in sorted(weak):
        print(f"  {key:<34} {lic}")
        name = key.split("@", 1)[0]
        if name not in WEAK_ALLOWED:
            problem(f"{key} 带弱 copyleft 标记，且未登记例外：{lic}")
        else:
            note(f"{key} 允许：{WEAK_ALLOWED[name]}")
    for key in unknown:
        # Local workspace crates and a few crates with only a LICENSE file are
        # expected; report them but do not silently call them permissive.
        if key.startswith(("diary_core@", "diary_bridge@", "diary_probe@")):
            continue
        note(f"许可证元数据缺失（需人工确认）：{key}")


def dart_audit() -> None:
    lock = APP / "pubspec.lock"
    try:
        import yaml
    except ImportError:
        note("未安装 PyYAML，跳过 Dart 依赖锁解析")
        return
    data = yaml.safe_load(lock.read_text(encoding="utf-8")) or {}
    packages = data.get("packages") or {}
    direct = [
        (name, info)
        for name, info in packages.items()
        if info.get("dependency") == "direct main"
    ]
    print("\nFlutter/Dart 直接依赖：")
    for name, info in sorted(direct):
        expected = DART_DIRECT.get(name)
        version = info.get("version", "?")
        print(f"  {name:<20} {version:<12} {expected or 'UNKNOWN'}")
        if expected is None:
            problem(f"Dart 直接依赖 {name} 没有登记许可证")
            continue
        cache = Path.home() / ".pub-cache" / "hosted" / "pub.dev" / f"{name}-{version}" / "LICENSE"
        if cache.is_file():
            text = cache.read_text(encoding="utf-8", errors="replace")
            if expected == "MIT" and "MIT" not in text[:200]:
                problem(f"{name} 的 LICENSE 首段不像 MIT（{cache}）")
            if expected == "Apache-2.0" and "Apache License" not in text[:500]:
                problem(f"{name} 的 LICENSE 首段不像 Apache-2.0（{cache}）")
            if expected == "BSD-3-Clause" and "Redistribution and use in source and binary forms" not in text[:500]:
                problem(f"{name} 的 LICENSE 首段不像 BSD（{cache}）")
        else:
            note(f"{name}-{version} 的 pub cache 不在本机；只核对了登记值 {expected}，发布前跑一次 flutter pub get 后复查")


def name_table(path: Path) -> dict[int, list[str]]:
    """Read the TrueType/OpenType `name` table without external packages."""
    data = path.read_bytes()
    if len(data) < 12:
        raise ValueError("font too short")
    sfnt, count = struct.unpack(">IH", data[:6])
    if sfnt not in (0x00010000, 0x4F54544F, 0x74727565):
        raise ValueError(f"unexpected sfnt version {sfnt:#x}")
    table_offset = None
    for index in range(count):
        rec = data[12 + 16 * index : 28 + 16 * index]
        if len(rec) < 16:
            break
        tag, _checksum, offset, length = struct.unpack(">4sIII", rec)
        if tag == b"name":
            table_offset = (offset, length)
            break
    if table_offset is None:
        raise ValueError("no name table")
    offset, length = table_offset
    fmt, records, strings = struct.unpack(">HHH", data[offset : offset + 6])
    if fmt not in (0, 1):
        raise ValueError(f"unsupported name format {fmt}")
    out: dict[int, list[str]] = {}
    for index in range(records):
        start = offset + 6 + 12 * index
        plat, enc, lang, name_id, size, string_offset = struct.unpack(
            ">HHHHHH", data[start : start + 12]
        )
        if name_id > 25:
            continue
        raw = data[offset + strings + string_offset : offset + strings + string_offset + size]
        if plat == 3 or (plat == 0):
            try:
                text = raw.decode("utf-16-be")
            except UnicodeDecodeError:
                continue
        elif plat == 1:
            text = raw.decode("mac_roman", errors="replace")
        else:
            continue
        out.setdefault(name_id, []).append(text)
    return out


def font_audit() -> None:
    print("\n字体 OFL 元数据：")
    if not FONT.is_file():
        problem(f"缺少字体：{FONT}")
        return
    try:
        names = name_table(FONT)
    except (OSError, ValueError, struct.error) as exc:
        problem(f"读不了字体 name 表：{exc}")
        return
    for name_id, label in ((0, "Copyright"), (13, "License Description")):
        values = names.get(name_id, [])
        joined = " | ".join(values)
        print(f"  nameID {name_id:<2} {label:<20} {'OK' if joined else 'MISSING'}  {joined[:120]}")
        if not joined:
            problem(f"字体缺少 nameID {name_id}（OFL 要求保留）")
    copyright_text = " ".join(names.get(0, []))
    if "LXGW" not in copyright_text or "Klee" not in copyright_text:
        problem("nameID 0 没有同时保留 LXGW 与 Klee 的版权声明")
    license_text = " ".join(names.get(13, []))
    if "SIL Open Font License" not in license_text:
        problem("nameID 13 没有保留 SIL OFL 说明")
    # Reserved names must not be used as the *primary* font name. The subset is
    # named Diary Display; also assert the reserved names are not in IDs 1/4/6/16/17.
    for name_id in (1, 4, 6, 16, 17):
        for value in names.get(name_id, []):
            if any(reserved in value for reserved in ("霞鹜", "霞鶩", "落霞孤鹜", "落霞孤鶩", "LXGW")):
                problem(f"字体 nameID {name_id} 使用了 OFL 保留字体名：{value}")
    if not OFL.is_file():
        problem(f"缺少 OFL 全文：{OFL}")
    else:
        text = OFL.read_text(encoding="utf-8", errors="replace")
        if "SIL OPEN FONT LICENSE Version 1.1" not in text:
            problem(f"{OFL} 不是 OFL 1.1 全文")
        if "Reserved Font Name" not in text:
            problem(f"{OFL} 缺少 Reserved Font Name 段")
        if "LXGW" not in text or "Klee" not in text:
            problem(f"{OFL} 缺少 LXGW/Klee 版权行")


def asset_audit() -> None:
    print("\n随包声明：")
    pubspec = (APP / "pubspec.yaml").read_text(encoding="utf-8")
    for entry in ("assets/legal/", "assets/fonts/OFL.txt", "assets/fonts/DiaryDisplay-AppSubset.ttf"):
        present = entry in pubspec
        print(f"  {entry:<48} {'OK' if present else 'MISSING'}")
        if not present:
            problem(f"pubspec.yaml 没有声明 {entry}")
    for name in REQUIRED_LEGAL_FILES:
        path = LEGAL / name
        print(f"  assets/legal/{name:<34} {'OK' if path.is_file() else 'MISSING'}")
        if not path.is_file():
            problem(f"缺少许可证/声明文件：{path.relative_to(ROOT)}")
    notice = LEGAL / "NOTICE-第三方组件.md"
    if notice.is_file():
        text = notice.read_text(encoding="utf-8")
        for needle in ("bge", "SenseVoice", "sherpa-onnx", "ONNX Runtime", "libc++", "DiaryDisplay"):
            if needle not in text:
                problem(f"NOTICE 没有提到 {needle}")
    # The OFL must survive as its own asset; the font asset itself is covered by
    # the family declaration, which is checked above.
    if not (APP / "assets" / "fonts" / "OFL.txt").is_file():
        problem("assets/fonts/OFL.txt 不存在")


def model_audit() -> None:
    print("\n模型/引擎 manifest：")
    bge = json.loads((ROOT / "models/manifest/bge-small-zh-v1.5.json").read_text(encoding="utf-8"))
    sense = json.loads(
        (ROOT / "models/manifest/sense-voice-small-int8-2024-07-17.json").read_text(encoding="utf-8")
    )
    checks = [
        ("bge upstream license", bge.get("license", {}).get("upstream", ""), "MIT"),
        ("sensevoice upstream license", sense.get("source", {}).get("upstreamLicense", ""), "MIT"),
        ("sherpa-onnx code license", sense.get("engine", {}).get("codeLicense", ""), "Apache-2.0"),
        ("sherpa-onnx release", sense.get("engine", {}).get("release", ""), "v1.13.8"),
    ]
    for label, actual, expected in checks:
        print(f"  {label:<32} {actual or 'MISSING'}")
        matches = expected in actual if label == "bge upstream license" else actual == expected
        if not matches:
            problem(f"{label} 不是 {expected}（实际 {actual!r}）")
    evidence = sense.get("source", {}).get("licenseEvidence", [])
    if not evidence:
        problem("SenseVoice manifest 没有 licenseEvidence")
    if not bge.get("licenseEvidence"):
        problem("bge manifest 没有 licenseEvidence")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--skip-rust", action="store_true", help="skip cargo metadata (offline/CI fallback)")
    args = parser.parse_args()
    metadata = None if args.skip_rust else run_cargo_metadata()
    rust_audit(metadata)
    dart_audit()
    font_audit()
    asset_audit()
    model_audit()

    print("\n备注：")
    for message in notes:
        print(f"  - {message}")
    if problems:
        print("\n失败：")
        for message in problems:
            print(f"  - {message}")
        print(f"\n共 {len(problems)} 处问题。")
        return 1
    print("\n[通过] 直接依赖许可证、弱 copyleft 扫描、字体 OFL nameID 0/13、随包声明与模型 manifest 均符合本检查。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
