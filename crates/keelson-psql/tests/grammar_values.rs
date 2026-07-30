//! The two `SELECT` shorthands of PostgreSQL's grammar: the standalone
//! `VALUES` statement and the `TABLE` command.
//!
//! Every case goes through [`assert_sql`] — libpg_query, a real PostgreSQL 17
//! under `--features live-docker`, and the whitespace-normalised comparison.
//!
//! **Where the expected strings come from.**
//! <https://www.postgresql.org/docs/17/sql-values.html>:
//!
//! ```text
//! VALUES ( expression [, ...] ) [, ...]
//!     [ ORDER BY sort_expression [ ASC | DESC | USING operator ] [, ...] ]
//!     [ LIMIT { count | ALL } ] [ OFFSET start [ ROW | ROWS ] ]
//!     [ FETCH { FIRST | NEXT } [ count ] { ROW | ROWS } { ONLY | WITH TIES } ]
//! ```
//!
//! and <https://www.postgresql.org/docs/17/sql-select.html>, the `TABLE`
//! section: `TABLE [ ONLY ] table_name [ * ]`, allowed only with `WITH`, the
//! set operations, `ORDER BY`, `LIMIT`/`OFFSET`/`FETCH` and the locking
//! clauses. The columns of a `VALUES` result are named `column1`, `column2`, …
//! — that is what its `ORDER BY` refers to. None of the strings was produced
//! by running the builder and pasting its output.
//!
//! **Every table named here is in `tests/schema/psql.sql`.**

use keelson_psql as psql;
use keelson_psql::{
    Chain, Error, Query, Value, arg, cast, quote, raw, select, subquery, table, values,
};
use keelson_sqlcheck::{Dialect, assert_sql};

/// Build, then run every check this build can: grammar, engine, and intent.
#[track_caller]
fn check(q: &impl Query, expected: &str) -> Vec<Value> {
    let (sql, args) = q.build().expect("the query should build");
    assert_sql(Dialect::Psql, &sql, expected);
    args
}

// ---------------------------------------------------------------------------
// VALUES as a statement of its own
// ---------------------------------------------------------------------------

#[test]
fn a_values_statement_stands_alone() {
    let q = psql::values((
        values::row((arg(1i32), arg("ada"))),
        values::row((arg(2i32), arg("bab"))),
    ));
    let args = check(&q, "VALUES ($1, $2), ($3, $4)");
    assert_eq!(
        args,
        vec![
            Value::I32(1),
            Value::Text("ada".into()),
            Value::I32(2),
            Value::Text("bab".into()),
        ]
    );
}

/// The tail clauses are the `SELECT` ones, in the same order. The single-cell
/// row keeps the shape minimal; `ORDER BY 1` names the first output column, as
/// sql-values.html suggests for a `VALUES` sort.
#[test]
fn a_values_statement_carries_its_own_tail_clauses() {
    let q = psql::values((
        values::row(arg(3i32)),
        values::order_by(raw("1")).desc(),
        values::limit(10),
        values::offset(2),
    ));
    let args = check(&q, "VALUES ($1) ORDER BY 1 DESC LIMIT 10 OFFSET 2");
    assert_eq!(args, vec![Value::I32(3)]);
}

/// `column1`, `column2`, … are the result's column names, so an `ORDER BY` may
/// name one; `FETCH` is the standard spelling of the limit, exactly as on a
/// `SELECT` — and, like there, `LIMIT` and `FETCH` together are a build error,
/// pinned below.
#[test]
fn values_orders_by_its_generated_column_names_and_takes_fetch() {
    let q = psql::values((
        values::rows([(arg(1i32), arg("a")), (arg(2i32), arg("b"))]),
        values::order_by(quote("column2")).asc(),
        values::fetch(1).with_ties(),
    ));
    check(
        &q,
        r#"VALUES ($1, $2), ($3, $4) ORDER BY "column2" ASC
           FETCH NEXT 1 ROWS WITH TIES"#,
    );
}

/// A `VALUES` is a `simple_select` alternative, so it combines: here the
/// leading `VALUES` carries its own `LIMIT`, which is exactly the case where
/// the leading query must be parenthesised so the trailing `ORDER BY` reads
/// against the combination. The cell is cast because a placeholder with no
/// other context resolves to `text`, and a `text`/`integer` union has no
/// common type the engine will pick.
#[test]
fn a_values_statement_participates_in_set_operations() {
    let q = psql::values((
        values::row(cast(arg(1i32), "int")),
        values::limit(1),
        values::union(psql::select((
            select::columns(quote("id")),
            select::from(quote("tags")),
        ))),
        values::order_by_combined(raw("1")),
    ));
    check(
        &q,
        r#"(VALUES (CAST($1 AS int)) LIMIT 1) UNION (SELECT "id" FROM "tags") ORDER BY 1"#,
    );
}

/// `WITH` precedes a `VALUES` statement, and a cell may be a scalar sub-query
/// over the CTE — which is how a CTE is reachable from `VALUES` at all.
#[test]
fn a_with_clause_precedes_values_and_a_cell_reads_it() {
    let biggest = psql::select((
        select::columns(psql::f("max", quote("id"))),
        select::from(quote("tags")),
    ));
    let q = psql::values((
        values::with("m", biggest),
        values::row((subquery(psql::select((
            select::columns(quote("max")),
            select::from(quote("m")),
        ))),)),
    ));
    check(
        &q,
        r#"WITH "m" AS (SELECT max("id") FROM "tags")
           VALUES ((SELECT "max" FROM "m"))"#,
    );
}

// ---------------------------------------------------------------------------
// VALUES as a from-item
// ---------------------------------------------------------------------------

/// A parenthesised `VALUES` is a `from_item` sub-query, and PostgreSQL requires
/// the alias there; the column-alias list is what names its columns something
/// better than `column1`.
#[test]
fn a_values_list_is_a_from_item_under_an_alias() {
    let vals = psql::values(values::rows([(arg(1i32), arg("a")), (arg(2i32), arg("b"))]));
    let q = psql::select((
        select::columns((quote(("v", "id")), quote(("v", "name")))),
        select::from(subquery(vals))
            .as_("v")
            .columns(["id", "name"]),
    ));
    let args = check(
        &q,
        r#"SELECT "v"."id", "v"."name"
           FROM (VALUES ($1, $2), ($3, $4)) AS "v" ("id", "name")"#,
    );
    assert_eq!(args.len(), 4);
}

/// The same from-item joins like any other: the idiom for updating many rows
/// from a literal list. The first row's key cell is cast so the `"id"` column
/// resolves to `integer` — a bare placeholder there resolves to `text`, and
/// `integer = text` has no operator; the second row's placeholder takes the
/// column's resolved type.
#[test]
fn a_values_from_item_drives_an_update() {
    use keelson_psql::update;
    let vals = psql::values(values::rows([
        (cast(arg(1i32), "int"), arg("x")),
        (arg(2i32), arg("y")),
    ]));
    let q = psql::update((
        update::table(quote("tags")),
        update::set_col("name").to(quote(("v", "name"))),
        update::from(subquery(vals))
            .as_("v")
            .columns(["id", "name"]),
        update::where_(quote(("tags", "id")).eq(quote(("v", "id")))),
    ));
    check(
        &q,
        r#"UPDATE "tags" SET "name" = "v"."name"
           FROM (VALUES (CAST($1 AS int), $2), ($3, $4)) AS "v" ("id", "name")
           WHERE ("tags"."id" = "v"."id")"#,
    );
}

// ---------------------------------------------------------------------------
// What a VALUES statement refuses
// ---------------------------------------------------------------------------

#[test]
fn a_values_statement_without_rows_is_a_recorded_failure() {
    let err = psql::values(()).build().unwrap_err();
    // The substring names the SQL concept, not the message wording.
    assert!(
        matches!(&err, Error::Incomplete(what) if what.contains("VALUES")),
        "got: {err}"
    );
}

/// `LIMIT` and `FETCH` are one production's two spellings on a `VALUES` exactly
/// as on a `SELECT`, so both at once is the same recorded collision.
#[test]
fn a_values_statement_with_limit_and_fetch_is_a_recorded_failure() {
    let q = psql::values((values::row(arg(1i32)), values::limit(1), values::fetch(2)));
    let err = q.build().unwrap_err();
    assert!(
        matches!(
            &err,
            Error::ConflictingClauses {
                first: "LIMIT",
                second: "FETCH"
            }
        ),
        "got: {err}"
    );
}

// ---------------------------------------------------------------------------
// TABLE
// ---------------------------------------------------------------------------

#[test]
fn table_is_the_select_star_shorthand() {
    let q = psql::table(table::name(quote("users")));
    assert!(check(&q, r#"TABLE "users""#).is_empty());
}

/// `TABLE [ ONLY ] table_name`, with the tail clauses the manual allows —
/// `ORDER BY`, `LIMIT`/`OFFSET` and a locking clause. `WHERE` is not among
/// them, and `table::where_` does not compile.
#[test]
fn table_only_with_its_allowed_tail_clauses() {
    let q = psql::table((
        table::name(quote("users")).only(),
        table::order_by(quote("id")).desc(),
        table::limit(5),
        table::offset(1),
        table::for_update().skip_locked(),
    ));
    check(
        &q,
        r#"TABLE ONLY "users" ORDER BY "id" DESC LIMIT 5 OFFSET 1
           FOR UPDATE SKIP LOCKED"#,
    );
}

/// The manual's own use for `TABLE`: a space-saving operand of a set
/// operation, on either side.
#[test]
fn table_combines_with_a_select_and_with_itself() {
    let q = psql::table((
        table::name(quote("users")),
        table::except(psql::select((
            select::columns((
                quote("id"),
                quote("name"),
                quote("email"),
                quote("age"),
                quote("is_active"),
                quote("created_at"),
            )),
            select::from(quote("users")),
            select::where_(psql::not(quote("is_active"))),
        ))),
    ));
    check(
        &q,
        r#"TABLE "users" EXCEPT
           (SELECT "id", "name", "email", "age", "is_active", "created_at"
            FROM "users" WHERE NOT "is_active")"#,
    );

    let q = psql::select((
        select::columns((quote("id"), quote("name"))),
        select::from(quote("tags")),
        select::union(psql::table(table::name(quote("tags")).only())),
    ));
    check(
        &q,
        r#"SELECT "id", "name" FROM "tags" UNION (TABLE ONLY "tags")"#,
    );
}

/// `WITH` precedes `TABLE`, and the CTE name is a legal table there — the
/// shortest way to read a CTE whole.
#[test]
fn with_table_reads_a_cte_whole() {
    let recent = psql::select((
        select::columns((quote("id"), quote("name"))),
        select::from(quote("tags")),
        select::where_(quote("id").gt(arg(10i32))),
    ));
    let q = psql::table((table::with("recent", recent), table::name(quote("recent"))));
    let args = check(
        &q,
        r#"WITH "recent" AS (SELECT "id", "name" FROM "tags" WHERE ("id" > $1))
           TABLE "recent""#,
    );
    assert_eq!(args, vec![Value::I32(10)]);
}

#[test]
fn a_table_statement_without_a_table_is_a_recorded_failure() {
    let err = psql::table(()).build().unwrap_err();
    assert!(
        matches!(&err, Error::Incomplete(what) if what.contains("TABLE")),
        "got: {err}"
    );
}
