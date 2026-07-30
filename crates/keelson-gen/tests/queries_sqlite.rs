//! Layer 4, SQLite: the same inference acceptance test as `queries_psql.rs`,
//! against a real database built from `tests/fixtures/sqlite_schema.sql`.
//!
//! The nullability rules are identical — they are properties of the SQL, not
//! of the engine — so the interesting differences here are the *types*: SQLite
//! integers are all `i64`, a comparison is an integer rather than a boolean,
//! and `sum` does not widen the way PostgreSQL's does. Every one of those is
//! asserted, so a silent drift towards the psql answers would fail.
//!
//! To regenerate the fixture after changing the emitter:
//!
//! ```text
//! KEELSON_GEN_BLESS=1 cargo test -p keelson-gen --test queries_sqlite
//! ```

use std::path::{Path, PathBuf};

use keelson_gen::Config;
use keelson_gen::queries;
use keelson_gen::schema::Schema;

fn manifest_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// A temp database built from the fixture DDL, introspected into the IR.
fn sqlite_ir() -> Schema {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let db_path = std::env::temp_dir().join(format!(
        "keelson-gen-queries-{}-{}.db",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&db_path);
    let conn = rusqlite::Connection::open(&db_path).expect("creating the fixture database");
    conn.execute_batch(include_str!("fixtures/sqlite_schema.sql"))
        .expect("applying the fixture DDL");
    drop(conn);

    let mut config = config();
    config.url = Some(format!("sqlite://{}", db_path.display()));
    let schema = keelson_gen::introspect::introspect(&config).expect("introspection");
    let _ = std::fs::remove_file(&db_path);
    schema
}

fn config() -> Config {
    Config::load(manifest_path("tests/fixtures/queries_sqlite.toml")).expect("fixture config")
}

fn generate() -> Vec<(String, String)> {
    queries::generate_from_schema(&sqlite_ir(), &config()).expect("generation")
}

fn analyses() -> Vec<queries::Analysis> {
    let file = queries::spec::load(&manifest_path("tests/queries/sqlite/posts.sql")).expect("load");
    queries::analyse(&sqlite_ir(), &config(), &file).expect("analysis")
}

fn analysis(name: &str) -> queries::Analysis {
    analyses()
        .into_iter()
        .find(|a| a.spec.name == name)
        .unwrap_or_else(|| panic!("no query named {name}"))
}

fn shape(name: &str) -> Vec<(String, String, bool, &'static str)> {
    analysis(name)
        .outputs
        .iter()
        .map(|c| (c.name.clone(), c.rust_type.clone(), c.nullable, c.rule))
        .collect()
}

#[track_caller]
fn expect(actual: Vec<(String, String, bool, &'static str)>, want: &[(&str, &str, bool, &str)]) {
    let want: Vec<(String, String, bool, &str)> = want
        .iter()
        .map(|(n, t, null, r)| ((*n).to_owned(), (*t).to_owned(), *null, *r))
        .collect();
    assert_eq!(actual, want);
}

// ───────────────────────── the inference acceptance test ─────────────────────

/// Rule N1, and SQLite's types: every integer column is `i64`, and
/// `published_at` is a `NaiveDateTime` only because the config says so.
#[test]
fn a_plain_select_types_every_column_from_the_ddl() {
    expect(
        shape("posts_for_user"),
        &[
            ("id", "i64", false, "N1"),
            ("title", "String", false, "N1"),
            ("status", "String", true, "N1"),
            ("views", "i64", false, "N1"),
            ("published_at", "chrono::NaiveDateTime", true, "N1"),
        ],
    );
    let params: Vec<_> = analysis("posts_for_user")
        .params
        .iter()
        .map(|p| (p.number, p.name.clone(), p.rust_type.clone()))
        .collect();
    assert_eq!(
        params,
        vec![
            (1, "user_id".to_owned(), "i64".to_owned()),
            (2, "limit".to_owned(), "i64".to_owned()),
        ]
    );
}

/// Rule N2 through a nullable foreign key.
#[test]
fn a_left_joined_tables_not_null_column_is_nullable() {
    expect(
        shape("comments_with_author"),
        &[
            ("id", "i64", false, "N1"),
            ("body", "String", false, "N1"),
            ("author__id", "i64", true, "N2"),
            ("author__name", "String", true, "N2"),
            ("author__email", "String", true, "N2"),
        ],
    );
}

/// Rules N3, N4, N5, N7 — and SQLite's own answer for `sum`, which does not
/// widen to a decimal the way PostgreSQL's does.
#[test]
fn the_aggregate_and_coalesce_rules() {
    expect(
        shape("user_stats"),
        &[
            ("id", "i64", false, "N1"),
            ("name", "String", false, "N1"),
            ("email", "String", true, "N1"),
            ("post_count", "i64", false, "N4"),
            ("best_views", "i64", true, "N5"),
            ("total_views", "i64", false, "N7"),
        ],
    );
}

/// Rules N9, N10, N11, N13 — with SQLite's integers where PostgreSQL has
/// booleans.
#[test]
fn the_expression_rules() {
    expect(
        shape("post_flags"),
        &[
            ("id", "i64", false, "N1"),
            ("has_status", "i64", false, "N11"),
            ("is_popular", "i64", false, "N10"),
            ("is_published", "i64", true, "N10"),
            ("heat", "String", false, "N9"),
            ("maybe_heat", "String", true, "N9"),
            ("views_text", "String", false, "N13"),
        ],
    );
}

#[test]
fn a_dotted_output_name_is_a_to_many_nested_group() {
    use keelson_gen::queries::Nesting;
    let a = analysis("posts_with_tags");
    let last = a.outputs.last().expect("a column");
    assert_eq!(last.name, "tags.name");
    assert_eq!(last.nesting, Nesting::ToMany("tags".to_owned()));
    assert!(last.nullable && last.outer_join && !last.inner_nullable);
}

#[test]
fn the_annotations_settle_what_inference_will_not() {
    expect(
        shape("annotated"),
        &[
            ("id", "i64", false, "N1"),
            ("shouty", "String", false, "N16"),
        ],
    );
}

#[test]
fn a_compound_select_merges_its_arms_row_types() {
    expect(shape("titles_union"), &[("title", "String", false, "N1")]);
    let why = analysis("titles_union")
        .clauses
        .unsupported
        .expect("a recorded refusal");
    assert!(why.contains("set operation"), "{why}");
}

/// The mod face's raw material: the clause spans, cut from the author's text.
#[test]
fn the_clause_spans_carve_up_the_original_text() {
    let file = queries::spec::load(&manifest_path("tests/queries/sqlite/posts.sql")).unwrap();
    let c = analysis("posts_for_user").clauses;
    let at = |s: Option<keelson_gen::queries::Span>| s.map(|s| s.of(&file.source).to_owned());
    assert_eq!(at(c.from).as_deref(), Some("posts p"));
    assert_eq!(at(c.where_).as_deref(), Some("p.user_id = ?1"));
    assert_eq!(at(c.order_by).as_deref(), Some("p.published_at DESC"));
    assert_eq!(at(c.limit).as_deref(), Some("?2"));
    assert!(c.unsupported.is_none());
}

// ───────────────────────── determinism and freshness ─────────────────────────

#[test]
fn the_same_schema_generates_byte_identical_output_twice() {
    assert_eq!(generate(), generate());
}

#[test]
fn the_output_matches_the_checked_in_fixture() {
    let files = generate();
    let dir = manifest_path("tests/generated/sqlite_queries");

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

/// The CLI generates Layer 4 on its own: a config carrying only `[queries]`
/// needs no model `out`, and the files land where it says.
#[test]
fn the_cli_generates_queries_without_a_models_out() {
    let scratch = std::env::temp_dir().join(format!("keelson-gen-q-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let sql_dir = scratch.join("queries");
    let out_dir = scratch.join("gen");
    std::fs::create_dir_all(&sql_dir).unwrap();
    std::fs::create_dir_all(&out_dir).unwrap();
    std::fs::write(
        sql_dir.join("users.sql"),
        "-- name: active_users :many\nSELECT id, name FROM users WHERE is_active = ?1;\n",
    )
    .unwrap();

    let db_path = scratch.join("schema.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(include_str!("fixtures/sqlite_schema.sql"))
        .unwrap();
    drop(conn);

    let config_path = scratch.join("keelson.toml");
    std::fs::write(
        &config_path,
        format!(
            "dialect = \"sqlite\"\n\n[queries]\ndir = {:?}\nout = {:?}\ninclude_prefix = \"../queries\"\n",
            sql_dir.display().to_string(),
            out_dir.display().to_string(),
        ),
    )
    .unwrap();

    let status = std::process::Command::new(env!("CARGO_BIN_EXE_keelson-gen"))
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "--url",
            &format!("sqlite://{}", db_path.display()),
        ])
        .status()
        .expect("running the keelson-gen binary");
    assert!(status.success());

    let generated = std::fs::read_to_string(out_dir.join("users.rs")).expect("users.rs written");
    assert!(generated.contains("include_str!(\"../queries/users.sql\")"));
    assert!(generated.contains("pub struct ActiveUsersRow"));
    assert!(generated.contains("pub fn active_users_mod("));
    assert!(out_dir.join("mod.rs").exists());

    let _ = std::fs::remove_dir_all(&scratch);
}
