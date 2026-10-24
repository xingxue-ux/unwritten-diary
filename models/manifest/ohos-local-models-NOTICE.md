# 随包本地模型

本 HAP 包含下面三组离线模型。

- SenseVoice Small INT8：`sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17-int8`，
  源模型 https://github.com/FunAudioLLM/SenseVoice ，转换产物来源与固定哈希见
  `sense-voice-small-int8-2024-07-17.json`。上游 MIT 许可全文随包保存在
  `SenseVoice-LICENSE-MIT.txt`（来源 https://raw.githubusercontent.com/FunAudioLLM/SenseVoice/main/LICENSE ）。
- BGE Small Chinese v1.5：由本项目从 `BAAI/bge-small-zh-v1.5` 的
  `7999e1d3359715c523056ef9478215996d62a620` 权重导出量化 ONNX。
  模型与分词器固定哈希见 `bge-small-zh-v1.5.json`，MIT 许可全文见
  `BGE-LICENSE-MIT.txt`。不分发历史第三方转换权重。
- Qwen3.5-0.8B：来自 Qwen，使用 ggml-org 转换的 Q4_0 GGUF（4 位量化），
  固定来源、版本和哈希见 `qwen3.5-0.8b-q4_0.json`。
  Apache-2.0 许可全文随包保存在 `Qwen-LICENSE-Apache-2.0.txt`。
  推理使用 llama.cpp `71255fefc092b0344f2acddb891388814ae961be`，
  MIT 许可全文随包保存在 `llama.cpp-LICENSE-MIT.txt`。
  随包只提供文字生成，不包含视觉投影模型。

安装包中的 `manifest.json` 从仓库的三个模型清单生成，包含所有运行文件的大小和 SHA-256。
