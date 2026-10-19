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
is parsed as JSON; unknown fields, free-form messages and malformed lines are
dropped.

Privacy is enforced by **exact allowlists**, never by “looks like a machine
code” shape checks:

* `error` / `jobs.error_code` survive only when the value is in the explicit
  whitelist built from `crates/diary_core/src/error.rs` (`ErrorCode::wire()`)
  plus the job-only codes in `crates/diary_core/src/jobs.rs`. Everything else
  becomes `<redacted>` — including short alphanumerics such as `password123`.
* The library file name is fixed to `<hidden>`; only an allowlisted extension
  survives. The log path is reduced to `{present, extension}`. No user-chosen
  file or directory name is exported.

A canary test in ``test_no_user_content.py`` plants body strings, short
sensitive words and Chinese file names, then searches every byte of the archive
for them.

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

TOOL_VERSION = "diagnostics-2"
FORMAT = "unwritten-diary-diagnostics"
# v2: exact error-code whitelist; user file names hidden (extension only);
# log source reduced to {present, extension}.
FORMAT_VERSION = 2
EXPECTED_SCHEMA = 13
EXPECTED_TOKENIZER = "jieba-0.11-gram1-v1"
EXPECTED_CHUNKER = "chunker-1-packday-line400-overlap48"
EXPECTED_MODEL_ID = "bge-small-zh-v1.5-ours"

# Every zip member gets the same fixed DOS timestamp. Python's `zipfile` would
# otherwise call `time.localtime()` per entry: the member time minus the
# manifest's UTC `generatedAt` reveals the user's UTC offset, and two exports
# of the same database hash differently. Pinned to the earliest legal DOS
# timestamp so the value can never be mistaken for "when this was exported".
ZIP_MEMBER_DATE_TIME = (1980, 1, 1, 0, 0, 0)
ZIP_MEMBER_MODE = 0o600

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
    # Written by `core_wakeup.dart` on the wakeup path. They used to be
    # dropped here even though the app emits them on every wake, which made
    # "the wake actually ran at the expected time" impossible to check.
    "expectedAt",
    "completedAt",
}
LOG_TIMESTAMP_KEYS = {"observedAt", "nextWakeup", "expectedAt", "completedAt"}
ISO_RE = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z?$")

# Exact machine-code whitelist. This is intentionally a membership test, not a
# regex: a regex only proves that a string *looks* like a code, so any short
# lowercase alphanumeric secret (`password123`, `secret42`, `mytoken1`, ...)
# would pass. Unknown values are redacted.
#
# Source of truth: `crates/diary_core/src/error.rs` — `ErrorCode::wire()`
# (contract §7). Keep this table in sync with the Rust enum; the test
# `test_error_code_whitelist_matches_error_rs` parses that file and fails on
# drift, so adding a code cannot silently desynchronise diagnostics.
ERROR_CODE_WHITELIST = frozenset(
    {
        "not_found",
        "invalid_state",
        "invalid_argument",
        "revision_conflict",
        "idempotency_conflict",
        "storage_full",
        "asset_missing",
        "permission_denied",
        "integrity_failed",
        "unsupported_format",
        "search_expired",
        "cursor_expired",
        "provider_not_configured",
        "authentication_failed",
        "network_unavailable",
        "rate_limited",
        "quota_exceeded",
        "provider_timeout",
        "model_unavailable",
        "corrupted_backup",
        "incompatible_backup",
    }
)

# Codes written to `jobs.error_code` that are not contract `ErrorCode` values.
# Source: `crates/diary_core/src/jobs.rs` — ERROR_NO_TARGET,
# ERROR_DEPENDENCY_UNMET, WAITING_EXTRACT, WAITING_TRANSCRIBE, WAITING_MODEL,
# plus the literals `no_handler` / `interrupted`. The drift test asserts every
# entry still appears in that file.
JOB_ONLY_ERROR_CODES = frozenset(
    {
        "no_handler",
        "interrupted",
        "no_target",
        "dependency_extract_unmet",
        "waiting_extract",
        "waiting_transcribe",
        "waiting_model",
    }
)

# `captures.state` and `jobs.state` are plain `TEXT NOT NULL` in schema.rs (no
# CHECK constraint), so a bug or a hand-edited library can store arbitrary text
# there. `GROUP BY state` used to copy it verbatim into the bundle. Same policy
# as error codes: only exact `wire()` values from
# `crates/diary_core/src/model.rs` survive; everything else is grouped as
# `<redacted>`. The drift test parses that file so a new state cannot be
# silently dropped (or a removed one silently kept).
CAPTURE_STATE_WHITELIST = frozenset({"draft", "committed", "trashed"})
JOB_STATE_WHITELIST = frozenset(
    {
        "queued",
        "running",
        "succeeded",
        "retry_wait",
        "waiting_configuration",
        "waiting_dependency",
        "waiting_network",
        "failed",
        "cancelled",
    }
)

# Machine codes emitted only by the Flutter wakeup logger
# (`apps/diary_app/lib/src/core_wakeup.dart`). `diagnosticErrorCode()` maps a
# thrown `DiaryException` to its wire code; Dart runtime type names are not
# machine codes and stay redacted. `unclassified_error` is the fixed literal
# for "not a DiaryException / PlatformException", so even that case is
# diagnosable without echoing a runtime type.
LOG_ONLY_ERROR_CODES = frozenset(
    {"installed_core_missing_job_capabilities", "unclassified_error"}
)

# `DiaryErrorCode` values that exist only in the Dart API package
# (`packages/diary_api/lib/src/values.dart`, plugin feature) and therefore are
# not part of `ErrorCode::wire()`. They can still be the `code.wire` of a real
# `DiaryException`, so the log must keep them; source is asserted by the drift
# test.
APP_ONLY_ERROR_CODES = frozenset({"plugin_denied", "plugin_budget_exceeded", "unknown"})

# NAPI host codes (`apps/ohos-diary-core/entry/src/main/cpp/napi_shim.cpp`).
# A `PlatformException` carries the JS `Error.code`, which is one of these when
# the call could not happen. They are stable machine codes, not free-form text.
NAPI_ERROR_CODES = frozenset(
    {
        "ERR_DIARY_LIB",
        "ERR_DIARY_ARGS",
        "ERR_DIARY_HANDLE",
        "ERR_DIARY_OPEN",
        "ERR_DIARY_CLOSE",
        "ERR_DIARY_CALL",
    }
)

JOB_ERROR_CODES = ERROR_CODE_WHITELIST | JOB_ONLY_ERROR_CODES
LOG_ERROR_CODES = (
    JOB_ERROR_CODES | LOG_ONLY_ERROR_CODES | APP_ONLY_ERROR_CODES | NAPI_ERROR_CODES
)

# `trigger` is a closed vocabulary in `core_wakeup.dart`. Treat it as a
# whitelist too, so a future free-form value cannot leak into the archive.
LOG_TRIGGERS = frozenset({"startup", "foreground-schedule", "wake"})

# Extensions that may be reported. An extension describes a container/format,
# not a user-chosen name. Anything not listed becomes `<unknown>`: otherwise a
# suffix like `report.password123` would smuggle the secret through.
SAFE_EXTENSIONS = frozenset(
    {
        # libraries and backups
        "sqlite",
        "sqlite3",
        "db",
        "db3",
        "diarybak",
        # structured logs
        "jsonl",
        "json",
        "log",
        # common text/document formats
        "txt",
        "md",
        "markdown",
        "pdf",
        "doc",
        "docx",
        "odt",
        "epub",
        "rtf",
        # images
        "png",
        "jpg",
        "jpeg",
        "webp",
        "gif",
        "heic",
        "bmp",
        # audio
        "m4a",
        "mp3",
        "wav",
        "aac",
        "flac",
        "ogg",
        "opus",
        # archives
        "zip",
        "7z",
    }
)


def safe_extension(name: str) -> str:
    """Return an allowlisted lower-case extension, never the file name.

    The suffix itself is checked against ``SAFE_EXTENSIONS`` so that an
    attacker-chosen suffix cannot carry a secret. No suffix -> ``<none>``.
    """
    suffix = Path(name).suffix.lower().lstrip(".")
    if not suffix:
        return "<none>"
    return suffix if suffix in SAFE_EXTENSIONS else "<unknown>"


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def safe_error(exc: BaseException, library: Path) -> str:
    """Keep SQLite errors useful without echoing a user's path or file name."""
    text = str(exc)
    for needle in (str(library), str(library.parent), library.name):
        if needle and needle not in {"/", "."}:
            text = text.replace(needle, "<library>")
    return text[:300]


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


def grouped_counts(
    conn: sqlite3.Connection, table: str, column: str, allowed: frozenset[str]
) -> dict[str, int] | None:
    """GROUP BY a closed-vocabulary column; unknown values become `<redacted>`.

    Never return the raw key: the column is `TEXT` without a CHECK constraint,
    so an arbitrary string (a user's draft, a leaked secret) can end up there.
    """
    if not table_exists(conn, table):
        return None
    try:
        rows = conn.execute(
            f'SELECT COALESCE("{column}", "<null>"), COUNT(*) FROM "{table}" '
            f'GROUP BY "{column}" ORDER BY "{column}"'
        ).fetchall()
    except sqlite3.Error:
        return None
    out: dict[str, int] = {}
    for key, value in rows:
        text = str(key)
        if text == "<null>" or text in allowed:
            out[text] = out.get(text, 0) + int(value)
        else:
            out["<redacted>"] = out.get("<redacted>", 0) + int(value)
    return out


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
            # Closed vocabulary from the app's wakeup logger.
            if value in LOG_TRIGGERS:
                out[key] = value
            else:
                redacted += 1
        elif key == "error":
            # Only exact machine codes survive. Free-form error text can embed
            # user text, paths or provider responses; a shape check is not
            # enough because `password123` is also short lowercase alphanumeric.
            if value in LOG_ERROR_CODES:
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
            "source": {"present": False, "extension": None},
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
        # Only "is there a log file" and its safe extension survive. The path
        # and file name are user-chosen and never exported.
        "source": {"present": True, "extension": safe_extension(path.name)},
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
    # Library text is used only to compute booleans below. `model_version` and
    # `chunker_version` are plain TEXT columns; a canary planted there used to
    # be copied verbatim into the bundle. Never export the value itself.
    model_versions: list[str] = []
    has_chunk_vectors = table_exists(conn, "chunk_vectors")
    if active_generation is not None and has_chunk_vectors:
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
    elif has_chunk_vectors:
        embedded_chunks = 0

    if not has_chunk_vectors:
        model_version_matches_expected: bool | None = None
    else:
        model_version_matches_expected = sorted(set(model_versions)) == [EXPECTED_MODEL_ID]

    chunker_version: str | None = None
    if table_exists(conn, "text_chunks"):
        chunker_version = scalar(
            conn,
            "SELECT chunker_version FROM text_chunks ORDER BY chunk_id LIMIT 1",
            None,
        )
    chunker_version_matches_expected: bool | None = (
        None if chunker_version is None else chunker_version == EXPECTED_CHUNKER
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
            # Booleans only: the versions themselves are user-writable TEXT
            # columns and must never be copied into a bundle.
            "modelVersionMatchesExpected": model_version_matches_expected,
            "chunkerVersionMatchesExpected": chunker_version_matches_expected,
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
        # Never echo the user's library file name. The extension is enough to
        # tell "was this a .sqlite / .diarybak" apart, and it is allowlisted.
        "fileName": "<hidden>",
        "extension": safe_extension(path.name),
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
        # `error_code` is supposed to be a machine code, but a bug (or a
        # future feature) could store arbitrary text there. Only exact
        # whitelisted codes survive; everything else is grouped as redacted.
        if code == "<none>" or code in JOB_ERROR_CODES:
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
            "byState": grouped_counts(conn, "captures", "state", CAPTURE_STATE_WHITELIST),
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
            "byState": grouped_counts(conn, "jobs", "state", JOB_STATE_WHITELIST),
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
            "method": "read-only aggregate SQL + explicit value allowlists",
            "excludedColumns": EXCLUDED_COLUMNS,
            "logAllowedKeys": sorted(LOG_ALLOWED_KEYS),
            "logTriggers": sorted(LOG_TRIGGERS),
            "stateWhitelists": {
                "captures": sorted(CAPTURE_STATE_WHITELIST),
                "jobs": sorted(JOB_STATE_WHITELIST),
            },
            "errorCodeWhitelist": sorted(LOG_ERROR_CODES),
            "errorCodeWhitelistSource": (
                "crates/diary_core/src/error.rs ErrorCode::wire() + "
                "crates/diary_core/src/jobs.rs + "
                "packages/diary_api/lib/src/values.dart DiaryErrorCode + "
                "apps/diary_app/lib/src/core_wakeup.dart + "
                "apps/ohos-diary-core/entry/src/main/cpp/napi_shim.cpp"
            ),
            "namePolicy": [
                "library fileName is fixed to <hidden>",
                "only an allowlisted extension is exported (else <none>/<unknown>)",
                "log source is reduced to {present, extension}; path and name are dropped",
            ],
            "logRules": [
                "non-JSON lines are counted and dropped",
                "unknown keys and free-form values are dropped",
                "error is kept only when it is in the exact machine-code whitelist",
                "trigger is kept only when it is in the closed trigger vocabulary",
                "unknown values become <redacted>; a shape match is never enough",
                "no paths, file names, blob bytes, capture/source/diary/chunk text are selected",
            ],
            "stateRules": [
                "captures.state / jobs.state are kept only when they are an exact "
                "CaptureState::wire()/JobState::wire() value from model.rs",
                "unknown states are grouped as <redacted>",
                "model/chunker versions are reported as matchesExpected booleans only",
            ],
            "verification": (
                "tools/diagnostics/test_no_user_content.py plants body, short-word and "
                "Chinese-file-name canaries and asserts every archive byte is clean"
            ),
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
不含日记正文、草稿、转述、原件内容、文件名、路径、日志原文或数据库文件。

错误码只保留白名单内的机器码，未知值一律脱敏为 <redacted>。
库文件名固定为 <hidden>，日志来源只保留 {present, extension}。

字段与排除项见 diagnostics.json 的 privacy 段。
生成工具：tools/diagnostics/export_diagnostics.py
"""


def zip_member(name: str) -> zipfile.ZipInfo:
    """A member with a fixed timestamp and mode: no local time, no umask leak."""
    info = zipfile.ZipInfo(name, date_time=ZIP_MEMBER_DATE_TIME)
    info.compress_type = zipfile.ZIP_DEFLATED
    info.create_system = 3  # Unix, so external_attr is interpreted as a mode
    info.external_attr = ZIP_MEMBER_MODE << 16
    return info


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
                archive.writestr(zip_member("diagnostics.json"), payload)
                archive.writestr(zip_member("README.txt"), README_TEXT)
                archive.writestr(
                    zip_member("manifest.json"),
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
        out = args.out or Path(
            f"diagnostics-{datetime.now(timezone.utc).strftime('%Y%m%d-%H%M%S')}.zip"
        )
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
