#!/usr/bin/env python3
"""Export a privacy-preserving diagnostic bundle for 「不写日记」.

The bundle intentionally contains **metadata only**:

* app version + current schema version;
* table row counts, file sizes, journal/page stats, quick_check;
* keyword/semantic index coverage counts;
* the last N structured wakeup log entries, filtered through an allowlist;
* the exact exclusion list and the whitelist used.

It never selects user text columns (capture drafts, source text, extracted text,
diary bodies, chunk text, asset names) and never copies blob bytes. The log path
is parsed as JSON; unknown fields, free-form messages, malformed lines and
non-code error strings are dropped. A canary test in
``test_no_user_content.py`` asserts that known body strings cannot appear in the
archive.

Usage::

    python3 tools/diagnostics/export_diagnostics.py \\
      --library "/path/to/unwritten_diary/library.sqlite" \\
      --out diagnostics.zip

Run ``--self-test`` for the built-in canary check. This is a tooling entry, not
a UI replacement for the future ``diagnostics.export`` API.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sqlite3
import sys
import zipfile
from collections import Counter, deque
from datetime import datetime, timezone
from pathlib import Path
from typing import Any
from urllib.parse import quote

TOOL_VERSION = "diagnostics-1"
FORMAT = "unwritten-diary-diagnostics"
FORMAT_VERSION = 1
EXPECTED_SCHEMA = 13
EXPECTED_TOKENIZER = "jieba-0.11-gram1-v1"
EXPECTED_CHUNKER = "chunker-1-packday-line400-overlap48"
EXPECTED_MODEL_ID = "bge-small-zh-v1.5-ours"

# Free-form / user-owned columns. Listed here so the exclusion is auditable in
# the bundle itself, and so the test can assert none of these names leak.
EXCLUDED_COLUMNS = [
    "captures.draft_text",
    "source_revisions.text",
    "extracted_contents.text / segments.text",
    "diary_versions.body",
    "text_chunks.text",
    "assets.original_name",
    "blobs bytes / object_ref",
    "log raw lines and unknown fields",
]

LOG_ALLOWED_KEYS = {
    "observedAt",
    "trigger",
    "error",
    "processed",
    "succeeded",
    "failed",
    "retrying",
    "backfilled",
    "recovered",
    "dueBacklog",
    "waitingDependency",
    "dependencyFailed",
    "nextWakeup",
}
LOG_TIMESTAMP_KEYS = {"observedAt", "nextWakeup"}
LOG_CODE_RE = re.compile(r"^[a-z0-9_.:-]{1,120}$")
LOG_TOKEN_RE = re.compile(r"^[a-z][a-z0-9_]{0,31}$")
ISO_RE = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z?$")


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def safe_error(exc: BaseException, library: Path) -> str:
    """Keep SQLite errors useful without echoing a user's full path."""
    text = str(exc)
    return text.replace(str(library), "<library>").replace(str(library.parent), "<library-dir>")[:300]


def table_exists(conn: sqlite3.Connection, table: str) -> bool:
    row = conn.execute(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name=? LIMIT 1", (table,)
    ).fetchone()
    return row is not None


def scalar(conn: sqlite3.Connection, sql: str, default: Any = None, params: tuple = ()) -> Any:
    try:
        row = conn.execute(sql, params).fetchone()
    except sqlite3.Error:
        return default
    if row is None or row[0] is None:
        return default
    return row[0]


def table_count(conn: sqlite3.Connection, table: str) -> int | None:
    if not table_exists(conn, table):
        return None
    return scalar(conn, f'SELECT COUNT(*) FROM "{table}"', None)


def grouped_counts(conn: sqlite3.Connection, table: str, column: str) -> dict[str, int] | None:
    if not table_exists(conn, table):
        return None
    try:
        rows = conn.execute(
            f'SELECT COALESCE("{column}", "<null>"), COUNT(*) FROM "{table}" '
            f'GROUP BY "{column}" ORDER BY "{column}"'
        ).fetchall()
    except sqlite3.Error:
        return None
    return {str(key): int(value) for key, value in rows}


def read_pubspec_version(root: Path) -> tuple[str | None, str | None]:
    path = root / "apps" / "diary_app" / "pubspec.yaml"
    if not path.is_file():
        return None, None
    version = build = None
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        if not line.startswith("version:"):
            continue
        raw = line.split(":", 1)[1].strip()
        if "+" in raw:
            version, build = raw.split("+", 1)
        else:
            version = raw
        break
    return version, build


def sanitize_log_entry(raw: Any) -> tuple[dict[str, Any] | None, int]:
    """Return (allowlisted entry, number of redacted/dropped values)."""
    if not isinstance(raw, dict):
        return None, 1
    out: dict[str, Any] = {}
    redacted = 0
    for key, value in raw.items():
        if key not in LOG_ALLOWED_KEYS:
            redacted += 1
            continue
        if isinstance(value, bool) or isinstance(value, (int, float)):
            out[key] = value
            continue
        if not isinstance(value, str):
            redacted += 1
            continue
        if key in LOG_TIMESTAMP_KEYS:
            if ISO_RE.match(value):
                out[key] = value
            else:
                redacted += 1
        elif key == "trigger":
            if LOG_TOKEN_RE.match(value):
                out[key] = value
            else:
                redacted += 1
        elif key == "error":
            # Only machine error codes survive. Free-form error text can embed
            # user text, paths or provider responses.
            if LOG_CODE_RE.match(value):
                out[key] = value
            else:
                out[key] = "<redacted>"
                redacted += 1
        else:
            redacted += 1
    return out, redacted


def read_logs(path: Path | None, limit: int) -> dict[str, Any]:
    if path is None or not path.is_file():
        return {
            "source": None,
            "limit": limit,
            "linesRead": 0,
            "malformedLines": 0,
            "redactedValues": 0,
            "entries": [],
        }
    entries: deque[dict[str, Any]] = deque(maxlen=limit)
    lines = malformed = redacted = 0
    with path.open("r", encoding="utf-8", errors="replace") as handle:
        for line in handle:
            lines += 1
            stripped = line.strip()
            if not stripped:
                continue
            try:
                raw = json.loads(stripped)
            except json.JSONDecodeError:
                malformed += 1
                continue
            entry, dropped = sanitize_log_entry(raw)
            redacted += dropped
            if entry is None:
                malformed += 1
            elif entry:
                entries.append(entry)
    return {
        "source": path.name,
        "limit": limit,
        "linesRead": lines,
        "malformedLines": malformed,
        "redactedValues": redacted,
        "entries": list(entries),
    }


def collect_index(conn: sqlite3.Connection) -> dict[str, Any]:
    total_segments = table_count(conn, "extracted_segments")
    indexed_segments = table_count(conn, "search_docs")
    stale_segments = scalar(
        conn,
        "SELECT COUNT(*) FROM search_docs WHERE tokenizer_version <> ?",
        None,
        (EXPECTED_TOKENIZER,),
    )
    failed_sources = scalar(
        conn, "SELECT COUNT(*) FROM extracted_contents WHERE status = 'failed'", None
    )
    total_captures_text = scalar(
        conn,
        "SELECT COUNT(*) FROM captures WHERE state <> 'trashed' "
        "AND draft_text IS NOT NULL AND length(trim(draft_text)) > 0",
        None,
    )
    indexed_captures = scalar(
        conn,
        "SELECT COUNT(*) FROM search_capture_docs WHERE tokenizer_version = ?",
        None,
        (EXPECTED_TOKENIZER,),
    )
    stale_captures = scalar(
        conn,
        "SELECT COUNT(*) FROM search_capture_docs WHERE tokenizer_version <> ?",
        None,
        (EXPECTED_TOKENIZER,),
    )
    index_rows = scalar(
        conn,
        "SELECT (SELECT COUNT(*) FROM search_grams) + (SELECT COUNT(*) FROM search_capture_grams)",
        None,
    )
    index_terms = scalar(
        conn,
        "SELECT COUNT(*) FROM ("
        "SELECT term FROM search_grams UNION SELECT term FROM search_capture_grams"
        ")",
        None,
    )
    try:
        index_bytes = scalar(
            conn,
            "SELECT COALESCE(SUM(pgsize), 0) FROM dbstat WHERE name IN "
            "('search_docs','search_grams','search_capture_docs','search_capture_grams',"
            "'text_chunks','chunk_spans','chunk_vectors','index_meta')",
            None,
        )
    except sqlite3.Error:
        index_bytes = None

    active_generation = building_generation = None
    if table_exists(conn, "index_meta"):
        row = conn.execute(
            "SELECT active_generation, building_generation FROM index_meta WHERE id = 1"
        ).fetchone()
        if row:
            active_generation, building_generation = row[0], row[1]

    total_chunks = table_count(conn, "text_chunks")
    embedded_chunks = None
    model_versions: list[str] = []
    if active_generation is not None and table_exists(conn, "chunk_vectors"):
        embedded_chunks = scalar(
            conn,
            "SELECT COUNT(DISTINCT chunk_id) FROM chunk_vectors WHERE generation = ?",
            None,
            (active_generation,),
        )
        try:
            model_versions = [
                str(row[0])
                for row in conn.execute(
                    "SELECT DISTINCT model_version FROM chunk_vectors WHERE generation = ? "
                    "ORDER BY model_version",
                    (active_generation,),
                ).fetchall()
            ]
        except sqlite3.Error:
            model_versions = []
    elif table_exists(conn, "chunk_vectors"):
        embedded_chunks = 0

    chunker_version = None
    if table_exists(conn, "text_chunks"):
        chunker_version = scalar(
            conn,
            "SELECT chunker_version FROM text_chunks ORDER BY chunk_id LIMIT 1",
            None,
        )

    keyword_ready = (
        indexed_segments is not None
        and indexed_captures is not None
        and (indexed_segments > 0 or indexed_captures > 0)
        and not stale_segments
        and not stale_captures
    )
    semantic_ready = (
        active_generation is not None
        and total_chunks is not None
        and total_chunks > 0
        and embedded_chunks == total_chunks
        and len(model_versions) == 1
    )
    if (
        total_segments is None
        or indexed_segments is None
        or total_captures_text is None
        or indexed_captures is None
    ):
        coverage = "unknown"
    elif total_segments == 0 and total_captures_text == 0 and not failed_sources:
        coverage = "unavailable"
    elif (
        total_segments == indexed_segments
        and total_captures_text == indexed_captures
        and not failed_sources
    ):
        coverage = "complete"
    elif not indexed_segments and not indexed_captures:
        coverage = "unavailable"
    else:
        coverage = "partial"

    return {
        "coverage": coverage,
        "keyword": {
            "expectedTokenizerVersion": EXPECTED_TOKENIZER,
            "indexedSegments": indexed_segments,
            "totalSegments": total_segments,
            "staleSegments": stale_segments,
            "failedSources": failed_sources,
            "indexedCaptures": indexed_captures,
            "totalCapturesWithText": total_captures_text,
            "staleCaptures": stale_captures,
            "indexRows": index_rows,
            "indexTerms": index_terms,
            "indexBytes": index_bytes,
            "ready": keyword_ready,
        },
        "semantic": {
            "expectedModelId": EXPECTED_MODEL_ID,
            "expectedChunkerVersion": EXPECTED_CHUNKER,
            "activeGeneration": active_generation,
            "buildingGeneration": building_generation,
            "totalChunks": total_chunks,
            "embeddedChunks": embedded_chunks,
            "modelVersions": model_versions,
            "chunkerVersion": chunker_version,
            "ready": semantic_ready,
        },
        "notes": [
            "Coverage is computed from aggregate counts only; it may differ from core.index_status "
            "when retired source revisions or non-ASCII whitespace are present.",
            "Index data is derived and may be rebuilt; it is never user content.",
        ],
    }


def collect_library(conn: sqlite3.Connection, path: Path, expected_schema: int) -> dict[str, Any]:
    schema = scalar(conn, "PRAGMA user_version", None)
    journal = scalar(conn, "PRAGMA journal_mode", None)
    page_size = scalar(conn, "PRAGMA page_size", None)
    page_count = scalar(conn, "PRAGMA page_count", None)
    freelist = scalar(conn, "PRAGMA freelist_count", None)
    try:
        quick = scalar(conn, "PRAGMA quick_check", "unavailable")
    except sqlite3.Error as exc:
        quick = f"error: {safe_error(exc, path)}"
    fk_problems: list[str] = []
    try:
        rows = conn.execute("PRAGMA foreign_key_check").fetchmany(20)
        fk_problems = [f"{row[0]}:{row[1]}" for row in rows]
    except sqlite3.Error:
        pass
    stat = path.stat()
    wal = Path(str(path) + "-wal")
    shm = Path(str(path) + "-shm")
    return {
        "fileName": path.name,
        "sizeBytes": stat.st_size,
        "walBytes": wal.stat().st_size if wal.is_file() else 0,
        "shmBytes": shm.stat().st_size if shm.is_file() else 0,
        "schemaVersion": schema,
        "expectedSchemaVersion": expected_schema,
        "schemaMatches": schema == expected_schema,
        "journalMode": journal,
        "pageSize": page_size,
        "pageCount": page_count,
        "freelistCount": freelist,
        "quickCheck": quick,
        "foreignKeyProblems": fk_problems,
    }


def grouped_error_codes(conn: sqlite3.Connection) -> dict[str, int] | None:
    if not table_exists(conn, "jobs"):
        return None
    try:
        rows = conn.execute(
            "SELECT COALESCE(error_code, '<none>'), COUNT(*) FROM jobs GROUP BY error_code"
        ).fetchall()
    except sqlite3.Error:
        return None
    out: dict[str, int] = {}
    for code, count in rows:
        code = str(code)
        # error_code is supposed to be a machine code. Do not let arbitrary
        # text (or a bug that stores user text there) leak into diagnostics.
        if code == "<none>" or LOG_CODE_RE.match(code):
            out[code] = int(count)
        else:
            out["<redacted>"] = out.get("<redacted>", 0) + int(count)
    return out


def collect_counts(conn: sqlite3.Connection) -> dict[str, Any]:
    asset_bytes = scalar(conn, "SELECT COALESCE(SUM(byte_size), 0) FROM assets", None)
    blob_bytes = scalar(conn, "SELECT COALESCE(SUM(byte_size), 0) FROM blobs", None)
    return {
        "captures": {
            "total": table_count(conn, "captures"),
            "byState": grouped_counts(conn, "captures", "state"),
        },
        "sourceItems": table_count(conn, "source_items"),
        "sourceRevisions": table_count(conn, "source_revisions"),
        "assets": table_count(conn, "assets"),
        "assetBytes": asset_bytes,
        "blobs": table_count(conn, "blobs"),
        "blobBytes": blob_bytes,
        "imports": {
            "total": table_count(conn, "import_sessions"),
            "inFlight": scalar(
                conn,
                "SELECT COUNT(*) FROM import_sessions WHERE state IN "
                "('prepared','copying','verifying','recoverable')",
                None,
            ),
        },
        "recordings": {
            "total": table_count(conn, "recording_sessions"),
            "open": scalar(
                conn,
                "SELECT COUNT(*) FROM recording_sessions WHERE state IN "
                "('preparing','recording','paused','stopping','interrupted','recoverable')",
                None,
            ),
        },
        "jobs": {
            "total": table_count(conn, "jobs"),
            "byState": grouped_counts(conn, "jobs", "state"),
            "byErrorCode": grouped_error_codes(conn),
        },
        "diary": {
            "days": table_count(conn, "diary_days"),
            "versions": table_count(conn, "diary_versions"),
        },
        "events": {
            "count": table_count(conn, "domain_events"),
            "lastSequence": scalar(conn, "SELECT COALESCE(MAX(sequence), 0) FROM domain_events", None),
        },
    }


def build_diagnostics(library: Path, logs: Path | None, limit: int, expected_schema: int) -> dict[str, Any]:
    if not library.is_file():
        raise FileNotFoundError(f"library not found: {library}")
    uri = f"file:{quote(str(library.resolve()))}?mode=ro"
    conn = sqlite3.connect(uri, uri=True, timeout=5.0)
    conn.execute("PRAGMA query_only = ON")
    conn.execute("PRAGMA busy_timeout = 5000")
    try:
        schema = scalar(conn, "PRAGMA user_version", None)
        expected = expected_schema
        app_version, app_build = read_pubspec_version(Path(__file__).resolve().parents[2])
        library_info = collect_library(conn, library, expected)
        counts = collect_counts(conn)
        index = collect_index(conn)
        log_info = read_logs(logs, limit)
    finally:
        conn.close()

    # `schema` is read again here only to keep the field explicit even when a
    # query helper failed; the library block is the source of truth.
    if schema is None:
        library_info["schemaVersion"] = None
        library_info["schemaMatches"] = False

    return {
        "format": FORMAT,
        "formatVersion": FORMAT_VERSION,
        "generatedAt": utc_now(),
        "toolVersion": TOOL_VERSION,
        "containsUserContent": False,
        "privacy": {
            "method": "read-only aggregate SQL + structured-log allowlist",
            "excludedColumns": EXCLUDED_COLUMNS,
            "logAllowedKeys": sorted(LOG_ALLOWED_KEYS),
            "logRules": [
                "non-JSON lines are counted and dropped",
                "unknown keys and free-form values are dropped",
                "error is kept only when it matches a machine-code pattern",
                "no paths, blob bytes, capture/source/diary/chunk text are selected",
            ],
            "verification": "tools/diagnostics/test_no_user_content.py plants canaries and asserts absence",
        },
        "app": {
            "version": app_version,
            "build": app_build,
            "toolVersion": TOOL_VERSION,
        },
        "library": library_info,
        "counts": counts,
        "index": index,
        "logs": log_info,
    }


README_TEXT = """不写日记 · 诊断包
=================

本包只含元数据：版本、库统计、索引覆盖、结构化日志白名单字段。
不含日记正文、草稿、转述、原件内容、文件名、日志原文或数据库文件。

字段与排除项见 diagnostics.json 的 privacy 段。
生成工具：tools/diagnostics/export_diagnostics.py
"""


def write_bundle(data: dict[str, Any], out: Path, force: bool) -> None:
    if out.exists() and not force:
        raise FileExistsError(f"output exists: {out} (use --force to overwrite)")
    payload = json.dumps(data, ensure_ascii=False, indent=2, sort_keys=False).encode("utf-8")
    digest = hashlib.sha256(payload).hexdigest()
    manifest = {
        "format": FORMAT,
        "formatVersion": FORMAT_VERSION,
        "generatedAt": data["generatedAt"],
        "containsUserContent": False,
        "files": [
            {"name": "diagnostics.json", "bytes": len(payload), "sha256": digest},
            {"name": "README.txt", "bytes": len(README_TEXT.encode("utf-8"))},
        ],
        "diagnosticsSha256": digest,
    }
    out.parent.mkdir(parents=True, exist_ok=True)
    tmp = out.with_name(out.name + ".partial")
    if tmp.exists():
        tmp.unlink()
    try:
        if out.suffix.lower() == ".json":
            tmp.write_bytes(payload)
        else:
            with zipfile.ZipFile(tmp, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
                archive.writestr("diagnostics.json", payload)
                archive.writestr("README.txt", README_TEXT)
                archive.writestr(
                    "manifest.json",
                    json.dumps(manifest, ensure_ascii=False, indent=2).encode("utf-8"),
                )
        os.replace(tmp, out)
        os.chmod(out, 0o600)
    finally:
        if tmp.exists():
            tmp.unlink()


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Export metadata-only diagnostics for 「不写日记」.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="The bundle contains no user text; see privacy in diagnostics.json.",
    )
    parser.add_argument("--library", type=Path, help="path to library.sqlite (read-only)")
    parser.add_argument("--logs", type=Path, help="wakeup-diagnostics.jsonl (optional)")
    parser.add_argument(
        "--out",
        type=Path,
        help="output .zip or .json (default: diagnostics-<timestamp>.zip)",
    )
    parser.add_argument("--limit", type=int, default=50, help="last N structured log entries (default 50)")
    parser.add_argument("--expected-schema", type=int, default=EXPECTED_SCHEMA)
    parser.add_argument("--force", action="store_true", help="overwrite an existing output")
    parser.add_argument("--self-test", action="store_true", help="run the canary test instead of exporting")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    if args.self_test:
        from test_no_user_content import run_self_test

        return run_self_test()
    if not args.library:
        print("必须提供 --library（或用 --self-test）", file=sys.stderr)
        return 2
    if args.limit < 0:
        print("--limit 不能为负", file=sys.stderr)
        return 2
    try:
        data = build_diagnostics(args.library, args.logs, args.limit, args.expected_schema)
        out = args.out or Path(f"diagnostics-{datetime.now().strftime('%Y%m%d-%H%M%S')}.zip")
        write_bundle(data, out, args.force)
    except FileNotFoundError as exc:
        print(f"[diagnostics][失败] {exc}", file=sys.stderr)
        return 2
    except FileExistsError as exc:
        print(f"[diagnostics][失败] {exc}", file=sys.stderr)
        return 2
    except (sqlite3.Error, OSError, json.JSONDecodeError) as exc:
        print(f"[diagnostics][失败] {exc}", file=sys.stderr)
        return 1
    print(f"诊断包：{out}（{out.stat().st_size} bytes）")
    print(f"schema={data['library']['schemaVersion']}  quick_check={data['library']['quickCheck']}")
    print(f"containsUserContent={data['containsUserContent']}  logs={len(data['logs']['entries'])}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
