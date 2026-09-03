//! The migrate -> regenerate -> compile loop, enforced.
//!
//! `src/models/`, `src/queries/` and `schema.snapshot.json` are generated
//! files that are *committed*, which is only safe if something notices when
//! they stop matching their sources. This test builds a throwaway database
//! from `schema.sql` and asks the generator the same question the CLI's
//! `--check` asks:
//!
//! ```text
//! cd examples
//! sqlite3 /tmp/blog.db < schema.sql
//! cargo run -p keelson-gen -- --config keelson.toml --url sqlite:///tmp/blog.db --check
//! ```
//!
//! It is deliberately the same call — [`keelson_gen::check`] — so this test
//! and the flag an application's own CI runs cannot disagree about what "out
//! of date" means. When it fails, the fix is to drop the `--check` and commit
//! the result.
//!
//! Nothing is written from here: `check` compares rendered contents in memory,
//! so a failing run cannot leave a half-regenerated tree behind.

use std::path::PathBuf;

use keelson_gen::{Config, Drift};

/// A database with `schema.sql` applied, in a temporary file. One per test:
/// the harness runs them in parallel, and a shared path is a shared schema
/// being applied twice.
fn temp_database(test: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "keelson-examples-fresh-{}-{test}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let conn = rusqlite::Connection::open(&path).expect("opening the temporary database");
    conn.execute_batch(&std::fs::read_to_string("schema.sql").expect("schema.sql"))
        .expect("applying schema.sql");
    path
}

fn assert_fresh(what: &str, drift: Vec<Drift>) {
    assert!(
        drift.is_empty(),
        "the committed {what} no longer match their sources:\n{}\n\n\
         Re-run keelson-gen (see this file's header).",
        drift
            .iter()
            .map(|d| format!("  {d}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn the_committed_models_and_queries_match_their_sources() {
    let db = temp_database("committed");
    let mut config = Config::load("keelson.toml").expect("keelson.toml");
    config.url = Some(format!("sqlite://{}", db.display()));

    // Against a live database, so the schema snapshot is checked too — it is
    // the one generated file that only a live run can prove stale.
    assert_fresh(
        "models",
        keelson_gen::check(&config).expect("checking the models"),
    );
    assert_fresh(
        "queries",
        keelson_gen::queries::check(&config).expect("checking the queries"),
    );

    let _ = std::fs::remove_file(&db);
}

#[test]
fn the_snapshot_alone_generates_what_the_database_does() {
    // The promise `snapshot` makes: a checkout with no database in reach
    // generates the same bytes. If this ever diverges, the offline lane is
    // quietly generating something nobody reviewed.
    let db = temp_database("offline");
    let mut live = Config::load("keelson.toml").expect("keelson.toml");
    live.url = Some(format!("sqlite://{}", db.display()));
    let offline = Config::load("keelson.toml").expect("keelson.toml");
    assert!(offline.url.is_none(), "keelson.toml must not carry a url");

    assert_eq!(
        keelson_gen::generate(&live).expect("generating from the database"),
        keelson_gen::generate(&offline).expect("generating from the snapshot"),
    );
    assert_eq!(
        keelson_gen::queries::generate(&live).expect("queries from the database"),
        keelson_gen::queries::generate(&offline).expect("queries from the snapshot"),
    );

    let _ = std::fs::remove_file(&db);
}
