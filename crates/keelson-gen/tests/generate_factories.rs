//! Factory generation (`[output] factories = true`): the SQLite lane
//! introspects a temp database built from `fixtures/sqlite_schema.sql`, the
//! PostgreSQL lane emits from a hand-built IR, and both are pinned against
//! checked-in fixtures that `generated_sqlite_factories.rs` /
//! `generated_psql_factories.rs` compile and run.
//!
//! Separate fixtures from `tests/generated/{sqlite,psql}`, because factories
//! are **off by default** and those fixtures are what proves the default.
//!
//! To regenerate after changing the factory emitter:
//!
//! ```text
//! KEELSON_GEN_BLESS=1 cargo test -p keelson-gen --test generate_factories
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

/// The PostgreSQL rendition of the shared schema (the same IR
/// `generate_psql.rs` pins against the live server), with the `tags.name`
/// unique constraint the factory emitter keys a sequence on.
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
                unique_keys: vec![],
            },
            // The shared schema's two views. They matter here for what they
            // do *not* produce: no template of their own, and no `Parent` /
            // `with_new_…` field on the tables that relate to them.
            TableDef {
                name: "post_authors".to_owned(),
                kind: TableKind::View,
                columns: vec![
                    col("post_id", "integer", true, None),
                    col("title", "text", true, None),
                    col("user_id", "integer", true, None),
                    col("user_name", "text", true, None),
                ],
                primary_key: vec![],
                foreign_keys: vec![],
                unique_keys: vec![],
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
                unique_keys: vec![],
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
                unique_keys: vec![],
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
                unique_keys: vec![vec!["name".to_owned()]],
            },
            TableDef {
                name: "user_emails".to_owned(),
                kind: TableKind::UpdatableView,
                columns: vec![
                    col("id", "integer", true, None),
                    col("email", "text", true, None),
                ],
                primary_key: vec![],
                foreign_keys: vec![],
                unique_keys: vec![],
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
                unique_keys: vec![],
            },
        ],
    }
}

fn generate_sqlite() -> Vec<(String, String)> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let db_path = std::env::temp_dir().join(format!(
        "keelson-gen-fac-{}-{}.db",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&db_path);
    let conn = rusqlite::Connection::open(&db_path).expect("creating the fixture database");
    conn.execute_batch(include_str!("fixtures/sqlite_schema.sql"))
        .expect("applying the fixture DDL");
    drop(conn);

    let mut config = Config::load(manifest_path("tests/fixtures/sqlite_factories.toml"))
        .expect("fixture config");
    config.url = Some(format!("sqlite://{}", db_path.display()));
    let files = keelson_gen::generate(&config).expect("generation");
    let _ = std::fs::remove_file(&db_path);
    files
}

fn generate_psql() -> Vec<(String, String)> {
    let config =
        Config::load(manifest_path("tests/fixtures/psql_factories.toml")).expect("fixture config");
    keelson_gen::generate_from_schema(&psql_ir(), &config).expect("generation")
}

fn check_fixture(files: &[(String, String)], dir: &Path) {
    if std::env::var_os("KEELSON_GEN_BLESS").is_some() {
        std::fs::create_dir_all(dir).expect("fixture dir");
        keelson_gen::write_files(dir, files).expect("blessing the fixture");
        return;
    }
    for (name, contents) in files {
        let checked_in = std::fs::read_to_string(dir.join(name))
            .unwrap_or_else(|e| panic!("{name}: reading the checked-in fixture: {e}"));
        assert_eq!(
            &checked_in, contents,
            "{name} drifted from the checked-in fixture; \
             regenerate with KEELSON_GEN_BLESS=1"
        );
    }
    let mut on_disk: Vec<String> = std::fs::read_dir(dir)
        .expect("fixture dir")
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".rs"))
        .collect();
    on_disk.sort();
    let mut expected: Vec<String> = files.iter().map(|(n, _)| n.clone()).collect();
    expected.sort();
    assert_eq!(on_disk, expected);
}

#[test]
fn the_sqlite_factories_match_the_checked_in_fixture() {
    check_fixture(
        &generate_sqlite(),
        &manifest_path("tests/generated/sqlite_factories"),
    );
}

#[test]
fn the_psql_factories_match_the_checked_in_fixture() {
    check_fixture(
        &generate_psql(),
        &manifest_path("tests/generated/psql_factories"),
    );
}

#[test]
fn factories_are_off_by_default() {
    let config = Config::load(manifest_path("tests/fixtures/sqlite.toml")).unwrap();
    assert!(!config.output.factories);
    let files = keelson_gen::generate_from_schema(&psql_ir(), &{
        let mut c = Config::load(manifest_path("tests/fixtures/psql.toml")).unwrap();
        c.out = None;
        c
    })
    .unwrap();
    assert!(
        !files.iter().any(|(n, _)| n == "factories.rs"),
        "no factories.rs without the switch"
    );
    assert!(
        !files
            .iter()
            .any(|(n, c)| n == "mod.rs" && c.contains("pub mod factories")),
        "and no module line for it either"
    );
}

/// The shapes the factory spec fixes, read off the emitted text: the
/// template, the parent triple, the child mod, and the sequence on a unique
/// column that is not a primary key (`tags.name`).
#[test]
fn the_emitted_factories_carry_the_specs_shapes() {
    let files = generate_psql();
    let fac = &files
        .iter()
        .find(|(n, _)| n == "factories.rs")
        .expect("factories.rs")
        .1;

    assert!(fac.contains("pub struct UserTemplate"));
    assert!(fac.contains("pub struct CommentTemplate"));
    // The FK pair: required parent, optional parent.
    assert!(
        fac.contains("pub post: keelson_factory::Parent<Box<super::posts::PostTemplate>, i32>")
    );
    assert!(fac.contains(
        "pub user: keelson_factory::OptionalParent<Box<super::users::UserTemplate>, i32>"
    ));
    // The parent triple and the child mod.
    for f in [
        "pub fn post(",
        "pub fn post_id(",
        "pub fn for_post(",
        "pub fn with_new_post(",
        "pub fn with_new_comment(",
    ] {
        assert!(fac.contains(f), "missing {f}");
    }
    // Sequences: the primary key, and the non-key unique column as text.
    assert!(fac.contains("SEQ.next_i32()"));
    assert!(
        fac.contains(r#"format!("tag-{}", SEQ.next_i32())"#),
        "the unique `tags.name` is sequence-backed"
    );
    // Schema-defaulted columns stay out of the statement.
    assert!(fac.contains("is_active: self.is_active.resolve(f, |_| keelson_models::Set::Unset)"));
}
