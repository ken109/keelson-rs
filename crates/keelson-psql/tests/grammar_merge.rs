//! A walk of PostgreSQL's `MERGE` grammar (PostgreSQL 15+; the 17-only forms
//! are marked where they stand).
//!
//! Every case goes through [`assert_sql`]: libpg_query — PostgreSQL 17's own
//! parser — accepts the SQL, a real PostgreSQL 17 accepts it too when one is
//! compiled in (`--features live-docker`), and the SQL equals the
//! whitespace-normalised string written here.
//!
//! **Where the expected strings come from.** Each is derived from
//! <https://www.postgresql.org/docs/17/sql-merge.html>:
//!
//! ```text
//! [ WITH with_query [, ...] ]
//! MERGE INTO [ ONLY ] target_table_name [ * ] [ [ AS ] target_alias ]
//!     USING data_source ON join_condition
//!     when_clause [...]
//!     [ RETURNING … ]
//!
//! data_source:
//!     { [ ONLY ] source_table_name [ * ] | ( source_query ) } [ [ AS ] source_alias ]
//!
//! when_clause:
//!     WHEN MATCHED [ AND condition ] THEN { merge_update | merge_delete | DO NOTHING }
//!   | WHEN NOT MATCHED BY SOURCE [ AND condition ] THEN
//!         { merge_update | merge_delete | DO NOTHING }
//!   | WHEN NOT MATCHED [ BY TARGET ] [ AND condition ] THEN
//!         { merge_insert | DO NOTHING }
//!
//! merge_insert:
//!     INSERT [( column_name [, ...] )]
//!         [ OVERRIDING { SYSTEM | USER } VALUE ]
//!         { VALUES ( { expression | DEFAULT } [, ...] ) | DEFAULT VALUES }
//! merge_update:
//!     UPDATE SET { column_name = { expression | DEFAULT } | … } [, ...]
//! merge_delete:
//!     DELETE
//! ```
//!
//! composed with the rendering rules `keelson_core::clause` documents. None was
//! produced by running the builder and pasting its output.
//!
//! **Every table and column named here is in `tests/schema/psql.sql`**, so the
//! engine tier can resolve names. The merge pairs `tags (id, name)` as target
//! with `posts (id, title, views)` as source, because their key columns are
//! both `integer`.

use keelson_psql as psql;
use keelson_psql::{Chain, Error, Query, Value, arg, merge, quote, raw, select, subquery};
use keelson_sqlcheck::{Dialect, assert_sql};

/// Build, then run every check this build can: grammar, engine, and intent.
#[track_caller]
fn check(q: &impl Query, expected: &str) -> Vec<Value> {
    let (sql, args) = q.build().expect("the query should build");
    assert_sql(Dialect::Psql, &sql, expected);
    args
}

// ---------------------------------------------------------------------------
// The core shape: target, source, ON, and the two everyday arms
// ---------------------------------------------------------------------------

#[test]
fn merge_with_an_update_and_an_insert_arm() {
    let q = psql::merge((
        merge::into(quote("tags")).as_("t"),
        merge::using(quote("posts")).as_("p"),
        merge::on(quote(("t", "id")).eq(quote(("p", "id")))),
        merge::when_matched().then_update(merge::set_col("name").to(quote(("p", "title")))),
        merge::when_not_matched()
            .then_insert()
            .columns(["id", "name"])
            .values((quote(("p", "id")), quote(("p", "title")))),
    ));
    assert!(
        check(
            &q,
            r#"MERGE INTO "tags" AS "t" USING "posts" AS "p" ON ("t"."id" = "p"."id")
               WHEN MATCHED THEN UPDATE SET "name" = "p"."title"
               WHEN NOT MATCHED THEN INSERT ("id", "name") VALUES ("p"."id", "p"."title")"#,
        )
        .is_empty()
    );
}

/// `ON join_condition` takes a single boolean expression, so several `on` calls
/// become one conjunction — the same reading a join's `ON` gives repeated
/// conditions. Placeholders number straight through `ON`, the arm conditions
/// and the inserted row, in write order.
#[test]
fn on_conditions_and_join_and_the_placeholders_run_in_write_order() {
    let q = psql::merge((
        merge::into(quote("tags")).as_("t"),
        merge::using(quote("posts")).as_("p"),
        merge::on(quote(("t", "id")).eq(quote(("p", "id")))),
        merge::on(quote(("p", "views")).gt(arg(10i32))),
        merge::when_matched()
            .and(quote(("p", "title")).ne(arg("x")))
            .then_delete(),
        merge::when_not_matched()
            .then_insert()
            .columns(["id", "name"])
            .values((quote(("p", "id")), arg("fresh"))),
    ));
    let args = check(
        &q,
        r#"MERGE INTO "tags" AS "t" USING "posts" AS "p"
           ON ("t"."id" = "p"."id") AND ("p"."views" > $1)
           WHEN MATCHED AND ("p"."title" <> $2) THEN DELETE
           WHEN NOT MATCHED THEN INSERT ("id", "name") VALUES ("p"."id", $3)"#,
    );
    assert_eq!(
        args,
        vec![
            Value::I32(10),
            Value::Text("x".into()),
            Value::Text("fresh".into()),
        ]
    );
}

/// `MERGE INTO [ ONLY ] target` — the same `ONLY` every table reference takes —
/// and `DO NOTHING` arms on both sides. A matched `DO NOTHING` is not the same
/// as omitting the arm: a row it captures is consumed by it.
#[test]
fn only_on_the_target_and_do_nothing_on_both_sides() {
    let q = psql::merge((
        merge::into(quote("tags")).only(),
        merge::using(quote("posts")).as_("p"),
        merge::on(quote(("tags", "id")).eq(quote(("p", "id")))),
        merge::when_matched().then_do_nothing(),
        merge::when_not_matched().then_do_nothing(),
    ));
    check(
        &q,
        r#"MERGE INTO ONLY "tags" USING "posts" AS "p" ON ("tags"."id" = "p"."id")
           WHEN MATCHED THEN DO NOTHING
           WHEN NOT MATCHED THEN DO NOTHING"#,
    );
}

/// `data_source` may be `( source_query ) [ AS alias ]` — the parentheses are
/// the sub-query's own, which is what [`subquery`] supplies.
#[test]
fn the_source_may_be_a_parenthesised_query() {
    let source = psql::select((
        select::columns((quote("id"), quote("title"))),
        select::from(quote("posts")),
        select::where_(quote("views").gt(arg(100i32))),
    ));
    let q = psql::merge((
        merge::into(quote("tags")).as_("t"),
        merge::using(subquery(source)).as_("hot"),
        merge::on(quote(("t", "id")).eq(quote(("hot", "id")))),
        merge::when_matched().then_update(merge::set_col("name").to(quote(("hot", "title")))),
    ));
    let args = check(
        &q,
        r#"MERGE INTO "tags" AS "t"
           USING (SELECT "id", "title" FROM "posts" WHERE ("views" > $1)) AS "hot"
           ON ("t"."id" = "hot"."id")
           WHEN MATCHED THEN UPDATE SET "name" = "hot"."title""#,
    );
    assert_eq!(args, vec![Value::I32(100)]);
}

/// Several arms of the same kind are legal — the first whose condition passes
/// wins — and the multi-column assignment form is the `UPDATE` one, because a
/// `merge_update` *is* an `UPDATE SET` list. Two columns, because a
/// single-element parenthesised right-hand side is not a row constructor and
/// the analyser refuses it — the same rule as in an `UPDATE`.
#[test]
fn repeated_matched_arms_and_a_row_assignment() {
    let q = psql::merge((
        merge::into(quote("tags")).as_("t"),
        merge::using(quote("posts")).as_("p"),
        merge::on(quote(("t", "id")).eq(quote(("p", "id")))),
        merge::when_matched()
            .and(quote(("p", "views")).gt(arg(1000i32)))
            .then_delete(),
        merge::when_matched().then_update(merge::set(keelson_psql::Expr::binary(
            keelson_psql::group((quote("id"), quote("name"))),
            "=",
            keelson_psql::group((quote(("p", "id")), quote(("p", "title")))),
        ))),
    ));
    check(
        &q,
        r#"MERGE INTO "tags" AS "t" USING "posts" AS "p" ON ("t"."id" = "p"."id")
           WHEN MATCHED AND ("p"."views" > $1) THEN DELETE
           WHEN MATCHED THEN UPDATE SET ("id", "name") = ("p"."id", "p"."title")"#,
    );
}

// ---------------------------------------------------------------------------
// merge_insert's own optional parts
// ---------------------------------------------------------------------------

/// A bare `THEN INSERT` is `INSERT DEFAULT VALUES` — the same reading an
/// `INSERT` statement gives an absent row source — and, unlike a plain
/// `INSERT`, `merge_insert`'s production puts `OVERRIDING` in front of
/// `DEFAULT VALUES` too.
#[test]
fn insert_default_values_with_and_without_overriding() {
    let q = psql::merge((
        merge::into(quote("tags")),
        merge::using(quote("posts")).as_("p"),
        merge::on(quote(("tags", "id")).eq(quote(("p", "id")))),
        merge::when_not_matched().then_insert(),
    ));
    check(
        &q,
        r#"MERGE INTO "tags" USING "posts" AS "p" ON ("tags"."id" = "p"."id")
           WHEN NOT MATCHED THEN INSERT DEFAULT VALUES"#,
    );

    let q = psql::merge((
        merge::into(quote("tags")),
        merge::using(quote("posts")).as_("p"),
        merge::on(quote(("tags", "id")).eq(quote(("p", "id")))),
        merge::when_not_matched()
            .then_insert()
            .columns(["id", "name"])
            .overriding_user()
            .values((quote(("p", "id")), quote(("p", "title")))),
    ));
    check(
        &q,
        r#"MERGE INTO "tags" USING "posts" AS "p" ON ("tags"."id" = "p"."id")
           WHEN NOT MATCHED THEN INSERT ("id", "name") OVERRIDING USER VALUE
           VALUES ("p"."id", "p"."title")"#,
    );
}

/// A cell of the inserted row may be the `DEFAULT` keyword, exactly as in an
/// `INSERT` statement's row.
#[test]
fn a_merge_insert_cell_may_be_default() {
    let q = psql::merge((
        merge::into(quote("posts")).as_("t"),
        merge::using(quote("comments")).as_("c"),
        merge::on(quote(("t", "id")).eq(quote(("c", "post_id")))),
        merge::when_not_matched()
            .then_insert()
            .columns(["id", "user_id", "title", "views"])
            .values((
                quote(("c", "post_id")),
                quote(("c", "user_id")),
                arg("recovered"),
                raw("DEFAULT"),
            )),
    ));
    check(
        &q,
        r#"MERGE INTO "posts" AS "t" USING "comments" AS "c" ON ("t"."id" = "c"."post_id")
           WHEN NOT MATCHED THEN INSERT ("id", "user_id", "title", "views")
           VALUES ("c"."post_id", "c"."user_id", $1, DEFAULT)"#,
    );
}

// ---------------------------------------------------------------------------
// WITH
// ---------------------------------------------------------------------------

/// `[ WITH with_query [, ...] ]` precedes `MERGE`, and the CTE's name is a
/// legal `source_table_name`. (`WITH RECURSIVE` is not: PostgreSQL rejects it
/// on `MERGE`, which is why `merge::recursive` does not exist.)
#[test]
fn a_cte_feeds_the_source() {
    let hot = psql::select((
        select::columns((quote("id"), quote("title"))),
        select::from(quote("posts")),
        select::where_(quote("views").gt(arg(50i32))),
    ));
    let q = psql::merge((
        merge::with("hot", hot),
        merge::into(quote("tags")).as_("t"),
        merge::using(quote("hot")),
        merge::on(quote(("t", "id")).eq(quote(("hot", "id")))),
        merge::when_matched().then_update(merge::set_col("name").to(quote(("hot", "title")))),
    ));
    let args = check(
        &q,
        r#"WITH "hot" AS (SELECT "id", "title" FROM "posts" WHERE ("views" > $1))
           MERGE INTO "tags" AS "t" USING "hot" ON ("t"."id" = "hot"."id")
           WHEN MATCHED THEN UPDATE SET "name" = "hot"."title""#,
    );
    assert_eq!(args, vec![Value::I32(50)]);
}

// ---------------------------------------------------------------------------
// PostgreSQL 17: BY SOURCE, BY TARGET, RETURNING
// ---------------------------------------------------------------------------

/// **PostgreSQL 17+**: `WHEN NOT MATCHED BY SOURCE` acts on the target rows the
/// source did not match, so its actions are the matched ones — here `DELETE`,
/// the sync-a-table idiom — and `BY TARGET` is the spelled-out default of the
/// insert arm.
#[test]
fn pg17_not_matched_by_source_and_by_target() {
    let q = psql::merge((
        merge::into(quote("tags")).as_("t"),
        merge::using(quote("posts")).as_("p"),
        merge::on(quote(("t", "id")).eq(quote(("p", "id")))),
        merge::when_matched().then_update(merge::set_col("name").to(quote(("p", "title")))),
        merge::when_not_matched()
            .by_target()
            .then_insert()
            .columns(["id", "name"])
            .values((quote(("p", "id")), quote(("p", "title")))),
        merge::when_not_matched_by_source().then_delete(),
    ));
    check(
        &q,
        r#"MERGE INTO "tags" AS "t" USING "posts" AS "p" ON ("t"."id" = "p"."id")
           WHEN MATCHED THEN UPDATE SET "name" = "p"."title"
           WHEN NOT MATCHED BY TARGET THEN INSERT ("id", "name") VALUES ("p"."id", "p"."title")
           WHEN NOT MATCHED BY SOURCE THEN DELETE"#,
    );
}

/// **PostgreSQL 17+**: a `BY SOURCE` arm takes an `AND` refinement and an
/// `UPDATE` action like any other matched arm.
#[test]
fn pg17_not_matched_by_source_with_a_condition_and_an_update() {
    let q = psql::merge((
        merge::into(quote("tags")).as_("t"),
        merge::using(quote("posts")).as_("p"),
        merge::on(quote(("t", "id")).eq(quote(("p", "id")))),
        merge::when_not_matched_by_source()
            .and(quote(("t", "name")).ne(arg("keep")))
            .then_update(merge::set_col("name").to(arg("orphaned"))),
    ));
    let args = check(
        &q,
        r#"MERGE INTO "tags" AS "t" USING "posts" AS "p" ON ("t"."id" = "p"."id")
           WHEN NOT MATCHED BY SOURCE AND ("t"."name" <> $1)
           THEN UPDATE SET "name" = $2"#,
    );
    assert_eq!(
        args,
        vec![Value::Text("keep".into()), Value::Text("orphaned".into())]
    );
}

/// **PostgreSQL 17+**: `RETURNING` closes the statement, after the last arm.
#[test]
fn pg17_returning_follows_the_last_when_clause() {
    let q = psql::merge((
        merge::into(quote("tags")).as_("t"),
        merge::using(quote("posts")).as_("p"),
        merge::on(quote(("t", "id")).eq(quote(("p", "id")))),
        merge::when_matched().then_delete(),
        merge::returning((quote(("t", "id")), quote(("t", "name")))),
    ));
    check(
        &q,
        r#"MERGE INTO "tags" AS "t" USING "posts" AS "p" ON ("t"."id" = "p"."id")
           WHEN MATCHED THEN DELETE RETURNING "t"."id", "t"."name""#,
    );
}

// ---------------------------------------------------------------------------
// What cannot render, and says so
// ---------------------------------------------------------------------------

/// USING, ON and the when-list are grammatically required — sql-merge.html puts
/// no brackets around them — so each absence is a recorded `build()` failure
/// naming the missing piece, never a shorter statement.
#[test]
fn a_merge_missing_a_required_piece_says_which() {
    // The substrings name the SQL concepts, not the message wording.
    let err = psql::merge(()).build().unwrap_err();
    assert!(
        matches!(&err, Error::Incomplete(what) if what.contains("MERGE")),
        "got: {err}"
    );

    let err = psql::merge(merge::into(quote("tags"))).build().unwrap_err();
    assert!(
        matches!(&err, Error::Incomplete(what) if what.contains("USING")),
        "got: {err}"
    );

    let err = psql::merge((merge::into(quote("tags")), merge::using(quote("posts"))))
        .build()
        .unwrap_err();
    assert!(
        matches!(&err, Error::Incomplete(what) if what.contains("ON")),
        "got: {err}"
    );

    let err = psql::merge((
        merge::into(quote("tags")),
        merge::using(quote("posts")),
        merge::on(raw("true")),
    ))
    .build()
    .unwrap_err();
    assert!(
        matches!(&err, Error::Incomplete(what) if what.contains("WHEN")),
        "got: {err}"
    );
}

/// `THEN UPDATE SET` with no assignments is not a `merge_update`, and which mod
/// arrived last must not decide what renders — so it is recorded.
#[test]
fn an_update_arm_with_no_assignments_is_a_recorded_failure() {
    let q = psql::merge((
        merge::into(quote("tags")),
        merge::using(quote("posts")).as_("p"),
        merge::on(quote(("tags", "id")).eq(quote(("p", "id")))),
        merge::when_matched().then_update(()),
    ));
    let err = q.build().unwrap_err();
    assert!(
        matches!(&err, Error::Incomplete(what) if what.contains("assignments")),
        "got: {err}"
    );
}
