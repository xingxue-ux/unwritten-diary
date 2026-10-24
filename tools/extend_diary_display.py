#!/usr/bin/env python3
"""Add HarmonyOS UI literals to the shared Diary Display font.

The source must be LXGWWenKaiGB-Regular.ttf from lxgw/LxgwWenkaiGB tag
v1.522 (SHA-256 295568c131648062107543aa159c97dd49564be791136c2abf74cad83eba3f7f).
Requires fonttools. The source font is intentionally not checked in.
"""
from __future__ import annotations

import argparse
import hashlib
from pathlib import Path
import re

from fontTools.ttLib import TTFont

from check_ui_parity import strip_comments


ROOT = Path(__file__).resolve().parents[1]
BASE = ROOT / 'apps/diary_app/assets/fonts/DiaryDisplay-AppSubset.ttf'
OHOS = ROOT / 'apps/ohos-diary-core/entry/src/main/resources/rawfile/fonts/DiaryDisplay-AppSubset.ttf'
ETS = ROOT / 'apps/ohos-diary-core/entry/src/main/ets'
SOURCE_SHA256 = '295568c131648062107543aa159c97dd49564be791136c2abf74cad83eba3f7f'
LITERAL = re.compile(r'''(['"`])((?:\\.|(?!\1)[^\\])*)\1''')


def ui_codepoints() -> set[int]:
    chars: set[int] = set()
    for path in ETS.rglob('*.ets'):
        source = strip_comments(path.read_text(encoding='utf-8'))
        for match in LITERAL.finditer(source):
            chars.update(map(ord, match.group(2)))
    return chars


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, required=True)
    args = parser.parse_args()
    if hashlib.sha256(args.source.read_bytes()).hexdigest() != SOURCE_SHA256:
        parser.error('source font does not match LXGW WenKai GB v1.522')

    font = TTFont(BASE)
    upstream = TTFont(args.source)
    cmap = font.getBestCmap()
    upstream_cmap = upstream.getBestCmap()
    order = font.getGlyphOrder()
    added = 0
    for codepoint in sorted(ui_codepoints() - cmap.keys()):
        source_name = upstream_cmap.get(codepoint)
        if source_name is None:
            continue
        glyph = upstream['glyf'][source_name]
        if glyph.isComposite():
            raise ValueError(f'composite glyph needs its components: U+{codepoint:04X}')
        name = f'uni{codepoint:04X}' if codepoint <= 0xFFFF else f'u{codepoint:06X}'
        if name in font['glyf'].glyphs:
            raise ValueError(f'glyph name already exists: {name}')
        font['glyf'][name] = glyph
        font['hmtx'].metrics[name] = upstream['hmtx'].metrics[source_name]
        font['vmtx'].metrics[name] = upstream['vmtx'].metrics[source_name]
        for table in font['cmap'].tables:
            if table.isUnicode():
                table.cmap[codepoint] = name
        added += 1

    if not added:
        print('Display font already covers all available UI literals.')
        return
    font.setGlyphOrder(order)
    font['glyf'].glyphOrder = order
    font['maxp'].numGlyphs = len(order)
    font['hhea'].numberOfHMetrics = len(order)
    font['vhea'].numberOfVMetrics = len(order)
    font.save(BASE)
    OHOS.write_bytes(BASE.read_bytes())
    print(f'Added {added} glyphs; shared font now has {len(order)} glyphs.')


if __name__ == '__main__':
    main()
