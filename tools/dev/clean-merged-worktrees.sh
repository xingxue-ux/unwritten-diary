#!/usr/bin/env bash
# 清理「分支已合入 main 且工作区干净」的 worktree 与它们的 CARGO_TARGET_DIR。
#
# 为什么必须手工清：每个 worktree 都要独立 CARGO_TARGET_DIR（共用 target 会因墙钟
# 跳变串用脏 rlib，我们为此吃过 58 个假错误的亏），而一个 target 动辄 5–25 GB。
# 2026-10-19 一次清掉 25 个 worktree，释放 WSL 145 GB / E 盘 118 GB。
#
# 用法：tools/dev/clean-merged-worktrees.sh [--dry-run]
# 保留：未合入的分支、有未提交改动的 worktree、以及 KEEP 里列出的目录名。
set -euo pipefail

DRY=0
[[ "${1:-}" == "--dry-run" ]] && DRY=1
REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
KEEP="${KEEP:-}"

git -C "$REPO" fetch origin main --quiet

git -C "$REPO" worktree list --porcelain | awk '/^worktree /{print substr($0,10)}' | while read -r wt; do
  [[ "$wt" == "$REPO" ]] && continue
  [[ -d "$wt" ]] || continue
  name=$(basename "$wt")
  skip=0; for k in $KEEP; do [[ "$name" == "$k" ]] && skip=1; done
  if [[ $skip -eq 1 ]]; then echo "跳过 $name（KEEP）"; continue; fi

  branch=$(git -C "$wt" branch --show-current 2>/dev/null || true)
  if [[ -z "$branch" ]]; then echo "跳过 $name（detached）"; continue; fi
  if ! git -C "$REPO" merge-base --is-ancestor "$branch" origin/main 2>/dev/null; then
    echo "跳过 $name（$branch 未合入 main）"; continue
  fi
  dirty=$(git -C "$wt" status --porcelain 2>/dev/null | wc -l)
  if [[ "$dirty" -ne 0 ]]; then echo "跳过 $name（有 $dirty 个未提交改动）"; continue; fi

  tgt="$HOME/dt-$name"
  if [[ $DRY -eq 1 ]]; then
    echo "会删：$wt${tgt:+ 与 }$tgt"
  else
    git -C "$REPO" worktree remove "$wt" --force
    [[ -d "$tgt" ]] && rm -rf "$tgt"
    echo "已删：$name（$branch）"
  fi
done

echo
echo "剩余 worktree："
git -C "$REPO" worktree list
echo
df -h "$HOME" /mnt/e 2>/dev/null | sed 's/^/  /'
