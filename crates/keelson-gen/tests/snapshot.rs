//! The schema snapshot: the committed IR that lets a checkout with no
//! database generate, and lets a CI job answer `--check`.
//!
//! The property that matters is a single equality — *snapshot in, same bytes
//! out* — because the moment offline generation and live generation disagree,
//! the offline lane is producing code nobody reviewed. Everything else here is
//! about the failures a snapshot introduces that a live catalog cannot have: a
//! file from a different generator, from a different engine, or hand-edited.

use std::path::{Path, PathBuf};

use keelson_gen::Config;
use keelson_gen::config::Dialect;
use keelson_gen::schema::{SNAPSHOT_VERSION, Snapshot};

fn manifest_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// A temp database built from the fixture DDL, and a config pointed at it.
fn live_config() -> (PathBuf, Config) {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let db_path = std::env::temp_dir().join(format!(
        "keelson-gen-snapshot-{}-{}.db",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&db_path);
    let conn = rusqlite::Connection::open(&db_path).expect("creating the fixture database");
    conn.execute_batch(include_str!("fixtures/sqlite_schema.sql"))
        .expect("applying the fixture DDL");
    drop(conn);

    let mut config =
        Config::load(manifest_path("tests/fixtures/sqlite.toml")).expect("fixture config");
    config.url = Some(format!("sqlite://{}", db_path.display()));
    (db_path, config)
}

/// Where this test's snapshot file goes.
fn snapshot_path(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "keelson-gen-snapshot-{}-{name}.json",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    path
}

/// A throwaway `out` directory, for the tests that call `run` (the fixture
/// config has none: the other suites render in memory).
fn out_dir(name: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "keelson-gen-snapshot-out-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir.display().to_string()
}

#[test]
fn generating_from_a_snapshot_produces_what_the_database_produces() {
    let (db, mut config) = live_config();
    let from_database = keelson_gen::generate(&config).expect("generating from the database");

    let snapshot = snapshot_path("parity");
    config.snapshot = Some(snapshot.display().to_string());
    config.out = Some(out_dir("parity"));
    keelson_gen::run(&config).expect("a run that writes the snapshot");

    // The offline lane: no url at all, so the snapshot is the only source.
    config.url = None;
    config.out = None; // `run` needs it; `generate` does not.
    let from_snapshot = keelson_gen::generate(&config).expect("generating from the snapshot");

    assert_eq!(from_database, from_snapshot);
    let _ = std::fs::remove_file(&db);
}

#[test]
fn a_run_against_a_database_refreshes_the_snapshot_and_a_run_without_one_does_not() {
    let (db, mut config) = live_config();
    let snapshot = snapshot_path("refresh");
    config.snapshot = Some(snapshot.display().to_string());
    config.out = Some(out_dir("refresh"));

    let written = keelson_gen::run(&config).expect("the live run");
    assert!(
        written.contains(&snapshot),
        "a live run must report the snapshot among the files it wrote"
    );
    let after_live = std::fs::read_to_string(&snapshot).expect("the snapshot");

    // Hand-edit it, then run offline: nothing may overwrite the edit, because
    // a run that *read* the snapshot has learned no new truth to record.
    std::fs::write(&snapshot, &after_live).expect("rewriting");
    config.url = None;
    let written = keelson_gen::run(&config).expect("the offline run");
    assert!(!written.contains(&snapshot));
    assert_eq!(
        std::fs::read_to_string(&snapshot).expect("the snapshot"),
        after_live
    );

    let _ = std::fs::remove_file(&db);
}

#[test]
fn a_stale_snapshot_is_drift_when_checked_against_the_database() {
    // The one thing only a live `--check` can catch: the models are fine, the
    // snapshot is what nobody committed.
    let (db, mut config) = live_config();
    let snapshot = snapshot_path("stale");
    config.snapshot = Some(snapshot.display().to_string());
    config.out = Some(out_dir("stale"));
    keelson_gen::run(&config).expect("the live run");

    assert_eq!(keelson_gen::check(&config).expect("checking"), vec![]);

    let mut json = std::fs::read_to_string(&snapshot).expect("the snapshot");
    json = json.replace("\"nullable\": false", "\"nullable\": true");
    std::fs::write(&snapshot, json).expect("tampering");

    let drift = keelson_gen::check(&config).expect("checking");
    assert!(
        drift.iter().any(|d| d.path() == snapshot),
        "the snapshot must be reported: {drift:?}"
    );

    let _ = std::fs::remove_file(&db);
}

#[test]
fn a_snapshot_from_another_format_version_is_refused_by_version_not_by_field() {
    let path = snapshot_path("version");
    let (db, config) = live_config();
    let schema = keelson_gen::introspect::introspect(&config).expect("introspection");
    let json = Snapshot::new(Dialect::Sqlite, schema)
        .to_json()
        .expect("serialising");
    let bumped = json.replace(
        &format!("\"version\": {SNAPSHOT_VERSION}"),
        &format!("\"version\": {}", SNAPSHOT_VERSION + 1),
    );
    std::fs::write(&path, bumped).expect("writing");

    let err = Snapshot::load(&path)
        .expect_err("a future version must be refused")
        .to_string();
    assert!(
        err.contains(&format!("version {}", SNAPSHOT_VERSION + 1)),
        "the error must name the version it found, not the first field that failed: {err}"
    );

    let _ = std::fs::remove_file(&db);
}

#[test]
fn a_snapshot_from_another_engine_is_refused_before_the_types_are() {
    // A PostgreSQL snapshot against a sqlite config would otherwise surface as
    // a pile of unmapped types, naming every column but not the cause.
    let path = snapshot_path("dialect");
    let (db, config) = live_config();
    let schema = keelson_gen::introspect::introspect(&config).expect("introspection");
    Snapshot::new(Dialect::Psql, schema)
        .save(&path)
        .expect("writing");

    let err = Snapshot::load(&path)
        .expect("loading")
        .schema_for(Dialect::Sqlite, &path)
        .expect_err("a mismatched dialect must be refused")
        .to_string();
    assert!(err.contains("psql") && err.contains("sqlite"), "{err}");

    let _ = std::fs::remove_file(&db);
}

#[test]
fn a_file_that_is_not_a_snapshot_says_so() {
    let path = snapshot_path("garbage");
    std::fs::write(&path, "{ not json at all").expect("writing");
    let err = Snapshot::load(&path)
        .expect_err("must be refused")
        .to_string();
    assert!(err.contains("not a keelson-gen schema snapshot"), "{err}");
}

#[test]
fn no_url_and_no_snapshot_names_both_ways_out() {
    let (db, mut config) = live_config();
    config.url = None;
    let err = keelson_gen::generate(&config)
        .expect_err("nothing to introspect")
        .to_string();
    assert!(err.contains("url") && err.contains("snapshot"), "{err}");
    let _ = std::fs::remove_file(&db);
}
