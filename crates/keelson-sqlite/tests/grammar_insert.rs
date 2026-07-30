//! `INSERT`, checked against SQLite's grammar, a real SQLite, and an expectation
//! derived from <https://www.sqlite.org/lang_insert.html> and
//! <https://www.sqlite.org/lang_upsert.html>.
//!
//! ```text
//! [ WITH [ RECURSIVE ] common-table-expression [, ...] ]
//! INSERT [ OR { ROLLBACK | ABORT | REPLACE | FAIL | IGNORE } ]
//!     INTO [ schema. ] table [ AS alias ] [ ( column [, ...] ) ]
//!     { VALUES ( expr [, ...] ) [, ...] [ upsert-clause ]
//!     | select-stmt [ upsert-clause ]
//!     | DEFAULT VALUES }
//!     [ RETURNING result-column [, ...] ]
//!
//! upsert-clause:
//!     ON CONFLICT [ ( indexed-column [, ...] ) [ WHERE expr ] ]
//!         { DO NOTHING | DO UPDATE SET assignment [, ...] [ WHERE expr ] }
//! ```

use keelson_sqlcheck::{Dialect, assert_sql};
use keelson_sqlite as sqlite;
use keelson_sqlite::{Chain, Query, Value, arg, excluded, insert, quote, raw, s, select};

#[track_caller]
fn built(q: impl Query, expected: &str) -> Vec<Value> {
    let (sql, args) = q.build().expect("the query must build");
    assert_sql(Dialect::Sqlite, &sql, expected);
    args
}

// ---------------------------------------------------------------------------
// The row source
// ---------------------------------------------------------------------------

#[test]
fn one_row_of_bound_values() {
    let args = built(
        sqlite::insert((
            insert::into(quote("users")).columns(["id", "name"]),
            insert::values((arg(1i32), arg("ada"))),
        )),
        r#"INSERT INTO "users" ("id", "name") VALUES (?1, ?2)"#,
    );
    assert_eq!(args, vec![Value::I32(1), Value::Text("ada".into())]);
}

#[test]
fn a_raw_table_name_and_column_list() {
    built(
        sqlite::insert((
            insert::into("tags").columns(["name"]),
            insert::values(arg("rust")),
        )),
        r#"INSERT INTO tags ("name") VALUES (?1)"#,
    );
}

#[test]
fn several_rows_at_once() {
    let args = built(
        sqlite::insert((
            insert::into(quote("post_tags")).columns(["post_id", "tag_id"]),
            insert::rows([(arg(1i32), arg(2i32)), (arg(3i32), arg(4i32))]),
        )),
        r#"INSERT INTO "post_tags" ("post_id", "tag_id") VALUES (?1, ?2), (?3, ?4)"#,
    );
    assert_eq!(args.len(), 4);
}

#[test]
fn several_values_calls_accumulate_as_rows() {
    built(
        sqlite::insert((
            insert::into(quote("tags")).columns(["name"]),
            insert::values(s("a")),
            insert::values(s("b")),
        )),
        r#"INSERT INTO "tags" ("name") VALUES ('a'), ('b')"#,
    );
}

/// The third alternative of the row source. An empty [`Values`] is what
/// `DEFAULT VALUES` *is* here, because an absent clause has to render nothing.
#[test]
fn no_rows_at_all_is_default_values() {
    built(
        sqlite::insert(insert::into(quote("users"))),
        r#"INSERT INTO "users" DEFAULT VALUES"#,
    );
}

#[test]
fn a_target_alias_precedes_the_column_list() {
    built(
        sqlite::insert((
            insert::into(quote("users"))
                .as_("u")
                .columns(["id", "name"]),
            insert::values((arg(1i32), arg("ada"))),
        )),
        r#"INSERT INTO "users" AS "u" ("id", "name") VALUES (?1, ?2)"#,
    );
}

#[test]
fn inserting_the_results_of_a_query() {
    let source = sqlite::select((
        select::columns(quote("name")),
        select::from(quote("users")),
        select::where_(quote("is_active").eq(arg(1i32))),
    ));
    let args = built(
        sqlite::insert((
            insert::into(quote("tags")).columns(["name"]),
            insert::query(source),
        )),
        r#"INSERT INTO "tags" ("name") SELECT "name" FROM "users" WHERE ("is_active" = ?1)"#,
    );
    assert_eq!(args, vec![Value::I32(1)]);
}

/// A query row source replaces any rows already added: the two are alternatives in
/// the grammar rather than things that combine, and the arguments of the discarded
/// rows go with them.
#[test]
fn a_query_row_source_replaces_recorded_rows() {
    let source = sqlite::select((select::columns(quote("name")), select::from(quote("users"))));
    let args = built(
        sqlite::insert((
            insert::into(quote("tags")).columns(["name"]),
            insert::values(arg("dropped")),
            insert::query(source),
        )),
        r#"INSERT INTO "tags" ("name") SELECT "name" FROM "users""#,
    );
    assert!(args.is_empty());
}

// ---------------------------------------------------------------------------
// OR <conflict-algorithm>
// ---------------------------------------------------------------------------

/// The five keywords of `conflict-clause`, in the order the diagram lists them.
#[test]
fn every_conflict_algorithm() {
    let rows = || {
        (
            insert::into(quote("tags")).columns(["name"]),
            insert::values(s("a")),
        )
    };
    built(
        sqlite::insert((insert::or_rollback(), rows())),
        r#"INSERT OR ROLLBACK INTO "tags" ("name") VALUES ('a')"#,
    );
    built(
        sqlite::insert((insert::or_abort(), rows())),
        r#"INSERT OR ABORT INTO "tags" ("name") VALUES ('a')"#,
    );
    built(
        sqlite::insert((insert::or_replace(), rows())),
        r#"INSERT OR REPLACE INTO "tags" ("name") VALUES ('a')"#,
    );
    built(
        sqlite::insert((insert::or_fail(), rows())),
        r#"INSERT OR FAIL INTO "tags" ("name") VALUES ('a')"#,
    );
    built(
        sqlite::insert((insert::or_ignore(), rows())),
        r#"INSERT OR IGNORE INTO "tags" ("name") VALUES ('a')"#,
    );
}

#[test]
fn the_conflict_algorithm_precedes_into() {
    // `INSERT OR REPLACE INTO …` is what SQLite's standalone `REPLACE INTO` is short
    // for; only the longer spelling is produced.
    built(
        sqlite::insert((
            insert::or_replace(),
            insert::into(quote("post_tags")).columns(["post_id", "tag_id"]),
            insert::values((arg(1i32), arg(2i32))),
        )),
        r#"INSERT OR REPLACE INTO "post_tags" ("post_id", "tag_id") VALUES (?1, ?2)"#,
    );
}

// ---------------------------------------------------------------------------
// The upsert clause
// ---------------------------------------------------------------------------

#[test]
fn do_nothing_with_an_inferred_target() {
    built(
        sqlite::insert((
            insert::into(quote("tags")).columns(["name"]),
            insert::values(arg("rust")),
            insert::on_conflict(quote("name")).do_nothing(),
        )),
        r#"INSERT INTO "tags" ("name") VALUES (?1) ON CONFLICT ("name") DO NOTHING"#,
    );
}

/// `ON CONFLICT [ ( … ) ]` — the target is optional, and `DO NOTHING` is the only
/// action that works without one.
#[test]
fn do_nothing_with_no_target_at_all() {
    built(
        sqlite::insert((
            insert::into(quote("tags")).columns(["name"]),
            insert::values(arg("rust")),
            insert::on_conflict(()).do_nothing(),
        )),
        r#"INSERT INTO "tags" ("name") VALUES (?1) ON CONFLICT DO NOTHING"#,
    );
}

#[test]
fn a_multi_column_conflict_target() {
    built(
        sqlite::insert((
            insert::into(quote("post_tags")).columns(["post_id", "tag_id"]),
            insert::values((arg(1i32), arg(2i32))),
            insert::on_conflict((quote("post_id"), quote("tag_id"))).do_nothing(),
        )),
        r#"INSERT INTO "post_tags" ("post_id", "tag_id") VALUES (?1, ?2) ON CONFLICT ("post_id", "tag_id") DO NOTHING"#,
    );
}

/// The pseudo-table is `excluded`, lower case and unquoted — SQLite's own spelling,
/// where PostgreSQL writes `EXCLUDED`.
#[test]
fn do_update_from_the_excluded_row() {
    built(
        sqlite::insert((
            insert::into(quote("tags")).columns(["id", "name"]),
            insert::values((arg(1i32), arg("rust"))),
            insert::on_conflict(quote("name")).do_update(insert::set_excluded(["name"])),
        )),
        r#"INSERT INTO "tags" ("id", "name") VALUES (?1, ?2) ON CONFLICT ("name") DO UPDATE SET "name" = excluded."name""#,
    );
}

#[test]
fn set_excluded_over_several_columns() {
    built(
        sqlite::insert((
            insert::into(quote("users")).columns(["id", "name", "email"]),
            insert::values((arg(1i32), arg("ada"), arg("ada@example.com"))),
            insert::on_conflict(quote("id")).do_update(insert::set_excluded(["name", "email"])),
        )),
        r#"INSERT INTO "users" ("id", "name", "email") VALUES (?1, ?2, ?3) ON CONFLICT ("id") DO UPDATE SET "name" = excluded."name", "email" = excluded."email""#,
    );
}

#[test]
fn an_assignment_written_out() {
    let args = built(
        sqlite::insert((
            insert::into(quote("posts")).columns(["id", "user_id", "title"]),
            insert::values((arg(1i32), arg(2i32), arg("hello"))),
            insert::on_conflict(quote("id")).do_update((
                insert::set_col("views").to(quote(("posts", "views")).plus(1)),
                insert::set_col("title").to_arg("hello again"),
            )),
        )),
        r#"INSERT INTO "posts" ("id", "user_id", "title") VALUES (?1, ?2, ?3) ON CONFLICT ("id") DO UPDATE SET "views" = ("posts"."views" + 1), "title" = ?4"#,
    );
    assert_eq!(args.len(), 4);
}

#[test]
fn the_excluded_helper_qualifies_one_column() {
    built(
        sqlite::insert((
            insert::into(quote("users")).columns(["id", "name"]),
            insert::values((arg(1i32), arg("ada"))),
            insert::on_conflict(quote("id"))
                .do_update(insert::set_col("name").to(excluded("name"))),
        )),
        r#"INSERT INTO "users" ("id", "name") VALUES (?1, ?2) ON CONFLICT ("id") DO UPDATE SET "name" = excluded."name""#,
    );
}

/// The action's `WHERE` filters which conflicting rows are updated.
#[test]
fn do_update_with_a_row_filter() {
    let args = built(
        sqlite::insert((
            insert::into(quote("users")).columns(["id", "name"]),
            insert::values((arg(1i32), arg("ada"))),
            insert::on_conflict(quote("id")).do_update((
                insert::set_excluded(["name"]),
                insert::where_(quote(("users", "age")).gt(arg(18i32))),
            )),
        )),
        r#"INSERT INTO "users" ("id", "name") VALUES (?1, ?2) ON CONFLICT ("id") DO UPDATE SET "name" = excluded."name" WHERE ("users"."age" > ?3)"#,
    );
    assert_eq!(args.len(), 3);
}

/// The target's `WHERE` is the *index* predicate, matched against a partial unique
/// index's own definition rather than evaluated per row. Both `WHERE`s in one
/// statement is the shape most easily got wrong.
#[test]
fn the_index_predicate_and_the_row_filter_are_different_wheres() {
    built(
        sqlite::insert((
            insert::into(quote("users")).columns(["id", "name"]),
            insert::values((arg(1i32), arg("ada"))),
            insert::on_conflict(quote("id"))
                .where_(quote("id").gt(0))
                .do_update((
                    insert::set_excluded(["name"]),
                    insert::where_(quote(("users", "email")).is_not_null()),
                )),
        )),
        r#"INSERT INTO "users" ("id", "name") VALUES (?1, ?2) ON CONFLICT ("id") WHERE ("id" > 0) DO UPDATE SET "name" = excluded."name" WHERE ("users"."email" IS NOT NULL)"#,
    );
}

/// SQLite 3.35 and later try several upsert clauses in order, and only the last may
/// omit its conflict target. PostgreSQL has exactly one, which is why this dialect
/// keeps a list where that one keeps a single slot.
#[test]
fn several_upsert_clauses_are_tried_in_order() {
    built(
        sqlite::insert((
            insert::into(quote("users")).columns(["id", "name"]),
            insert::values((arg(1i32), arg("ada"))),
            insert::on_conflict(quote("id")).do_update(insert::set_excluded(["name"])),
            insert::on_conflict(()).do_nothing(),
        )),
        r#"INSERT INTO "users" ("id", "name") VALUES (?1, ?2) ON CONFLICT ("id") DO UPDATE SET "name" = excluded."name" ON CONFLICT DO NOTHING"#,
    );
}

/// A `select-stmt` row source followed by an upsert clause needs a `WHERE`, even a
/// trivial one, or the parser reads the `ON` as a join condition. SQLite's own
/// parser says so in as many words, so this is checked rather than merely believed.
#[test]
fn a_query_row_source_with_an_upsert_needs_a_where() {
    let source = sqlite::select((
        select::columns(quote("name")),
        select::from(quote("users")),
        select::where_(quote("is_active").eq(1)),
    ));
    built(
        sqlite::insert((
            insert::into(quote("tags")).columns(["name"]),
            insert::query(source),
            insert::on_conflict(quote("name")).do_nothing(),
        )),
        r#"INSERT INTO "tags" ("name") SELECT "name" FROM "users" WHERE ("is_active" = 1) ON CONFLICT ("name") DO NOTHING"#,
    );
}

// ---------------------------------------------------------------------------
// RETURNING and WITH
// ---------------------------------------------------------------------------

#[test]
fn returning_a_star() {
    built(
        sqlite::insert((
            insert::into(quote("tags")).columns(["name"]),
            insert::values(arg("rust")),
            insert::returning("*"),
        )),
        r#"INSERT INTO "tags" ("name") VALUES (?1) RETURNING *"#,
    );
}

#[test]
fn returning_named_columns_and_an_expression() {
    built(
        sqlite::insert((
            insert::into(quote("tags")).columns(["name"]),
            insert::values(arg("rust")),
            insert::returning((quote("id"), quote("id").plus(1).as_("next"))),
        )),
        r#"INSERT INTO "tags" ("name") VALUES (?1) RETURNING "id", ("id" + 1) AS "next""#,
    );
}

#[test]
fn returning_after_an_upsert() {
    built(
        sqlite::insert((
            insert::into(quote("tags")).columns(["name"]),
            insert::values(arg("rust")),
            insert::on_conflict(quote("name")).do_nothing(),
            insert::returning(quote("id")),
        )),
        r#"INSERT INTO "tags" ("name") VALUES (?1) ON CONFLICT ("name") DO NOTHING RETURNING "id""#,
    );
}

#[test]
fn returning_from_default_values() {
    built(
        sqlite::insert((insert::into(quote("users")), insert::returning(quote("id")))),
        r#"INSERT INTO "users" DEFAULT VALUES RETURNING "id""#,
    );
}

#[test]
fn a_cte_in_front_of_an_insert() {
    let source = sqlite::select((
        select::columns(quote("name")),
        select::from(quote("recent")),
    ));
    let cte = sqlite::select((
        select::columns(quote("name")),
        select::from(quote("users")),
        select::where_(quote("age").gt(arg(18i32))),
    ));
    built(
        sqlite::insert((
            insert::with("recent", cte),
            insert::into(quote("tags")).columns(["name"]),
            insert::query(source),
        )),
        r#"WITH "recent" AS (SELECT "name" FROM "users" WHERE ("age" > ?1)) INSERT INTO "tags" ("name") SELECT "name" FROM "recent""#,
    );
}

#[test]
fn a_recursive_cte_in_front_of_an_insert() {
    let step = sqlite::select((
        select::columns(quote("n").plus(1)),
        select::from(quote("series")),
        select::where_(quote("n").lt(3)),
    ));
    let seed = sqlite::select((select::values(1), select::union_all(step)));
    let source = sqlite::select((
        select::columns(raw("'tag-' || \"n\"")),
        select::from(quote("series")),
    ));
    built(
        sqlite::insert((
            insert::recursive(true),
            insert::with("series", seed).columns(["n"]),
            insert::into(quote("tags")).columns(["name"]),
            insert::query(source),
        )),
        r#"WITH RECURSIVE "series" ("n") AS (VALUES (1) UNION ALL SELECT ("n" + 1) FROM "series" WHERE ("n" < 3)) INSERT INTO "tags" ("name") SELECT 'tag-' || "n" FROM "series""#,
    );
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

#[test]
fn an_insert_with_no_target_table_refuses_to_build() {
    let err = sqlite::insert(insert::values(arg(1i32)))
        .build()
        .unwrap_err();
    // The substring names the SQL concept (an INSERT's target table), not the
    // message wording.
    assert!(
        matches!(&err, sqlite::Error::Incomplete(what) if what.contains("INSERT")),
        "got: {err}"
    );
}

/// `DO UPDATE` with no `SET` does not parse, so it is refused rather than written.
#[test]
fn do_update_with_no_assignments_refuses_to_build() {
    let err = sqlite::insert((
        insert::into(quote("tags")).columns(["name"]),
        insert::values(arg("rust")),
        insert::on_conflict(quote("name")).do_update(()),
    ))
    .build()
    .unwrap_err();
    // The substring names the SQL concept (the missing assignments), not the
    // message wording.
    assert!(
        matches!(&err, sqlite::Error::Incomplete(what) if what.contains("assignments")),
        "got: {err}"
    );
}

/// In the grammar the `upsert-clause` hangs off the `VALUES` and `select-stmt`
/// alternatives only, never off `DEFAULT VALUES` — and SQLite's own parser agrees,
/// rejecting `DEFAULT VALUES ON CONFLICT …` outright. So the combination is refused
/// at build time rather than handed over to be rejected.
#[test]
fn default_values_with_an_upsert_refuses_to_build() {
    let err = sqlite::insert((
        insert::into(quote("users")),
        insert::on_conflict(()).do_nothing(),
    ))
    .build()
    .unwrap_err();
    // The substrings name the SQL concepts (DEFAULT VALUES vs ON CONFLICT),
    // not the message wording.
    assert!(
        matches!(&err, sqlite::Error::Other(msg)
            if msg.contains("DEFAULT VALUES") && msg.contains("ON CONFLICT")),
        "got: {err}"
    );
}

/// The index predicate hangs off the parenthesised column list and cannot stand
/// without one — `ON CONFLICT WHERE …` is not a production.
#[test]
fn an_index_predicate_with_no_column_list_refuses_to_build() {
    let err = sqlite::insert((
        insert::into(quote("tags")).columns(["name"]),
        insert::values(arg("rust")),
        insert::on_conflict(())
            .where_(quote("id").gt(0))
            .do_nothing(),
    ))
    .build()
    .unwrap_err();
    // The substring names the SQL concept (the missing column list), not the
    // message wording.
    assert!(
        matches!(&err, sqlite::Error::Incomplete(what) if what.contains("column list")),
        "got: {err}"
    );
}
