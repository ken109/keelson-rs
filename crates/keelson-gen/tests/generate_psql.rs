//! PostgreSQL generation: the offline lane emits from a hand-built
//! [`keelson_gen::schema::Schema`] IR of `tests/schema/psql.sql` (emission
//! and introspection are separate stages, so no server is needed to pin the
//! emitted code); the `live-docker` lane introspects the real containerised
//! PostgreSQL 17 and asserts it produces **exactly this IR** — together they
//! pin the whole pipeline without making every CI run need Docker.
//!
//! To regenerate the fixture after changing the emitter:
//!
//! ```text
//! KEELSON_GEN_BLESS=1 cargo test -p keelson-gen --test generate_psql
//! ```

use std::path::{Path, PathBuf};

use keelson_gen::Config;
use keelson_gen::schema::{ColumnDef, ForeignKey, Schema, TableDef, TableKind};

fn manifest_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn col(name: &str, db_type: &str, nullable: bool, default: Option<&str>) -> ColumnDef {
    ColumnDef {
        name: name.to_owned(),
        db_type: db_type.to_owned(),
        nullable,
        default: default.map(str::to_owned),
        autoincrement: false,
        comment: None,
    }
}

fn fk(column: &str, ref_table: &str, ref_column: &str) -> ForeignKey {
    ForeignKey {
        columns: vec![column.to_owned()],
        ref_table: ref_table.to_owned(),
        ref_columns: vec![ref_column.to_owned()],
    }
}

/// `tests/schema/psql.sql`, as `pg_catalog` reports it (`format_type`
/// spellings, `pg_get_expr` default texts).
fn psql_ir() -> Schema {
    Schema {
        tables: vec![
            TableDef {
                name: "comments".to_owned(),
                kind: TableKind::Table,
                columns: vec![
                    col("id", "integer", false, None),
                    col("post_id", "integer", false, None),
                    col("user_id", "integer", true, None),
                    col("body", "text", false, None),
                    col(
                        "created_at",
                        "timestamp with time zone",
                        false,
                        Some("now()"),
                    ),
                ],
                primary_key: vec!["id".to_owned()],
                foreign_keys: vec![fk("post_id", "posts", "id"), fk("user_id", "users", "id")],
            },
            TableDef {
                name: "post_tags".to_owned(),
                kind: TableKind::Table,
                columns: vec![
                    col("post_id", "integer", false, None),
                    col("tag_id", "integer", false, None),
                ],
                primary_key: vec!["post_id".to_owned(), "tag_id".to_owned()],
                foreign_keys: vec![fk("post_id", "posts", "id"), fk("tag_id", "tags", "id")],
            },
            TableDef {
                name: "posts".to_owned(),
                kind: TableKind::Table,
                columns: vec![
                    col("id", "integer", false, None),
                    col("user_id", "integer", false, None),
                    col("title", "text", false, None),
                    col("status", "text", true, None),
                    col("views", "integer", false, Some("0")),
                    col("published_at", "timestamp with time zone", true, None),
                ],
                primary_key: vec!["id".to_owned()],
                foreign_keys: vec![fk("user_id", "users", "id")],
            },
            TableDef {
                name: "tags".to_owned(),
                kind: TableKind::Table,
                columns: vec![
                    col("id", "integer", false, None),
                    col("name", "text", false, None),
                ],
                primary_key: vec!["id".to_owned()],
                foreign_keys: vec![],
            },
            TableDef {
                name: "users".to_owned(),
                kind: TableKind::Table,
                columns: vec![
                    col("id", "integer", false, None),
                    col("name", "text", false, None),
                    col("email", "text", true, None),
                    col("age", "integer", true, None),
                    col("is_active", "boolean", false, Some("true")),
                    col(
                        "created_at",
                        "timestamp with time zone",
                        false,
                        Some("now()"),
                    ),
                ],
                primary_key: vec!["id".to_owned()],
                foreign_keys: vec![],
            },
        ],
    }
}

fn generate() -> Vec<(String, String)> {
    let config = Config::load(manifest_path("tests/fixtures/psql.toml")).expect("fixture config");
    keelson_gen::generate_from_schema(&psql_ir(), &config).expect("generation")
}

#[test]
fn the_same_ir_generates_byte_identical_output_twice() {
    assert_eq!(generate(), generate());
}

#[test]
fn the_output_matches_the_checked_in_fixture() {
    let files = generate();
    let dir = manifest_path("tests/generated/psql");

    if std::env::var_os("KEELSON_GEN_BLESS").is_some() {
        keelson_gen::write_files(&dir, &files).expect("blessing the fixture");
        return;
    }

    for (name, contents) in &files {
        let checked_in = std::fs::read_to_string(dir.join(name))
            .unwrap_or_else(|e| panic!("{name}: reading the checked-in fixture: {e}"));
        assert_eq!(
            &checked_in, contents,
            "{name} drifted from the checked-in fixture; \
             regenerate with KEELSON_GEN_BLESS=1"
        );
    }
    let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
        .expect("fixture dir")
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".rs"))
        .collect();
    on_disk.sort();
    let mut expected: Vec<String> = files.iter().map(|(n, _)| n.clone()).collect();
    expected.sort();
    assert_eq!(on_disk, expected);
}

/// The live half: introspecting the real containerised PostgreSQL 17 (the
/// shared schema, loaded by keelson-sqlcheck's container) produces exactly
/// the IR the offline lane emits from.
#[cfg(feature = "live-docker")]
#[test]
fn live_introspection_equals_the_hand_built_ir() {
    let url = keelson_sqlcheck::live::psql_url().to_owned();
    let mut config = Config::load(manifest_path("tests/fixtures/psql.toml")).unwrap();
    config.url = Some(url);
    let schema = keelson_gen::introspect::introspect(&config).expect("live introspection");
    assert_eq!(schema, psql_ir());
}
