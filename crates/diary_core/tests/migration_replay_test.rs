//! Replay a **real** old library against the current schema migration.
//!
//! The normal migration tests in this crate reconstruct old shapes by dropping
//! tables and resetting `PRAGMA user_version`. That catches SQL mistakes, but
//! it does not prove that bytes written by an actually shipped old build still
//! open. This ignored test closes that gap: point it at a *copy* of a real old
//! `library.sqlite` and it will run the same `Core::open` path users run.
//!
//! Run it through `tools/migration/replay_old_library.sh`, which makes the copy,
//! records hashes, and refuses to touch the source. Direct invocation:
//!
//! ```text
//! DIARY_REPLAY_LIBRARY=/safe/copy/library.sqlite \
//!   cargo test -p diary_core --test migration_replay_test -- --ignored --nocapture
//! ```
//!
//! The test is `#[ignore]` on purpose: CI has no real old library, and a
//! synthetic one would defeat the point.

use std::env;
use std::path::PathBuf;

use diary_core::{Core, SCHEMA_VERSION};

#[test]
#[ignore = "needs DIARY_REPLAY_LIBRARY pointing at a copy of a real old library"]
fn replay_real_old_library_through_current_migration() {
    let path = env::var("DIARY_REPLAY_LIBRARY")
        .expect("set DIARY_REPLAY_LIBRARY to a COPY of a real library.sqlite");
    let path = PathBuf::from(path);
    assert!(path.is_file(), "library does not exist: {}", path.display());
    assert_eq!(
        path.file_name().and_then(|name| name.to_str()),
        Some("library.sqlite"),
        "expected a library.sqlite file, got {}",
        path.display()
    );

    // Refuse the classic footgun: running the migration against the only copy.
    let guard = env::var("DIARY_REPLAY_ALLOW_IN_PLACE").ok();
    assert!(
        guard.as_deref() != Some("1"),
        "in-place replay is forbidden; the replay script must copy the library first"
    );

    let before = std::fs::metadata(&path)
        .map(|meta| meta.len())
        .unwrap_or_default();
    let core = Core::open(&path).expect("current core must open the old library");
    let schema = core.schema_version().expect("read schema version");
    assert_eq!(
        schema, SCHEMA_VERSION,
        "migration must land on the current schema"
    );

    let (assets, asset_bytes) = core.asset_stats().expect("asset stats");
    let index = core.index_status(None).expect("index status");
    let captures = core.count_captures().expect("capture count");
    let imports = core.count_imports_in_flight().expect("import count");
    let recordings = core.count_open_recordings().expect("recording count");
    let events = core.last_event_sequence().expect("event sequence");

    println!("REPLAY source={}", path.display());
    println!("REPLAY sourceBytesBefore={before}");
    println!("REPLAY schemaVersion={schema}");
    println!("REPLAY captures={captures}");
    println!("REPLAY assets={assets} assetBytes={asset_bytes}");
    println!("REPLAY importsInFlight={imports} openRecordings={recordings}");
    println!("REPLAY lastEventSequence={events}");
    println!(
        "REPLAY coverage={:?} keywordReady={} semanticReady={} totalChunks={} embeddedChunks={}",
        index.coverage,
        index.keyword_index_ready,
        index.semantic_index_ready,
        index.total_chunks,
        index.embedded_chunks
    );

    // The migration is additive for user data. Counts must never go negative or
    // disappear; the exact old values are compared by the replay script with
    // sqlite3 before and after.
    assert!(captures >= 0);
    assert!(assets >= 0);
    assert!(asset_bytes >= 0);
    assert!(index.total_captures >= 0);
    assert!(index.total_segments >= 0);
}
