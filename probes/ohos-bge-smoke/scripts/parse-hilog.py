#!/usr/bin/env python3
"""把设备 hilog 里的 BGE 探针输出拼回完整 JSON。

设备侧 ArkTS 把 shim 返回的 JSON 按 900 字符分片打成：

    ... BGE_MARKER BGE_JSON[<i>/<n>] <chunk>

本脚本按顺序拼回，再解开 shim 的包装层（`{dlopen_ok, ..., core: "<核心 JSON 原文>"}`），
把核心 JSON 落成 `evidence/device-bge.json`（排好序、缩进），并打印一份摘要。

用法：
    python3 scripts/parse-hilog.py <hilog.txt> [--out evidence/device-bge.json] [--raw out.json]
"""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import sys

CHUNK_RE = re.compile(r"BGE_JSON\[(\d+)/(\d+)\]\s?(.*)$")
MARKER_RE = re.compile(r"BGE_MARKER\s+(.*)$")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("hilog")
    ap.add_argument("--out", default="evidence/device-bge.json")
    ap.add_argument("--raw", default="evidence/device-wrapper.json")
    args = ap.parse_args()

    lines = pathlib.Path(args.hilog).read_text(encoding="utf-8", errors="replace").splitlines()

    chunks: dict[int, str] = {}
    total = None
    markers: list[str] = []
    for line in lines:
        m = CHUNK_RE.search(line)
        if m:
            idx, tot, text = int(m.group(1)), int(m.group(2)), m.group(3)
            chunks[idx] = text
            total = tot
            continue
        m = MARKER_RE.search(line)
        if m:
            markers.append(m.group(1))

    print(f"[parse] hilog 行数={len(lines)} 分片={len(chunks)}/{total} 标记行={len(markers)}")
    for m in markers:
        if "BGE_JSON" not in m:
            print(f"[marker] {m}")

    if not chunks or total is None:
        print("[parse] !! 没找到 BGE_JSON 分片：应用可能没起来 / 安装失败 / 日志被冲掉", file=sys.stderr)
        return 2

    missing = [i for i in range(1, total + 1) if i not in chunks]
    if missing:
        print(f"[parse] !! 缺分片 {missing}（hilog 缓冲可能被冲掉，重跑 run-on-device.sh）", file=sys.stderr)
        return 3

    wrapper_text = "".join(chunks[i] for i in range(1, total + 1))
    wrapper = json.loads(wrapper_text)

    out_raw = pathlib.Path(args.raw)
    out_raw.parent.mkdir(parents=True, exist_ok=True)
    out_raw.write_text(json.dumps(wrapper, ensure_ascii=False, indent=2), encoding="utf-8")

    core_text = wrapper.get("core")
    if not isinstance(core_text, str):
        print(f"[parse] !! 包装层里没有 core（dlopen/dlsym 失败？）：{json.dumps(wrapper, ensure_ascii=False)[:800]}",
              file=sys.stderr)
        return 4

    core = json.loads(core_text)
    out = pathlib.Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(core, ensure_ascii=False, indent=2, sort_keys=True), encoding="utf-8")

    print(f"[parse] dlopen_ok={wrapper.get('dlopen_ok')} used={wrapper.get('dlopen_used')} "
          f"dlsym_ok={wrapper.get('dlsym_ok')} abi={wrapper.get('abi')}")
    print(f"[parse] core_loaded_at={wrapper.get('core_loaded_at')}")
    print(f"[parse] stage={core.get('stage')} load_ok={core.get('load_ok')} dims={core.get('dims')} "
          f"model_version={core.get('model_version')}")
    print(f"[parse] load_ms={core.get('load_ms')} first_embed_ms={core.get('first_embed_ms')} "
          f"total_ms={core.get('total_ms')}")
    print(f"[parse] corpus_sha256={core.get('corpus_sha256')}")
    print(f"[parse] errors={core.get('errors')}")
    print(f"[parse] → {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())