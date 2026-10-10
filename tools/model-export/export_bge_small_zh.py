#!/usr/bin/env python3
"""从 MIT 权重的原始仓库自己导出 ONNX（issue #18）。

## 为什么不用现成的转换产物

`Xenova/bge-small-zh-v1.5` 提供了 ONNX 文件，但那个仓库**没有声明 license 字段、
仓库里也没有 LICENSE 文件**（2026-10 用 HuggingFace API 核对过，证据记在
`models/manifest/bge-small-zh-v1.5.json` 的 `licenseEvidence`）。上游
`BAAI/bge-small-zh-v1.5` 声明 MIT，但上游**不发 ONNX**（只有 safetensors/pytorch）。
所以再分发那份转换产物之前需要拿到作者的明确许可；这条路径我们不赌。

自己从 MIT 权重导出：权重授权清楚（MIT，保留版权与许可声明即可），工具链是
Apache-2.0/BSD 的（transformers / optimum / onnx / onnxruntime / torch），
导出产物是我们自己的文件，产物哈希由本脚本打印。

## 用法

```bash
python3 tools/model-export/export_bge_small_zh.py --out models/bge-small-zh-v1.5-ours
```

先决条件：`torch`（CPU 版够用）、`transformers`、`onnx`、`onnxruntime`（`onnxscript` 是
新版 torch 导出器的依赖）。**实际用到的版本会写进 `export-info.json`**，不在这里硬编码
——硬编码迟早会和实际不一致。

脚本做四件事，每一步都往 stdout 打印可核对的事实：

1. 按**固定的 upstream revision** 取权重（不用 `main`，避免上游悄悄换文件）；
2. 导出 fp32 ONNX（输入名必须是 `input_ids` / `attention_mask` / `token_type_ids`，
   与探针 `tools/probe/src/model.rs` 的 `ort::inputs!` 一致）；
3. 动态量化成 INT8，得到 `model_quantized.onnx`；
4. 用 onnxruntime 跑一遍 M0 的 3 条质量样例，打印余弦分数，并打印两个文件的
   sha256 与字节数 —— 把这几行填进 manifest 就完成了「自己导出的脚本与哈希」。

**注意**：这个脚本**不修改**任何 manifest 或文档，它只打印结果。哈希与结论由人
核对后写进 `models/manifest/bge-small-zh-v1.5.json`（避免脚本悄悄改事实记录）。
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
from pathlib import Path

# 上游权重仓库与固定 revision（与 manifest 里的 upstreamRevision 一致）。
UPSTREAM_REPO = "BAAI/bge-small-zh-v1.5"
UPSTREAM_REVISION = "7999e1d3359715c523056ef9478215996d62a620"

# 质量样例：**与探针 `tools/probe/src/model.rs` 的 QUALITY_CASES 逐字一致**。
# 两处不一致的话，「自检 3/3 正确」就不是同一件事的证据——这一条踩过：脚本里
# 一开始凭记忆写了另一组句子，注释却写着「与探针一致」。
QUALITY_CASES = [
    (
        "妈妈最近身体不好",
        [
            "妈妈打电话来说她体检结果不太好，我有点担心。",
            "今天项目上线，加班到十点才回家。",
            "房东说要涨房租，我打算周末去看别的房子。",
        ],
    ),
    (
        "想换个工作",
        [
            "下午跟主管聊完，认真考虑要不要离职。",
            "晚上跑步五公里，回来洗了个澡。",
            "妈妈让我周末回家吃饭。",
        ],
    ),
    (
        "最近总是睡不着",
        [
            "连续几天失眠，凌晨两点还醒着，白天没精神。",
            "同事推荐了一家咖啡馆，咖啡还不错。",
            "合同签完了，流程比想象中顺利。",
        ],
    ),
]


def sha256_of(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", required=True, help="输出目录（会创建）")
    args = parser.parse_args()
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)

    import torch
    import transformers
    from transformers import AutoModel, AutoTokenizer

    print("== 工具版本 ==")
    print(f"  torch {torch.__version__} · transformers {transformers.__version__}")

    # 1) 取固定 revision 的权重。
    print(f"== 取权重 {UPSTREAM_REPO}@{UPSTREAM_REVISION[:7]} ==")
    tokenizer = AutoTokenizer.from_pretrained(UPSTREAM_REPO, revision=UPSTREAM_REVISION)
    model = AutoModel.from_pretrained(UPSTREAM_REPO, revision=UPSTREAM_REVISION)
    model.eval()
    tokenizer.save_pretrained(out)
    print(f"  分词器已存到 {out}")

    # 2) 导出 fp32 ONNX。输入名与探针一致：input_ids / attention_mask / token_type_ids。
    #
    # 包一层是为了让图**只有一个输出**（`last_hidden_state`）：探针取的是
    # `outputs[0]`，而且 HF 模型默认返回 dataclass，直接导出容易出成多输出元组。
    class LastHiddenState(torch.nn.Module):
        def __init__(self, inner):
            super().__init__()
            self.inner = inner

        def forward(self, input_ids, attention_mask, token_type_ids):
            return self.inner(
                input_ids=input_ids,
                attention_mask=attention_mask,
                token_type_ids=token_type_ids,
            ).last_hidden_state

    fp32_path = out / "model.onnx"
    dummy = tokenizer("导出用的样例句子", return_tensors="pt")
    torch.onnx.export(
        LastHiddenState(model),
        (dummy["input_ids"], dummy["attention_mask"], dummy["token_type_ids"]),
        str(fp32_path),
        input_names=["input_ids", "attention_mask", "token_type_ids"],
        output_names=["last_hidden_state"],
        dynamic_axes={
            "input_ids": {0: "batch", 1: "sequence"},
            "attention_mask": {0: "batch", 1: "sequence"},
            "token_type_ids": {0: "batch", 1: "sequence"},
            "last_hidden_state": {0: "batch", 1: "sequence"},
        },
        opset_version=17,
        do_constant_folding=True,
    )
    print(f"  已导出 {fp32_path}（{fp32_path.stat().st_size} 字节）")

    # 3) 先把外置权重合并回单文件，再动态量化成 INT8。
    #
    # 新版 torch 导出器会把大权重写到 `model.onnx.data`（我们这份 94.8 MB）。合并成
    # 单文件有两个好处：量化器只看到一个自洽的模型，而且哈希是对「一个文件」算的。
    import onnx

    consolidated = onnx.load(str(fp32_path))          # 会连外置数据一起读进来
    onnx.save(consolidated, str(fp32_path))
    external = fp32_path.with_suffix(".onnx.data")
    if external.exists():
        external.unlink()
    print(f"  已合并外置权重：{fp32_path}（{fp32_path.stat().st_size} 字节）")

    from onnxruntime.quantization import QuantType, quantize_dynamic

    int8_path = out / "model_quantized.onnx"
    quantize_dynamic(
        model_input=str(fp32_path),
        model_output=str(int8_path),
        weight_type=QuantType.QInt8,
    )
    print(f"  已量化 {int8_path}（{int8_path.stat().st_size} 字节）")

    # 4) 用 onnxruntime 自己复核一遍：池化与归一化要跟探针一致（mean pooling +
    #    L2），分数与 M0 记录的 0.640 / 0.502 / 0.659 对比。
    print("== 自检（onnxruntime，mean pooling + L2）==")
    import numpy as np
    import onnxruntime as ort

    session = ort.InferenceSession(str(int8_path), providers=["CPUExecutionProvider"])

    def embed(text: str) -> np.ndarray:
        batch = tokenizer(text, return_tensors="np")
        outputs = session.run(
            None,
            {
                "input_ids": batch["input_ids"].astype(np.int64),
                "attention_mask": batch["attention_mask"].astype(np.int64),
                "token_type_ids": batch["token_type_ids"].astype(np.int64),
            },
        )[0]
        mask = batch["attention_mask"][..., None].astype(np.float32)
        pooled = (outputs * mask).sum(axis=1) / np.clip(mask.sum(axis=1), 1e-9, None)
        return pooled[0] / np.linalg.norm(pooled[0])

    for query, candidates in QUALITY_CASES:
        query_vec = embed(query)
        scored = sorted(
            ((float(query_vec @ embed(candidate)), candidate) for candidate in candidates),
            reverse=True,
        )
        ok = scored[0][1] == candidates[0]
        print(f"  查询「{query}」→ {'正确' if ok else '排序错误'}；相关句 {scored[0][0]:.3f}")

    print("== 哈希（填进 manifest）==")
    files = {}
    for path in [int8_path, out / "tokenizer.json"]:
        digest = sha256_of(path)
        files[path.name] = {"bytes": path.stat().st_size, "sha256": digest}
        print(f"  {path.name}: {path.stat().st_size} 字节 sha256={digest}")

    # 机器可读的结果文件：版本、上游 revision、两个文件的哈希。manifest 引用它，
    # 不靠人手抄（抄错哈希就等于没有证据）。
    info = {
        "upstreamRepository": UPSTREAM_REPO,
        "upstreamRevision": UPSTREAM_REVISION,
        "tools": {
            "torch": torch.__version__,
            "transformers": transformers.__version__,
        },
        "opset": 17,
        "quantization": {"weightType": "QInt8", "dynamic": True},
        "files": files,
    }
    (out / "export-info.json").write_text(
        json.dumps(info, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    print(f"  已写 {out / 'export-info.json'}")

    print("== 与 manifest 的对照 ==")
    print("  M0 实测用的是 Xenova 的转换产物：model_quantized.onnx 24010842 字节")
    print("  sha256=15b717c382bcb518ba457b93ea6850ede7f4f1cd8937454aa06972366cd19bcc")
    print("  如果上面的哈希与它不同，说明我们自己导出的版本不是同一份文件——")
    print("  这没关系（那正是目的），但要用它重跑一次探针，把耗时/内存/分数重新记进文档。")
    print(f"  复核用：DIARY_MODEL_DIR={out} cargo run --release -p diary_probe --features model -- vector-model")
    return 0


if __name__ == "__main__":
    sys.exit(main())