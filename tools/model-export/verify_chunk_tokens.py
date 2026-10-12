#!/usr/bin/env python3
"""用真实分词器核对分块结果的 token 数（issue #50 / B3c-1 的证据之一）。

核心的分块器按**字符**切（`crates/diary_core/src/chunker.rs`），不为了切块把分词器
拉进核心。而任务书 5.4 节的约束是「每块不超过约 400 token」——这条约束必须用真实
分词器验，不能只写在注释里。

用法：

```bash
# 1) 让探针把块正文 dump 出来（跑质量集时顺带做）
DIARY_DUMP_CHUNKS=/tmp/chunks.json cargo run --release -p diary_probe -- quality-set
# 2) 用真实分词器核对
python3 tools/model-export/verify_chunk_tokens.py /tmp/chunks.json
```

脚本打印每块的 token 数分布（max / p99 / 超限块数），并在**有块超过上限**时以
非零退出码结束——那说明 `MAX_CHUNK_CHARS` 需要往下调，同时必须改 `CHUNKER_VERSION`
（否则库里已有的块不会被重算）。
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

# 与 manifest 记录的上游 revision 一致；分词器文件随我们的导出产物一起分发。
UPSTREAM_REPO = "BAAI/bge-small-zh-v1.5"
UPSTREAM_REVISION = "7999e1d3359715c523056ef9478215996d62a620"

# 任务书 5.4 节的约束。带「约」字，所以这里给一个明确的判定上限：
LIMIT_TOKENS = 400
# 允许的最大超出比例（0 表示严格不超过）。BERT 会在序列两端各加 [CLS]/[SEP]，
# 所以按「块本身 ≤ 400」判定，不把这两个特殊 token 算进块长。
TOLERANCE = 0.0


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__)
        return 2
    chunks = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
    if not chunks:
        print("没有任何块，没什么可核的")
        return 0

    from transformers import AutoTokenizer

    tokenizer = AutoTokenizer.from_pretrained(UPSTREAM_REPO, revision=UPSTREAM_REVISION)
    counts = []
    worst = None
    for index, text in enumerate(chunks):
        # add_special_tokens=False：量的是块本身，不含 [CLS]/[SEP]。
        tokens = tokenizer(text, add_special_tokens=False)["input_ids"]
        counts.append(len(tokens))
        if worst is None or len(tokens) > worst[1]:
            worst = (index, len(tokens), text[:40])

    counts.sort()
    total = sum(counts)
    maximum = counts[-1]
    p99 = counts[min(int(len(counts) * 0.99), len(counts) - 1)]
    limit = int(LIMIT_TOKENS * (1 + TOLERANCE))
    over = sum(1 for count in counts if count > limit)

    print(f"块数：{len(counts)}")
    print(f"token 数：最长 {maximum} · p99 {p99} · 平均 {total / len(counts):.1f}")
    print(f"最长的那块（第 {worst[0]} 块，{worst[1]} token）：{worst[2]!r}…")
    print(f"超过 {limit} token 的块：{over} 个")

    if over:
        print(
            "\n结论：**有块超过上限**。要么调小 MAX_CHUNK_CHARS，要么换更省的切分规则；"
            "两者都必须同时改 CHUNKER_VERSION（否则库里已有的块不会被重算）。"
        )
        return 1
    print(f"\n结论：全部块都在 {limit} token 以内（chunker 的字符上界成立）。")
    return 0


if __name__ == "__main__":
    sys.exit(main())