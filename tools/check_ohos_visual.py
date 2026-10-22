#!/usr/bin/env python3
"""跨端视觉资源与控件基线检查；不能替代 ArkTS 编译和屏幕验收。"""
from __future__ import annotations

import argparse
from pathlib import Path
import re

from check_ui_parity import strip_comments

ETS = Path('apps/ohos-diary-core/entry/src/main/ets')
FLUTTER = Path('apps/diary_app/lib/src')
RES = ETS.parent / 'resources'


def check(root: Path) -> list[str]:
    failures: list[str] = []

    def read(path: Path) -> str:
        full = root / path
        if not full.is_file():
            failures.append(f'missing: {path.as_posix()}')
            return ''
        return strip_comments(full.read_text(encoding='utf-8-sig'))

    flutter = read(FLUTTER / 'sketch_ui.dart')
    arkts = read(ETS / 'ui/Sketch.ets')
    palette = dict(re.findall(r'static const (\w+) = Color\(0xFF([0-9A-Fa-f]{6})\)', flutter))
    actual = dict(re.findall(r'static readonly (\w+): string = \'#([0-9A-Fa-f]{6})\'', arkts))
    if not palette:
        failures.append('palette: Windows palette could not be parsed')
    for key, value in palette.items():
        if actual.get(key, '').upper() != value.upper():
            failures.append(f'palette.{key}: Windows #{value}, OHOS #{actual.get(key, "missing")}')
    if 'extends DrawModifier' not in arkts or 'context.sizeInPixel' not in arkts:
        failures.append('outline: hand drawn frames must use host drawing bounds, not percentage overlays')
    button_style = arkts.split('export class SketchButtonStyle', 1)[-1].split('export const sketchButton', 1)[0]
    if '.drawModifier(' in button_style:
        failures.append('outline: API 23 requires drawModifier on the component, outside AttributeModifier')

    for name in ('DiaryDisplay-AppSubset.ttf', 'OFL.txt'):
        baseline = root / 'apps/diary_app/assets/fonts' / name
        target = root / RES / 'rawfile/fonts' / name
        if not baseline.is_file() or not target.is_file() or baseline.read_bytes() != target.read_bytes():
            failures.append(f'font: {name} must match Windows, including its license')

    shell = read(ETS / 'pages/Index.ets')
    windows_shell = read(FLUTTER / 'experience_shell.dart')
    windows_breakpoint = re.search(r'bounds.maxWidth\s*>=\s*(\d+)', windows_shell)
    ohos_breakpoint = re.search(r'Number\(area.width\)\s*>=\s*(\d+)', shell)
    if windows_breakpoint is None or ohos_breakpoint is None or windows_breakpoint[1] != ohos_breakpoint[1]:
        failures.append('layout: desktop breakpoints must match')
    if "familySrc: $rawfile('fonts/DiaryDisplay-AppSubset.ttf')" not in shell:
        failures.append('font: bundled display font is not registered')
    if 'PlaceholderPage(' in shell:
        failures.append('routes: temporary placeholder is still in the product shell')
    for route, component in (('styles', 'StylesPage'), ('export', 'ExportPage')):
        if not re.search(rf"name === '{route}'[\s\S]*?{component}\(", shell):
            failures.append(f'routes: {route} must open {component}')
        read(ETS / f'pages/{component}.ets')

    pages = sorted((root / ETS / 'pages').glob('*.ets'))
    for page in [*pages, root / ETS / 'ui/Common.ets']:
        if not page.is_file():
            continue
        source = strip_comments(page.read_text(encoding='utf-8-sig'))
        if re.search(r"\$r\(['\"]sys\.color\.", source):
            failures.append(f'theme: system palette leaked into {page.name}')
        if re.search(r'\.borderRadius\([1-9]\d*\)', source):
            failures.append(f'theme: rounded system control in {page.name}')
        if re.search(r'\.(?:fontColor|color)\(SketchColors.button\)', source):
            failures.append(f'contrast: background tint used as foreground in {page.name}')
        if '临时功能界面' in source or '占位页：' in source:
            failures.append(f'copy: temporary implementation copy in {page.name}')
        # 顶栏铃铛是透明图标按钮；其余文字按钮必须使用同一修改器。
        if page.name != 'Index.ets':
            buttons = len(re.findall(r'\bButton\(', source))
            styled = len(re.findall(r'\.attributeModifier\(sketchButton\)', source))
            if buttons != styled:
                failures.append(f'buttons: {page.name} has {buttons} buttons but {styled} shared styles')
    for icon in ('bell', 'record', 'diary', 'search', 'snippets', 'profile'):
        read(RES / f'base/media/sketch_{icon}.svg')
    return failures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=Path(__file__).resolve().parents[1])
    args = parser.parse_args()
    failures = check(args.root)
    for failure in failures:
        print(f'[FAIL] {failure}')
    if not failures:
        print('[PASS] Windows/OHOS palette, font/license, responsive breakpoint, routes and control styles match.')
        print('Source checks only; HAP compilation and device visual checks are separate requirements.')
    return 1 if failures else 0


if __name__ == '__main__':
    raise SystemExit(main())
