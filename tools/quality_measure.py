#!/usr/bin/env python3
"""核对 AI 质量集里每条用例的「可衡量量」：可见字数、材料内数字、ngram 有依据比例、最长无依据片段。

它不是判定器（判定器在 Rust 里）——它只用来**给阈值定一个有安全边际的值**：
阈值必须与合格参考输出的实测值拉开距离，否则参考输出自己就在阈值边缘上，
以后改一个字就红。判定口径与 Rust 保持一致（同一套规范化与 n-gram 规则）。
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
QUALITY = ROOT / "tests" / "quality"

# 与 Rust 的 text.rs 同一份词表。
CN_NUMERALS = set("零一二三四五六七八九十百千万亿两")
VAGUE_WORDS = [
    "一下", "两下", "一点", "一会", "一阵", "一半", "半天",
    "一直", "一起", "一定", "一边", "一番", "一句", "一声",
]
# 只收**确数单位**；不收 点/天/年/月（那些大量是虚指或程度副词）。
MEASURE_WORDS = set("个只次块斤条杯张件本岁间门句场")


def is_punct_or_symbol(ch: str) -> bool:
    """与 Rust 的 is_punct_or_symbol 同一口径（逐段核对过的区间）。"""
    code = ord(ch)
    return (
        # CJK 符号与标点（、。「」『』〜…）
        0x3000 <= code <= 0x303F
        # 全角 ！＂＃…／（从 FF01 起：FF10 是数字０）
        or 0xFF01 <= code <= 0xFF0F
        or 0xFF1A <= code <= 0xFF20
        or 0xFF3B <= code <= 0xFF40
        or 0xFF5B <= code <= 0xFF65
        # 连字符与各类破折号
        or 0x2010 <= code <= 0x2027
        # 千分号、引号、省略号、间隔号
        or 0x2030 <= code <= 0x205E
        or code == 0x00B7
        or 0xFE30 <= code <= 0xFE4F
        or (ch.isascii() and not ch.isalnum() and not ch.isspace())
        or ch.isspace()
    )


def is_punct_at(chars: list[str], index: int) -> bool:
    """带上下文的判断：`.`/`-` 夹在数字之间时是小数点或日期分隔，不是标点。"""
    ch = chars[index]
    if ch in ".-":
        before = chars[index - 1] if index > 0 else ""
        after = chars[index + 1] if index + 1 < len(chars) else ""
        if before.isascii() and before.isdigit() and after.isascii() and after.isdigit():
            return False
    return is_punct_or_symbol(ch)


def fold(ch: str) -> str:
    code = ord(ch)
    if 0xFF10 <= code <= 0xFF19 or 0xFF21 <= code <= 0xFF3A or 0xFF41 <= code <= 0xFF5A:
        return chr(code - 0xFEE0)
    return ch


def normalize(text: str) -> str:
    """与 Rust 的 normalize_cjk / punct_normalize 同一口径（去空白去标点、折全角）。"""
    chars = list(text)
    return "".join(
        fold(ch) for index, ch in enumerate(chars) if not is_punct_at(chars, index)
    )


def is_numeral(ch: str) -> bool:
    return (ch.isascii() and ch.isdigit()) or ch in CN_NUMERALS


def should_extract(run: str, nxt: str) -> bool:
    """与 Rust 的 text::should_extract 同一口径。"""
    if all(ch.isascii() and ch.isdigit() for ch in run):
        return True
    if len(run) >= 2:
        return True
    return nxt in MEASURE_WORDS


def numbers_in(text: str) -> list[str]:
    """与 Rust 的 text::numbers_in 同一口径：虚指区间 + 最大连续数字串 + 确数单位。"""
    chars = list(normalize(text))
    vague: list[tuple[int, int]] = []
    for word in VAGUE_WORDS:
        size = len(word)
        for i in range(max(0, len(chars) - size + 1)):
            if "".join(chars[i : i + size]) == word:
                vague.append((i, i + size))

    def blocked(start: int, end: int) -> bool:
        return any(start >= low and end <= high for low, high in vague)

    found: list[str] = []
    i = 0
    while i < len(chars):
        if not is_numeral(chars[i]):
            i += 1
            continue
        start = i
        ascii_run = chars[i].isascii() and chars[i].isdigit()
        while i < len(chars):
            if is_numeral(chars[i]):
                i += 1
            elif (
                ascii_run
                and chars[i] in ".-"
                and i + 1 < len(chars)
                and chars[i + 1].isascii()
                and chars[i + 1].isdigit()
            ):
                i += 1
            else:
                break
        if blocked(start, i):
            continue
        run = "".join(chars[start:i])
        nxt = chars[i] if i < len(chars) else "\0"
        if should_extract(run, nxt):
            found.append(run)
    return found


def is_cjk(ch: str) -> bool:
    code = ord(ch)
    return 0x4E00 <= code <= 0x9FFF or 0x3400 <= code <= 0x4DBF or 0xF900 <= code <= 0xFAFF


def cjk_runs(text: str) -> list[str]:
    runs, current = [], ""
    for ch in normalize(text):
        if is_cjk(ch):
            current += ch
        elif current:
            runs.append(current)
            current = ""
    if current:
        runs.append(current)
    return runs


def ngram_backing(text: str, reference: str, size: int) -> tuple[int, int]:
    hay = normalize(text)
    needles = {
        normalize(reference)[i : i + size]
        for i in range(max(0, len(normalize(reference)) - size + 1))
    }
    if len(hay) < size:
        return len(hay), 0
    total = backed = 0
    for i in range(len(hay) - size + 1):
        total += 1
        if hay[i : i + size] in needles:
            backed += 1
    return total, backed


def novel_runs(text: str, reference: str, size: int, min_len: int) -> list[str]:
    """与 Rust 的 text::novel_runs 同一口径（含「锚定过滤」）。"""
    norm_ref = normalize(reference)
    needles = {norm_ref[i : i + size] for i in range(max(0, len(norm_ref) - size + 1))}

    def backed_at(chars: list[str], start: int, end: int) -> bool:
        cursor = start
        while cursor + size <= end:
            if "".join(chars[cursor : cursor + size]) in needles:
                return True
            cursor += 1
        return False

    runs = []
    for run in cjk_runs(text):
        chars = list(run)
        if len(chars) < min_len:
            continue
        start = 0
        while start < len(chars):
            end = start
            while end < len(chars) and not backed_at(chars, start, end + 1):
                end += 1
            if end - start >= min_len and not backed_at(chars, start, end):
                runs.append("".join(chars[start:end]))
            start = end if end > start else start + 1
    return runs


def materials_text(materials: list[dict]) -> str:
    return "\n".join(f"{m['name']}\n{m['text']}" for m in materials)


def report_generation(set_data: dict) -> None:
    print("=" * 100)
    print("生成质量集")
    for case in set_data["cases"]:
        reference = materials_text(case["material"])
        out = case["replayOutput"]
        rules = case["expect"]["invention"]
        size = rules["ngramSize"]
        total, backed = ngram_backing(out, reference, size)
        ratio = backed / total if total else 1.0
        runs = novel_runs(out, reference, size, rules["novelRunChars"])
        longest = max((len(r) for r in runs), default=0)
        mat_numbers = numbers_in(reference)
        kept = [n for n in mat_numbers if n in normalize(out)]
        template = case["template"]["length"]
        print(f"\n{case['id']}  模板={case['template']['id']} {case['template']['person']}/"
              f"{case['template']['tense']}/{case['template']['style']}")
        print(f"  可见字数 = {len(normalize(out))}   声明区间 = [{template['min']}, {template['max']}]")
        print(f"  材料数字 = {mat_numbers}   输出保留 = {kept}  ({len(kept)}/{len(mat_numbers)})")
        print(f"  {size}-gram 有依据 = {backed}/{total} = {ratio:.3f}   "
              f"声明 minBackedRatio = {rules.get('minBackedRatio', 0.0)}")
        print(f"  最长无依据片段 = {longest} 字（阈值 {rules['novelRunChars']}）  {runs}")
        # 保留率阈值建议
        if mat_numbers:
            print(f"  → 建议 minFactRetention ≤ {len(kept) / len(mat_numbers):.2f}")
        print(f"  → 建议长度区间 [{max(1, len(normalize(out)) - 25)}, {len(normalize(out)) + 30}]")
        print(f"  → 建议 minBackedRatio ≤ {max(0.0, ratio - 0.10):.2f}")


def report_transcript(set_data: dict) -> None:
    print("=" * 100)
    print("转写质量集（CER 手算核对）")
    for case in set_data["cases"]:
        # 与 Rust 的 cer() 同一口径：normalize（去空白、去标点、折全角）
        ref = normalize(case["reference"])
        hyp = normalize(case["replayHypothesis"])
        # 朴素 Levenshtein
        rows, cols = len(ref) + 1, len(hyp) + 1
        table = [[0] * cols for _ in range(rows)]
        for i in range(rows):
            table[i][0] = i
        for j in range(cols):
            table[0][j] = j
        for i in range(1, rows):
            for j in range(1, cols):
                table[i][j] = min(
                    table[i - 1][j - 1] + (ref[i - 1] != hyp[j - 1]),
                    table[i - 1][j] + 1,
                    table[i][j - 1] + 1,
                )
        errors = table[rows - 1][cols - 1]
        cer = errors / len(ref) if ref else None
        print(f"\n{case['id']}  参考 {len(ref)} 字 · 假设 {len(hyp)} 字 · 编辑距离 {errors} "
              f"· CER = {cer:.4f}" if cer is not None else f"\n{case['id']} 参考为空")
        print(f"  参考: {ref}")
        print(f"  假设: {hyp}")
        print(f"  → 建议 maxCer ≥ {cer + 0.02:.2f}（实测 {cer:.4f}，声明 {case['expect']['maxCer']}）")


def report_injection(set_data: dict) -> None:
    print("=" * 100)
    print("防提示注入集")
    for case in set_data["cases"]:
        for label, key in (("Clean", "clean"), ("Poisoned", "poisoned")):
            out = case[f"replay{label}Output"]
            reference = materials_text(case[key])
            rules = case["expect"]["invention"]
            runs = novel_runs(out, reference, rules["ngramSize"], rules["novelRunChars"])
            total, backed = ngram_backing(out, reference, rules["ngramSize"])
            marker_hits = [m for m in case["markers"] if m in out]
            print(f"\n{case['id']}（{label}）可见字数 = {len(normalize(out))} · "
                  f"最长无依据片段 = {max((len(r) for r in runs), default=0)} · "
                  f"ngram 有依据 = {(backed / total if total else 1.0):.3f} · "
                  f"markers 命中 = {marker_hits}")
            print(f"  声明区间 = [{case['template']['length']['min']}, "
                  f"{case['template']['length']['max']}]  → "
                  f"建议 [{max(1, len(normalize(out)) - 15)}, {len(normalize(out)) + 20}]")
        clean = case["replayCleanOutput"]
        poison = case["replayPoisonedOutput"]
        print(f"  两侧输出相同？ {clean == poison}"
              f"（相同最稳；不同也不是错，但要逐条硬规则一致）")


def main() -> int:
    for name, fn in (
        ("ai-quality-set.json", report_generation),
        ("ai-transcript-set.json", report_transcript),
        ("ai-injection-set.json", report_injection),
    ):
        path = QUALITY / name
        if not path.is_file():
            print(f"[跳过] 还没有 {name}")
            continue
        fn(json.loads(path.read_text(encoding="utf-8")))
    return 0


if __name__ == "__main__":
    sys.exit(main())