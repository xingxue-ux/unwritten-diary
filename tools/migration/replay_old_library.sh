#!/usr/bin/env bash
# Replay a real old diary library through the current migration, without ever
# writing to the source.
#
# What it does:
#   1. takes a consistent copy of the source DB with SQLite's `.backup`
#      (the copy includes committed WAL content; a plain `cp library.sqlite`
#      would silently lose it);
#   2. snapshots table counts and hashes of the source DB/-wal;
#   3. runs crates/diary_core/tests/migration_replay_test.rs against the copy;
#   4. checks quick_check / foreign_key_check and compares business counts;
#   5. verifies the source DB/-wal hashes did not change.
#
# Usage:
#   tools/migration/replay_old_library.sh --source <library.sqlite|library-dir> [options]
#
# Options:
#   --workdir DIR          default build/migration-replay/<timestamp>
#   --with-blobs           also copy <source-dir>/blobs for an asset drill
#   --expected-schema N    default 17 (current SCHEMA_VERSION)
#   --keep                  keep the replay directory (default: keep; flag is documentation)
#   --dry-run              stop after the consistent copy and pre-migration checks
#
# Never point --source at the live library while the app is running. Close the
# app first; the script refuses only some forms of in-place replay, so closing
# the app is the real safety measure.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
SOURCE=""
WORKDIR=""
WITH_BLOBS=0
EXPECTED_SCHEMA=17
DRY_RUN=0

usage() { sed -n '2,26p' "$0" | sed 's/^# \{0,1\}//'; }
fail() { echo "[replay][失败] $*" >&2; exit 2; }
step() { echo; echo "==> $*"; }
need() { command -v "$1" >/dev/null || fail "找不到 $1"; }

while (($#)); do
  case "$1" in
    --source) SOURCE="${2:-}"; shift 2 ;;
    --workdir) WORKDIR="${2:-}"; shift 2 ;;
    --with-blobs) WITH_BLOBS=1; shift ;;
    --expected-schema) EXPECTED_SCHEMA="${2:-}"; shift 2 ;;
    --keep) shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "未知参数：$1（--help 看用法）" ;;
  esac
done

[[ -n "$SOURCE" ]] || fail "必须用 --source 指定一份真实旧库（library.sqlite 或其目录）"
need sqlite3
need cargo
need sha256sum
need python3

if [[ -d "$SOURCE" ]]; then
  SRC_DB="$SOURCE/library.sqlite"
  SRC_DIR="$SOURCE"
else
  SRC_DB="$SOURCE"
  SRC_DIR="$(cd "$(dirname "$SOURCE")" && pwd)"
fi
[[ -f "$SRC_DB" ]] || fail "找不到资料库：$SRC_DB"
[[ -f "$SRC_DB" ]] || fail "不是文件：$SRC_DB"
case "$SRC_DB" in
  -*) fail "源库路径不能以 - 开头" ;;
esac

if [[ -z "$WORKDIR" ]]; then
  WORKDIR="$ROOT/build/migration-replay/$(date +%Y%m%d-%H%M%S)"
fi
mkdir -p "$WORKDIR"
WORKDIR="$(cd "$WORKDIR" && pwd)"
case "$WORKDIR/" in
  "$SRC_DIR/"*) fail "workdir 不能放在源资料库目录里：$WORKDIR" ;;
esac
if [[ -n "$(find "$WORKDIR" -mindepth 1 -maxdepth 1 -print -quit)" ]]; then
  fail "workdir 不是空目录：$WORKDIR（换一个或先手工清理）"
fi

DST_DB="$WORKDIR/library.sqlite"
SRC_HASH_BEFORE="$WORKDIR/source-before.sha256"
SRC_HASH_AFTER="$WORKDIR/source-after.sha256"
REPLAY_LOG="$WORKDIR/replay.log"

hash_source() {
  : > "$1"
  # -shm is a transient index and may change on read; DB and WAL must not.
  sha256sum "$SRC_DB" >> "$1"
  [[ -f "$SRC_DB-wal" ]] && sha256sum "$SRC_DB-wal" >> "$1" || true
}

sql() { sqlite3 -readonly "$1" "$2"; }
count_if_table() {
  local db="$1" table="$2"
  local exists
  exists="$(sqlite3 -readonly "$db" "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='$table';")"
  if [[ "$exists" == "1" ]]; then
    sqlite3 -readonly "$db" "SELECT COUNT(*) FROM \"$table\";"
  else
    echo "n/a"
  fi
}

step "1/6 源库与前置检查"
echo "source:  $SRC_DB"
echo "workdir: $WORKDIR"
BEFORE_SCHEMA="$(sqlite3 -readonly "$SRC_DB" 'PRAGMA user_version;')"
BEFORE_QUICK="$(sqlite3 -readonly "$SRC_DB" 'PRAGMA quick_check;' 2>&1 || true)"
echo "source schema: $BEFORE_SCHEMA"
echo "source quick_check: $BEFORE_QUICK"
[[ "$BEFORE_SCHEMA" =~ ^[0-9]+$ ]] || fail "读不出源库的 PRAGMA user_version"
if ((BEFORE_SCHEMA > EXPECTED_SCHEMA)); then
  fail "源库 schema=$BEFORE_SCHEMA 高于本构建的 $EXPECTED_SCHEMA；这不能用来验升级，先换旧库"
fi
if ((BEFORE_SCHEMA == EXPECTED_SCHEMA)); then
  echo "[replay][注意] 源库已经是 schema=$EXPECTED_SCHEMA；本次只验“开库不坏”，不验旧版→新版迁移。"
fi
hash_source "$SRC_HASH_BEFORE"
cat "$SRC_HASH_BEFORE"

step "2/6 一致快照（SQLite .backup，含已提交 WAL）"
# `-readonly` 很关键：带未检查点 WAL 的旧库如果按读写打开，SQLite 会在连接关闭时
# 做检查点并删掉 -wal，源库就被改了（本脚本的核心承诺是不写源库）。只读连接走同一个
# backup API，但不会回写源库；它可能创建一个临时的 -shm，不算资料内容。
sqlite3 -readonly -cmd '.timeout 5000' "$SRC_DB" ".backup '$DST_DB'"
[[ -f "$DST_DB" ]] || fail "备份 API 没有产出 $DST_DB"
# The copy must already be at the source schema; migration happens only when
# Core::open runs in step 4.
COPY_SCHEMA="$(sqlite3 -readonly "$DST_DB" 'PRAGMA user_version;')"
[[ "$COPY_SCHEMA" == "$BEFORE_SCHEMA" ]] || fail "拷贝的 schema=$COPY_SCHEMA 与源库=$BEFORE_SCHEMA 不一致"
echo "consistent copy: $DST_DB ($(stat -c%s "$DST_DB") bytes)"
if ((WITH_BLOBS)); then
  if [[ -d "$SRC_DIR/blobs" ]]; then
    cp -a "$SRC_DIR/blobs" "$WORKDIR/"
    echo "blobs copied: $(find "$WORKDIR/blobs" -type f | wc -l) files"
  else
    echo "[replay][注意] 源目录没有 blobs/，--with-blobs 无内容可拷"
  fi
fi
if ((DRY_RUN)); then
  echo "[dry-run] 已停止：未运行迁移测试。"
  exit 0
fi

step "3/6 记录迁移前业务行数"
TABLES=(captures source_items assets blobs diary_versions diary_days extracted_contents media_jobs)
: > "$WORKDIR/counts-before.txt"
for table in "${TABLES[@]}"; do
  printf '%s\t%s\n' "$table" "$(count_if_table "$DST_DB" "$table")" | tee -a "$WORKDIR/counts-before.txt"
done

step "4/6 用当前核心打开拷贝（会跑全部逐级迁移）"
export DIARY_REPLAY_LIBRARY="$DST_DB"
export DIARY_REPLAY_ALLOW_IN_PLACE=0
(
  cd "$ROOT"
  CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" \
    cargo test -p diary_core --test migration_replay_test -- --ignored --nocapture
) 2>&1 | tee "$REPLAY_LOG"
# `tee` masks the pipeline status without pipefail; it is on above.
grep -q 'REPLAY schemaVersion=' "$REPLAY_LOG" || fail "迁移测试没有输出 REPLAY 统计；看 $REPLAY_LOG"

step "5/6 迁移后校验"
AFTER_SCHEMA="$(sqlite3 -readonly "$DST_DB" 'PRAGMA user_version;')"
[[ "$AFTER_SCHEMA" == "$EXPECTED_SCHEMA" ]] || fail "迁移后 schema=$AFTER_SCHEMA，期望 $EXPECTED_SCHEMA"
QUICK="$(sqlite3 -readonly "$DST_DB" 'PRAGMA quick_check;')"
[[ "$QUICK" == "ok" ]] || fail "迁移后 quick_check=$QUICK"
FK="$(sqlite3 -readonly "$DST_DB" 'PRAGMA foreign_key_check;')"
[[ -z "$FK" ]] || fail "迁移后 foreign_key_check 非空：$FK"
echo "schema=$AFTER_SCHEMA quick_check=ok foreign_key_check=empty"

: > "$WORKDIR/counts-after.txt"
for table in "${TABLES[@]}"; do
  printf '%s\t%s\n' "$table" "$(count_if_table "$DST_DB" "$table")" | tee -a "$WORKDIR/counts-after.txt"
done
python3 - "$WORKDIR/counts-before.txt" "$WORKDIR/counts-after.txt" <<'PY'
import sys
def read(path):
    out = {}
    for line in open(path, encoding="utf-8"):
        name, value = line.rstrip("\n").split("\t", 1)
        out[name] = value
    return out
before, after = read(sys.argv[1]), read(sys.argv[2])
bad = []
for name, old in before.items():
    new = after.get(name)
    if old == "n/a" or new == "n/a":
        continue
    if old != new:
        bad.append(f"{name}: {old} -> {new}")
if bad:
    print("[replay][失败] 业务行数发生变化：", file=sys.stderr)
    for line in bad:
        print("  " + line, file=sys.stderr)
    sys.exit(2)
print("[replay] 业务行数逐表一致（n/a 表示旧库本来没有该表）")
PY

# Bring the migrated copy back to a plain WAL-free file for the final hash and
# for handing to a human. checkpoint is safe on the copy, never the source.
sqlite3 "$DST_DB" 'PRAGMA wal_checkpoint(TRUNCATE);' >/dev/null
FINAL_SHA="$(sha256sum "$DST_DB" | cut -d' ' -f1)"
echo "migrated copy sha256=$FINAL_SHA"
echo "$FINAL_SHA  migrated-library.sqlite" > "$WORKDIR/migrated.sha256"

step "6/6 源库未被修改"
hash_source "$SRC_HASH_AFTER"
if ! diff -u "$SRC_HASH_BEFORE" "$SRC_HASH_AFTER"; then
  fail "源库或 -wal 的哈希变了；立即停止并检查是否有进程在写源库（本脚本只应读它）"
fi
echo "source unchanged: $SRC_DB"
cat <<EOF

回放通过。
  source:        $SRC_DB
  migrated copy: $DST_DB
  log:           $REPLAY_LOG
  counts:        $WORKDIR/counts-before.txt / counts-after.txt
请把 workdir 里的日志与计数留作升级证据；不要用迁移后的库覆盖用户原库。
EOF
