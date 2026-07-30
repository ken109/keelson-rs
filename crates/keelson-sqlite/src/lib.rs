//! The SQLite dialect for keelson.
//!
//! Four statement types, each shaped by the syntax diagrams at
//! <https://www.sqlite.org/lang.html>, plus the mods that fill them in and the
//! expression starters they are filled in with.
//!
//! ```
//! use keelson_sqlite as sqlite;
//! use keelson_sqlite::{Chain, Query, arg, quote, select};
//!
//! let q = sqlite::select((
//!     select::columns((quote("id"), quote("name"))),
//!     select::from(quote("users")),
//!     select::where_(quote("age").gte(arg(21i32))),
//! ));
//!
//! let (sql, args) = q.build()?;
//! assert_eq!(sql, r#"SELECT "id", "name" FROM "users" WHERE ("age" >= ?1)"#);
//! assert_eq!(args, vec![keelson_core::Value::I32(21)]);
//! # Ok::<_, keelson_core::Error>(())
//! ```
//!
//! # Where this sits
//!
//! Layer 1 of keelson, for SQLite. It is a complete way to use keelson on its
//! own: it depends only on [keelson-core](https://docs.rs/keelson-core) and produces a SQL
//! string and an argument list, which you may run with any driver you like. To
//! run it through keelson, add Layer 2 ([keelson-exec](https://docs.rs/keelson-exec) plus a
//! backend such as [keelson-sqlx](https://docs.rs/keelson-sqlx)); to have typed models built
//! out of these mods, add Layers 3 and 4 ([keelson-models](https://docs.rs/keelson-models),
//! [keelson-gen](https://docs.rs/keelson-gen)). Because SQLite needs no server, it is also
//! the engine keelson's own always-on end-to-end tests run against. The whole
//! map is the [keelson](https://docs.rs/keelson) facade crate.
//!
//! # How it is put together
//!
//! The assembly rules are the ones `keelson_psql` documents, because they are
//! keelson's rather than any dialect's: **a starter is a function of one mod**, and a
//! tuple of mods is a mod; **a mod module shares its name with its starter**, so
//! `sqlite::select` is both a function and a module; **a mod is written once**,
//! generic over the `keelson_core::clause` `Has*` trait it needs; and **a raw
//! `&str` works wherever an expression does**, so `select::from("users")` writes
//! `FROM users` while `select::from(quote("users"))` writes `FROM "users"`.
//!
//! # Where SQLite is not PostgreSQL
//!
//! This crate is hand-written against SQLite's grammar rather than derived from the
//! PostgreSQL one, and the differences are load-bearing. The five worth knowing
//! before writing a query:
//!
//! 1. **A compound operand takes no parentheses.** SQLite's `compound-select-stmt`
//!    is a run of bare `select-core`s, so `(SELECT 1) UNION (SELECT 2)` is a syntax
//!    error. Pass a query or [`query`] to [`select::union`], never [`subquery`].
//! 2. **There is one `ORDER BY`/`LIMIT` per statement**, and in a compound it
//!    belongs to the whole compound. PostgreSQL's `order_by_combined` family has
//!    nothing to correspond to here.
//! 3. **`OFFSET` is part of the `LIMIT` production**, so an offset with no limit is
//!    a build-time [`Error::Incomplete`].
//! 4. **`UNION ALL` is the only `ALL`.** `INTERSECT ALL` and `EXCEPT ALL` do not
//!    exist, which is why [`CompoundOp`] folds `ALL` into the operator instead of
//!    carrying it as a flag.
//! 5. **`VALUES (…), (…)` is a statement.** [`select::values`] and [`select::rows`]
//!    fill the other alternative of `select-core`.
//!
//! Beyond those: `?1` and `:name` placeholders, `INDEXED BY`/`NOT INDEXED` on any
//! table reference, `INSERT OR REPLACE` and `UPDATE OR IGNORE`, several
//! `ON CONFLICT` clauses on one `INSERT`, `RETURNING` on all three mutations, a
//! `CROSS JOIN` that takes an `ON` — and no locking clause, no `FETCH`, no
//! `TABLESAMPLE`, no `LATERAL`, no `GROUPING SETS`, no `DISTINCT ON`, and no
//! `USING` on a `DELETE`.
//!
//! # Sub-queries
//!
//! The four query types implement [`IntoExpr`], so one goes straight into any
//! expression slot: `select::union(other)`, `select::with("c", other)`,
//! `insert::query(other)`. Those slots want the query **bare** — SQLite parenthesises
//! neither a `WITH` body's contents twice nor a compound operand at all. Where the
//! parentheses belong to the sub-query itself — a `FROM` item, a scalar
//! sub-expression, an `IN (…)` operand — use [`subquery`]. Placeholders re-index
//! across the nesting on their own, because the counter belongs to the writer.

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
pub mod select;
pub mod update;
pub mod window;

pub use dialect::Sqlite;
pub use extras::{
    Compound, CompoundOp, Compounds, HasCompounds, HasOr, HasUpserts, Or, excluded, query, subquery,
};
pub use function::Function;
pub use ops::SqliteOps;
pub use statement::{
    DeleteQuery, HasExtraTables, HasTargetTable, InsertQuery, SelectQuery, UpdateQuery,
};

// The core vocabulary a caller needs in order to use any of the above, re-exported
// so that a program building SQLite queries needs one dependency and one `use`.
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
/// [`build()`](keelson_core::Query::build) takes no arguments.
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

/// Raw SQL whose `?` are rewritten to `?1`, `?2`, … with `args` interleaved. Write
/// `\?` for a literal question mark.
pub fn template(sql: impl Into<Cow<'static, str>>, args: impl IntoIterator<Item = RawArg>) -> Expr {
    expr::template(sql, args)
}

/// A single-quoted string literal. `s("A")` renders `'A'`.
///
/// Nothing is escaped: this is for SQL the program itself wrote — a keyword, an enum
/// label, a collation name. Text from outside belongs in [`arg`], where it is bound.
pub fn s(literal: impl Into<Cow<'static, str>>) -> Expr {
    expr::literal(literal)
}

/// A quoted identifier: `quote("age")` gives `"age"`, `quote(("users", "id"))` gives
/// `"users"."id"`.
pub fn quote(parts: impl IntoIdent) -> Expr {
    expr::quote(parts)
}

/// One bound argument, rendered `?n`.
pub fn arg(value: impl ToValue) -> Expr {
    expr::arg(value)
}

/// Several bound arguments, comma-separated and *not* parenthesised — for a slot
/// that brings its own, such as `VALUES (…)`.
pub fn args<V: ToValue>(values: impl IntoIterator<Item = V>) -> Expr {
    expr::args(values)
}

/// Several bound arguments, parenthesised: `(?1, ?2, ?3)`.
pub fn arg_group<V: ToValue>(values: impl IntoIterator<Item = V>) -> Expr {
    expr::arg_group(values)
}

/// A named parameter: `named("cutoff")` renders `:cutoff`.
///
/// SQLite is the one dialect keelson targets that has these, so this starter has no
/// counterpart in `keelson_psql`. A named parameter binds nothing and consumes no
/// positional slot — it exists so a statement can be prepared now and its values
/// supplied by whatever rebinds it.
pub fn named(name: impl Into<Cow<'static, str>>) -> Expr {
    expr::named(name)
}

/// `n` unbound `?n` placeholders, each binding `NULL`, so a statement can be
/// prepared now and its values supplied by whatever rebinds it.
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
/// Returns keelson-sqlite's own [`Function`], which carries `DISTINCT`, the
/// aggregate `ORDER BY`, `FILTER` and `OVER` — everything SQLite hangs off a call and
/// core deliberately does not know about.
pub fn f(name: impl Into<Cow<'static, str>>, args: impl IntoExprList) -> Function {
    Function::new(name, args)
}

/// A `CASE` expression: `case_().when(cond, then).else_(other)`.
///
/// Named with a trailing underscore because `case` is not available as a plain
/// identifier; the SQL is unaffected.
pub fn case_() -> CaseBuilder {
    expr::case()
}

/// `CAST(expr AS type_name)`.
///
/// SQLite has no `::` shorthand, so this is the only spelling. Not wrapped in
/// parentheses of its own: `CAST(…)` is already self-delimiting.
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
            r#"SELECT "id" FROM "users" WHERE ("id" = ?1)"#
        );
    }

    #[test]
    fn the_query_type_is_carried_rather_than_reparsed() {
        assert_eq!(select(()).query_type(), QueryType::Select);
        assert_eq!(insert(()).query_type(), QueryType::Insert);
        assert_eq!(update(()).query_type(), QueryType::Update);
        assert_eq!(delete(()).query_type(), QueryType::Delete);
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

    /// `LIMIT expr [ ( OFFSET | , ) expr ]`: there is no production in which an
    /// offset stands alone, so this is refused at build time rather than handed to
    /// the database.
    #[test]
    fn an_offset_without_a_limit_is_refused() {
        let q = select((select::from(quote("users")), select::offset(5)));
        let err = q.build().unwrap_err();
        // The substrings name the SQL concepts (the missing LIMIT, the dangling
        // OFFSET), not the message wording.
        assert!(
            matches!(&err, Error::Incomplete(what)
                if what.contains("LIMIT") && what.contains("OFFSET")),
            "got: {err}"
        );
    }

    /// A `VALUES` core is an alternative to the whole `SELECT` core, not a clause
    /// alongside it.
    #[test]
    fn a_values_statement_refuses_the_clauses_only_a_select_core_has() {
        // The substrings name the SQL concepts (VALUES plus the refused
        // clause), not the message wording.
        let q = select((select::values((1, 2)), select::from(quote("users"))));
        let err = q.build().unwrap_err();
        assert!(
            matches!(&err, Error::Other(msg)
                if msg.contains("VALUES") && msg.contains("FROM")),
            "got: {err}"
        );
        let q = select((select::values((1, 2)), select::distinct()));
        let err = q.build().unwrap_err();
        assert!(
            matches!(&err, Error::Other(msg)
                if msg.contains("VALUES") && msg.contains("DISTINCT")),
            "got: {err}"
        );
    }
}
