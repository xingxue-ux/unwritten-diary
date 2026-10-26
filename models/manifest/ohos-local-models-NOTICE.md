# 随包本地模型

本 HAP 包含以下三组离线模型和本地文本生成引擎。

- SenseVoice Small INT8：`sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17-int8`，
  源模型 https://github.com/FunAudioLLM/SenseVoice ，转换产物来源与固定哈希见
  `sense-voice-small-int8-2024-07-17.json`。上游 MIT 许可全文随包保存在
  `SenseVoice-LICENSE-MIT.txt`（来源 https://raw.githubusercontent.com/FunAudioLLM/SenseVoice/main/LICENSE ）。
- BGE Small Chinese v1.5：由本项目从 `BAAI/bge-small-zh-v1.5` 的
  `7999e1d3359715c523056ef9478215996d62a620` 权重导出量化 ONNX。
  模型与分词器固定哈希见 `bge-small-zh-v1.5.json`，MIT 许可全文见
  `BGE-LICENSE-MIT.txt`。不分发历史第三方转换权重。

- Qwen3.5-4B，Unsloth Q4_K_M 转换权重。来源、固定版本和校验值见
  `qwen3.5-4b-q4_k_m.json`。上游 Apache-2.0 许可全文见
  `Qwen3.5-LICENSE-Apache-2.0.txt`。
- llama.cpp 版本 `71255fefc092b0344f2acddb891388814ae961be`，用于进程内推理；
  MIT 许可全文见 `llama.cpp-LICENSE-MIT.txt`。

安装包中的 `manifest.json` 从仓库的三个模型清单生成，包含所有运行文件的大小和 SHA-256。
