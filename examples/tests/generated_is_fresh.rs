//! The migrate -> regenerate -> compile loop, enforced.
//!
//! `src/models/` and `src/queries/` are generated files that are *committed*,
//! which is only safe if something notices when they stop matching their
//! sources. This test builds a throwaway database from `schema.sql`, runs the
//! generator over it and over `queries/blog.sql` exactly as `keelson.toml`
//! says, and compares the result with what is on disk.
//!
//! When it fails, the fix is to regenerate:
//!
//! ```text
//! cd examples
//! sqlite3 /tmp/blog.db < schema.sql
//! cargo run -p keelson-gen -- --config keelson.toml --url sqlite:///tmp/blog.db
//! ```
//!
//! Nothing is written from here: the generator's file *contents* are compared
//! in memory, so a failing run cannot leave a half-regenerated tree behind.

use std::path::{Path, PathBuf};

use keelson_gen::Config;

/// A database with `schema.sql` applied, in a temporary file.
fn temp_database() -> PathBuf {
    let path =
        std::env::temp_dir().join(format!("keelson-examples-fresh-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let conn = rusqlite::Connection::open(&path).expect("opening the temporary database");
    conn.execute_batch(&std::fs::read_to_string("schema.sql").expect("schema.sql"))
        .expect("applying schema.sql");
    path
}

fn compare(dir: &str, files: &[(String, String)]) {
    let dir = Path::new(dir);
    for (name, generated) in files {
        let committed = std::fs::read_to_string(dir.join(name)).unwrap_or_else(|e| {
            panic!(
                "{}: {e} — the generator writes this file; commit it",
                dir.join(name).display()
            )
        });
        assert_eq!(
            &committed,
            generated,
            "{} is out of date; re-run keelson-gen (see this file's header)",
            dir.join(name).display()
        );
    }

    let mut on_disk: Vec<String> = std::fs::read_dir(dir)
        .expect("the generated directory")
        .map(|e| {
            e.expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|n| n.ends_with(".rs"))
        .collect();
    on_disk.sort();
    let mut expected: Vec<String> = files.iter().map(|(n, _)| n.clone()).collect();
    expected.sort();
    assert_eq!(
        on_disk,
        expected,
        "{} holds files the generator no longer writes (or is missing one)",
        dir.display()
    );
}

#[test]
fn the_committed_models_and_queries_match_their_sources() {
    let db = temp_database();
    let mut config = Config::load("keelson.toml").expect("keelson.toml");
    config.url = Some(format!("sqlite://{}", db.display()));

    let schema = keelson_gen::introspect::introspect(&config).expect("introspection");
    compare(
        config.out.as_deref().expect("`out` in keelson.toml"),
        &keelson_gen::generate_from_schema(&schema, &config).expect("generating the models"),
    );
    compare(
        &config
            .queries
            .as_ref()
            .expect("`[queries]` in keelson.toml")
            .out,
        &keelson_gen::queries::generate_from_schema(&schema, &config)
            .expect("generating the queries"),
    );

    let _ = std::fs::remove_file(&db);
}
