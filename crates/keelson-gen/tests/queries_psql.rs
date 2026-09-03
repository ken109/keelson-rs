//! Layer 4, PostgreSQL: the inference acceptance test, plus determinism and
//! freshness of the checked-in `tests/generated/psql_queries` fixture.
//!
//! The acceptance test is the point of the file. For every query in
//! `tests/queries/psql/posts.sql` the expected output columns — **name, Rust
//! type, nullability and the rule that decided it** — are written out by hand
//! from the DDL and the SQL, and compared with what the analyser produced.
//! That the generated code then compiles and returns those types from a real
//! server is `generated_psql_queries.rs`'s job.
//!
//! To regenerate the fixture after changing the emitter:
//!
//! ```text
//! KEELSON_GEN_BLESS=1 cargo test -p keelson-gen --test queries_psql
//! ```

use std::path::{Path, PathBuf};

use keelson_gen::Config;
use keelson_gen::queries;
use keelson_gen::schema::{ColumnDef, ForeignKey, Schema, TableDef, TableKind};

fn manifest_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn col(name: &str, db_type: &str, nullable: bool) -> ColumnDef {
    ColumnDef {
        name: name.to_owned(),
        db_type: db_type.to_owned(),
        nullable,
        default: None,
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

fn table(name: &str, columns: Vec<ColumnDef>, pk: &[&str], fks: Vec<ForeignKey>) -> TableDef {
    TableDef {
        name: name.to_owned(),
        kind: TableKind::Table,
        columns,
        unique_keys: vec![],
        primary_key: pk.iter().map(|s| (*s).to_owned()).collect(),
        foreign_keys: fks,
    }
}

/// `tests/schema/psql.sql`, as `pg_catalog` reports it — the same IR
/// `generate_psql.rs` pins against the live server.
fn psql_ir() -> Schema {
    Schema {
        tables: vec![
            table(
                "comments",
                vec![
                    col("id", "integer", false),
                    col("post_id", "integer", false),
                    col("user_id", "integer", true),
                    col("body", "text", false),
                    col("created_at", "timestamp with time zone", false),
                ],
                &["id"],
                vec![fk("post_id", "posts", "id"), fk("user_id", "users", "id")],
            ),
            table(
                "post_tags",
                vec![
                    col("post_id", "integer", false),
                    col("tag_id", "integer", false),
                ],
                &["post_id", "tag_id"],
                vec![fk("post_id", "posts", "id"), fk("tag_id", "tags", "id")],
            ),
            table(
                "posts",
                vec![
                    col("id", "integer", false),
                    col("user_id", "integer", false),
                    col("title", "text", false),
                    col("status", "text", true),
                    col("views", "integer", false),
                    col("published_at", "timestamp with time zone", true),
                ],
                &["id"],
                vec![fk("user_id", "users", "id")],
            ),
            table(
                "tags",
                vec![col("id", "integer", false), col("name", "text", false)],
                &["id"],
                vec![],
            ),
            table(
                "users",
                vec![
                    col("id", "integer", false),
                    col("name", "text", false),
                    col("email", "text", true),
                    col("age", "integer", true),
                    col("is_active", "boolean", false),
                    col("created_at", "timestamp with time zone", false),
                ],
                &["id"],
                vec![],
            ),
        ],
    }
}

fn config() -> Config {
    Config::load(manifest_path("tests/fixtures/queries_psql.toml")).expect("fixture config")
}

fn generate() -> Vec<(String, String)> {
    queries::generate_from_schema(&psql_ir(), &config()).expect("generation")
}

fn analyses() -> Vec<queries::Analysis> {
    let file = queries::spec::load(&manifest_path("tests/queries/psql/posts.sql")).expect("load");
    queries::analyse(&psql_ir(), &config(), &file).expect("analysis")
}

fn analysis(name: &str) -> queries::Analysis {
    analyses()
        .into_iter()
        .find(|a| a.spec.name == name)
        .unwrap_or_else(|| panic!("no query named {name}"))
}

/// `(output name, Rust type, nullable, rule)` for one query.
fn shape(name: &str) -> Vec<(String, String, bool, &'static str)> {
    analysis(name)
        .outputs
        .iter()
        .map(|c| (c.name.clone(), c.rust_type.clone(), c.nullable, c.rule))
        .collect()
}

fn params(name: &str) -> Vec<(usize, String, String)> {
    analysis(name)
        .params
        .iter()
        .map(|p| (p.number, p.name.clone(), p.rust_type.clone()))
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

/// Rule N1: nothing but the DDL decides, and a placeholder takes its type from
/// the column it is compared with (P1) or from being a row count (P3).
#[test]
fn a_plain_select_types_every_column_from_the_ddl() {
    expect(
        shape("posts_for_user"),
        &[
            ("id", "i32", false, "N1"),
            ("title", "String", false, "N1"),
            ("status", "String", true, "N1"),
            ("views", "i32", false, "N1"),
            ("published_at", "chrono::DateTime<chrono::Utc>", true, "N1"),
        ],
    );
    assert_eq!(
        params("posts_for_user"),
        vec![
            (1, "user_id".to_owned(), "i32".to_owned()),
            (2, "limit".to_owned(), "i64".to_owned()),
        ]
    );
}

/// Rule N2, the hard one: `users.name` is `NOT NULL` in the DDL and still
/// comes back nullable, because the `LEFT JOIN` can find no row at all.
#[test]
fn a_left_joined_tables_not_null_column_is_nullable() {
    expect(
        shape("comments_with_author"),
        &[
            ("id", "i32", false, "N1"),
            ("body", "String", false, "N1"),
            ("author__id", "i32", true, "N2"),
            ("author__name", "String", true, "N2"),
            ("author__email", "String", true, "N2"),
        ],
    );

    // And the shape that follows from it: the whole side is one `Option`, and
    // inside it every field goes back to its own DDL nullability.
    let a = analysis("comments_with_author");
    let author: Vec<_> = a
        .outputs
        .iter()
        .filter(|c| matches!(&c.nesting, keelson_gen::queries::Nesting::ToOne(n) if n == "author"))
        .map(|c| (c.field.clone(), c.outer_join, c.inner_nullable))
        .collect();
    assert_eq!(
        author,
        vec![
            ("id".to_owned(), true, false),
            ("name".to_owned(), true, false),
            ("email".to_owned(), true, true),
        ]
    );
}

/// Rules N3, N4, N5 and N7 in one query: `count` never NULL, `max` and `sum`
/// nullable, `coalesce` not — and a `WHERE … IS NOT NULL` filter leaves
/// `email` an `Option<String>`, because a filter narrows rows, not types.
#[test]
fn the_aggregate_and_coalesce_rules() {
    expect(
        shape("user_stats"),
        &[
            ("id", "i32", false, "N1"),
            ("name", "String", false, "N1"),
            ("email", "String", true, "N1"),
            ("post_count", "i64", false, "N4"),
            ("best_views", "i32", true, "N5"),
            ("total_views", "i64", false, "N7"),
        ],
    );
}

/// Rules N9, N10, N11 and N13.
#[test]
fn the_expression_rules() {
    expect(
        shape("post_flags"),
        &[
            ("id", "i32", false, "N1"),
            // `IS NOT NULL` is a predicate, always defined.
            ("has_status", "bool", false, "N11"),
            // `views` is NOT NULL and a bound parameter is a value, so the
            // comparison cannot be NULL…
            ("is_popular", "bool", false, "N10"),
            // …while `status` is nullable, so this one can.
            ("is_published", "bool", true, "N10"),
            ("heat", "String", false, "N9"),
            ("maybe_heat", "String", true, "N9"),
            ("views_wide", "i64", false, "N13"),
            // PostgreSQL writes a cast target as its internal type name, and
            // `name` is one the analyser's own copy of the type table had
            // never listed — it inferred nothing here. Casts now go through
            // `typemap`, the table the model generator reads for columns.
            ("title_name", "String", false, "N13"),
        ],
    );
}

/// The to-many nested naming: `tags.name` is a nested group, and the column is
/// `NOT NULL` inside a row that exists (its nullability is the join's).
#[test]
fn a_dotted_output_name_is_a_to_many_nested_group() {
    use keelson_gen::queries::Nesting;
    let a = analysis("posts_with_tags");
    let last = a.outputs.last().expect("a column");
    assert_eq!(last.name, "tags.name");
    assert_eq!(last.nesting, Nesting::ToMany("tags".to_owned()));
    assert_eq!(last.field, "name");
    assert!(last.nullable && last.outer_join && !last.inner_nullable);
}

/// Rule N16 and the `-- column:` / `-- param:` annotations.
#[test]
fn the_annotations_settle_what_inference_will_not() {
    expect(
        shape("annotated"),
        &[
            ("id", "i32", false, "N1"),
            // `upper(...)` is inferrable, but the annotation is what fixes the
            // type here; the nullability annotation overrides rule N10.
            ("shouty", "String", false, "N16"),
        ],
    );
    assert_eq!(
        params("annotated"),
        vec![(1, "title_pattern".to_owned(), "String".to_owned())]
    );
}

/// Rule N14: a set operation is one row type, and a column nullable in any arm
/// is nullable in the result. Here both arms are `NOT NULL`, so it is not.
#[test]
fn a_set_operation_merges_its_arms_row_types() {
    expect(shape("titles_union"), &[("title", "String", false, "N1")]);
}

// ───────────────────────────── the two faces ─────────────────────────────────

/// Every clause span is the author's own bytes, and they add back up to the
/// statement.
#[test]
fn the_clause_spans_carve_up_the_original_text() {
    let file = queries::spec::load(&manifest_path("tests/queries/psql/posts.sql")).unwrap();
    let a = analysis("posts_for_user");
    let c = &a.clauses;
    let at = |s: Option<keelson_gen::queries::Span>| s.map(|s| s.of(&file.source).to_owned());
    assert_eq!(
        at(c.select_list).as_deref(),
        Some("p.id, p.title, p.status, p.views, p.published_at")
    );
    assert_eq!(at(c.from).as_deref(), Some("posts p"));
    assert_eq!(at(c.where_).as_deref(), Some("p.user_id = $1"));
    assert_eq!(at(c.order_by).as_deref(), Some("p.published_at DESC"));
    assert_eq!(at(c.limit).as_deref(), Some("$2"));
    assert!(c.group_by.is_none() && c.having.is_none() && c.offset.is_none());
    assert!(c.unsupported.is_none());
}

/// A query the mod face cannot serve honestly says so, with a reason — and
/// keeps its query face.
#[test]
fn a_set_operation_is_refused_a_mod_face_in_writing() {
    let a = analysis("titles_union");
    let why = a.clauses.unsupported.expect("a recorded refusal");
    assert!(why.contains("set operation"), "{why}");
}

/// The `FROM` span carries the joins with it — that is what lets the merged
/// statement stay flat instead of nesting.
#[test]
fn the_from_span_includes_the_joins() {
    let file = queries::spec::load(&manifest_path("tests/queries/psql/posts.sql")).unwrap();
    let a = analysis("comments_with_author");
    assert_eq!(
        a.clauses.from.unwrap().of(&file.source),
        "comments c\nLEFT JOIN users u ON u.id = c.user_id"
    );
}

// ─────────────────────────────── refusals ────────────────────────────────────

fn analyse_one(sql: &str) -> keelson_gen::Result<queries::Analysis> {
    let specs = queries::spec::parse(sql).expect("annotations");
    queries::psql::analyse(&psql_ir(), &config(), &specs[0], sql)
}

#[test]
fn an_uninferrable_parameter_names_the_annotation_that_would_fix_it() {
    let err = analyse_one("-- name: q :many\nSELECT id FROM users WHERE $1").expect_err("refusal");
    assert!(err.to_string().contains("-- param: $1"), "{err}");
}

#[test]
fn an_uninferrable_column_type_names_the_annotation_that_would_fix_it() {
    let err =
        analyse_one("-- name: q :many\nSELECT pg_typeof(id) AS t FROM users").expect_err("refusal");
    assert!(err.to_string().contains("-- column: t"), "{err}");
}

#[test]
fn an_ambiguous_column_is_refused_rather_than_guessed() {
    let err = analyse_one("-- name: q :many\nSELECT id FROM users, posts").expect_err("refusal");
    assert!(err.to_string().contains("ambiguous"), "{err}");
}

#[test]
fn a_column_that_is_not_in_the_schema_is_refused() {
    let err = analyse_one("-- name: q :many\nSELECT nope FROM users").expect_err("refusal");
    assert!(err.to_string().contains("no column `nope`"), "{err}");
}

#[test]
fn mysql_query_generation_is_refused_with_the_reason_recorded() {
    let mut config = config();
    config.dialect = keelson_gen::config::Dialect::Mysql;
    let file = queries::spec::load(&manifest_path("tests/queries/psql/posts.sql")).unwrap();
    let err = queries::analyse(&psql_ir(), &config, &file).expect_err("refusal");
    assert!(
        err.to_string().contains("no trustworthy static parse tree"),
        "{err}"
    );
}

// ───────────────────────── determinism and freshness ─────────────────────────

#[test]
fn the_same_schema_generates_byte_identical_output_twice() {
    assert_eq!(generate(), generate());
}

#[test]
fn the_output_matches_the_checked_in_fixture() {
    let files = generate();
    let dir = manifest_path("tests/generated/psql_queries");

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

/// `-- prefix:` replaces the default separators for one query: what would be
/// two flat fields becomes one nested to-one group.
#[test]
fn a_prefix_annotation_switches_the_nested_row_separator() {
    use keelson_gen::queries::Nesting;
    let a = analysis("comments_with_prefixed_author");
    let nesting: Vec<_> = a
        .outputs
        .iter()
        .map(|c| (c.name.clone(), c.nesting.clone(), c.field.clone()))
        .collect();
    assert_eq!(
        nesting,
        vec![
            ("id".to_owned(), Nesting::Flat, "id".to_owned()),
            (
                "author_id".to_owned(),
                Nesting::ToOne("author".to_owned()),
                "id".to_owned()
            ),
            (
                "author_name".to_owned(),
                Nesting::ToOne("author".to_owned()),
                "name".to_owned()
            ),
        ]
    );
}

/// A mutation is typed from its `RETURNING` list — empty for `:exec` — and its
/// parameters still come from the columns they are compared with.
#[test]
fn an_exec_statement_has_parameters_but_no_row_type() {
    let a = analysis("bump_views");
    assert!(a.outputs.is_empty());
    assert_eq!(
        params("bump_views"),
        vec![(1, "id".to_owned(), "i32".to_owned())]
    );
    let why = a.clauses.unsupported.expect("a recorded refusal");
    assert!(why.contains("only a SELECT"), "{why}");
}
