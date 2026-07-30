//! `DELETE`, checked against SQLite's grammar, a real SQLite, and an expectation
//! derived from <https://www.sqlite.org/lang_delete.html>.
//!
//! ```text
//! [ WITH [ RECURSIVE ] common-table-expression [, ...] ]
//! DELETE FROM qualified-table-name
//!     [ WHERE expr ]
//!     [ RETURNING result-column [, ...] ]
//! ```
//!
//! That is the whole statement. There is no `USING`, no `FROM` list, no join and no
//! `OR` clause, so a delete driven by another table is written with a sub-query in
//! the `WHERE` — which is what most of these cases do.

use keelson_sqlcheck::{Dialect, assert_sql};
use keelson_sqlite as sqlite;
use keelson_sqlite::{
    Chain, Expr, Query, SqliteOps, Value, arg, delete, quote, s, select, subquery,
};

#[track_caller]
fn built(q: impl Query, expected: &str) -> Vec<Value> {
    let (sql, args) = q.build().expect("the query must build");
    assert_sql(Dialect::Sqlite, &sql, expected);
    args
}

#[test]
fn a_delete_with_no_filter_removes_every_row() {
    built(
        sqlite::delete(delete::from(quote("comments"))),
        r#"DELETE FROM "comments""#,
    );
}

#[test]
fn a_bound_filter() {
    let args = built(
        sqlite::delete((
            delete::from(quote("comments")),
            delete::where_(quote("id").eq(arg(1i32))),
        )),
        r#"DELETE FROM "comments" WHERE ("id" = ?1)"#,
    );
    assert_eq!(args, vec![Value::I32(1)]);
}

#[test]
fn several_conditions_are_and_joined() {
    let args = built(
        sqlite::delete((
            delete::from(quote("posts")),
            delete::where_(quote("status").eq(arg("draft"))),
            delete::where_(quote("views").lt(arg(10i32))),
            delete::where_(quote("published_at").is_null()),
        )),
        r#"DELETE FROM "posts" WHERE ("status" = ?1) AND ("views" < ?2) AND ("published_at" IS NULL)"#,
    );
    assert_eq!(args, vec![Value::Text("draft".into()), Value::I32(10)]);
}

#[test]
fn a_raw_table_name_is_written_verbatim() {
    built(
        sqlite::delete((delete::from("comments"), delete::where_("id = 1"))),
        "DELETE FROM comments WHERE id = 1",
    );
}

#[test]
fn a_target_alias_qualifies_the_columns() {
    built(
        sqlite::delete((
            delete::from(quote("posts")).as_("p"),
            delete::where_(quote(("p", "id")).eq(arg(1i32))),
        )),
        r#"DELETE FROM "posts" AS "p" WHERE ("p"."id" = ?1)"#,
    );
}

/// The target is a `qualified-table-name`, so the index directive applies here as it
/// does to a `SELECT`'s from-item. The index named is the one SQLite creates for
/// `tags.name UNIQUE` in the shared schema.
#[test]
fn indexed_by_on_the_target() {
    built(
        sqlite::delete((
            delete::from(quote("tags")).indexed_by("sqlite_autoindex_tags_1"),
            delete::where_(quote("name").eq(arg("rust"))),
        )),
        r#"DELETE FROM "tags" INDEXED BY "sqlite_autoindex_tags_1" WHERE ("name" = ?1)"#,
    );
}

#[test]
fn not_indexed_on_the_target() {
    built(
        sqlite::delete((
            delete::from(quote("tags")).not_indexed(),
            delete::where_(quote("name").eq(arg("rust"))),
        )),
        r#"DELETE FROM "tags" NOT INDEXED WHERE ("name" = ?1)"#,
    );
}

#[test]
fn not_indexed_after_an_alias() {
    built(
        sqlite::delete((
            delete::from(quote("tags")).as_("t").not_indexed(),
            delete::where_(quote(("t", "name")).eq(arg("rust"))),
        )),
        r#"DELETE FROM "tags" AS "t" NOT INDEXED WHERE ("t"."name" = ?1)"#,
    );
}

/// SQLite has no `USING`, so this is how a delete driven by another table is written.
#[test]
fn a_sub_query_stands_in_for_using() {
    let drafts = sqlite::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::where_(quote("status").eq(arg("draft"))),
    ));
    let args = built(
        sqlite::delete((
            delete::from(quote("post_tags")),
            delete::where_(quote("post_id").in_(subquery(drafts))),
        )),
        r#"DELETE FROM "post_tags" WHERE ("post_id" IN ((SELECT "id" FROM "posts" WHERE ("status" = ?1))))"#,
    );
    assert_eq!(args, vec![Value::Text("draft".into())]);
}

#[test]
fn a_correlated_not_exists_filter() {
    let referenced = sqlite::select((
        select::columns(Expr::raw("1")),
        select::from(quote("posts")),
        select::where_(quote(("posts", "id")).eq(quote(("post_tags", "post_id")))),
    ));
    built(
        sqlite::delete((
            delete::from(quote("post_tags")),
            delete::where_(Expr::prefix("NOT EXISTS", subquery(referenced))),
        )),
        r#"DELETE FROM "post_tags" WHERE NOT EXISTS (SELECT 1 FROM "posts" WHERE ("posts"."id" = "post_tags"."post_id"))"#,
    );
}

#[test]
fn a_filter_using_a_sqlite_only_operator() {
    built(
        sqlite::delete((
            delete::from(quote("tags")),
            delete::where_(quote("name").glob(s("tmp-*"))),
        )),
        r#"DELETE FROM "tags" WHERE ("name" GLOB 'tmp-*')"#,
    );
}

#[test]
fn returning_a_star() {
    built(
        sqlite::delete((
            delete::from(quote("comments")),
            delete::where_(quote("id").eq(arg(1i32))),
            delete::returning("*"),
        )),
        r#"DELETE FROM "comments" WHERE ("id" = ?1) RETURNING *"#,
    );
}

#[test]
fn returning_named_columns_and_an_expression() {
    built(
        sqlite::delete((
            delete::from(quote("comments")),
            delete::where_(quote("post_id").eq(arg(1i32))),
            delete::returning((
                quote("id"),
                quote("body"),
                Expr::func("length", quote("body")).as_("len"),
            )),
        )),
        r#"DELETE FROM "comments" WHERE ("post_id" = ?1) RETURNING "id", "body", length("body") AS "len""#,
    );
}

#[test]
fn a_cte_in_front_of_a_delete() {
    let stale = sqlite::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::where_(quote("published_at").lt(arg("2020-01-01"))),
    ));
    let ids = sqlite::select((select::columns(quote("id")), select::from(quote("stale"))));
    let args = built(
        sqlite::delete((
            delete::with("stale", stale),
            delete::from(quote("comments")),
            delete::where_(quote("post_id").in_(subquery(ids))),
        )),
        r#"WITH "stale" AS (SELECT "id" FROM "posts" WHERE ("published_at" < ?1)) DELETE FROM "comments" WHERE ("post_id" IN ((SELECT "id" FROM "stale")))"#,
    );
    assert_eq!(args, vec![Value::Text("2020-01-01".into())]);
}

#[test]
fn a_recursive_cte_in_front_of_a_delete() {
    let step = sqlite::select((
        select::columns(quote("n").plus(1)),
        select::from(quote("ids")),
        select::where_(quote("n").lt(3)),
    ));
    let seed = sqlite::select((select::values(1), select::union_all(step)));
    let ids = sqlite::select((select::columns(quote("n")), select::from(quote("ids"))));
    built(
        sqlite::delete((
            delete::recursive(true),
            delete::with("ids", seed).columns(["n"]),
            delete::from(quote("comments")),
            delete::where_(quote("id").in_(subquery(ids))),
        )),
        r#"WITH RECURSIVE "ids" ("n") AS (VALUES (1) UNION ALL SELECT ("n" + 1) FROM "ids" WHERE ("n" < 3)) DELETE FROM "comments" WHERE ("id" IN ((SELECT "n" FROM "ids")))"#,
    );
}

#[test]
fn returning_after_a_cte() {
    let stale = sqlite::select((select::columns(quote("id")), select::from(quote("posts"))));
    let ids = sqlite::select((
        select::columns(quote("id")),
        select::from(quote("all_posts")),
    ));
    built(
        sqlite::delete((
            delete::with("all_posts", stale),
            delete::from(quote("post_tags")),
            delete::where_(quote("post_id").in_(subquery(ids))),
            delete::returning(quote("tag_id")),
        )),
        r#"WITH "all_posts" AS (SELECT "id" FROM "posts") DELETE FROM "post_tags" WHERE ("post_id" IN ((SELECT "id" FROM "all_posts"))) RETURNING "tag_id""#,
    );
}

#[test]
fn a_delete_with_no_table_refuses_to_build() {
    assert_eq!(
        sqlite::delete(delete::where_("1"))
            .build()
            .unwrap_err()
            .to_string(),
        "query is missing the table of a DELETE"
    );
}
