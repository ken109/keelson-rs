//! The expression starters: everything a query is written *with*.
//!
//! These are re-exported at the crate root, so they read as `psql::arg(1)`,
//! `psql::quote("id")`, `psql::case_()`.
//!
//! Most of them take `impl Expression` rather than
//! [`IntoExpr`](crate::IntoExpr), and that is deliberate: parenthesisation is
//! decided from the *static* type of the argument (see
//! [`keelson_core::expr::x`]), so erasing it first would wrap things that should
//! stay bare.

use keelson_core::expr::{CaseChain, ExprBuilder, QuoteParts, RawArg, x, x_group, x_raw};
use keelson_core::{Expression, Mod, QueryType, ToValue};

use crate::expr::Expr;
use crate::function::Function;
use crate::into_expr::Exprs;
use crate::query::Query;
use crate::select::SelectQuery;

/// A `SELECT` statement.
///
/// The mods are one argument, so a tuple is how several are passed and `()` is
/// how none are: `psql::select(())` is `SELECT *`.
pub fn select<M: Mod<SelectQuery>>(query_mods: M) -> Query<SelectQuery> {
    let mut query = SelectQuery::default();
    query_mods.apply(&mut query);
    Query::new(query, QueryType::Select)
}

/// A function call: `psql::f("generate_series", (1, 3))`.
///
/// Refine it with [`fm`](crate::fm) mods through
/// [`Function::apply`](crate::Function::apply), and turn it into a chainable
/// expression with [`Function::expr`](crate::Function::expr).
pub fn f(name: impl Into<String>, args: impl Exprs) -> Function {
    Function::new(name, args)
}

/// A single-quoted string literal: `'a string'`.
pub fn s(literal: impl Into<String>) -> Expr {
    Expr::s(literal)
}

/// `NOT expression`.
pub fn not(expression: impl Expression + 'static) -> Expr {
    Expr::not(expression)
}

// `Expr` shadows `or` / `and` / `concat` with variadic inherent methods, so the
// no-receiver starters have to name the trait.
/// `(a OR b OR c)`.
pub fn or(expressions: impl Exprs) -> Expr {
    <Expr as ExprBuilder>::or(expressions.into_exprs())
}

/// `(a AND b AND c)`.
pub fn and(expressions: impl Exprs) -> Expr {
    <Expr as ExprBuilder>::and(expressions.into_exprs())
}

/// `(a || b || c)`.
pub fn concat(expressions: impl Exprs) -> Expr {
    <Expr as ExprBuilder>::concat(expressions.into_exprs())
}

/// One bound argument: `$1`.
pub fn arg(value: impl ToValue) -> Expr {
    Expr::arg(value)
}

/// Several bound arguments: `$1, $2, $3`.
pub fn args<V: ToValue>(values: impl IntoIterator<Item = V>) -> Expr {
    Expr::args(values)
}

/// Several bound arguments as a row: `($1, $2, $3)`.
pub fn arg_group<V: ToValue>(values: impl IntoIterator<Item = V>) -> Expr {
    Expr::arg_group(values)
}

/// `n` placeholders bound to `NULL`, for a statement whose values arrive later.
pub fn placeholder(n: usize) -> Expr {
    Expr::placeholders(n)
}

/// `(a, b)`.
pub fn group(expressions: impl Exprs) -> Expr {
    Expr::group(expressions.into_exprs())
}

/// A quoted identifier: `psql::quote("id")`, `psql::quote(("users", "id"))`.
pub fn quote(parts: impl QuoteParts) -> Expr {
    Expr::quote(parts)
}

/// Raw SQL, written verbatim.
pub fn raw(query: impl Into<String>) -> Expr {
    Expr::raw(query)
}

/// Raw SQL whose `?` placeholders are replaced by arguments or expressions.
pub fn raw_with(query: impl Into<String>, args: impl IntoIterator<Item = RawArg>) -> Expr {
    Expr::raw_with(query, args)
}

/// `CAST(expression AS type_name)`.
pub fn cast(expression: impl Expression + 'static, type_name: impl Into<String>) -> Expr {
    Expr::cast(expression, type_name)
}

/// The start of a `CASE WHEN … THEN … END`.
///
/// Named with a trailing underscore because `case` is a Rust keyword.
pub fn case_() -> CaseChain<Expr> {
    Expr::case()
}

/// Wrap an expression, parenthesising it unless it already prints as a unit.
///
/// The general entry point every operator method goes through; reach for it when
/// a hand-written fragment needs to become an [`Expr`] so that operators can be
/// chained onto it.
pub fn e(expression: impl Expression + 'static) -> Expr {
    x(expression)
}

/// [`e`] without the inspection: never parenthesises.
pub fn e_raw(expression: impl Expression + 'static) -> Expr {
    x_raw(expression)
}

/// [`e`] without the inspection: always parenthesises.
pub fn e_group(expression: impl Expression + 'static) -> Expr {
    x_group(expression)
}
