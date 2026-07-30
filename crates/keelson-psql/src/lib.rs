//! The PostgreSQL dialect for keelson.
//!
//! The statement types — `SELECT`, `INSERT`, `UPDATE`, `DELETE`, `MERGE`, and
//! the `VALUES` and `TABLE` shorthands — each shaped by the production in
//! PostgreSQL's own reference manual, plus the mods that fill them in and the
//! expression starters they are filled in with.
//!
//! ```
//! use keelson_psql as psql;
//! use keelson_psql::{Chain, Query, arg, quote, select};
//!
//! let q = psql::select((
//!     select::columns((quote("id"), quote("name"))),
//!     select::from(quote("users")),
//!     select::where_(quote("age").gte(arg(21i32))),
//! ));
//!
//! let (sql, args) = q.build()?;
//! assert_eq!(sql, r#"SELECT "id", "name" FROM "users" WHERE ("age" >= $1)"#);
//! assert_eq!(args, vec![keelson_core::Value::I32(21)]);
//! # Ok::<_, keelson_core::Error>(())
//! ```
//!
//! # Where this sits
//!
//! Layer 1 of keelson, for PostgreSQL. It is a complete way to use keelson on
//! its own: it depends only on [keelson-core](https://docs.rs/keelson-core) and produces a
//! SQL string and an argument list, which you may run with any driver you like.
//! To run it through keelson, add Layer 2 ([keelson-exec](https://docs.rs/keelson-exec) plus a
//! backend such as [keelson-sqlx](https://docs.rs/keelson-sqlx)); to have typed models built
//! out of these mods, add Layers 3 and 4 ([keelson-models](https://docs.rs/keelson-models),
//! [keelson-gen](https://docs.rs/keelson-gen)). The whole map, and one dependency line for it,
//! is the [keelson](https://docs.rs/keelson) facade crate.
//!
//! # How it is put together
//!
//! **A starter is a function of one mod.** `psql::select(mods)` takes a single
//! `impl Mod<SelectQuery>` — and a tuple of mods is a mod, so
//! `psql::select(())` and `psql::select((a, b, c))` are both that one argument.
//! Arity is never a ceiling, because tuples nest.
//!
//! **A mod module shares its name with its starter.** `psql::select` is a function
//! *and* a module: Rust keeps values and modules in separate namespaces, so
//! `psql::select((select::from("users"),))` needs no import gymnastics. The modules
//! are named after the statement — [`select`](mod@select), [`insert`](mod@insert),
//! [`update`](mod@update), [`delete`](mod@delete),
//! [`window`], [`frame`] — never bob's `sm`/`im`/`um`/`dm`/`wm`/`fm`.
//!
//! **A mod is written once.** The mods live in one place, generic over the
//! `keelson_core::clause` `Has*` trait they need, and each statement module
//! re-exports the ones that apply to it. `select::where_` and `update::where_` are
//! the same function; `insert::where_` is too, and resolves only against the
//! `ON CONFLICT … DO UPDATE` body, because an `INSERT` has no `WHERE` of its own.
//! An inapplicable mod is a compile error.
//!
//! **A raw `&str` works wherever an expression does.** Every slot takes
//! `impl IntoExpr`, and a `&'static str` is raw SQL. `select::from("users")` writes
//! `FROM users`; `select::from(quote("users"))` writes `FROM "users"`.
//!
//! # Sub-queries
//!
//! The four query types implement [`IntoExpr`], so one goes straight into any
//! expression slot: `select::union(other)`, `select::with("c", other)`,
//! `insert::query(other)`. Those slots supply their own parentheses. Where the
//! parentheses belong to the sub-query itself — a `FROM` item, a scalar
//! sub-expression — use [`subquery`]. Placeholders re-index across the nesting on
//! their own, because the counter belongs to the writer.

#![warn(missing_docs)]

mod dialect;
mod extras;
mod function;
mod ops;
pub mod shared;
mod statement;

pub mod delete;
pub mod frame;
pub mod insert;
pub mod merge;
pub mod select;
pub mod table;
pub mod update;
pub mod values;
pub mod window;

pub use dialect::Psql;
pub use extras::{Distinct, Overriding, cube, excluded, grouping_sets, query, rollup, subquery};
pub use function::{ColumnDef, Function, TableFunction};
pub use ops::PsqlOps;
pub use statement::{
    DeleteQuery, HasExtraTables, HasTargetTable, InsertQuery, MergeAction, MergeInsert,
    MergeMatchKind, MergeQuery, MergeWhen, SelectQuery, TableQuery, UpdateQuery, ValuesQuery,
};

// The core vocabulary a caller needs in order to use any of the above, re-exported
// so that a program building PostgreSQL queries needs one dependency and one `use`.
pub use keelson_core::expr::{CaseBuilder, Chain, Expr, IntoExpr, IntoExprList, IntoIdent, RawArg};
pub use keelson_core::{Error, Mod, Query, QueryType, Result, Value};

use std::borrow::Cow;

use keelson_core::ToValue;
use keelson_core::expr;

// ---------------------------------------------------------------------------
// Statement starters
// ---------------------------------------------------------------------------

/// Build a `SELECT` from one mod — usually a tuple of them.
///
/// The returned query knows its own dialect, so
/// [`build()`](keelson_core::Query::build) takes no arguments. bob wraps its query
/// in a `BaseQuery` to carry the dialect; here the query type carries it itself.
pub fn select(mods: impl Mod<SelectQuery>) -> SelectQuery {
    let mut q = SelectQuery::default();
    mods.apply(&mut q);
    q
}

/// Build an `INSERT` from one mod.
pub fn insert(mods: impl Mod<InsertQuery>) -> InsertQuery {
    let mut q = InsertQuery::default();
    mods.apply(&mut q);
    q
}

/// Build an `UPDATE` from one mod.
pub fn update(mods: impl Mod<UpdateQuery>) -> UpdateQuery {
    let mut q = UpdateQuery::default();
    mods.apply(&mut q);
    q
}

/// Build a `DELETE` from one mod.
pub fn delete(mods: impl Mod<DeleteQuery>) -> DeleteQuery {
    let mut q = DeleteQuery::default();
    mods.apply(&mut q);
    q
}

/// Build a `MERGE` from one mod (PostgreSQL 15+).
///
/// The grammar requires a target ([`merge::into`]), a source ([`merge::using`]),
/// an [`merge::on`] condition and at least one `WHEN` clause; a `MERGE` missing
/// any of them is a [`build()`](keelson_core::Query::build) error naming the
/// absent piece.
pub fn merge(mods: impl Mod<MergeQuery>) -> MergeQuery {
    let mut q = MergeQuery::default();
    mods.apply(&mut q);
    q
}

/// Build a standalone `VALUES` statement from one mod.
///
/// The rows come from [`values::row`]/[`values::rows`]; with none the statement
/// is a [`build()`](keelson_core::Query::build) error, because `VALUES` with no
/// rows is not a statement.
pub fn values(mods: impl Mod<ValuesQuery>) -> ValuesQuery {
    let mut q = ValuesQuery::default();
    mods.apply(&mut q);
    q
}

/// Build a `TABLE name` command from one mod — PostgreSQL's shorthand for
/// `SELECT * FROM name`.
///
/// The table comes from [`table::name`]; with none the statement is a
/// [`build()`](keelson_core::Query::build) error.
pub fn table(mods: impl Mod<TableQuery>) -> TableQuery {
    let mut q = TableQuery::default();
    mods.apply(&mut q);
    q
}

// ---------------------------------------------------------------------------
// Expression starters
// ---------------------------------------------------------------------------

/// Raw SQL, verbatim. `?` is left alone — see [`template`].
///
/// The progressive-enhancement entry point: a hand-written fragment goes anywhere a
/// structured expression does.
pub fn raw(sql: impl Into<Cow<'static, str>>) -> Expr {
    expr::raw(sql)
}

/// Raw SQL whose `?` are rewritten to `$1`, `$2`, … with `args` interleaved. Write
/// `\?` for a literal question mark.
pub fn template(sql: impl Into<Cow<'static, str>>, args: impl IntoIterator<Item = RawArg>) -> Expr {
    expr::template(sql, args)
}

/// A single-quoted string literal — bob's `S`. `s("A")` renders `'A'`.
///
/// Nothing is escaped: this is for SQL the program itself wrote — a keyword, an enum
/// label. Text from outside belongs in [`arg`], where it is bound.
pub fn s(literal: impl Into<Cow<'static, str>>) -> Expr {
    expr::literal(literal)
}

/// A quoted identifier: `quote("age")` gives `"age"`, `quote(("users", "id"))` gives
/// `"users"."id"`.
pub fn quote(parts: impl IntoIdent) -> Expr {
    expr::quote(parts)
}

/// One bound argument, rendered `$n`.
pub fn arg(value: impl ToValue) -> Expr {
    expr::arg(value)
}

/// Several bound arguments, comma-separated and *not* parenthesised — for a slot
/// that brings its own, such as `VALUES (…)`.
pub fn args<V: ToValue>(values: impl IntoIterator<Item = V>) -> Expr {
    expr::args(values)
}

/// Several bound arguments, parenthesised: `($1, $2, $3)`.
pub fn arg_group<V: ToValue>(values: impl IntoIterator<Item = V>) -> Expr {
    expr::arg_group(values)
}

/// `n` unbound placeholders, each binding `NULL`, so a statement can be prepared
/// now and its values supplied by whatever rebinds it.
pub fn placeholders(n: usize) -> Expr {
    expr::placeholders(n)
}

/// A parenthesised, comma-separated list: `(a, b)`. One element gives plain
/// parentheses.
pub fn group(items: impl IntoExprList) -> Expr {
    expr::group(items)
}

/// A function call: `f("count", "*")`, `f("row_number", ()).over(())`.
///
/// Returns keelson-psql's own [`Function`], which carries `DISTINCT`,
/// `ORDER BY`, `WITHIN GROUP`, `FILTER`, column definitions and `OVER` — everything
/// PostgreSQL hangs off a call and core deliberately does not know about.
pub fn f(name: impl Into<Cow<'static, str>>, args: impl IntoExprList) -> Function {
    Function::new(name, args)
}

/// A `CASE` expression: `case_().when(cond, then).else_(other)`.
///
/// Named with a trailing underscore because `case` is not available as a plain
/// identifier in a way that reads well next to `match`; the SQL is unaffected.
pub fn case_() -> CaseBuilder {
    expr::case()
}

/// `CAST(expr AS type_name)`. [`PsqlOps::cast_to`] is the `::` shorthand.
///
/// Not wrapped in parentheses of its own: `CAST(…)` is already self-delimiting, so
/// a wrapping pair could never disambiguate anything.
pub fn cast(expression: impl IntoExpr, type_name: impl Into<Cow<'static, str>>) -> Expr {
    expr::cast(expression, type_name)
}

/// `NOT expr`. The operand is parenthesised if it needs it; the result is not,
/// because `NOT` binds looser than anything it can contain.
pub fn not(expression: impl IntoExpr) -> Expr {
    expr::not(expression)
}

/// `(a AND b AND c)`.
pub fn and(items: impl IntoExprList) -> Expr {
    expr::and(items)
}

/// `(a OR b OR c)`.
pub fn or(items: impl IntoExprList) -> Expr {
    expr::or(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_starter_takes_no_mods_at_all() {
        // `()` is a mod, so the empty query needs no separate constructor.
        assert_eq!(select(()).build().unwrap().0, "SELECT *");
    }

    #[test]
    fn apply_adds_mods_to_a_built_query_and_clone_leaves_the_original_alone() {
        let base = select((select::columns(quote("id")), select::from(quote("users"))));
        let mut narrowed = base.clone();
        narrowed.apply(select::where_(quote("id").eq(arg(1i32))));

        assert_eq!(
            base.build().unwrap().0,
            r#"SELECT "id" FROM "users""#,
            "the clone did not disturb the original"
        );
        assert_eq!(
            narrowed.build().unwrap().0,
            r#"SELECT "id" FROM "users" WHERE ("id" = $1)"#
        );
    }

    #[test]
    fn the_query_type_is_carried_rather_than_reparsed() {
        assert_eq!(select(()).query_type(), QueryType::Select);
        assert_eq!(insert(()).query_type(), QueryType::Insert);
        assert_eq!(update(()).query_type(), QueryType::Update);
        assert_eq!(delete(()).query_type(), QueryType::Delete);
        assert_eq!(merge(()).query_type(), QueryType::Merge);
        // VALUES and TABLE are SELECT shorthands: rows come back.
        assert_eq!(values(()).query_type(), QueryType::Select);
        assert_eq!(table(()).query_type(), QueryType::Select);
    }

    #[test]
    fn a_statement_missing_a_clause_it_cannot_render_without_says_so() {
        // The substrings name the SQL concepts, not the message wording.
        let err = insert(()).build().unwrap_err();
        assert!(
            matches!(&err, Error::Incomplete(what) if what.contains("INSERT")),
            "got: {err}"
        );
        let err = update(update::table(quote("users"))).build().unwrap_err();
        assert!(
            matches!(&err, Error::Incomplete(what)
                if what.contains("assignments") && what.contains("UPDATE")),
            "got: {err}"
        );
        let err = delete(()).build().unwrap_err();
        assert!(
            matches!(&err, Error::Incomplete(what) if what.contains("DELETE")),
            "got: {err}"
        );
    }
}
