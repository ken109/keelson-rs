//! Expressions: one enum, its rendering, and the operator chain.
//!
//! This is the layer bob spreads over its `expr/` package. The design differs in
//! one big way and follows bob closely in another.
//!
//! **The big difference.** bob has a struct per shape behind a
//! `bob.Expression` interface; keelson has a single [`Expr`] enum. Expressions
//! stay inspectable — which is what Layer 4's query rewriting needs — there is no
//! dynamic dispatch on the hot path, `Clone` is cheap, and all the rendering
//! decisions sit in one `match` where they can be read against the grammar.
//! [`Expr::Custom`] holds an erased [`Expression`](crate::Expression) so a dialect
//! can add shapes core has never heard of.
//!
//! **The close following.** Where the parentheses go, which separator a clause
//! uses, when a fragment omits itself entirely — that is where bob's real value
//! is, and it is reproduced exactly. In particular
//! [`Expr::is_atomic`]/[`Expr::grouped`] is bob's `expr.X`, the rule that decides
//! whether an expression is wrapped in parentheses, and it is the single most
//! output-visible piece of logic in this module.
//!
//! # Entry points
//!
//! The free functions here mirror bob's `Builder` methods, which means they apply
//! the parenthesisation rule; the associated constructors on [`Expr`] build a node
//! and nothing more. For the atomic shapes — [`raw`], [`quote`], [`literal`],
//! [`arg`] — the two are the same thing, because the rule leaves those alone.
//!
//! ```
//! use keelson_core::expr::{Chain, arg, quote};
//!
//! # #[derive(Debug)] struct Psql;
//! # impl keelson_core::Dialect for Psql {
//! #     fn write_arg(&self, w: &mut keelson_core::SqlWriter<'_>, position: usize) {
//! #         w.push_str("$"); w.push_str(&position.to_string());
//! #     }
//! #     fn write_quoted(&self, w: &mut keelson_core::SqlWriter<'_>, s: &str) {
//! #         w.push_str("\""); w.push_str(s); w.push_str("\"");
//! #     }
//! # }
//! let e = quote("age").gte(arg(21i32));
//! let (sql, args) = keelson_core::build(&Psql, &e)?;
//! assert_eq!(sql, r#"("age" >= $1)"#);
//! # Ok::<_, keelson_core::Error>(())
//! ```

mod case;
mod chain;
mod convert;
mod func;
mod node;
mod raw;

pub use case::CaseBuilder;
pub use chain::Chain;
pub use convert::{IntoExpr, IntoExprList, IntoIdent};
pub use func::FuncExpr;
pub use node::Expr;
pub use raw::RawArg;

use std::borrow::Cow;

use crate::value::ToValue;

/// Raw SQL, written verbatim. `?` is *not* rewritten — see [`template`].
///
/// The progressive-enhancement entry point: a hand-written fragment goes anywhere
/// a structured expression does.
pub fn raw(sql: impl Into<Cow<'static, str>>) -> Expr {
    Expr::raw(sql)
}

/// Raw SQL with `?` placeholders, rewritten into the dialect's own syntax with
/// `args` interleaved. Write `\?` for a literal question mark.
///
/// A `?` may be filled by a value or by a whole expression, so
/// `IN (?)` can expand to `IN ($3, $4, $5)`. The counts must match; a mismatch is
/// [`Error::RawArgCount`](crate::Error::RawArgCount).
pub fn template(
    sql: impl Into<Cow<'static, str>>,
    args: impl IntoIterator<Item = RawArg>,
) -> Expr {
    Expr::template(sql, args)
}

/// A single-quoted SQL string literal — bob's `S()`. `literal("A")` renders `'A'`.
///
/// Nothing is escaped. This is for SQL the program wrote; text from outside
/// belongs in [`arg`].
pub fn literal(s: impl Into<Cow<'static, str>>) -> Expr {
    Expr::literal(s)
}

/// A quoted identifier: `quote("age")` gives `"age"`, `quote(("users", "id"))`
/// gives `"users"."id"`.
pub fn quote(parts: impl IntoIdent) -> Expr {
    Expr::ident(parts)
}

/// One bound argument, rendered as the dialect's placeholder.
pub fn arg(v: impl ToValue) -> Expr {
    Expr::arg(v)
}

/// Several bound arguments, comma-separated and *not* parenthesised — for slots
/// that bring their own parentheses, such as `VALUES (..)`.
pub fn args<V: ToValue>(vals: impl IntoIterator<Item = V>) -> Expr {
    Expr::args(vals)
}

/// Several bound arguments, parenthesised: `($1, $2, $3)` — bob's `ArgGroup`.
pub fn arg_group<V: ToValue>(vals: impl IntoIterator<Item = V>) -> Expr {
    Expr::group(Expr::args(vals))
}

/// A named argument placeholder, for preparing a statement whose values arrive at
/// bind time. Fails on a dialect with no named-argument syntax.
pub fn named(name: impl Into<Cow<'static, str>>) -> Expr {
    Expr::named_arg(name)
}

/// `n` unbound placeholders, comma-separated — bob's `Placeholder(n)`.
pub fn placeholders(n: usize) -> Expr {
    Expr::placeholders(n)
}

/// A parenthesised, comma-separated list. One element gives plain parentheses.
pub fn group(items: impl IntoExprList) -> Expr {
    Expr::group(items)
}

/// A function call, optionally windowed: `f("count", "*")`,
/// `f("row_number", ()).over(w)`.
pub fn f(name: impl Into<Cow<'static, str>>, args: impl IntoExprList) -> FuncExpr {
    FuncExpr::new(name, args)
}

/// A `CASE` expression. See [`CaseBuilder`].
pub fn case() -> CaseBuilder {
    CaseBuilder::new()
}

/// `CAST(expr AS type_name)`.
///
/// Unlike bob's builder method this does not add outer parentheses: `CAST(..)` is
/// already self-delimiting, so they would be pure noise. Wrap it in [`group`] if
/// you want them.
pub fn cast(expr: impl IntoExpr, type_name: impl Into<Cow<'static, str>>) -> Expr {
    Expr::cast(expr, type_name)
}

/// `NOT expr`.
///
/// The operand is parenthesised if it needs it, the result is not — matching bob,
/// where `Not` is the one builder method that does not wrap its own output. It
/// does not need to: `NOT` binds looser than anything it can contain, so
/// `NOT ("a" = $1)` is already unambiguous, and an enclosing operator will
/// parenthesise it if one comes along.
pub fn not(e: impl IntoExpr) -> Expr {
    Expr::prefix("NOT", e.into_expr().grouped())
}

/// `(a AND b AND c)`.
pub fn and(items: impl IntoExprList) -> Expr {
    Expr::join_with(" AND ", items).grouped()
}

/// `(a OR b OR c)`.
pub fn or(items: impl IntoExprList) -> Expr {
    Expr::join_with(" OR ", items).grouped()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::value::Value;
    use crate::writer::build;

    fn sql(e: Expr) -> String {
        build(&Numbered, &e).expect("render").0
    }

    #[test]
    fn the_atomic_entry_points_render_as_themselves() {
        assert_eq!(sql(raw("a = 1")), "a = 1");
        assert_eq!(sql(literal("A")), "'A'");
        assert_eq!(sql(quote(("users", "id"))), r#""users"."id""#);
        assert_eq!(sql(arg(1i32)), "$1");
        assert_eq!(sql(args([1i32, 2])), "$1, $2");
        assert_eq!(sql(arg_group([1i32, 2])), "($1, $2)");
        assert_eq!(sql(placeholders(2)), "$1, $2");
        assert_eq!(sql(group(("a", "b"))), "(a, b)");
    }

    #[test]
    fn boolean_combinators_parenthesise_their_result() {
        assert_eq!(sql(and(("a", "b"))), "(a AND b)");
        assert_eq!(sql(or(("a", "b"))), "(a OR b)");
    }

    #[test]
    fn not_parenthesises_its_operand_but_not_itself() {
        assert_eq!(sql(not(Expr::binary("a", "=", arg(1i32)))), "NOT (a = $1)");
        // Already atomic: no parentheses are added at all.
        assert_eq!(sql(not(quote("flag"))), r#"NOT "flag""#);
        // A chain result is already grouped, so `NOT` does not double-wrap it —
        // the property that makes bob's "already a chain value" arm unnecessary.
        assert_eq!(
            sql(not(quote("a").eq(arg(1i32)))),
            r#"NOT ("a" = $1)"#
        );
    }

    #[test]
    fn cast_is_not_wrapped_because_it_is_already_self_delimiting() {
        assert_eq!(sql(cast(quote("a"), "int")), r#"CAST("a" AS int)"#);
    }

    #[test]
    fn a_named_argument_binds_nothing() {
        let (s, a) = build(&crate::dialect::testing::TestDialect, &named("id")).unwrap();
        assert_eq!(s, ":id");
        assert!(a.is_empty());
    }

    #[test]
    fn every_entry_point_shares_one_argument_counter() {
        let e = Expr::join((
            arg(1i32),
            arg_group([2i32, 3]),
            template("f(?)", [RawArg::value(4i32)]),
            f("g", (arg(5i32),)).into_expr(),
        ));
        let (s, a) = build(&Numbered, &e).unwrap();
        assert_eq!(s, "$1 ($2, $3) f($4) g($5)");
        assert_eq!(
            a,
            vec![
                Value::I32(1),
                Value::I32(2),
                Value::I32(3),
                Value::I32(4),
                Value::I32(5)
            ]
        );
    }
}
