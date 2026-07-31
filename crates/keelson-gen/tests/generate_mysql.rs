//! MySQL generation: the offline lane emits from a hand-built
//! [`keelson_gen::schema::Schema`] IR of `tests/schema/mysql.sql`, and the
//! `live-docker` lane introspects the real containerised MySQL 8.4 and
//! asserts it produces **exactly this IR** — the same two-lane arrangement
//! `generate_psql.rs` uses, so no CI run needs Docker to pin the emitted
//! code.
//!
//! To regenerate the fixture after changing the emitter:
//!
//! ```text
//! KEELSON_GEN_BLESS=1 cargo test -p keelson-gen --test generate_mysql
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

/// `tests/schema/mysql.sql`, as `information_schema` reports it:
/// `COLUMN_TYPE` spellings (so `tinyint(1)`, not `tinyint`), `COLUMN_DEFAULT`
/// texts, and the `UNIQUE` constraint on `tags.name`.
fn mysql_ir() -> Schema {
    Schema {
        tables: vec![
            TableDef {
                name: "comments".to_owned(),
                kind: TableKind::Table,
                columns: vec![
                    col("id", "int", false, None),
                    col("post_id", "int", false, None),
                    col("user_id", "int", true, None),
                    col("body", "text", false, None),
                    col("created_at", "datetime", false, Some("CURRENT_TIMESTAMP")),
                ],
                primary_key: vec!["id".to_owned()],
                foreign_keys: vec![fk("post_id", "posts", "id"), fk("user_id", "users", "id")],
                unique_keys: vec![],
            },
            // Half of the mutually referencing pair: `messages.thread_id →
            // threads` and `threads.first_message_id → messages`, which is
            // what makes the generated to-one `rel` fields a cycle.
            TableDef {
                name: "messages".to_owned(),
                kind: TableKind::Table,
                columns: vec![
                    col("id", "int", false, None),
                    col("thread_id", "int", false, None),
                    col("body", "text", false, None),
                ],
                primary_key: vec!["id".to_owned()],
                foreign_keys: vec![fk("thread_id", "threads", "id")],
                unique_keys: vec![],
            },
            // A view: no key, no foreign keys, and every column reported as
            // MySQL reports it. `IS_UPDATABLE` is the one flag MySQL computes
            // for a whole view, and the `live-docker` lane is what pins these
            // two `kind`s to what the server actually says.
            TableDef {
                name: "post_authors".to_owned(),
                kind: TableKind::UpdatableView,
                columns: vec![
                    col("post_id", "int", false, None),
                    col("title", "varchar(255)", false, None),
                    col("user_id", "int", false, None),
                    col("user_name", "varchar(255)", false, None),
                ],
                primary_key: vec![],
                foreign_keys: vec![],
                unique_keys: vec![],
            },
            TableDef {
                name: "post_tags".to_owned(),
                kind: TableKind::Table,
                columns: vec![
                    col("post_id", "int", false, None),
                    col("tag_id", "int", false, None),
                ],
                primary_key: vec!["post_id".to_owned(), "tag_id".to_owned()],
                foreign_keys: vec![fk("post_id", "posts", "id"), fk("tag_id", "tags", "id")],
                unique_keys: vec![],
            },
            TableDef {
                name: "posts".to_owned(),
                kind: TableKind::Table,
                columns: vec![
                    col("id", "int", false, None),
                    col("user_id", "int", false, None),
                    col("title", "varchar(255)", false, None),
                    col("status", "varchar(64)", true, None),
                    col("views", "int", false, Some("0")),
                    col("published_at", "datetime", true, None),
                ],
                primary_key: vec!["id".to_owned()],
                foreign_keys: vec![fk("user_id", "users", "id")],
                unique_keys: vec![],
            },
            TableDef {
                name: "tags".to_owned(),
                kind: TableKind::Table,
                columns: vec![
                    col("id", "int", false, None),
                    col("name", "varchar(255)", false, None),
                ],
                primary_key: vec!["id".to_owned()],
                foreign_keys: vec![],
                unique_keys: vec![vec!["name".to_owned()]],
            },
            // The other half of the pair. `first_message_id` is nullable
            // because a thread has to be insertable before the message that
            // opens it exists.
            TableDef {
                name: "threads".to_owned(),
                kind: TableKind::Table,
                columns: vec![
                    col("id", "int", false, None),
                    col("title", "varchar(255)", false, None),
                    col("first_message_id", "int", true, None),
                ],
                primary_key: vec!["id".to_owned()],
                foreign_keys: vec![fk("first_message_id", "messages", "id")],
                unique_keys: vec![],
            },
            TableDef {
                name: "user_emails".to_owned(),
                kind: TableKind::UpdatableView,
                columns: vec![
                    col("id", "int", false, None),
                    col("email", "varchar(255)", true, None),
                ],
                primary_key: vec![],
                foreign_keys: vec![],
                unique_keys: vec![],
            },
            TableDef {
                name: "users".to_owned(),
                kind: TableKind::Table,
                columns: vec![
                    col("id", "int", false, None),
                    col("name", "varchar(255)", false, None),
                    col("email", "varchar(255)", true, None),
                    col("age", "int", true, None),
                    col("is_active", "tinyint(1)", false, Some("1")),
                    col("created_at", "datetime", false, Some("CURRENT_TIMESTAMP")),
                ],
                primary_key: vec!["id".to_owned()],
                foreign_keys: vec![],
                unique_keys: vec![],
            },
        ],
    }
}

fn generate() -> Vec<(String, String)> {
    let config = Config::load(manifest_path("tests/fixtures/mysql.toml")).expect("fixture config");
    keelson_gen::generate_from_schema(&mysql_ir(), &config).expect("generation")
}

#[test]
fn the_same_ir_generates_byte_identical_output_twice() {
    assert_eq!(generate(), generate());
}

#[test]
fn the_output_matches_the_checked_in_fixture() {
    let files = generate();
    let dir = manifest_path("tests/generated/mysql");

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

/// The MySQL model's `INSERT` carries no `RETURNING` — the emitted text is
/// the proof, and the keyed read-back stands in its place.
#[test]
fn the_emitted_mysql_model_has_no_returning_and_reads_back_by_key() {
    let files = generate();
    let users = &files
        .iter()
        .find(|(n, _)| n == "users.rs")
        .expect("users.rs")
        .1;
    assert!(
        !users.contains("returning"),
        "MySQL has no RETURNING on any statement"
    );
    assert!(users.contains("pub fn by_pk("), "the keyed read-back");
    assert!(
        users.contains("last_insert_id"),
        "the auto-increment fallback"
    );
    // The generic ModelTable surface (whose `insert(…).one()` decodes the
    // INSERT's own rows) is not what `table()` hands out.
    assert!(
        users.contains("pub fn table() -> Users"),
        "table() returns the marker, not ModelTable"
    );
}

/// The live half: introspecting the real containerised MySQL 8.4 (the shared
/// schema, loaded by keelson-sqlcheck's container) produces exactly the IR
/// the offline lane emits from.
#[cfg(feature = "live-docker")]
#[test]
fn live_introspection_equals_the_hand_built_ir() {
    let url = keelson_sqlcheck::live::mysql_url().to_owned();
    let mut config = Config::load(manifest_path("tests/fixtures/mysql.toml")).unwrap();
    config.url = Some(url);
    let schema = keelson_gen::introspect::introspect(&config).expect("live introspection");
    assert_eq!(schema, mysql_ir());
}
