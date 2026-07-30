//! The two table-value statements of MySQL 8.0.19+: `VALUES` and `TABLE`.
//! See `tests/common/mod.rs` for what each assertion runs and where the
//! expected strings come from.
//!
//! From <https://dev.mysql.com/doc/refman/8.4/en/values.html>:
//!
//! ```text
//! VALUES row_constructor_list [ORDER BY column_designator] [LIMIT number]
//! row_constructor_list: ROW(value_list) [, ROW(value_list)] ...
//! ```
//!
//! and <https://dev.mysql.com/doc/refman/8.4/en/table.html>:
//!
//! ```text
//! TABLE table_name [ORDER BY column_name] [LIMIT number [OFFSET number]]
//! ```
//!
//! A `VALUES` result's columns are named `column_0`, `column_1`, … — that is
//! what its `ORDER BY` refers to. `sqlparser` parses the `VALUES` statement but
//! not `TABLE` at all (its statement dispatch has no `TABLE` arm), so the
//! `TABLE` cases go through `check_without_grammar` and are judged by a real
//! MySQL under `--features live-docker`.

mod common;

use common::{check, check_without_grammar};
use keelson_mysql as mysql;
use keelson_mysql::{Error, Query, Value, arg, quote, subquery, table, values};

// ---------------------------------------------------------------------------
// VALUES
// ---------------------------------------------------------------------------

#[test]
fn a_values_statement_spells_each_row_with_the_row_keyword() {
    let q = mysql::values((
        values::row((arg(1i32), arg("ada"))),
        values::row((arg(2i32), arg("bab"))),
    ));
    let args = check(&q, "VALUES ROW(?, ?), ROW(?, ?)");
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

/// `ORDER BY column_designator` names a generated column, and `LIMIT` takes a
/// row count — the statement's whole tail.
#[test]
fn a_values_statement_orders_by_a_generated_column_and_limits() {
    let q = mysql::values((
        values::rows([(arg(1i32), arg("a")), (arg(2i32), arg("b"))]),
        values::order_by(quote("column_0")).desc(),
        values::limit(1),
    ));
    check(
        &q,
        "VALUES ROW(?, ?), ROW(?, ?) ORDER BY `column_0` DESC LIMIT 1",
    );
}

/// A parenthesised `VALUES` is a derived table (8.0.19+), whose columns keep
/// their generated names; the derived table needs its alias, as every MySQL
/// derived table does.
#[test]
fn a_values_statement_is_a_derived_table_under_an_alias() {
    use keelson_mysql::select;
    let vals = mysql::values(values::rows([(arg(1i32), arg("a")), (arg(2i32), arg("b"))]));
    let q = mysql::select((
        select::columns((quote(("v", "column_0")), quote(("v", "column_1")))),
        select::from(subquery(vals)).as_("v"),
    ));
    let args = check(
        &q,
        "SELECT `v`.`column_0`, `v`.`column_1` \
         FROM (VALUES ROW(?, ?), ROW(?, ?)) AS `v`",
    );
    assert_eq!(args.len(), 4);
}

#[test]
fn a_values_statement_without_rows_is_a_recorded_failure() {
    let err = mysql::values(()).build().unwrap_err();
    // The substring names the SQL concept, not the message wording.
    assert!(
        matches!(&err, Error::Incomplete(what) if what.contains("VALUES")),
        "got: {err}"
    );
}

// ---------------------------------------------------------------------------
// TABLE
// ---------------------------------------------------------------------------

#[test]
fn table_is_the_select_star_shorthand() {
    let q = mysql::table(table::name(quote("users")));
    assert!(check_without_grammar(&q, "TABLE `users`", "the TABLE statement").is_empty());
}

#[test]
fn table_with_its_whole_tail() {
    let q = mysql::table((
        table::name(quote("users")),
        table::order_by(quote("id")).desc(),
        table::limit(5),
        table::offset(1),
    ));
    check_without_grammar(
        &q,
        "TABLE `users` ORDER BY `id` DESC LIMIT 5 OFFSET 1",
        "the TABLE statement",
    );
}

#[test]
fn a_table_statement_without_a_table_is_a_recorded_failure() {
    let err = mysql::table(()).build().unwrap_err();
    assert!(
        matches!(&err, Error::Incomplete(what) if what.contains("TABLE")),
        "got: {err}"
    );
}
