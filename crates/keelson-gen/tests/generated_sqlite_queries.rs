//! The strongest form of the Layer 4 tests: the checked-in generated SQLite
//! query modules (`tests/generated/sqlite_queries`, pinned byte-for-byte by
//! `queries_sqlite.rs`) are **compiled here and run against real SQLite**.
//!
//! Two things are being proved, and only a real engine can prove either:
//!
//! 1. **The inferred types are the engine's types.** The hand-written
//!    expectations in `queries_sqlite.rs` are asserted against the SQL; here
//!    rows with NULL-producing shapes are inserted and read back through the
//!    generated functions, so an `Option<T>` that should hold a `None` does.
//! 2. **The two faces agree.** The same query is run as a query object and
//!    applied as a mod to a host statement, and the host's SQL is judged: one
//!    flat statement, no sub-select.

// `pub` throughout the generated files because that is what the generator
// emits into an application's crate; this test binary has no external readers.
#[allow(unreachable_pub, dead_code)]
// The fixture is prettyplease-formatted by the generator; rustfmt must not
// rewrite it, or the byte-identical freshness test would fight `cargo fmt`.
#[rustfmt::skip]
#[path = "generated/sqlite_queries/mod.rs"]
mod queries;

use keelson_core::Query as _;
use keelson_exec::ExecError;
use keelson_sqlite::{Chain as _, arg, quote, select};
use keelson_sqlx::sqlite::Pool;

use queries::posts;

/// A fresh database from the same fixture DDL generation ran against.
async fn db() -> Pool {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "keelson-gen-queries-behavior-{}-{}.db",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&path);
    let conn = rusqlite::Connection::open(&path).expect("creating the database");
    conn.execute_batch(include_str!("fixtures/sqlite_schema.sql"))
        .expect("applying the fixture DDL");
    conn.execute_batch(SEED).expect("seeding");
    drop(conn);
    Pool::connect(&format!("sqlite://{}", path.display()))
        .await
        .expect("opening the SQLite database")
}

/// Two users, three posts, three comments — one of them **authorless**, which
/// is the row that makes the `LEFT JOIN` produce NULLs for `users.name`
/// despite its `NOT NULL` declaration.
const SEED: &str = r#"
INSERT INTO users (id, name, email, age) VALUES
    (1, 'Stephen', 'stephen@example.com', 41),
    (2, 'Ada',     NULL,                  36);

INSERT INTO posts (id, user_id, title, status, views, published_at) VALUES
    (1, 1, 'keel laid',  'published', 300, '2026-01-01 10:00:00'),
    (2, 1, 'second',     NULL,          7, NULL),
    (3, 2, 'notes',      'draft',      99, '2026-02-02 12:00:00');

INSERT INTO comments (id, post_id, user_id, body) VALUES
    (1, 1, 1,    'first'),
    (2, 1, NULL, 'from a stranger'),
    (3, 1, 2,    'third');

INSERT INTO tags (id, name) VALUES (1, 'rust'), (2, 'sql');
INSERT INTO post_tags (post_id, tag_id) VALUES (1, 1), (1, 2);
"#;

// ───────────────────── the inference, against the engine ─────────────────────

/// Rule N1 end to end: a nullable column comes back `None` and a `NOT NULL`
/// one is a bare value — and the `LIMIT` parameter really limits.
#[tokio::test]
async fn a_plain_select_returns_the_types_the_ddl_justifies() {
    let db = db().await;
    let rows = posts::posts_for_user(&db, (1i64, 10i64)).await.unwrap();
    assert_eq!(rows.len(), 2);

    let published = &rows[0];
    assert_eq!(published.title, "keel laid");
    assert_eq!(published.status.as_deref(), Some("published"));
    assert_eq!(published.views, 300);
    assert!(published.published_at.is_some());

    // The row whose nullable columns are actually NULL.
    let draft = &rows[1];
    assert_eq!(draft.status, None);
    assert_eq!(draft.published_at, None);

    let one = posts::posts_for_user(&db, (1i64, 1i64)).await.unwrap();
    assert_eq!(one.len(), 1, "the LIMIT parameter is bound, not inlined");
}

/// **Rule N2, proved by the engine.** `users.name` is `NOT NULL` in the DDL;
/// the authorless comment still comes back with no author at all, which is
/// exactly why the generated field is an `Option`.
#[tokio::test]
async fn a_left_joined_not_null_column_really_does_come_back_null() {
    let db = db().await;
    let rows = posts::comments_with_author(&db, 1i64).await.unwrap();
    assert_eq!(rows.len(), 3);

    let first = rows[0].author.as_ref().expect("comment 1 has an author");
    assert_eq!(first.name, "Stephen");
    assert_eq!(first.email.as_deref(), Some("stephen@example.com"));

    assert_eq!(
        rows[1].author, None,
        "the outer join found no row, so the whole nested side is None"
    );

    let third = rows[2].author.as_ref().expect("comment 3 has an author");
    assert_eq!(third.name, "Ada");
    assert_eq!(
        third.email, None,
        "present author, absent email — the field keeps its own DDL nullability"
    );
}

/// Rules N4, N5, N7 against the engine: `count` is `0` (not NULL) for a user
/// with no posts, `max` *is* NULL, and `coalesce` is not.
#[tokio::test]
async fn the_aggregate_rules_hold_over_an_empty_group() {
    use keelson_exec::Execute as _;

    let db = db().await;
    // A third user with no posts at all, and an email so the WHERE keeps them.
    keelson_sqlite::insert((
        keelson_sqlite::insert::into(quote("users")).columns(["id", "name", "email"]),
        keelson_sqlite::insert::values((arg(3i64), arg("Grace"), arg("grace@example.com"))),
    ))
    .execute(&db)
    .await
    .unwrap();

    let rows = posts::user_stats(&db, ()).await.unwrap();
    // Ada has no email, so the `IS NOT NULL` filter drops her.
    assert_eq!(
        rows.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![1, 3],
        "a WHERE narrows the rows"
    );

    let stephen = &rows[0];
    assert_eq!(stephen.post_count, 2);
    assert_eq!(stephen.best_views, Some(300));
    assert_eq!(stephen.total_views, 307);

    let grace = &rows[1];
    assert_eq!(
        grace.post_count, 0,
        "COUNT over an empty group is 0, not NULL"
    );
    assert_eq!(grace.best_views, None, "MAX over an empty group is NULL");
    assert_eq!(grace.total_views, 0, "COALESCE turns that into a value");
    assert_eq!(
        grace.email.as_deref(),
        Some("grace@example.com"),
        "still an Option<String>: the filter changed no type"
    );
}

/// Rules N9, N10, N11, N13 against the engine — in particular that
/// `p.status = 'published'` really is NULL for the row whose status is NULL.
#[tokio::test]
async fn the_expression_rules_hold_against_the_engine() {
    let db = db().await;
    let rows = posts::post_flags(&db, 100i64).await.unwrap();
    assert_eq!(rows.len(), 3);

    assert_eq!(rows[0].has_status, 1);
    assert_eq!(rows[0].is_popular, 1);
    assert_eq!(rows[0].is_published, Some(1));
    assert_eq!(rows[0].heat, "hot");
    assert_eq!(rows[0].maybe_heat.as_deref(), Some("hot"));
    assert_eq!(rows[0].views_text, "300");

    // The NULL-status post: the comparison is NULL, the IS NOT NULL is not,
    // and the ELSE-less CASE falls through to NULL.
    assert_eq!(rows[1].has_status, 0);
    assert_eq!(rows[1].is_published, None);
    assert_eq!(rows[1].heat, "cold");
    assert_eq!(rows[1].maybe_heat, None);
}

/// The to-many nested shape: three flat result rows fold into two posts, one
/// carrying two tags and one carrying none.
#[tokio::test]
async fn a_to_many_group_folds_the_flat_rows() {
    let db = db().await;
    let rows = posts::posts_with_tags(&db, ()).await.unwrap();
    assert_eq!(rows.len(), 3, "one row per post, not per (post, tag) pair");
    assert_eq!(
        rows[0]
            .tags
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        vec!["rust", "sql"]
    );
    assert!(
        rows[1].tags.is_empty(),
        "no tags means an empty Vec, not a None"
    );
}

/// `:one` means one, through the execution layer's own contract.
#[tokio::test]
async fn one_means_one() {
    let db = db().await;
    let u = posts::user_by_id(&db, 1i64).await.unwrap();
    assert_eq!(u.name, "Stephen");
    assert_eq!(u.age, Some(41));
    assert!(u.is_active);

    let missing = posts::user_by_id(&db, 99i64).await;
    assert!(matches!(missing, Err(ExecError::RowNotFound)));
}

/// A compound select still runs as a query, even though it has no mod face.
#[tokio::test]
async fn a_compound_select_runs_as_a_query() {
    let db = db().await;
    let mut titles: Vec<String> = posts::titles_union(&db, ())
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.title)
        .collect();
    titles.sort();
    assert_eq!(titles, vec!["keel laid", "notes", "rust", "second", "sql"]);
}

/// The annotations, end to end.
#[tokio::test]
async fn the_annotated_query_runs_with_its_annotated_parameter() {
    let db = db().await;
    let rows = posts::annotated(&db, "%e%".to_owned()).await.unwrap();
    assert_eq!(
        rows.iter().map(|r| r.shouty.as_str()).collect::<Vec<_>>(),
        vec!["KEEL LAID", "SECOND", "NOTES"]
    );
}

// ────────────────────────────── the two faces ────────────────────────────────

/// **Face 1.** The query object renders the file's own SQL, with the
/// placeholders re-numbered by the writer rather than copied — so it is a
/// `Query` like any other, judged by SQLite's own grammar.
#[test]
fn the_query_face_is_the_files_own_sql() {
    let q = posts::posts_for_user_query((1i64, 10i64));
    let (sql, args) = q.build().unwrap();
    keelson_sqlcheck::assert_sql(
        keelson_sqlcheck::Dialect::Sqlite,
        &sql,
        concat!(
            "SELECT p.id, p.title, p.status, p.views, p.published_at\n",
            "FROM posts p\n",
            "WHERE p.user_id = ?1\n",
            "ORDER BY p.published_at DESC\n",
            "LIMIT ?2",
        ),
    );
    assert_eq!(
        args,
        vec![keelson_core::Value::I64(1), keelson_core::Value::I64(10)]
    );
}

/// **Face 2, the signature feature.** The same query applied as a mod to a
/// host `SELECT` produces **one flat statement**: the `WHERE` is `AND`ed onto
/// the host's own condition, the `FROM` is taken because the host had none,
/// and nothing is nested as a sub-select.
#[test]
fn the_mod_face_merges_flat_into_a_host_statement() {
    let q = keelson_sqlite::select((
        select::columns((quote(("p", "id")), quote(("p", "title")))),
        posts::posts_for_user_mod((1i64, 10i64)),
        select::where_(quote(("p", "status")).eq(arg("published"))),
    ));
    let (sql, args) = q.build().unwrap();
    keelson_sqlcheck::assert_sql(
        keelson_sqlcheck::Dialect::Sqlite,
        &sql,
        concat!(
            r#"SELECT "p"."id", "p"."title" FROM posts p "#,
            r#"WHERE (p.user_id = ?1) AND ("p"."status" = ?2) "#,
            "ORDER BY p.published_at DESC LIMIT ?3",
        ),
    );
    assert!(
        !sql.contains("SELECT p.id, p.title, p.status"),
        "the query's own select list is not contributed: the host owns its projection"
    );
    assert_eq!(
        args,
        vec![
            keelson_core::Value::I64(1),
            keelson_core::Value::Text("published".to_owned()),
            keelson_core::Value::I64(10),
        ],
        "the placeholders were re-numbered by the host's writer, in render order"
    );
}

/// The `FROM` a query contributes carries its joins, and is skipped when the
/// host already has one of its own — the rule that lets a mod ride on a model
/// query over the same table.
#[test]
fn the_mod_face_yields_its_from_to_a_host_that_has_one() {
    let hosted = keelson_sqlite::select((
        select::from(quote("posts")),
        posts::posts_for_user_mod((1i64, 10i64)),
    ));
    let (sql, _) = hosted.build().unwrap();
    assert!(sql.contains(r#"FROM "posts" WHERE"#), "{sql}");
    assert!(!sql.contains("FROM posts p"), "{sql}");

    let joined = keelson_sqlite::select((
        select::columns(quote(("c", "id"))),
        posts::comments_with_author_mod(1i64),
    ));
    let (sql, _) = joined.build().unwrap();
    assert!(
        sql.contains("FROM comments c\nLEFT JOIN users u ON u.id = c.user_id"),
        "the join rides along inside the FROM item, keeping one flat statement: {sql}"
    );
    assert!(!sql.contains("(SELECT"), "nothing nests: {sql}");
}

/// The two faces really do run the same rows: the mod-face statement, executed,
/// returns what the query face returns.
#[tokio::test]
async fn both_faces_return_the_same_rows() {
    use keelson_exec::Execute as _;

    let db = db().await;
    let direct = posts::posts_for_user(&db, (1i64, 10i64)).await.unwrap();

    let hosted: Vec<posts::PostsForUserRow> = keelson_sqlite::select((
        select::columns((
            quote(("p", "id")),
            quote(("p", "title")),
            quote(("p", "status")),
            quote(("p", "views")),
            quote(("p", "published_at")),
        )),
        posts::posts_for_user_mod((1i64, 10i64)),
    ))
    .fetch_all(&db)
    .await
    .unwrap();

    assert_eq!(direct, hosted);
}

/// `:optional` and `:exec`, end to end.
#[tokio::test]
async fn the_optional_and_exec_cardinalities_run() {
    let db = db().await;

    let found = posts::user_by_email(&db, "stephen@example.com".to_owned())
        .await
        .unwrap();
    assert_eq!(found.map(|u| u.name), Some("Stephen".to_owned()));
    let missing = posts::user_by_email(&db, "nobody@example.com".to_owned())
        .await
        .unwrap();
    assert!(missing.is_none(), "`:optional` is None, not an error");

    let result = posts::bump_views(&db, 1i64).await.unwrap();
    assert_eq!(result.rows_affected, 1);
    let after = posts::posts_for_user(&db, (1i64, 10i64)).await.unwrap();
    assert_eq!(after[0].views, 301);
}

/// The hazard raw clause text creates, and the guard against it: a fragment
/// whose top level is an `OR` must arrive parenthesised, or `AND`ing the
/// host's own condition onto it would silently change what the author wrote.
#[test]
fn an_or_condition_is_parenthesised_before_it_merges() {
    let q = keelson_sqlite::select((
        select::columns(quote(("p", "id"))),
        posts::hot_or_recent_mod(100i64),
        select::where_(quote(("p", "user_id")).eq(arg(1i64))),
    ));
    let (sql, _) = q.build().unwrap();
    keelson_sqlcheck::assert_sql(
        keelson_sqlcheck::Dialect::Sqlite,
        &sql,
        concat!(
            r#"SELECT "p"."id" FROM posts p "#,
            r#"WHERE (p.views > ?1 OR p.status = 'published') AND ("p"."user_id" = ?2)"#,
        ),
    );
}

/// …and it means the same thing when the engine runs it.
#[tokio::test]
async fn the_or_condition_means_the_same_thing_merged() {
    use keelson_exec::Execute as _;

    let db = db().await;
    let ids: Vec<i64> = keelson_sqlite::select((
        select::columns(quote(("p", "id"))),
        posts::hot_or_recent_mod(100i64),
        select::where_(quote(("p", "user_id")).eq(arg(1i64))),
    ))
    .fetch_scalars(&db)
    .await
    .unwrap();
    // Post 1 (300 views, published) and post 2 (7 views, no status) both belong
    // to user 1; only post 1 satisfies the OR.
    assert_eq!(ids, vec![1]);
}
