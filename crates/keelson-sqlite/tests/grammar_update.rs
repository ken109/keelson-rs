//! `UPDATE`, checked against SQLite's grammar, a real SQLite, and an expectation
//! derived from <https://www.sqlite.org/lang_update.html>.
//!
//! ```text
//! [ WITH [ RECURSIVE ] common-table-expression [, ...] ]
//! UPDATE [ OR { ROLLBACK | ABORT | REPLACE | FAIL | IGNORE } ] qualified-table-name
//!     SET { column | ( column [, ...] ) } = expr [, ...]
//!     [ FROM table-or-subquery [, ...] | join-clause ]
//!     [ WHERE expr ]
//!     [ RETURNING result-column [, ...] ]
//! ```
//!
//! `UPDATE … FROM` is SQLite 3.33 and later, `RETURNING` is 3.35.

use keelson_sqlcheck::{Dialect, assert_sql};
use keelson_sqlite as sqlite;
use keelson_sqlite::{Chain, Expr, Query, Value, arg, group, quote, s, select, subquery, update};

#[track_caller]
fn built(q: impl Query, expected: &str) -> Vec<Value> {
    let (sql, args) = q.build().expect("the query must build");
    assert_sql(Dialect::Sqlite, &sql, expected);
    args
}

// ---------------------------------------------------------------------------
// SET
// ---------------------------------------------------------------------------

#[test]
fn one_assignment_and_a_filter() {
    let args = built(
        sqlite::update((
            update::table(quote("users")),
            update::set_col("name").to_arg("ada"),
            update::where_(quote("id").eq(arg(1i32))),
        )),
        r#"UPDATE "users" SET "name" = ?1 WHERE ("id" = ?2)"#,
    );
    assert_eq!(args, vec![Value::Text("ada".into()), Value::I32(1)]);
}

#[test]
fn several_assignments_are_comma_separated() {
    built(
        sqlite::update((
            update::table(quote("users")),
            update::set_col("name").to(s("ada")),
            update::set_col("email").to(s("ada@example.com")),
            update::set_col("is_active").to(1),
            update::where_(quote("id").eq(arg(1i32))),
        )),
        r#"UPDATE "users" SET "name" = 'ada', "email" = 'ada@example.com', "is_active" = 1 WHERE ("id" = ?1)"#,
    );
}

#[test]
fn an_assignment_may_read_the_column_it_writes() {
    built(
        sqlite::update((
            update::table(quote("posts")),
            update::set_col("views").to(quote("views").plus(1)),
            update::where_(quote("id").eq(arg(1i32))),
        )),
        r#"UPDATE "posts" SET "views" = ("views" + 1) WHERE ("id" = ?1)"#,
    );
}

/// `SET ( column [, ...] ) = expr` — one assignment with a row on each side, which is
/// why an assignment is a whole expression here rather than a column/value pair.
#[test]
fn a_multi_column_assignment_from_a_sub_select() {
    let source = sqlite::select((
        select::columns((quote("name"), quote("email"))),
        select::from(quote("users")),
        select::where_(quote("id").eq(arg(2i32))),
        select::limit(1),
    ));
    built(
        sqlite::update((
            update::table(quote("users")),
            update::set(Expr::binary(
                group((quote("name"), quote("email"))),
                "=",
                subquery(source),
            )),
            update::where_(quote("id").eq(arg(1i32))),
        )),
        r#"UPDATE "users" SET ("name", "email") = (SELECT "name", "email" FROM "users" WHERE ("id" = ?1) LIMIT 1) WHERE ("id" = ?2)"#,
    );
}

#[test]
fn an_assignment_from_a_correlated_sub_select() {
    let counted = sqlite::select((
        select::columns(Expr::func("count", "*")),
        select::from(quote("comments")),
        select::where_(quote(("comments", "post_id")).eq(quote(("posts", "id")))),
    ));
    built(
        sqlite::update((
            update::table(quote("posts")),
            update::set_col("views").to(subquery(counted)),
        )),
        r#"UPDATE "posts" SET "views" = (SELECT count(*) FROM "comments" WHERE ("comments"."post_id" = "posts"."id"))"#,
    );
}

// ---------------------------------------------------------------------------
// The target — a qualified-table-name
// ---------------------------------------------------------------------------

#[test]
fn a_target_alias_qualifies_the_columns() {
    built(
        sqlite::update((
            update::table(quote("posts")).as_("p"),
            update::set_col("views").to(quote(("p", "views")).plus(1)),
            update::where_(quote(("p", "id")).eq(arg(1i32))),
        )),
        r#"UPDATE "posts" AS "p" SET "views" = ("p"."views" + 1) WHERE ("p"."id" = ?1)"#,
    );
}

/// The target is a `qualified-table-name`, so it takes the index directive — which is
/// why `update::table` is the same chain type as `select::from`.
#[test]
fn indexed_by_on_the_target() {
    built(
        sqlite::update((
            update::table(quote("tags")).indexed_by("sqlite_autoindex_tags_1"),
            update::set_col("name").to_arg("rust"),
            update::where_(quote("name").eq(arg("Rust"))),
        )),
        r#"UPDATE "tags" INDEXED BY "sqlite_autoindex_tags_1" SET "name" = ?1 WHERE ("name" = ?2)"#,
    );
}

#[test]
fn not_indexed_on_the_target() {
    built(
        sqlite::update((
            update::table(quote("tags")).not_indexed(),
            update::set_col("name").to_arg("rust"),
            update::where_(quote("name").eq(arg("Rust"))),
        )),
        r#"UPDATE "tags" NOT INDEXED SET "name" = ?1 WHERE ("name" = ?2)"#,
    );
}

// ---------------------------------------------------------------------------
// OR <conflict-algorithm>
// ---------------------------------------------------------------------------

/// The five keywords of `conflict-clause`, in the order the diagram lists them.
#[test]
fn every_conflict_algorithm() {
    let rest = || {
        (
            update::table(quote("tags")),
            update::set_col("name").to(s("a")),
        )
    };
    built(
        sqlite::update((update::or_rollback(), rest())),
        r#"UPDATE OR ROLLBACK "tags" SET "name" = 'a'"#,
    );
    built(
        sqlite::update((update::or_abort(), rest())),
        r#"UPDATE OR ABORT "tags" SET "name" = 'a'"#,
    );
    built(
        sqlite::update((update::or_replace(), rest())),
        r#"UPDATE OR REPLACE "tags" SET "name" = 'a'"#,
    );
    built(
        sqlite::update((update::or_fail(), rest())),
        r#"UPDATE OR FAIL "tags" SET "name" = 'a'"#,
    );
    built(
        sqlite::update((update::or_ignore(), rest())),
        r#"UPDATE OR IGNORE "tags" SET "name" = 'a'"#,
    );
}

#[test]
fn the_conflict_algorithm_precedes_the_target() {
    built(
        sqlite::update((
            update::or_ignore(),
            update::table(quote("tags")).as_("t"),
            update::set_col("name").to_arg("rust"),
            update::where_(quote(("t", "id")).eq(arg(1i32))),
        )),
        r#"UPDATE OR IGNORE "tags" AS "t" SET "name" = ?1 WHERE ("t"."id" = ?2)"#,
    );
}

// ---------------------------------------------------------------------------
// FROM
// ---------------------------------------------------------------------------

#[test]
fn update_from_another_table() {
    built(
        sqlite::update((
            update::table(quote("posts")).as_("p"),
            update::set_col("status").to(s("archived")),
            update::from(quote("users")).as_("u"),
            update::where_(quote(("u", "id")).eq(quote(("p", "user_id")))),
            update::where_(quote(("u", "is_active")).eq(0)),
        )),
        r#"UPDATE "posts" AS "p" SET "status" = 'archived' FROM "users" AS "u" WHERE ("u"."id" = "p"."user_id") AND ("u"."is_active" = 0)"#,
    );
}

/// The joins attach to the from-item, never to the target — which is the whole reason
/// `table` and `from` are different mods.
#[test]
fn a_join_hangs_off_the_from_item() {
    built(
        sqlite::update((
            update::table(quote("posts")).as_("p"),
            update::set_col("views").to(0),
            update::from(quote("users")).as_("u"),
            update::inner_join(quote("comments"))
                .as_("c")
                .on_eq(quote(("c", "user_id")), quote(("u", "id"))),
            update::where_(quote(("u", "id")).eq(quote(("p", "user_id")))),
        )),
        r#"UPDATE "posts" AS "p" SET "views" = 0 FROM "users" AS "u" INNER JOIN "comments" AS "c" ON ("c"."user_id" = "u"."id") WHERE ("u"."id" = "p"."user_id")"#,
    );
}

#[test]
fn a_comma_separated_from_list() {
    built(
        sqlite::update((
            update::table(quote("posts")).as_("p"),
            update::set_col("views").to(0),
            update::from(quote("users")).as_("u"),
            update::from_also(quote("comments")).as_("c"),
            update::where_(quote(("u", "id")).eq(quote(("p", "user_id")))),
            update::where_(quote(("c", "post_id")).eq(quote(("p", "id")))),
        )),
        r#"UPDATE "posts" AS "p" SET "views" = 0 FROM "users" AS "u", "comments" AS "c" WHERE ("u"."id" = "p"."user_id") AND ("c"."post_id" = "p"."id")"#,
    );
}

#[test]
fn update_from_a_sub_query() {
    let source = sqlite::select((
        select::columns((quote("user_id"), Expr::func("count", "*").as_("n"))),
        select::from(quote("comments")),
        select::group_by(quote("user_id")),
    ));
    built(
        sqlite::update((
            update::table(quote("posts")).as_("p"),
            update::set_col("views").to(quote(("t", "n"))),
            update::from(subquery(source)).as_("t"),
            update::where_(quote(("t", "user_id")).eq(quote(("p", "user_id")))),
        )),
        r#"UPDATE "posts" AS "p" SET "views" = "t"."n" FROM (SELECT "user_id", count(*) AS "n" FROM "comments" GROUP BY "user_id") AS "t" WHERE ("t"."user_id" = "p"."user_id")"#,
    );
}

#[test]
fn a_left_join_on_the_from_item() {
    built(
        sqlite::update((
            update::table(quote("posts")).as_("p"),
            update::set_col("status").to(s("orphan")),
            update::from(quote("users")).as_("u"),
            update::left_join(quote("comments"))
                .as_("c")
                .on_eq(quote(("c", "user_id")), quote(("u", "id"))),
            update::where_(quote(("u", "id")).eq(quote(("p", "user_id")))),
            update::where_(quote(("c", "id")).is_null()),
        )),
        r#"UPDATE "posts" AS "p" SET "status" = 'orphan' FROM "users" AS "u" LEFT JOIN "comments" AS "c" ON ("c"."user_id" = "u"."id") WHERE ("u"."id" = "p"."user_id") AND ("c"."id" IS NULL)"#,
    );
}

// ---------------------------------------------------------------------------
// RETURNING and WITH
// ---------------------------------------------------------------------------

#[test]
fn returning_named_columns() {
    built(
        sqlite::update((
            update::table(quote("posts")),
            update::set_col("views").to(0),
            update::where_(quote("id").eq(arg(1i32))),
            update::returning((quote("id"), quote("views"))),
        )),
        r#"UPDATE "posts" SET "views" = 0 WHERE ("id" = ?1) RETURNING "id", "views""#,
    );
}

#[test]
fn returning_a_star_after_a_from_clause() {
    built(
        sqlite::update((
            update::table(quote("posts")).as_("p"),
            update::set_col("views").to(0),
            update::from(quote("users")).as_("u"),
            update::where_(quote(("u", "id")).eq(quote(("p", "user_id")))),
            update::returning("*"),
        )),
        r#"UPDATE "posts" AS "p" SET "views" = 0 FROM "users" AS "u" WHERE ("u"."id" = "p"."user_id") RETURNING *"#,
    );
}

#[test]
fn a_cte_in_front_of_an_update() {
    let cte = sqlite::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        select::where_(quote("age").lt(arg(18i32))),
    ));
    let ids = sqlite::select((select::columns(quote("id")), select::from(quote("minors"))));
    built(
        sqlite::update((
            update::with("minors", cte),
            update::table(quote("posts")),
            update::set_col("status").to(s("hidden")),
            update::where_(quote("user_id").in_(subquery(ids))),
        )),
        r#"WITH "minors" AS (SELECT "id" FROM "users" WHERE ("age" < ?1)) UPDATE "posts" SET "status" = 'hidden' WHERE ("user_id" IN ((SELECT "id" FROM "minors")))"#,
    );
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

#[test]
fn an_update_with_no_target_table_refuses_to_build() {
    let err = sqlite::update(update::set_col("name").to(s("a")))
        .build()
        .unwrap_err();
    // The substring names the SQL concept (an UPDATE's table), not the message
    // wording.
    assert!(
        matches!(&err, sqlite::Error::Incomplete(what) if what.contains("UPDATE")),
        "got: {err}"
    );
}

/// `UPDATE t` with no `SET` is not a statement, so an empty assignment list is a
/// recorded failure rather than a clause that renders nothing.
#[test]
fn an_update_with_no_assignments_refuses_to_build() {
    let err = sqlite::update(update::table(quote("users")))
        .build()
        .unwrap_err();
    // The substrings name the SQL concepts (an UPDATE's assignments), not the
    // message wording.
    assert!(
        matches!(&err, sqlite::Error::Incomplete(what)
            if what.contains("assignments") && what.contains("UPDATE")),
        "got: {err}"
    );
}
