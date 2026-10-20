#!/usr/bin/env python3
"""Canary test: prove the diagnostic bundle cannot contain user text or names.

The fixture puts distinctive strings into every user-owned column, a blob file,
short lowercase-alphanumeric secrets into `jobs.error_code` / the log `error`
field, and Chinese characters + a short secret into the library and log file
names. The exporter is then run through its real CLI; every byte of the
resulting archive (entry names *and* contents) is searched for those canaries.

This is deliberately stronger than checking a field allowlist in code: it
validates the produced artifact. It also parses the Rust sources to prove the
error-code whitelist has not drifted from `ErrorCode::wire()`.

Run directly or via the exporter's ``--self-test``. All scratch files live under
the repository's ignored ``build/`` directory (never /tmp).
"""

from __future__ import annotations

import hashlib
import json
import re
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time
import unittest
import zipfile
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = Path(__file__).resolve().with_name("export_diagnostics.py")
ERROR_RS = ROOT / "crates" / "diary_core" / "src" / "error.rs"
JOBS_RS = ROOT / "crates" / "diary_core" / "src" / "jobs.rs"
MODEL_RS = ROOT / "crates" / "diary_core" / "src" / "model.rs"
EMBEDDING_RS = ROOT / "crates" / "diary_core" / "src" / "embedding.rs"
CHUNKER_RS = ROOT / "crates" / "diary_core" / "src" / "chunker.rs"
SEARCH_RS = ROOT / "crates" / "diary_core" / "src" / "search.rs"
CORE_WAKEUP_DART = ROOT / "apps" / "diary_app" / "lib" / "src" / "core_wakeup.dart"
API_VALUES_DART = ROOT / "packages" / "diary_api" / "lib" / "src" / "values.dart"
NAPI_SHIM_CPP = (
    ROOT / "apps" / "ohos-diary-core" / "entry" / "src" / "main" / "cpp" / "napi_shim.cpp"
)

CANARIES = [
    "CANARY_DRAFT_7f3a9c",
    "CANARY_SOURCE_2b8d1e",
    "CANARY_DIARY_5a6f0c",
    "CANARY_CHUNK_9d4e2a",
    "CANARY_ASSET_1c7b5d",
    "CANARY_LOG_8e2f4a",
    "CANARY_BLOB_3a9c6e",
    "CANARY_JOB_6b1d8f",
    # Canaries for columns that were previously copied verbatim:
    "CANARY_STATE_4c1e8a",      # captures.state
    "CANARY_STATE_7d2f9b",      # jobs.state
    "CANARY_MODEL_2f6a0c@abc123",  # chunk_vectors.model_version
    "CANARY_CHUNKER_8b3d5e",    # text_chunks.chunker_version
    "CANARY_LOGTIME_6e4a1d",    # log expectedAt/completedAt must be ISO
]

# Short lowercase alphanumerics are exactly the shape a "looks like a machine
# code" regex accepts. They must still be redacted.
SHORT_SECRETS = ["password123", "secret42", "mytoken1"]

CHINESE_NAME = "体检报告"
LIBRARY_FILENAME = f"{CHINESE_NAME}-{SHORT_SECRETS[0]}.sqlite"
LOG_FILENAME = f"{CHINESE_NAME}-{SHORT_SECRETS[1]}.jsonl"

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

# `ErrorCode::wire()` arms only: `Self::Variant => "wire_value",`.
WIRE_ARM_RE = re.compile(r'Self::\w+ => "([a-z0-9_]+)"')


def build_fixture(base: Path) -> tuple[Path, Path, Path]:
    library = base / LIBRARY_FILENAME
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
            # `captures.state` is plain TEXT in schema.rs: a poisoned/edited
            # library can put arbitrary text there. It must be redacted.
            ("cap_3", CANARIES[8], ""),
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
    # Real on-disk format: `MODEL_ID@sha256[:12]` (embedding.rs). The healthy
    # fixture must keep it so the `modelVersionMatchesExpected == true`
    # positive case exists; a canary here made that branch untestable.
    conn.execute(
        "INSERT INTO chunk_vectors VALUES('chunk_1', 1, 'bge-small-zh-v1.5-ours@abc123', 512, X'00000000')"
    )
    conn.execute("INSERT INTO index_meta VALUES(1, 1, NULL)")
    conn.execute("INSERT INTO blobs VALUES('sha_1','blobs/sha_1',11)")
    conn.execute(
        "INSERT INTO assets VALUES('asset_1','sha_1',?,11)",
        (f"{CHINESE_NAME}-{SHORT_SECRETS[2]}-{CANARIES[4]}.pdf",),
    )
    # job_1 / job_5 use real machine codes; the others are short secrets or a
    # free-form canary and must all collapse into <redacted>.
    conn.execute("INSERT INTO jobs VALUES('job_1','queued','no_handler')")
    conn.execute("INSERT INTO jobs VALUES('job_2','failed',?)", (CANARIES[7],))
    conn.execute("INSERT INTO jobs VALUES('job_3','failed',?)", (SHORT_SECRETS[0],))
    conn.execute("INSERT INTO jobs VALUES('job_4','failed',?)", (SHORT_SECRETS[1],))
    conn.execute("INSERT INTO jobs VALUES('job_5','failed','invalid_argument')")
    # `jobs.state` is plain TEXT too; the canary state must be grouped as
    # <redacted> instead of appearing in counts.jobs.byState.
    conn.execute("INSERT INTO jobs VALUES('job_6',?,'no_handler')", (CANARIES[9],))
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

    logs = base / LOG_FILENAME
    logs.write_text(
        "\n".join(
            [
                json.dumps(
                    {
                        "observedAt": "2026-10-18T08:00:00Z",
                        "trigger": "wake",
                        # Both are written by core_wakeup.dart on every wake;
                        # dropping them made "the wake ran on time" uncheckable.
                        "expectedAt": "2026-10-18T07:59:30Z",
                        "completedAt": "2026-10-18T08:00:01Z",
                        "processed": 2,
                        "succeeded": 1,
                        "failed": 1,
                        "dueBacklog": 0,
                        # A real code must survive; otherwise the bundle would
                        # lose the diagnostic signal the reviewer asked for.
                        "error": "invalid_argument",
                        "body": CANARIES[5],  # unknown key dropped
                        "message": CANARIES[5],  # free-form dropped
                    },
                    ensure_ascii=False,
                ),
                json.dumps(
                    {
                        "observedAt": "2026-10-18T08:01:00Z",
                        "trigger": "wake",
                        "error": SHORT_SECRETS[2],  # short secret -> <redacted>
                    },
                    ensure_ascii=False,
                ),
                json.dumps(
                    {
                        "observedAt": "2026-10-18T08:02:00Z",
                        "trigger": "wake",
                        "error": CANARIES[5],  # free-form -> <redacted>
                    },
                    ensure_ascii=False,
                ),
                json.dumps(
                    {
                        "observedAt": "2026-10-18T08:03:00Z",
                        # Unknown trigger (even one shaped like a code) is
                        # dropped by the closed trigger vocabulary.
                        "trigger": SHORT_SECRETS[0],
                    },
                    ensure_ascii=False,
                ),
                json.dumps(
                    {
                        # Timestamp fields accept ISO only: a canary (or any
                        # free-form text) is redacted, not echoed.
                        "expectedAt": CANARIES[12],
                        "completedAt": CANARIES[12],
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


def parse_error_rs_wire_codes() -> set[str]:
    text = ERROR_RS.read_text(encoding="utf-8")
    body = re.search(
        r"pub fn wire\(self\) -> &'static str \{(.*?)\n    \}", text, re.DOTALL
    )
    if body is None:
        raise AssertionError("could not find ErrorCode::wire() in error.rs")
    return set(WIRE_ARM_RE.findall(body.group(1)))


def parse_model_wire_values(enum_name: str) -> set[str]:
    """Parse one `impl <Enum> { pub fn wire() }` block out of model.rs."""
    text = MODEL_RS.read_text(encoding="utf-8")
    impl = re.search(rf"impl {enum_name} \{{(.*?)\n\}}", text, re.DOTALL)
    if impl is None:
        raise AssertionError(f"could not find impl {enum_name} in model.rs")
    body = re.search(
        r"pub fn wire\(self\) -> &'static str \{(.*?)\n    \}",
        impl.group(1),
        re.DOTALL,
    )
    if body is None:
        raise AssertionError(f"could not find {enum_name}::wire() in model.rs")
    return set(WIRE_ARM_RE.findall(body.group(1)))


def parse_rust_str_const(path: Path, name: str) -> str:
    """Read `pub const <name>: &str = "…";` out of a Rust source file."""
    text = path.read_text(encoding="utf-8")
    match = re.search(rf'pub const {name}: &str = "([^"]+)";', text)
    if match is None:
        raise AssertionError(f"could not find {name} in {path.name}")
    return match.group(1)


def build_fixed_data(library: Path, logs: Path) -> dict[str, Any]:
    """Build the real bundle payload once, with a frozen `generatedAt`."""
    from export_diagnostics import build_diagnostics

    data = build_diagnostics(library, logs, 50, 13)
    data["generatedAt"] = "2026-10-18T14:07:13Z"
    return data


class DiagnosticsPrivacyTest(unittest.TestCase):
    def setUp(self) -> None:
        scratch = ROOT / "build"
        scratch.mkdir(exist_ok=True)
        self.base = Path(tempfile.mkdtemp(prefix="diag-selftest-", dir=scratch))
        self.library, self.logs, self.blob = build_fixture(self.base)
        self.out = self.base / "diagnostics.zip"

    def tearDown(self) -> None:
        shutil.rmtree(self.base, ignore_errors=True)

    def run_export(
        self, out: Path, library: Path | None = None
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--library",
                str(library or self.library),
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

    def assert_no_user_names_or_secrets(self, raw: bytes) -> None:
        for canary in CANARIES + SHORT_SECRETS + [CHINESE_NAME, LIBRARY_FILENAME, LOG_FILENAME]:
            self.assertNotIn(canary.encode("utf-8"), raw, f"leaked: {canary}")
        # A leaked absolute path would expose the user's directory name even if
        # the file name itself were hidden.
        self.assertNotIn(self.base.name.encode("utf-8"), raw, "leaked: base directory name")

    # ---------------------------------------------------------------- artifact

    def test_archive_contains_no_user_names_or_short_secrets(self) -> None:
        result = self.run_export(self.out)
        self.assertEqual(result.returncode, 0, result.stderr)
        raw = all_archive_bytes(self.out)
        self.assert_no_user_names_or_secrets(raw)
        # The useful part must still be there: a real machine code.
        self.assertIn(b"invalid_argument", raw)
        self.assertIn(b"no_handler", raw)

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
        self.assertEqual(manifest["diagnosticsSha256"], hashlib.sha256(
            json.dumps(data, ensure_ascii=False, indent=2, sort_keys=False).encode("utf-8")
        ).hexdigest())
        self.assertEqual(data["formatVersion"], 2)
        self.assertEqual(data["library"]["schemaVersion"], 13)
        self.assertTrue(data["library"]["schemaMatches"])

        # User-chosen names never appear; only a safe extension does.
        self.assertEqual(data["library"]["fileName"], "<hidden>")
        self.assertEqual(data["library"]["extension"], "sqlite")
        self.assertEqual(data["logs"]["source"], {"present": True, "extension": "jsonl"})

        self.assertEqual(data["counts"]["captures"]["total"], 3)
        self.assertEqual(data["counts"]["captures"]["byState"]["committed"], 1)
        self.assertEqual(data["counts"]["captures"]["byState"]["trashed"], 1)
        # Canary state -> grouped, never echoed.
        self.assertEqual(data["counts"]["captures"]["byState"]["<redacted>"], 1)
        self.assertNotIn(CANARIES[8], json.dumps(data, ensure_ascii=False))
        job_states = data["counts"]["jobs"]["byState"]
        self.assertEqual(job_states["queued"], 1)
        self.assertEqual(job_states["failed"], 4)
        self.assertEqual(job_states["<redacted>"], 1)
        self.assertNotIn(CANARIES[9], json.dumps(data, ensure_ascii=False))
        by_error = data["counts"]["jobs"]["byErrorCode"]
        self.assertEqual(by_error.get("<redacted>"), 3)
        self.assertEqual(by_error.get("no_handler"), 2)
        self.assertEqual(by_error.get("invalid_argument"), 1)
        self.assertNotIn(SHORT_SECRETS[0], json.dumps(data, ensure_ascii=False))
        self.assertNotIn(SHORT_SECRETS[1], json.dumps(data, ensure_ascii=False))
        self.assertEqual(data["index"]["semantic"]["totalChunks"], 1)
        self.assertEqual(data["index"]["semantic"]["embeddedChunks"], 1)
        self.assertTrue(data["index"]["semantic"]["ready"])
        # Only matchesExpected booleans; the raw model/chunker strings never
        # leave the exporter (they are user-writable TEXT columns). A healthy
        # library stores `MODEL_ID@sha256[:12]`, so both must be true.
        semantic = data["index"]["semantic"]
        self.assertTrue(semantic["modelVersionMatchesExpected"])
        self.assertTrue(semantic["chunkerVersionMatchesExpected"])
        self.assertNotIn("modelVersions", semantic)
        self.assertNotIn("chunkerVersion", semantic)
        # The hash half is user-specific (it identifies the local model file):
        # only the expected id constant may appear, never `MODEL_ID@hash`.
        self.assertNotIn("abc123", json.dumps(data, ensure_ascii=False))
        self.assertNotIn("CANARY", json.dumps(data, ensure_ascii=False))

        keys = set(walk_keys(data))
        self.assertFalse(keys & BANNED_KEYS, f"banned keys leaked: {keys & BANNED_KEYS}")

        entries = data["logs"]["entries"]
        self.assertEqual(len(entries), 4)
        for entry in entries:
            self.assertTrue(set(entry).issubset(
                {
                    "observedAt", "trigger", "error", "processed", "succeeded",
                    "failed", "retrying", "backfilled", "recovered", "dueBacklog",
                    "waitingDependency", "dependencyFailed", "nextWakeup",
                    "expectedAt", "completedAt",
                }
            ))
        # The two wakeup-path timestamps now survive instead of being dropped.
        self.assertEqual(entries[0]["expectedAt"], "2026-10-18T07:59:30Z")
        self.assertEqual(entries[0]["completedAt"], "2026-10-18T08:00:01Z")
        self.assertEqual(entries[0]["error"], "invalid_argument")
        self.assertEqual(entries[1]["error"], "<redacted>")
        self.assertEqual(entries[2]["error"], "<redacted>")
        self.assertNotIn("trigger", entries[3])
        self.assertGreaterEqual(data["logs"]["malformedLines"], 1)
        self.assertGreaterEqual(data["logs"]["redactedValues"], 7)

        # The bundle documents its own exact whitelist and name policy.
        privacy = data["privacy"]
        self.assertIn("invalid_argument", privacy["errorCodeWhitelist"])
        self.assertIn("storage_full", privacy["errorCodeWhitelist"])
        self.assertNotIn("password123", privacy["errorCodeWhitelist"])
        self.assertTrue(privacy["namePolicy"])
        self.assertTrue(any("whitelist" in rule for rule in privacy["logRules"]))
        self.assertEqual(
            privacy["stateWhitelists"]["captures"], ["committed", "draft", "trashed"]
        )
        self.assertIn("queued", privacy["stateWhitelists"]["jobs"])
        self.assertIn("ERR_DIARY_CALL", privacy["errorCodeWhitelist"])
        self.assertIn("plugin_denied", privacy["errorCodeWhitelist"])

    def test_json_output_is_also_metadata_only(self) -> None:
        out = self.base / "diagnostics.json"
        result = self.run_export(out)
        self.assertEqual(result.returncode, 0, result.stderr)
        raw = out.read_bytes()
        self.assert_no_user_names_or_secrets(raw)
        data = json.loads(raw)
        self.assertFalse(data["containsUserContent"])
        self.assertEqual(data["library"]["fileName"], "<hidden>")

    # ------------------------------------------------------------- whitelist

    def test_error_code_whitelist_matches_error_rs(self) -> None:
        from export_diagnostics import ERROR_CODE_WHITELIST

        rust_codes = parse_error_rs_wire_codes()
        self.assertTrue(rust_codes, "parsed no wire codes from error.rs")
        self.assertEqual(
            sorted(ERROR_CODE_WHITELIST),
            sorted(rust_codes),
            "diagnostics whitelist drifted from crates/diary_core/src/error.rs",
        )

    def test_job_only_whitelist_is_sourced_from_jobs_rs(self) -> None:
        from export_diagnostics import ERROR_CODE_WHITELIST, JOB_ONLY_ERROR_CODES

        jobs_text = JOBS_RS.read_text(encoding="utf-8")
        for code in sorted(JOB_ONLY_ERROR_CODES):
            # Most are Rust string literals; `interrupted` only appears inside
            # SQL string literals in this file.
            self.assertTrue(
                f'"{code}"' in jobs_text or f"'{code}'" in jobs_text,
                f"{code} is no longer present in jobs.rs",
            )
        self.assertFalse(
            JOB_ONLY_ERROR_CODES & ERROR_CODE_WHITELIST,
            "contract codes must not be duplicated into the job-only set",
        )

    def test_log_codes_and_triggers_are_sourced_from_the_app(self) -> None:
        from export_diagnostics import LOG_ONLY_ERROR_CODES, LOG_TRIGGERS

        dart = CORE_WAKEUP_DART.read_text(encoding="utf-8")
        for code in sorted(LOG_ONLY_ERROR_CODES):
            self.assertTrue(f"'{code}'" in dart, f"{code} is no longer in core_wakeup.dart")
        for trigger in sorted(LOG_TRIGGERS):
            self.assertTrue(
                f"'{trigger}'" in dart, f"trigger {trigger} is no longer in core_wakeup.dart"
            )

    def test_state_whitelists_match_model_rs(self) -> None:
        from export_diagnostics import CAPTURE_STATE_WHITELIST, JOB_STATE_WHITELIST

        self.assertEqual(
            sorted(CAPTURE_STATE_WHITELIST),
            sorted(parse_model_wire_values("CaptureState")),
            "diagnostics capture-state whitelist drifted from model.rs",
        )
        self.assertEqual(
            sorted(JOB_STATE_WHITELIST),
            sorted(parse_model_wire_values("JobState")),
            "diagnostics job-state whitelist drifted from model.rs",
        )

    def test_expected_versions_match_rust_constants(self) -> None:
        # The exporter repeats three Rust constants. If either side changes the
        # format/name (as `MODEL_ID@sha` did), the matchesExpected booleans go
        # silently false; pin them to the source of truth instead.
        from export_diagnostics import (
            EXPECTED_CHUNKER,
            EXPECTED_MODEL_ID,
            EXPECTED_TOKENIZER,
        )

        self.assertEqual(
            EXPECTED_MODEL_ID, parse_rust_str_const(EMBEDDING_RS, "MODEL_ID")
        )
        self.assertEqual(
            EXPECTED_CHUNKER, parse_rust_str_const(CHUNKER_RS, "CHUNKER_VERSION")
        )
        self.assertEqual(
            EXPECTED_TOKENIZER, parse_rust_str_const(SEARCH_RS, "TOKENIZER_VERSION")
        )

    def test_app_only_log_codes_match_dart_enum(self) -> None:
        from export_diagnostics import APP_ONLY_ERROR_CODES, LOG_ONLY_ERROR_CODES

        text = API_VALUES_DART.read_text(encoding="utf-8")
        enum = re.search(r"enum DiaryErrorCode \{(.*?)\n\}", text, re.DOTALL)
        self.assertIsNotNone(enum, "could not find enum DiaryErrorCode in values.dart")
        wires = set(re.findall(r"[a-zA-Z]+\(\'([a-z0-9_]+)\'\)", enum.group(1)))
        self.assertTrue(APP_ONLY_ERROR_CODES.issubset(wires))
        self.assertIn("unclassified_error", LOG_ONLY_ERROR_CODES)
        self.assertIn("unclassified_error", CORE_WAKEUP_DART.read_text(encoding="utf-8"))

    def test_napi_error_codes_are_sourced_from_shim(self) -> None:
        from export_diagnostics import NAPI_ERROR_CODES

        cpp = NAPI_SHIM_CPP.read_text(encoding="utf-8")
        for code in sorted(NAPI_ERROR_CODES):
            self.assertIn(f'"{code}"', cpp, f"{code} is no longer in napi_shim.cpp")

    def test_zip_members_use_utc_fixed_time_and_are_deterministic(self) -> None:
        """Same payload, different local clock => identical zip bytes.

        Before the fix `zipfile` stamped every member with `time.localtime()`,
        so two exports from different timezones differed (and the member time
        minus `generatedAt` exposed the user's UTC offset).

        `time.tzset` is POSIX-only (Windows has no such attribute) and on
        Windows `TZ` does not affect `localtime` anyway, so a `tzset`-based
        probe is either a hard ERROR or a fake green there. Poison
        `time.localtime` itself instead: no platform API, and any regression
        that starts stamping members from the local clock changes the bytes.
        """
        from export_diagnostics import ZIP_MEMBER_DATE_TIME, write_bundle

        data = build_fixed_data(self.library, self.logs)
        first = self.base / "first.zip"
        second = self.base / "second.zip"
        write_bundle(data, first, True)
        real_localtime = time.localtime
        try:
            # A deliberately absurd local time; if any member picked it up,
            # the digest below would differ.
            time.localtime = lambda *_args, **_kwargs: time.struct_time(
                (2038, 1, 2, 3, 4, 5, 6, 7, -1)
            )
            write_bundle(data, second, True)
        finally:
            time.localtime = real_localtime

        self.assertEqual(
            hashlib.sha256(first.read_bytes()).hexdigest(),
            hashlib.sha256(second.read_bytes()).hexdigest(),
            "same data exported under a different local clock produced different bytes",
        )
        with zipfile.ZipFile(first) as archive:
            self.assertEqual(
                archive.namelist(),
                ["diagnostics.json", "README.txt", "manifest.json"],
            )
            for info in archive.infolist():
                self.assertEqual(info.date_time, ZIP_MEMBER_DATE_TIME)
                self.assertEqual(info.create_system, 3)
                self.assertEqual(info.external_attr >> 16, 0o600)

    def test_semantic_version_booleans_from_function(self) -> None:
        data = build_fixed_data(self.library, self.logs)
        semantic = data["index"]["semantic"]
        self.assertEqual(
            set(semantic),
            {
                "expectedModelId",
                "expectedChunkerVersion",
                "activeGeneration",
                "buildingGeneration",
                "totalChunks",
                "embeddedChunks",
                "modelVersionMatchesExpected",
                "chunkerVersionMatchesExpected",
                "ready",
            },
        )

    def test_model_and_chunker_version_canaries_stay_redacted(self) -> None:
        # `chunk_vectors.model_version` / `text_chunks.chunker_version` are
        # writable TEXT columns; a poisoned library must only yield `false`,
        # never echo the value itself.
        conn = sqlite3.connect(self.library)
        conn.execute("UPDATE chunk_vectors SET model_version = ?", (CANARIES[10],))
        conn.execute("UPDATE text_chunks SET chunker_version = ?", (CANARIES[11],))
        conn.commit()
        conn.close()

        result = self.run_export(self.out)
        self.assertEqual(result.returncode, 0, result.stderr)
        raw = all_archive_bytes(self.out)
        for canary in (CANARIES[10], CANARIES[11]):
            self.assertNotIn(canary.encode("utf-8"), raw, f"leaked: {canary}")
        with zipfile.ZipFile(self.out) as archive:
            semantic = json.loads(archive.read("diagnostics.json"))["index"]["semantic"]
        self.assertFalse(semantic["modelVersionMatchesExpected"])
        self.assertFalse(semantic["chunkerVersionMatchesExpected"])

    def test_no_effective_generation_reports_null_model_match(self) -> None:
        # Without an effective generation there is nothing to compare: the
        # field must be `null` ("unknown"), not `false` ("the model is wrong").
        mutations = [
            ("active_generation NULL", "UPDATE index_meta SET active_generation = NULL", 0),
            ("chunk_vectors table missing", "DROP TABLE chunk_vectors", None),
        ]
        for index, (label, sql, embedded) in enumerate(mutations):
            with self.subTest(label):
                conn = sqlite3.connect(self.library)
                conn.execute(sql)
                conn.commit()
                conn.close()
                out = self.base / f"empty-{index}.zip"
                result = self.run_export(out)
                self.assertEqual(result.returncode, 0, result.stderr)
                with zipfile.ZipFile(out) as archive:
                    semantic = json.loads(archive.read("diagnostics.json"))["index"]["semantic"]
                self.assertIsNone(semantic["modelVersionMatchesExpected"], label)
                self.assertFalse(semantic["ready"], label)
                self.assertEqual(semantic["embeddedChunks"], embedded, label)

    def test_unknown_short_code_is_redacted_by_function(self) -> None:
        from export_diagnostics import sanitize_log_entry

        for secret in SHORT_SECRETS + ["abc"]:
            entry, dropped = sanitize_log_entry({"error": secret})
            self.assertEqual(entry, {"error": "<redacted>"})
            self.assertEqual(dropped, 1)
        entry, dropped = sanitize_log_entry({"error": "storage_full"})
        self.assertEqual(entry, {"error": "storage_full"})
        self.assertEqual(dropped, 0)

    def test_extension_never_carries_the_name(self) -> None:
        from export_diagnostics import safe_extension

        self.assertEqual(safe_extension(f"{CHINESE_NAME}.pdf"), "pdf")
        self.assertEqual(safe_extension("backup.DIARYBAK"), "diarybak")
        self.assertEqual(safe_extension("report.password123"), "<unknown>")
        self.assertEqual(safe_extension("password123"), "<none>")

    def test_unknown_suffix_is_not_echoed_end_to_end(self) -> None:
        # A path like `...-mytoken1.numbers` must not smuggle the secret through
        # the extension: the suffix is not allowlisted, so only "<unknown>"
        # may be reported.
        tricky = self.base / f"{CHINESE_NAME}-{SHORT_SECRETS[2]}.numbers"
        shutil.copy2(self.library, tricky)
        out = self.base / "tricky.zip"
        result = self.run_export(out, library=tricky)
        self.assertEqual(result.returncode, 0, result.stderr)
        raw = all_archive_bytes(out)
        for needle in (CHINESE_NAME, SHORT_SECRETS[2], "numbers", tricky.name):
            self.assertNotIn(needle.encode("utf-8"), raw, f"leaked: {needle}")
        with zipfile.ZipFile(out) as archive:
            data = json.loads(archive.read("diagnostics.json"))
        self.assertEqual(data["library"]["fileName"], "<hidden>")
        self.assertEqual(data["library"]["extension"], "<unknown>")

    # --------------------------------------------------------------- failures

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
