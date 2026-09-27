# tools/probe

M0 技术验证脚手架。**这不是产品代码**：它的存在只是为了给契约 1.4 节里的未决实现选择拿到真实测量数字。结论记在 [`docs/architecture/M0-技术验证.md`](../../docs/architecture/M0-技术验证.md)。

M0 结束后应整体删除或迁移到测试目录。

## 子命令

```bash
# 中文短词检索：比较 unicode61 / trigram / 自建 n-gram / jieba+2-gram 四条路径
cargo run --release -p diary_probe -- short-word-search [片段数，默认 20000]

# 插件运行时候选：fuel 指令预算、线性内存上限、宿主权限边界、调用开销
cargo run --release -p diary_probe -- plugin-runtime

# 本地向量模型：体积、加载、推理耗时、峰值内存、短查询质量（需要额外准备）
cargo run --release -p diary_probe --features model -- vector-model

# 两个都跑
cargo run --release -p diary_probe -- all
```

## 向量模型需要自己准备两样东西

`model` feature 不在 CI 里跑，因为它要下载权重与 ONNX Runtime。

**1. 权重与分词器**（不入库，见 `models/manifest/bge-small-zh-v1.5.json` 里的哈希）：

```bash
mkdir -p models/bge-small-zh-v1.5
curl -fL -o models/bge-small-zh-v1.5/model_quantized.onnx \
  https://huggingface.co/Xenova/bge-small-zh-v1.5/resolve/main/onnx/model_quantized.onnx
curl -fL -o models/bge-small-zh-v1.5/tokenizer.json \
  https://huggingface.co/Xenova/bge-small-zh-v1.5/resolve/main/tokenizer.json
sha256sum models/bge-small-zh-v1.5/*
```

**2. ONNX Runtime 共享库**，然后通过 `ORT_DYLIB_PATH` 指给它：

```bash
curl -fL -o /tmp/ort.tgz https://github.com/microsoft/onnxruntime/releases/download/v1.22.0/onnxruntime-linux-x64-1.22.0.tgz
mkdir -p ~/dev && tar xzf /tmp/ort.tgz -C ~/dev
export ORT_DYLIB_PATH=~/dev/onnxruntime-linux-x64-1.22.0/lib/libonnxruntime.so
cargo run --release -p diary_probe --features model -- vector-model
```

用 `load-dynamic` 而不是 `download-binaries` 是有原因的：后者会在构建期用 `ureq` 拉二进制，而它属于 `ort-sys` 的 build-dependencies，与正常依赖图不共享 feature，导致没法用 `openssl/vendored` 补救本机缺少 OpenSSL 开发头的问题。自己给 `.so` 也更接近真实打包方式。

**已知问题**：`load-dynamic` 模式下进程退出阶段会 SIGSEGV（动态库卸载与 ORT 析构顺序冲突）。探针泄漏 session 规避，真实集成前必须解决。

## feature

| feature | 内容 | 说明 |
|---|---|---|
| `search`（默认开） | bundled SQLite + jieba 词典 | 拉进 C 工具链，交叉编译需要 NDK |
| `plugin`（默认开） | wasmi + wat，纯 Rust | 可以单独开启，用于 Android 交叉编译检查 |

Android arm64 的编译检查只开 `plugin`，避免为 bundled SQLite 拉进整套 NDK C 工具链：

```bash
cargo check --release -p diary_probe --no-default-features --features plugin --target aarch64-linux-android
```

## 注意

- 语料是虚构数据、确定性生成（固定种子），数字可复现。
- 检索实验在临时目录建 SQLite 文件，跑完不会留在仓库里。
- 这里的测量都在 x86_64 主机上；真机数字需要用同一套命令在目标平台复跑。