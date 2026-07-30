//! The checked-in generated PostgreSQL query modules, compiled and judged.
//!
//! Offline (always): the SQL of **both faces** goes through libpg_query — the
//! query face is the file's own statement, the mod face is one flat statement
//! merged into a host `SELECT`, never a sub-select.
//!
//! With `--features live-docker`: the inferred types are checked against what
//! PostgreSQL 17 actually returns, including a deliberately NULL-producing
//! `LEFT JOIN` row, which is the only way to prove rule N2 rather than assert
//! it.

// `pub` throughout the generated files because that is what the generator
// emits into an application's crate; this test binary has no external readers.
#[allow(unreachable_pub, dead_code)]
// The fixture is prettyplease-formatted by the generator; rustfmt must not
// rewrite it, or the byte-identical freshness test would fight `cargo fmt`.
#[rustfmt::skip]
#[path = "generated/psql_queries/mod.rs"]
mod queries;

use keelson_core::{Query as _, Value};
use keelson_psql::{Chain as _, arg, quote, select};
use keelson_sqlcheck::Dialect;

use queries::posts;

/// **Face 1.** The query object renders the author's own statement, with the
/// placeholders re-bound through the writer rather than copied.
#[test]
fn the_query_face_is_the_files_own_sql() {
    let (sql, args) = posts::posts_for_user_query((1i32, 10i64)).build().unwrap();
    keelson_sqlcheck::assert_sql(
        Dialect::Psql,
        &sql,
        concat!(
            "SELECT p.id, p.title, p.status, p.views, p.published_at\n",
            "FROM posts p\n",
            "WHERE p.user_id = $1\n",
            "ORDER BY p.published_at DESC\n",
            "LIMIT $2",
        ),
    );
    assert_eq!(args, vec![Value::I32(1), Value::I64(10)]);
}

/// A query object is a `Query` like any other, so it nests — and the writer
/// re-numbers its placeholders on the way in, which is the property that makes
/// the byte-slicing safe.
#[test]
fn the_query_face_renumbers_when_nested() {
    let outer = keelson_psql::select((
        select::columns(quote("id")),
        select::from(keelson_psql::subquery(posts::posts_for_user_query((
            1i32, 10i64,
        )))),
        select::where_(quote("id").gt(arg(0i32))),
    ));
    let (sql, args) = outer.build().unwrap();
    assert!(
        sql.contains("$1") && sql.contains("$2") && sql.contains("$3"),
        "{sql}"
    );
    assert_eq!(
        args,
        vec![Value::I32(1), Value::I64(10), Value::I32(0)],
        "the nested query's arguments come first, in render order"
    );
}

/// **Face 2, the signature feature.** The same query as a mod produces one
/// flat statement: its `WHERE` `AND`ed onto the host's, its `FROM` taken
/// because the host had none, and no sub-select anywhere.
#[test]
fn the_mod_face_merges_flat_into_a_host_statement() {
    let q = keelson_psql::select((
        select::columns((quote(("p", "id")), quote(("p", "title")))),
        posts::posts_for_user_mod((1i32, 10i64)),
        select::where_(quote(("p", "status")).eq(arg("published"))),
    ));
    let (sql, args) = q.build().unwrap();
    keelson_sqlcheck::assert_sql(
        Dialect::Psql,
        &sql,
        concat!(
            r#"SELECT "p"."id", "p"."title" FROM posts p "#,
            r#"WHERE (p.user_id = $1) AND ("p"."status" = $2) "#,
            "ORDER BY p.published_at DESC LIMIT $3",
        ),
    );
    assert!(!sql.contains("(SELECT"), "nothing nests: {sql}");
    assert_eq!(
        args,
        vec![
            Value::I32(1),
            Value::Text("published".to_owned()),
            Value::I64(10),
        ]
    );
}

/// The `FROM` a query contributes carries its joins with it — the reason the
/// merged statement is flat rather than nested — and is yielded to a host that
/// already has one.
#[test]
fn the_from_carries_its_joins_and_yields_to_a_host() {
    let joined = keelson_psql::select((
        select::columns(quote(("c", "id"))),
        posts::comments_with_author_mod(1i32),
    ));
    let (sql, _) = joined.build().unwrap();
    keelson_sqlcheck::check(Dialect::Psql, &sql).expect("a real PostgreSQL statement");
    assert!(
        sql.contains("FROM comments c\nLEFT JOIN users u ON u.id = c.user_id"),
        "{sql}"
    );
    assert!(!sql.contains("(SELECT"), "{sql}");

    let hosted = keelson_psql::select((
        select::from(quote("posts")),
        posts::posts_for_user_mod((1i32, 10i64)),
    ));
    let (sql, _) = hosted.build().unwrap();
    assert!(sql.contains(r#"FROM "posts" WHERE"#), "{sql}");
}

/// A `GROUP BY`/`HAVING`-shaped query merges those clauses too.
#[test]
fn group_by_and_order_by_merge_as_clauses_not_as_text() {
    let q = keelson_psql::select((
        select::columns(quote(("u", "id"))),
        posts::user_stats_mod(()),
    ));
    let (sql, _) = q.build().unwrap();
    keelson_sqlcheck::check(Dialect::Psql, &sql).expect("a real PostgreSQL statement");
    assert!(sql.contains("GROUP BY u.id, u.name, u.email"), "{sql}");
    assert!(sql.contains("ORDER BY u.id"), "{sql}");
}

/// A statement with no mod face still has its query face, and the generated
/// module says why in a doc comment rather than nesting it as a sub-select.
#[test]
fn a_set_operation_keeps_its_query_face() {
    let (sql, args) = posts::titles_union_query(()).build().unwrap();
    keelson_sqlcheck::check(Dialect::Psql, &sql).expect("a real PostgreSQL statement");
    assert!(sql.contains("UNION ALL"), "{sql}");
    assert!(args.is_empty());
}

/// The staleness guard: the generated module asserts the query file's length
/// at compile time, so editing the SQL without re-running the generator is a
/// compile error rather than a silently misaligned byte range.
#[test]
fn the_generated_module_pins_the_query_files_length() {
    let source = include_str!("generated/psql_queries/posts.rs");
    let sql = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/queries/psql/posts.sql"),
    )
    .unwrap();
    assert!(
        source.contains(&format!("SOURCE.len() == {}usize", sql.len())),
        "the fixture pins the current query file's length"
    );
}

#[cfg(feature = "live-docker")]
mod live {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicI32, Ordering};

    use keelson_exec::{BeginExt as _, ExecError};

    use super::queries::posts;

    /// Process-unique positive i32 keys, so runs against a shared or
    /// persistent server never collide.
    fn key() -> i32 {
        static NEXT: AtomicI32 = AtomicI32::new(0);
        static BASE: OnceLock<i32> = OnceLock::new();
        let base = *BASE.get_or_init(|| {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            ((nanos as i64) & 0x3fff_ff00) as i32
        });
        base + NEXT.fetch_add(1, Ordering::Relaxed)
    }

    async fn pool() -> keelson_sqlx::psql::Pool {
        let url = tokio::task::spawn_blocking(|| keelson_sqlcheck::live::psql_url().to_owned())
            .await
            .unwrap();
        keelson_sqlx::psql::Pool::connect(&url)
            .await
            .expect("connecting to the live PostgreSQL")
    }

    /// The acceptance test the whole layer exists for: the hand-written
    /// expectations in `queries_psql.rs` are checked against **what
    /// PostgreSQL actually returns**, including a `LEFT JOIN` that finds no
    /// row and so hands back NULL for a `NOT NULL` column.
    #[tokio::test]
    async fn the_inferred_types_are_the_servers_types() {
        use keelson_exec::Execute as _;
        use keelson_psql::{arg, insert, quote};

        let db = pool().await;
        let (uid, uid2, uid3) = (key(), key(), key());
        let (pid, pid2) = (key(), key());
        let (cid, cid2) = (key(), key());

        let out: Result<(), ExecError> = db
            .within(async |tx| {
                for (id, name, email) in [
                    (uid, "Stephen", Some("stephen@example.com")),
                    (uid2, "Ada", None),
                    (uid3, "Grace", Some("grace@example.com")),
                ] {
                    keelson_psql::insert((
                        insert::into(quote("users")).columns(["id", "name", "email"]),
                        insert::values((arg(id), arg(name), arg(email))),
                    ))
                    .execute(tx)
                    .await?;
                }
                for (id, title, status, views) in [
                    (pid, "keel laid", Some("published"), 300i32),
                    (pid2, "second", None, 7i32),
                ] {
                    keelson_psql::insert((
                        insert::into(quote("posts"))
                            .columns(["id", "user_id", "title", "status", "views"]),
                        insert::values((arg(id), arg(uid), arg(title), arg(status), arg(views))),
                    ))
                    .execute(tx)
                    .await?;
                }
                // One comment with an author, one without — the NULL-producing
                // row rule N2 is about.
                for (id, user_id, body) in [(cid, Some(uid), "first"), (cid2, None, "stranger")] {
                    keelson_psql::insert((
                        insert::into(quote("comments"))
                            .columns(["id", "post_id", "user_id", "body"]),
                        insert::values((arg(id), arg(pid), arg(user_id), arg(body))),
                    ))
                    .execute(tx)
                    .await?;
                }

                // Rule N1: nullable columns really are None.
                let rows = posts::posts_for_user(tx, (uid, 10i64)).await?;
                assert_eq!(rows.len(), 2);
                let draft = rows.iter().find(|r| r.title == "second").unwrap();
                assert_eq!(draft.status, None);
                assert_eq!(draft.published_at, None);

                // Rule N2: `users.name` is NOT NULL and still absent.
                let mut comments = posts::comments_with_author(tx, pid).await?;
                comments.sort_by_key(|c| c.id);
                assert_eq!(comments.len(), 2);
                assert_eq!(
                    comments[0].author.as_ref().map(|a| a.name.as_str()),
                    Some("Stephen")
                );
                assert_eq!(
                    comments[1].author, None,
                    "the outer join found no row: the Option holds it"
                );

                // Rules N4/N5/N7 over an empty group, and N3's non-effect.
                let stats = posts::user_stats(tx, ()).await?;
                let grace = stats.iter().find(|s| s.id == uid3).unwrap();
                assert_eq!(grace.post_count, 0, "COUNT is never NULL");
                assert_eq!(grace.best_views, None, "MAX over nothing is NULL");
                assert_eq!(grace.total_views, 0, "COALESCE makes it a value");
                assert!(
                    stats.iter().all(|s| s.id != uid2),
                    "Ada has no email, so the IS NOT NULL filter drops her row"
                );

                // Rules N9/N10/N11/N13, in particular the nullable comparison.
                let flags = posts::post_flags(tx, 100i32).await?;
                let hot = flags.iter().find(|f| f.id == pid).unwrap();
                assert!(hot.has_status && hot.is_popular);
                assert_eq!(hot.is_published, Some(true));
                assert_eq!(hot.heat, "hot");
                assert_eq!(hot.views_wide, 300i64);
                let cold = flags.iter().find(|f| f.id == pid2).unwrap();
                assert!(!cold.has_status);
                assert_eq!(
                    cold.is_published, None,
                    "`status = 'published'` over a NULL status is NULL, not false"
                );
                assert_eq!(cold.maybe_heat, None, "an ELSE-less CASE falls to NULL");

                // `:one` means one.
                let u = posts::user_by_id(tx, uid).await?;
                assert_eq!(u.name, "Stephen");
                assert!(matches!(
                    posts::user_by_id(tx, -1i32).await,
                    Err(ExecError::RowNotFound)
                ));

                Err(ExecError::other("deliberate rollback"))
            })
            .await;
        assert!(out.is_err(), "the fixture rows rolled back");
    }

    /// Both faces run, and return the same rows from the same server.
    #[tokio::test]
    async fn both_faces_return_the_same_rows() {
        use keelson_exec::Execute as _;
        use keelson_psql::{arg, insert, quote, select};

        let db = pool().await;
        let uid = key();
        let pid = key();

        let out: Result<(), ExecError> = db
            .within(async |tx| {
                keelson_psql::insert((
                    insert::into(quote("users")).columns(["id", "name"]),
                    insert::values((arg(uid), arg("Stephen"))),
                ))
                .execute(tx)
                .await?;
                keelson_psql::insert((
                    insert::into(quote("posts")).columns(["id", "user_id", "title"]),
                    insert::values((arg(pid), arg(uid), arg("keel laid"))),
                ))
                .execute(tx)
                .await?;

                let direct = posts::posts_for_user(tx, (uid, 10i64)).await?;
                let hosted: Vec<posts::PostsForUserRow> = keelson_psql::select((
                    select::columns((
                        quote(("p", "id")),
                        quote(("p", "title")),
                        quote(("p", "status")),
                        quote(("p", "views")),
                        quote(("p", "published_at")),
                    )),
                    posts::posts_for_user_mod((uid, 10i64)),
                ))
                .fetch_all(tx)
                .await?;
                assert_eq!(direct, hosted);

                Err(ExecError::other("deliberate rollback"))
            })
            .await;
        assert!(out.is_err());
    }
}
