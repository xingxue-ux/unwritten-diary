#!/usr/bin/env bash
# 重新生成 crates/diary_core/tests/fixtures/v15-library 里的 v15 样本。
#
# 用真正的 v15 构建（默认 origin/main 的 f31ee95）写一个资料库，写完 abort()，
# 故意留下未检查点的 -wal；然后拷进仓库。不要在已发布的用户库上跑。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OLD_COMMIT="${OLD_COMMIT:-f31ee95}"
WORKTREE="${WORKTREE:-$ROOT/../styles-v15-fixture-build}"
OUT="${1:-$ROOT/crates/diary_core/tests/fixtures/v15-library}"
TARGET="${CARGO_TARGET_DIR:-$HOME/targets/styles-v15-fixture}"

step() { echo; echo "==> $*"; }
fail() { echo "[fixture][失败] $*" >&2; exit 2; }
[[ -f "$ROOT/Cargo.toml" ]] || fail "不在仓库根目录：$ROOT"
command -v cargo >/dev/null || fail "找不到 cargo"
command -v sqlite3 >/dev/null || fail "找不到 sqlite3"

step "1/5 检出 v15 构建 ($OLD_COMMIT)"
if [[ -d "$WORKTREE" ]]; then
  git -C "$WORKTREE" rev-parse --verify HEAD >/dev/null 2>&1 \
    || fail "worktree 已存在但不是 git 工作树：$WORKTREE"
  [[ "$(git -C "$WORKTREE" rev-parse HEAD)" == "$(git -C "$ROOT" rev-parse "$OLD_COMMIT")" ]] \
    || fail "worktree 不是 $OLD_COMMIT：$WORKTREE（先删掉或换 WORKTREE=）"
else
  git -C "$ROOT" worktree add --detach "$WORKTREE" "$OLD_COMMIT"
fi

step "2/5 写一个只存在于临时 worktree 的生成 example"
mkdir -p "$WORKTREE/crates/diary_core/examples"
cat > "$WORKTREE/crates/diary_core/examples/make_v15_fixture.rs" <<'RS'
//! 生成一份由 v15 构建真实写出的资料库样本。写完 abort() 留下未检查点 WAL。
use std::path::Path;
use diary_core::{Core, CreateDraftInput};

fn main() {
    let path = std::env::args().nth(1).expect("usage: make_v15_fixture <library.sqlite>");
    let path = Path::new(&path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut core = Core::open(path).expect("open v15 library");
    for (day, text, op) in [
        ("2026-10-03", "样本一：十月初的一条记录。", "fixture-v15-a"),
        ("2026-10-05", "样本二：带一点异常值 🙂 与换行\n第二行。", "fixture-v15-b"),
    ] {
        let occurred = chrono::DateTime::parse_from_rfc3339(&format!("{day}T02:00:00Z"))
            .unwrap()
            .with_timezone(&chrono::Utc);
        let capture = core
            .create_draft(CreateDraftInput {
                occurred_at: Some(occurred),
                time_zone: "Asia/Shanghai",
                utc_offset_minutes: 480,
                operation_id: &format!("{op}-create"),
            })
            .expect("create");
        assert_eq!(capture.day_key, day);
        core.save_draft(&capture.id, text, capture.revision, op)
            .expect("save");
        core.commit(&capture.id, capture.revision + 1, &format!("{op}-commit"))
            .expect("commit");
    }
    // 不检查点、不关连接：模拟断电/强杀，WAL 留在文件里。
    std::process::abort();
}
RS

step "3/5 用 v15 构建写库（abort 是预期结果）"
TMP="$WORKTREE/build/v15-fixture"
rm -rf "$TMP"
mkdir -p "$TMP"
set +e
(
  cd "$WORKTREE"
  CARGO_TARGET_DIR="$TARGET" CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-3}" \
    cargo run --offline -p diary_core --example make_v15_fixture -- "$TMP/library.sqlite"
)
status=$?
set -e
if [[ $status -ne 0 && $status -ne 134 ]]; then
  fail "生成进程意外退出：$status（134=abort 是预期）"
fi
[[ -f "$TMP/library.sqlite" && -f "$TMP/library.sqlite-wal" ]] \
  || fail "没有产出 library.sqlite + -wal；WAL 可能被检查点了，重跑一次"
[[ "$(sqlite3 -readonly "$TMP/library.sqlite" 'PRAGMA user_version;')" == "15" ]] \
  || fail "生成的库不是 schema 15"

step "4/5 拷进仓库 fixture 目录"
mkdir -p "$OUT"
cp "$TMP/library.sqlite" "$OUT/library.sqlite"
cp "$TMP/library.sqlite-wal" "$OUT/library.sqlite-wal"
sha256sum "$OUT/library.sqlite" "$OUT/library.sqlite-wal"

step "5/5 验证样本（只读；会创建 -shm，不写 -wal）"
sqlite3 -readonly "$OUT/library.sqlite" \
  'PRAGMA user_version; SELECT COUNT(*) FROM captures; SELECT version FROM schema_migrations ORDER BY version;'
echo
echo "[fixture] 完成：$OUT"
echo "[fixture] 如果 sha256 变了，记得同步更新同目录 README 里的哈希。"
