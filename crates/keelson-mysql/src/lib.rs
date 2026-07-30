//! The MySQL dialect for keelson.
//!
//! Five statement types, each shaped by the production in the MySQL 8.4 reference
//! manual, plus the mods that fill them in and the expression starters they are
//! filled in with.
//!
//! ```
//! use keelson_mysql as mysql;
//! use keelson_mysql::{Chain, Query, arg, quote, select};
//!
//! let q = mysql::select((
//!     select::columns((quote("id"), quote("name"))),
//!     select::from(quote("users")),
//!     select::where_(quote("age").gte(arg(21i32))),
//! ));
//!
//! let (sql, args) = q.build()?;
//! assert_eq!(sql, "SELECT `id`, `name` FROM `users` WHERE (`age` >= ?)");
//! assert_eq!(args, vec![keelson_core::Value::I32(21)]);
//! # Ok::<_, keelson_core::Error>(())
//! ```
//!
//! # How it is put together
//!
//! **A starter is a function of one mod.** `mysql::select(mods)` takes a single
//! `impl Mod<SelectQuery>` — and a tuple of mods is a mod, so `mysql::select(())`
//! and `mysql::select((a, b, c))` are both that one argument. Arity is never a
//! ceiling, because tuples nest.
//!
//! **A mod module shares its name with its starter.** `mysql::select` is a function
//! *and* a module: Rust keeps values and modules in separate namespaces, so
//! `mysql::select((select::from("users"),))` needs no import gymnastics. The modules
//! are named after the statement — `select`, `insert`, `replace`, `update`,
//! `delete`, [`window`], [`frame`] — never bob's `sm`/`im`/`um`/`dm`/`wm`/`fm`.
//!
//! **A mod is written once.** The mods live in [`shared`], generic over the
//! `keelson_core::clause` `Has*` trait they need, and each statement module
//! re-exports the ones that apply to it. An inapplicable mod is a compile error.
//!
//! **A raw `&str` works wherever an expression does.** Every slot takes
//! `impl IntoExpr`, and a `&'static str` is raw SQL. `select::from("users")` writes
//! `FROM users`; `select::from(quote("users"))` writes ``FROM `users` ``.
//!
//! # What MySQL does not have
//!
//! The list is worth reading before looking for a mod that is not here, because each
//! absence is deliberate — a construct a dialect lacks does not exist for it.
//!
//! | absent | why |
//! |---|---|
//! | `RETURNING` | MySQL has none, on any statement. There is no `Returning` field in this crate. |
//! | `FETCH … ROWS ONLY` | not in the grammar; `LIMIT` is the only row limiter. |
//! | `DISTINCT ON` | PostgreSQL's. [`select::distinct`] is the whole of MySQL's. |
//! | `FULL JOIN` | not in the grammar. Three join kinds plus `CROSS` and `STRAIGHT_JOIN`. |
//! | `NULLS FIRST` / `NULLS LAST`, `USING operator` | not in `ORDER BY`; the chain has `asc`, `desc` and `collate`. |
//! | `GROUPS` frame mode, `EXCLUDE` | not in the frame clause. |
//! | `FILTER (WHERE …)`, `WITHIN GROUP` | not on a function call. |
//! | `ROLLUP(…)`, `CUBE(…)`, `GROUPING SETS(…)`, `GROUP BY DISTINCT` | `GROUP BY … WITH ROLLUP` is the only super-aggregate. |
//! | `ON CONFLICT` | MySQL's upsert is `ON DUPLICATE KEY UPDATE`, and [`REPLACE`](replace()). |
//! | `WITH` on `INSERT`/`REPLACE` | permitted only immediately before the sub-`SELECT`. |
//! | `TABLESAMPLE`, `ONLY`, `WITH ORDINALITY` | PostgreSQL's from-item decorations. |
//! | `IS DISTINCT FROM` | on the shared chain and *not* valid MySQL; use [`MysqlOps::null_safe_eq`]. |
//!
//! # Sub-queries
//!
//! The statement types implement [`IntoExpr`], so one goes straight into any
//! expression slot: `select::union(other)`, `select::with("c", other)`,
//! `insert::query(other)`. Those slots supply their own parentheses. Where the
//! parentheses belong to the sub-query itself — a derived table, a scalar
//! sub-expression — use [`subquery`]. Placeholders re-index across the nesting on
//! their own, because the counter belongs to the writer; that matters here even
//! though every MySQL placeholder looks the same, because the *argument order* is
//! what the counter fixes.

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
pub mod replace;
pub mod select;
pub mod table;
pub mod update;
pub mod values;
pub mod window;

pub use dialect::Mysql;
pub use extras::{
    HasDuplicateKeyUpdate, HasHints, HasModifiers, HasRowAlias, Hints, Modifier, Modifiers,
    RowAlias, match_against, match_against_mode, query, row_value, subquery, values_of,
};
pub use function::Function;
pub use ops::MysqlOps;
pub use statement::{
    DeleteQuery, HasDeleteTables, HasExtraTables, HasTargetTable, InsertQuery, ReplaceQuery,
    SelectQuery, TableQuery, UpdateQuery, ValuesQuery,
};

// The core vocabulary a caller needs in order to use any of the above, re-exported
// so that a program building MySQL queries needs one dependency and one `use`.
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

/// Build a `REPLACE` from one mod.
///
/// A statement type of its own, not a flag on `INSERT`: `REPLACE` has no `IGNORE`,
/// no `HIGH_PRIORITY`, no row alias and no `ON DUPLICATE KEY UPDATE`, and the way to
/// say so is for those mods not to apply to it.
pub fn replace(mods: impl Mod<ReplaceQuery>) -> ReplaceQuery {
    let mut q = ReplaceQuery::default();
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

/// Build a standalone `VALUES` statement from one mod (MySQL 8.0.19+).
///
/// The rows come from [`values::row`]/[`values::rows`] and are spelled
/// `ROW(…)`, as the standalone grammar requires; with none the statement is a
/// [`build()`](keelson_core::Query::build) error.
pub fn values(mods: impl Mod<ValuesQuery>) -> ValuesQuery {
    let mut q = ValuesQuery::default();
    mods.apply(&mut q);
    q
}

/// Build a `TABLE` statement from one mod (MySQL 8.0.19+) — MySQL's shorthand
/// for `SELECT * FROM t`.
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

/// Raw SQL whose `?` are rewritten with `args` interleaved. Write `\?` for a literal
/// question mark.
///
/// MySQL's own placeholder is already `?`, so this looks like a no-op and is not: the
/// holes are *counted*, and the arguments are bound in the writer's order, which is
/// what makes a template safe to nest inside a query that already has arguments.
pub fn template(sql: impl Into<Cow<'static, str>>, args: impl IntoIterator<Item = RawArg>) -> Expr {
    expr::template(sql, args)
}

/// A single-quoted string literal — bob's `S`. `s("A")` renders `'A'`.
///
/// Nothing is escaped: this is for SQL the program itself wrote — a keyword, an enum
/// label, a JSON path. Text from outside belongs in [`arg`], where it is bound.
pub fn s(literal: impl Into<Cow<'static, str>>) -> Expr {
    expr::literal(literal)
}

/// A quoted identifier: ``quote("age")`` gives `` `age` ``, `quote(("users", "id"))`
/// gives `` `users`.`id` ``.
pub fn quote(parts: impl IntoIdent) -> Expr {
    expr::quote(parts)
}

/// One bound argument, rendered `?`.
pub fn arg(value: impl ToValue) -> Expr {
    expr::arg(value)
}

/// Several bound arguments, comma-separated and *not* parenthesised — for a slot that
/// brings its own, such as `VALUES (…)`.
pub fn args<V: ToValue>(values: impl IntoIterator<Item = V>) -> Expr {
    expr::args(values)
}

/// Several bound arguments, parenthesised: `(?, ?, ?)`.
pub fn arg_group<V: ToValue>(values: impl IntoIterator<Item = V>) -> Expr {
    expr::arg_group(values)
}

/// `n` unbound placeholders, each binding `NULL`, so a statement can be prepared now
/// and its values supplied by whatever rebinds it.
pub fn placeholders(n: usize) -> Expr {
    expr::placeholders(n)
}

/// A parenthesised, comma-separated list: `(a, b)`. One element gives plain
/// parentheses.
pub fn group(items: impl IntoExprList) -> Expr {
    expr::group(items)
}

/// A function call: `f("COUNT", "*")`, `f("ROW_NUMBER", ()).over(())`.
///
/// Returns keelson-mysql's own [`Function`], which carries `DISTINCT`, `ORDER BY`,
/// `SEPARATOR` and `OVER` — everything MySQL hangs off a call and core deliberately
/// does not know about.
pub fn f(name: impl Into<Cow<'static, str>>, args: impl IntoExprList) -> Function {
    Function::new(name, args)
}

/// A `CASE` expression: `case_().when(cond, then).else_(other)`.
///
/// Named with a trailing underscore because `case` does not read well as a plain
/// identifier next to `match`; the SQL is unaffected.
pub fn case_() -> CaseBuilder {
    expr::case()
}

/// `CAST(expr AS type_name)`.
///
/// MySQL has no `::` shorthand, so this is the only spelling. Not wrapped in
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
            "SELECT `id` FROM `users`",
            "the clone did not disturb the original"
        );
        assert_eq!(
            narrowed.build().unwrap().0,
            "SELECT `id` FROM `users` WHERE (`id` = ?)"
        );
    }

    #[test]
    fn the_query_type_is_carried_rather_than_reparsed() {
        assert_eq!(select(()).query_type(), QueryType::Select);
        assert_eq!(insert(()).query_type(), QueryType::Insert);
        // A REPLACE writes rows, which is all the layers above need to know.
        assert_eq!(replace(()).query_type(), QueryType::Insert);
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
        let err = replace(()).build().unwrap_err();
        assert!(
            matches!(&err, Error::Incomplete(what) if what.contains("REPLACE")),
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
