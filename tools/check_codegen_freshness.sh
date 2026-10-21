#!/usr/bin/env bash
# 生成物是否与 Rust 源码同步（#148）。
#
# `crates/diary_bridge/src/frb_generated.rs` 和 `packages/diary_bridge/lib/src/rust/`
# 都是 `flutter_rust_bridge_codegen generate` 的产物。改了 `crates/diary_bridge/src/api.rs`、
# `crates/diary_core/src/model.rs`（以及它们引用的类型），却忘了重跑 codegen 时，
# **cargo build 与 dart test 仍然是绿的**——#143 就这么漂了 2 行注释，#153 又漂了一次
# （`events_since` 的文档注释没带进产物）。所以这里不靠人记：跑一遍真正的 codegen，
# 再要求生成目录**逐字节不变**。
#
# 用法（在哪个目录跑都行，脚本自己 cd 到仓库根）：
#   bash tools/check_codegen_freshness.sh
#
# 退出码：
#   0 = 生成物与源码同步；
#   1 = 有漂移（打印 `git diff --stat` 与完整 diff，并把重生成后的文件**留在工作区**，
#       方便直接 `git add` 修好）；
#   2 = 环境问题（没装 codegen，或装错版本）。

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

# 版本必须与以下三处一致：
#   - Cargo.toml 的 `flutter_rust_bridge = { version = "=2.13.0" }`
#   - packages/diary_bridge/pubspec.yaml 的 `flutter_rust_bridge: 2.13.0`
#   - .github/workflows/check.yml 的 `FRB_CODEGEN_VERSION`
# 版本不一致时 Rust 侧启动会直接拒绝加载（刻意的），所以这里先拦一道，免得把
# 「版本装错了」误报成「产物漂移」。
EXPECTED_VERSION="${FRB_CODEGEN_VERSION:-2.13.0}"

# 只对生成目录判差异。codegen 不碰仓库别的地方；把整仓库纳入会把无关的本地改动
# 也算成漂移，反而让人不信任这一步。
GENERATED_PATHS=(
  "crates/diary_bridge/src/frb_generated.rs"
  "packages/diary_bridge/lib/src/rust"
)

if ! command -v flutter_rust_bridge_codegen >/dev/null 2>&1; then
  echo "找不到 flutter_rust_bridge_codegen（需要 ${EXPECTED_VERSION}）。" >&2
  echo "装：cargo install flutter_rust_bridge_codegen --version ${EXPECTED_VERSION} --locked" >&2
  exit 2
fi

actual_version="$(flutter_rust_bridge_codegen --version | awk '{print $NF}')"
if [[ "${actual_version}" != "${EXPECTED_VERSION}" ]]; then
  echo "codegen 版本不符：装的是 ${actual_version}，仓库钉的是 ${EXPECTED_VERSION}。" >&2
  echo "重装：cargo install flutter_rust_bridge_codegen --version ${EXPECTED_VERSION} --locked --force" >&2
  exit 2
fi

echo "== flutter_rust_bridge_codegen ${actual_version}：重新生成 =="
# codegen 要读宏展开后的源码；缺 cargo-expand 时它**自己装一个**（首次约 2 分钟）。
# 只打印版本便于对齐环境，不强求一致：展开结果由 rustc + 宏决定，与它的版本无关。
if command -v cargo-expand >/dev/null 2>&1; then
  echo "== cargo-expand $(cargo expand --version 2>/dev/null | awk '{print $2}') =="
else
  echo "== 没装 cargo-expand：codegen 会自动装一个（首次约 2 分钟）=="
fi
flutter_rust_bridge_codegen generate

# 有差异时 `--quiet` 退出码为 1。哪个文件不同、各增删几行，下面用 `--numstat`
# 摘成摘要；完整 diff 跟在后面。
tracked_dirty=0
if ! git diff --quiet -- "${GENERATED_PATHS[@]}"; then
  tracked_dirty=1
fi

# `git diff` 看不见未跟踪文件：codegen 新增了文件却没 `git add` 时，上面那关会
# 静默通过，而别人拉下来就少一个文件。单独查一遍。
untracked="$(git ls-files --others --exclude-standard -- "${GENERATED_PATHS[@]}")"

if (( tracked_dirty == 0 )) && [[ -z "${untracked}" ]]; then
  echo "生成物与源码同步：${GENERATED_PATHS[*]} 无变化。"
  exit 0
fi

# 失败信息先回答「哪个文件不同」：文件名与增删行数摘成一行（GitHub 的 error
# 注解也用它），完整 diff 放在后面。以前要翻完整 diff 才知道是哪个文件。
summary_files=()
while IFS=$'\t' read -r added removed path; do
  [[ -z "${path}" ]] && continue
  summary_files+=("${path}（+${added} -${removed}）")
done < <(git diff --numstat -- "${GENERATED_PATHS[@]}")
while IFS= read -r path; do
  [[ -z "${path}" ]] && continue
  summary_files+=("${path}（未跟踪的新文件）")
done <<< "${untracked}"
summary_line="$(IFS='、'; echo "${summary_files[*]}")"

if [[ "${GITHUB_ACTIONS:-}" == "true" ]]; then
  echo "::error title=codegen 产物与源码不同步::不同的文件：${summary_line}"
fi

echo
echo "生成物与源码不同步（${#summary_files[@]} 个文件）："
printf '  %s\n' "${summary_files[@]}"
echo
echo "完整差异："
echo "------------------------------------------------------------------"
git diff -- "${GENERATED_PATHS[@]}"
if [[ -n "${untracked}" ]]; then
  echo "------------------------------------------------------------------"
  echo "未跟踪的新生成文件（忘了 git add）："
  echo "${untracked}"
fi
echo "------------------------------------------------------------------"
cat <<'EOF'

怎么修（这一步在 CI 上缓存命中约 1 分钟、未命中约 4 分钟）：

  flutter_rust_bridge_codegen generate          # 版本见上；本脚本已经跑过了，产物就在工作区
  git add crates/diary_bridge/src/frb_generated.rs packages/diary_bridge/lib/src/rust
  git commit -m "chore(bridge): 重跑 codegen"

为什么要拦：生成物是 Dart 侧看到的唯一接口。忘了带进同一个 PR 时，Dart 要么编译不过，
要么「签名漂移但类型兼容」——调用方静默用错字段，cargo test 和 dart test 全绿也发现不了。
EOF
exit 1