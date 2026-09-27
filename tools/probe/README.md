# tools/probe

M0 技术验证脚手架。**这不是产品代码**：它的存在只是为了给契约 1.4 节里的未决实现选择拿到真实测量数字。结论记在 [`docs/architecture/M0-技术验证.md`](../../docs/architecture/M0-技术验证.md)。

M0 结束后应整体删除或迁移到测试目录。

## 子命令

```bash
# 中文短词检索：比较 unicode61 / trigram / 自建 n-gram / jieba+2-gram 四条路径
cargo run --release -p diary_probe -- short-word-search [片段数，默认 20000]

# 插件运行时候选：fuel 指令预算、线性内存上限、宿主权限边界、调用开销
cargo run --release -p diary_probe -- plugin-runtime

# 两个都跑
cargo run --release -p diary_probe -- all
```

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