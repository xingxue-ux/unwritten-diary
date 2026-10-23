# 随包本地模型

本 HAP 包含下面两组离线模型，不包含 LLM 权重。

- SenseVoice Small INT8：`sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17-int8`，
  源模型 https://github.com/FunAudioLLM/SenseVoice ，转换产物来源与固定哈希见
  `sense-voice-small-int8-2024-07-17.json`。上游 MIT 许可全文随包保存在
  `SenseVoice-LICENSE-MIT.txt`（来源 https://raw.githubusercontent.com/FunAudioLLM/SenseVoice/main/LICENSE ）。
- BGE Small Chinese v1.5：由本项目从 `BAAI/bge-small-zh-v1.5` 的
  `7999e1d3359715c523056ef9478215996d62a620` 权重导出量化 ONNX。
  模型与分词器固定哈希见 `bge-small-zh-v1.5.json`，MIT 许可全文见
  `BGE-LICENSE-MIT.txt`。不分发历史第三方转换权重。

安装包中的 `manifest.json` 从仓库的两个模型清单生成，包含所有运行文件的大小和 SHA-256。
