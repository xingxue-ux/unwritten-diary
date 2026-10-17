#!/usr/bin/env python3
"""设备 vs 宿主：真 bge 嵌入结果的**逐项对比**（本任务的硬要求）。

输入：
  - 设备侧：parse-hilog.py 落下的 `evidence/device-bge.json`（核心 run() 的 JSON）
  - 宿主侧：`cargo test --release -- --nocapture host_control` 输出里的 `HOST_CONTROL <JSON>`
            （同代码、同语料常量、同 int8 模型文件；ORT 1.31.0 x86-64 对设备 ORT 1.29.1）

输出：
  1. 每个文本的 **余弦相似度** 与 **最大逐元素偏差**（int8 量化下不期望逐位相同，
     所以这里报的是**差异量级**，不是等值断言）；
  2. 语义判据（相近 > 无关）在两边是否都成立；
  3. 5 个查询的候选排序在两边是否完全一致（这关系到「检索结果会不会变」）；
  4. 耗时 / 内存对照表。

用法：
    python3 scripts/compare-host-device.py evidence/device-bge.json evidence/host-control.txt \
        [--out evidence/compare.json] [--device2 evidence/device-bge-warm.json]
"""

from __future__ import annotations

import argparse
import json
import math
import pathlib
import re
import struct
import sys


def decode_hex_vec(s: str) -> list[float]:
    raw = bytes.fromhex(s)
    return list(struct.unpack("<%df" % (len(raw) // 4), raw))


def cosine(a: list[float], b: list[float]) -> float:
    dot = sum(x * y for x, y in zip(a, b))
    na = math.sqrt(sum(x * x for x in a))
    nb = math.sqrt(sum(x * x for x in b))
    if na == 0 or nb == 0:
        return float("nan")
    return dot / (na * nb)


def load_host(path: pathlib.Path) -> dict:
    text = path.read_text(encoding="utf-8", errors="replace")
    hits = [m.group(1) for m in re.finditer(r"HOST_CONTROL (\{.*\})", text)]
    if not hits:
        raise SystemExit(f"{path} 里没有 HOST_CONTROL 行；确认 cargo test 输出被完整重定向了")
    return json.loads(hits[-1])


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("device")
    ap.add_argument("host")
    ap.add_argument("--out", default="evidence/compare.json")
    ap.add_argument("--device2", default=None, help="第二次（热）启动的设备 JSON，可选")
    args = ap.parse_args()

    dev = json.loads(pathlib.Path(args.device).read_text(encoding="utf-8"))
    host = load_host(pathlib.Path(args.host))

    report: dict = {"device": {}, "host": {}, "vectors": {}, "ranking": {}, "semantic": {}}

    # ── 0. 前置断言：两边跑的是同一份语料、同一个模型、同一条口径 ─────────────
    checks = {
        "corpus_sha256": (dev.get("corpus_sha256"), host.get("corpus_sha256")),
        "model_sha256": (dev.get("model_sha256"), host.get("model_sha256")),
        "model_bytes": (dev.get("model_bytes"), host.get("model_bytes")),
        "dims": (dev.get("dims"), host.get("dims")),
        "pooling": (dev.get("pooling"), host.get("pooling")),
        "query_instruction": (dev.get("query_instruction"), host.get("query_instruction")),
        "model_version": (dev.get("model_version"), host.get("model_version")),
    }
    print("== 前置一致性（设备 | 宿主）")
    same_all = True
    for k, (a, b) in checks.items():
        ok = a == b
        same_all &= ok
        print(f"  {'OK ' if ok else '!! '} {k}: {a} | {b}")

    # ── 1. 逐向量：余弦 + 最大逐元素偏差 ────────────────────────────────────
    dev_vecs = {e["key"]: decode_hex_vec(e["vec_hex"]) for e in dev.get("embeds", [])}
    host_vecs = {e["key"]: decode_hex_vec(e["vec_hex"]) for e in host.get("embeds", [])}
    dev_sha = {e["key"]: e["vec_sha256"] for e in dev.get("embeds", [])}
    host_sha = {e["key"]: e["vec_sha256"] for e in host.get("embeds", [])}

    print("\n== 逐文本：余弦相似度 / 最大逐元素偏差 / 逐位一致")
    print(f"  {'key':<8} {'cos':>12} {'max|d|':>12} {'mean|d|':>12} {'bit==':>6}  {'dims':>4}")
    worst_cos = 1.0
    worst_max = 0.0
    bit_identical_keys: list[str] = []
    for key in sorted(set(dev_vecs) & set(host_vecs)):
        a, b = dev_vecs[key], host_vecs[key]
        if len(a) != len(b):
            print(f"  !! {key}: 维度不同 {len(a)} vs {len(b)}")
            same_all = False
            continue
        cos = cosine(a, b)
        diffs = [abs(x - y) for x, y in zip(a, b)]
        mx = max(diffs)
        mean = sum(diffs) / len(diffs)
        bit = dev_sha[key] == host_sha[key]
        if bit:
            bit_identical_keys.append(key)
        worst_cos = min(worst_cos, cos)
        worst_max = max(worst_max, mx)
        report["vectors"][key] = {
            "cosine": cos, "max_abs_diff": mx, "mean_abs_diff": mean,
            "bit_identical": bit, "dims": len(a),
            "device_sha256": dev_sha[key], "host_sha256": host_sha[key],
        }
        print(f"  {key:<8} {cos:>12.9f} {mx:>12.3e} {mean:>12.3e} {str(bit):>6}  {len(a):>4}")

    report["overall"] = {
        "min_cosine": worst_cos,
        "max_abs_diff": worst_max,
        "bit_identical_keys": bit_identical_keys,
        "all_bit_identical": len(bit_identical_keys) == len(dev_vecs) and len(dev_vecs) > 0,
    }
    print(f"\n  最差余弦={worst_cos:.9f}  最大逐元素偏差={worst_max:.3e}  "
          f"逐位一致的文本={len(bit_identical_keys)}/{len(dev_vecs)}")

    # ── 2. 语义判据 ────────────────────────────────────────────────────────
    print("\n== 语义判据（相近 vs 无关）")
    for k in ("near_a__near_b", "near_a__far", "main__near_a", "main__far",
              "near_gt_far", "main_near_gt_main_far"):
        dv, hv = dev.get("semantic", {}).get(k), host.get("semantic", {}).get(k)
        if isinstance(dv, float) and isinstance(hv, float):
            print(f"  {k:<24} 设备={dv:.6f}  宿主={hv:.6f}  d={abs(dv - hv):.2e}")
        else:
            ok = dv == hv
            same_all &= ok
            print(f"  {'OK ' if ok else '!! '} {k:<24} 设备={dv}  宿主={hv}")
        report["semantic"][k] = {"device": dv, "host": hv}

    # ── 3. 排序一致性 ──────────────────────────────────────────────────────
    print("\n== 排序一致性（5 查询 × 5 候选）")
    dev_order = {o["query"]: o["order"] for o in dev.get("ranking", {}).get("order", [])}
    host_order = {o["query"]: o["order"] for o in host.get("ranking", {}).get("order", [])}
    all_same = True
    for q in sorted(set(dev_order) & set(host_order)):
        same = dev_order[q] == host_order[q]
        all_same &= same
        print(f"  {'OK ' if same else '!! '} {q}: 设备={' > '.join(dev_order[q])}")
        if not same:
            print(f"      宿主={' > '.join(host_order[q])}")
        report["ranking"][q] = {"device": dev_order[q], "host": host_order[q], "same": same}
    report["ranking_all_same"] = all_same
    print(f"\n  全部查询排序一致: {all_same}")

    dev_cos = dev.get("ranking", {}).get("cos", {})
    host_cos = host.get("ranking", {}).get("cos", {})
    mx = 0.0
    for q, row in dev_cos.items():
        for d, v in row.items():
            mx = max(mx, abs(v - host_cos.get(q, {}).get(d, float("nan"))))
    report["ranking_cos_max_abs_diff"] = mx
    print(f"  相似度矩阵最大绝对差: {mx:.3e}")

    # ── 4. 耗时 / 内存 ─────────────────────────────────────────────────────
    print("\n== 耗时 / 内存（设备 | 宿主）")
    perf_keys = ["load_ms", "first_embed_ms", "model_sha256_cold_ms", "model_read_only_ms",
                 "tokenizer_load_ms", "session_create_only_ms", "total_ms",
                 "proc_rss_kb_before_load", "proc_rss_kb_after_load", "proc_rss_kb_after_embed",
                 "proc_rss_kb_end", "proc_hwm_kb_before_load", "proc_hwm_kb_after_load",
                 "proc_hwm_kb_after_embed", "proc_hwm_kb_end", "cpu_count"]
    for k in perf_keys:
        print(f"  {k:<28} 设备={dev.get(k)}  宿主={host.get(k)}")
    report["device"] = {k: dev.get(k) for k in perf_keys}
    report["host"] = {k: host.get(k) for k in perf_keys}

    if args.device2:
        warm = json.loads(pathlib.Path(args.device2).read_text(encoding="utf-8"))
        print("\n== 第二次启动（模型/页面缓存已热）")
        for k in ["load_ms", "first_embed_ms", "model_sha256_cold_ms", "model_read_only_ms",
                  "tokenizer_load_ms", "session_create_only_ms", "total_ms"]:
            print(f"  {k:<28} 第一次={dev.get(k)}  第二次={warm.get(k)}")
        report["device_warm"] = {k: warm.get(k) for k in perf_keys}

    print("\n== 逐文本耗时（冷 / 热中位数，ms）")
    dev_e = {e["key"]: e for e in dev.get("embeds", [])}
    host_e = {e["key"]: e for e in host.get("embeds", [])}
    print(f"  {'key':<8} {'tokens':>7} {'dev冷':>9} {'dev热':>9} {'host冷':>9} {'host热':>9} {'倍数':>6}")
    tok = dev.get("token_counts", {})
    for key in sorted(dev_e):
        d, h = dev_e[key], host_e.get(key, {})
        ratio = (d.get("hot_ms_median") or 0) / (h.get("hot_ms_median") or float("nan"))
        print(f"  {key:<8} {tok.get(key, '?'):>7} {d.get('cold_ms'):>9} {d.get('hot_ms_median'):>9} "
              f"{h.get('cold_ms'):>9} {h.get('hot_ms_median'):>9} {ratio:>6.2f}")
    report["per_text"] = {
        k: {
            "device_cold_ms": dev_e[k].get("cold_ms"),
            "device_hot_ms": dev_e[k].get("hot_ms_median"),
            "host_cold_ms": host_e.get(k, {}).get("cold_ms"),
            "host_hot_ms": host_e.get(k, {}).get("hot_ms_median"),
            "tokens": tok.get(k),
            "chars": dev_e[k].get("chars"),
            "device_bit_identical_repeats": dev_e[k].get("repeat_bit_identical"),
        }
        for k in sorted(dev_e)
    }

    print("\n== 越界文本（>512 token）")
    print(f"  设备: {json.dumps(dev.get('overlength'), ensure_ascii=False)[:400]}")
    print(f"  宿主: {json.dumps(host.get('overlength'), ensure_ascii=False)[:400]}")
    report["overlength"] = {"device": dev.get("overlength"), "host": host.get("overlength")}

    report["preconditions_all_same"] = bool(same_all)
    out = pathlib.Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, ensure_ascii=False, indent=2, sort_keys=True), encoding="utf-8")
    print(f"\n[compare] → {out}")
    print(f"[compare] 前置一致性={same_all} 排序一致={all_same}")
    return 0 if (same_all and all_same) else 1


if __name__ == "__main__":
    raise SystemExit(main())