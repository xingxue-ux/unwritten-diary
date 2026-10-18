#!/usr/bin/env python3
"""Canary test: prove the diagnostic bundle cannot contain user text.

The fixture puts distinctive strings into every user-owned column, a blob file,
and the log file. The exporter is then run through its real CLI; every byte of
the resulting archive is searched for those canaries. This is deliberately
stronger than checking a field allowlist in code: it validates the produced
artifact.

Run directly or via the exporter's ``--self-test``. All scratch files live under
the repository's ignored ``build/`` directory (never /tmp).
"""

from __future__ import annotations

import json
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = Path(__file__).resolve().with_name("export_diagnostics.py")
CANARIES = [
    "CANARY_DRAFT_7f3a9c",
    "CANARY_SOURCE_2b8d1e",
    "CANARY_DIARY_5a6f0c",
    "CANARY_CHUNK_9d4e2a",
    "CANARY_ASSET_1c7b5d",
    "CANARY_LOG_8e2f4a",
    "CANARY_BLOB_3a9c6e",
    "CANARY_JOB_6b1d8f",
]
BANNED_KEYS = {
    "body",
    "draftText",
    "draft_text",
    "text",
    "originalName",
    "original_name",
    "content",
    "message",
    "rawLine",
}


def build_fixture(base: Path) -> tuple[Path, Path, Path]:
    library = base / "library.sqlite"
    conn = sqlite3.connect(library)
    conn.executescript(
        """
        PRAGMA user_version = 13;
        CREATE TABLE captures(id TEXT PRIMARY KEY, state TEXT, draft_text TEXT);
        CREATE TABLE source_items(source_id TEXT PRIMARY KEY, capture_id TEXT);
        CREATE TABLE source_revisions(revision_id TEXT PRIMARY KEY, source_id TEXT, text TEXT);
        CREATE TABLE extracted_contents(id TEXT PRIMARY KEY, status TEXT);
        CREATE TABLE extracted_segments(id TEXT PRIMARY KEY, content_id TEXT);
        CREATE TABLE search_docs(
            segment_id TEXT PRIMARY KEY, text_length INTEGER, tokenizer_version TEXT
        );
        CREATE TABLE search_capture_docs(
            capture_id TEXT PRIMARY KEY, text_length INTEGER, tokenizer_version TEXT
        );
        CREATE TABLE search_grams(term TEXT, doc_id INTEGER);
        CREATE TABLE search_capture_grams(term TEXT, doc_id INTEGER);
        CREATE TABLE text_chunks(
            chunk_id TEXT PRIMARY KEY, text TEXT, chunker_version TEXT
        );
        CREATE TABLE chunk_vectors(
            chunk_id TEXT, generation INTEGER, model_version TEXT, dims INTEGER, vector BLOB
        );
        CREATE TABLE index_meta(id INTEGER PRIMARY KEY, active_generation INTEGER, building_generation INTEGER);
        CREATE TABLE blobs(sha256 TEXT PRIMARY KEY, object_ref TEXT, byte_size INTEGER);
        CREATE TABLE assets(
            id TEXT PRIMARY KEY, sha256 TEXT, original_name TEXT, byte_size INTEGER
        );
        CREATE TABLE jobs(id TEXT PRIMARY KEY, state TEXT, error_code TEXT);
        CREATE TABLE import_sessions(id TEXT PRIMARY KEY, state TEXT);
        CREATE TABLE recording_sessions(id TEXT PRIMARY KEY, state TEXT);
        CREATE TABLE domain_events(sequence INTEGER PRIMARY KEY);
        CREATE TABLE diary_versions(id TEXT PRIMARY KEY, day_key TEXT, body TEXT);
        CREATE TABLE diary_days(day_key TEXT PRIMARY KEY);
        """
    )
    conn.executemany(
        "INSERT INTO captures VALUES(?,?,?)",
        [
            ("cap_1", "committed", f"日记草稿 {CANARIES[0]}"),
            ("cap_2", "trashed", "已删除"),
        ],
    )
    conn.execute("INSERT INTO source_items VALUES('src_1','cap_1')")
    conn.execute(
        "INSERT INTO source_revisions VALUES('rev_1','src_1',?)", (CANARIES[1],)
    )
    conn.execute("INSERT INTO extracted_contents VALUES('ext_1','success')")
    conn.execute("INSERT INTO extracted_segments VALUES('seg_1','ext_1')")
    conn.execute(
        "INSERT INTO search_docs VALUES('seg_1', 10, 'jieba-0.11-gram1-v1')"
    )
    conn.execute(
        "INSERT INTO search_capture_docs VALUES('cap_1', 12, 'jieba-0.11-gram1-v1')"
    )
    conn.execute("INSERT INTO search_grams VALUES('日记', 1)")
    conn.execute("INSERT INTO search_capture_grams VALUES('草稿', 1)")
    conn.execute(
        "INSERT INTO text_chunks VALUES('chunk_1', ?, 'chunker-1-packday-line400-overlap48')",
        (CANARIES[3],),
    )
    conn.execute(
        "INSERT INTO chunk_vectors VALUES('chunk_1', 1, 'bge-small-zh-v1.5-ours@abc123', 512, X'00000000')"
    )
    conn.execute("INSERT INTO index_meta VALUES(1, 1, NULL)")
    conn.execute("INSERT INTO blobs VALUES('sha_1','blobs/sha_1',11)")
    conn.execute("INSERT INTO assets VALUES('asset_1','sha_1',?,11)", (CANARIES[4],))
    conn.execute("INSERT INTO jobs VALUES('job_1','queued','no_handler')")
    conn.execute("INSERT INTO jobs VALUES('job_2','failed',?)", (CANARIES[7],))
    conn.execute("INSERT INTO import_sessions VALUES('imp_1','prepared')")
    conn.execute("INSERT INTO recording_sessions VALUES('rec_1','recording')")
    conn.execute("INSERT INTO domain_events VALUES(7)")
    conn.execute("INSERT INTO diary_versions VALUES('ver_1','2026-10-18',?)", (CANARIES[2],))
    conn.execute("INSERT INTO diary_days VALUES('2026-10-18')")
    conn.commit()
    conn.close()

    blob = base / "blobs"
    blob.mkdir()
    (blob / "sha_1").write_text(CANARIES[6], encoding="utf-8")

    logs = base / "wakeup-diagnostics.jsonl"
    logs.write_text(
        "\n".join(
            [
                json.dumps(
                    {
                        "observedAt": "2026-10-18T08:00:00Z",
                        "trigger": "wake",
                        "processed": 2,
                        "succeeded": 1,
                        "failed": 1,
                        "dueBacklog": 0,
                        "error": "no_handler",  # machine code survives
                        "body": CANARIES[5],  # unknown key dropped
                        "message": CANARIES[5],  # free-form dropped
                    },
                    ensure_ascii=False,
                ),
                json.dumps(
                    {
                        "observedAt": "2026-10-18T08:01:00Z",
                        "trigger": "wake",
                        "error": CANARIES[5],  # free-form error becomes <redacted>
                    },
                    ensure_ascii=False,
                ),
                "not-json " + CANARIES[5],
            ]
        )
        + "\n",
        encoding="utf-8",
    )
    return library, logs, blob


def all_archive_bytes(path: Path) -> bytes:
    chunks = []
    with zipfile.ZipFile(path) as archive:
        for name in archive.namelist():
            chunks.append(name.encode("utf-8"))
            chunks.append(archive.read(name))
    return b"\n".join(chunks)


def walk_keys(value: Any):
    if isinstance(value, dict):
        for key, child in value.items():
            yield key
            yield from walk_keys(child)
    elif isinstance(value, list):
        for child in value:
            yield from walk_keys(child)


class DiagnosticsPrivacyTest(unittest.TestCase):
    def setUp(self) -> None:
        scratch = ROOT / "build"
        scratch.mkdir(exist_ok=True)
        self.base = Path(tempfile.mkdtemp(prefix="diag-selftest-", dir=scratch))
        self.library, self.logs, self.blob = build_fixture(self.base)
        self.out = self.base / "diagnostics.zip"

    def tearDown(self) -> None:
        shutil.rmtree(self.base, ignore_errors=True)

    def run_export(self, out: Path) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--library",
                str(self.library),
                "--logs",
                str(self.logs),
                "--out",
                str(out),
                "--force",
            ],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
        )

    def test_archive_contains_no_canary(self) -> None:
        result = self.run_export(self.out)
        self.assertEqual(result.returncode, 0, result.stderr)
        raw = all_archive_bytes(self.out)
        for canary in CANARIES:
            self.assertNotIn(canary.encode(), raw, f"canary leaked: {canary}")

    def test_metadata_and_privacy_contract(self) -> None:
        result = self.run_export(self.out)
        self.assertEqual(result.returncode, 0, result.stderr)
        with zipfile.ZipFile(self.out) as archive:
            self.assertIn("manifest.json", archive.namelist())
            self.assertIn("README.txt", archive.namelist())
            data = json.loads(archive.read("diagnostics.json"))
            manifest = json.loads(archive.read("manifest.json"))
        self.assertFalse(data["containsUserContent"])
        self.assertFalse(manifest["containsUserContent"])
        self.assertEqual(manifest["diagnosticsSha256"], __import__("hashlib").sha256(
            json.dumps(data, ensure_ascii=False, indent=2, sort_keys=False).encode("utf-8")
        ).hexdigest())
        self.assertEqual(data["formatVersion"], 1)
        self.assertEqual(data["library"]["schemaVersion"], 13)
        self.assertTrue(data["library"]["schemaMatches"])
        self.assertEqual(data["counts"]["captures"]["total"], 2)
        self.assertEqual(data["counts"]["captures"]["byState"]["committed"], 1)
        self.assertEqual(data["counts"]["jobs"]["byErrorCode"].get("<redacted>"), 1)
        self.assertEqual(data["counts"]["jobs"]["byErrorCode"].get("no_handler"), 1)
        self.assertEqual(data["index"]["semantic"]["totalChunks"], 1)
        self.assertEqual(data["index"]["semantic"]["embeddedChunks"], 1)
        self.assertTrue(data["index"]["semantic"]["ready"])
        self.assertNotIn("CANARY", json.dumps(data, ensure_ascii=False))

        keys = set(walk_keys(data))
        self.assertFalse(keys & BANNED_KEYS, f"banned keys leaked: {keys & BANNED_KEYS}")

        entries = data["logs"]["entries"]
        self.assertEqual(len(entries), 2)
        for entry in entries:
            self.assertTrue(set(entry).issubset(
                {
                    "observedAt", "trigger", "error", "processed", "succeeded",
                    "failed", "retrying", "backfilled", "recovered", "dueBacklog",
                    "waitingDependency", "dependencyFailed", "nextWakeup",
                }
            ))
        self.assertEqual(entries[1]["error"], "<redacted>")
        self.assertGreaterEqual(data["logs"]["malformedLines"], 1)
        self.assertGreaterEqual(data["logs"]["redactedValues"], 2)

    def test_json_output_is_also_metadata_only(self) -> None:
        out = self.base / "diagnostics.json"
        result = self.run_export(out)
        self.assertEqual(result.returncode, 0, result.stderr)
        raw = out.read_bytes()
        for canary in CANARIES:
            self.assertNotIn(canary.encode(), raw, f"canary leaked: {canary}")
        data = json.loads(raw)
        self.assertFalse(data["containsUserContent"])

    def test_missing_library_fails_cleanly(self) -> None:
        result = subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--library",
                str(self.base / "missing.sqlite"),
                "--out",
                str(self.base / "x.zip"),
            ],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("library not found", result.stderr)


def run_self_test() -> int:
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(DiagnosticsPrivacyTest)
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    return 0 if result.wasSuccessful() else 1


if __name__ == "__main__":
    sys.exit(run_self_test())
