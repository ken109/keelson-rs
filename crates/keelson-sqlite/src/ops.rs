use std::borrow::Cow;

use keelson_core::expr::{Chain, Expr, IntoExpr};

/// The operators SQLite has and the other two dialects do not.
///
/// An extension trait over [`Chain`] with a blanket impl, so nothing in core
/// changes and these are reachable only where this trait is imported.
///
/// ```
/// use keelson_sqlite::{SqliteOps, quote, s};
///
/// let e = quote("name").glob(s("Ada*"));
/// ```
///
/// The list is short on purpose. SQLite's `LIKE` is already case-insensitive for
/// ASCII, so there is no `ILIKE`; it has no `SIMILAR TO`, no `~` operators, no
/// array or range containment, no `ANY`/`ALL` quantifiers, no `IS TRUE`/`IS FALSE`
/// (it has no boolean type to test), no `::` cast shorthand and no
/// `AT TIME ZONE`. Every one of those is a PostgreSQL operator, and none of them is
/// reachable from a SQLite expression.
///
/// Every method finishes through [`Chain::step`] or [`Chain::op`], so the
/// parenthesisation rule is applied for it.
// The `is_*` methods take `self` by value like every other operator: they are SQL
// keywords being spelled out, not Rust predicates returning `bool`.
#[allow(clippy::wrong_self_convention)]
pub trait SqliteOps: Chain {
    // -- pattern matching ----------------------------------------------------

    /// `self GLOB rhs` — Unix file-glob matching, and case-sensitive, which is
    /// what distinguishes it from `LIKE`.
    #[must_use]
    fn glob(self, rhs: impl IntoExpr) -> Self {
        self.op("GLOB", rhs)
    }

    /// `self NOT GLOB rhs`.
    #[must_use]
    fn not_glob(self, rhs: impl IntoExpr) -> Self {
        self.op("NOT GLOB", rhs)
    }

    /// `self REGEXP rhs`.
    ///
    /// The operator is in the grammar, but SQLite ships **no** `regexp()`
    /// implementation: a statement using it prepares only against a connection that
    /// has registered one, and fails with *no such function: REGEXP* otherwise.
    /// That is a property of the build, not of the SQL, so the operator is offered
    /// and the connection is left to answer for it.
    #[must_use]
    fn regexp(self, rhs: impl IntoExpr) -> Self {
        self.op("REGEXP", rhs)
    }

    /// `self NOT REGEXP rhs`. See [`regexp`](Self::regexp) about availability.
    #[must_use]
    fn not_regexp(self, rhs: impl IntoExpr) -> Self {
        self.op("NOT REGEXP", rhs)
    }

    /// `self MATCH rhs` — the full-text and R-tree extension operator.
    ///
    /// Named with a trailing underscore because `match` is a Rust keyword.
    #[must_use]
    fn match_(self, rhs: impl IntoExpr) -> Self {
        self.op("MATCH", rhs)
    }

    /// `self NOT MATCH rhs`.
    #[must_use]
    fn not_match(self, rhs: impl IntoExpr) -> Self {
        self.op("NOT MATCH", rhs)
    }

    /// `self NOT LIKE rhs`.
    #[must_use]
    fn not_like(self, rhs: impl IntoExpr) -> Self {
        self.op("NOT LIKE", rhs)
    }

    /// `self LIKE pattern ESCAPE escape`.
    ///
    /// `ESCAPE` is a third operand of `LIKE` in SQLite's grammar rather than a
    /// separate operator, which is why it cannot be a chain step of its own.
    #[must_use]
    fn like_escape(self, pattern: impl IntoExpr, escape: impl IntoExpr) -> Self {
        self.step(move |lhs| {
            Expr::join((lhs, Expr::raw("LIKE"), pattern, Expr::raw("ESCAPE"), escape))
        })
    }

    /// `self NOT LIKE pattern ESCAPE escape`.
    #[must_use]
    fn not_like_escape(self, pattern: impl IntoExpr, escape: impl IntoExpr) -> Self {
        self.step(move |lhs| {
            Expr::join((
                lhs,
                Expr::raw("NOT LIKE"),
                pattern,
                Expr::raw("ESCAPE"),
                escape,
            ))
        })
    }

    // -- json ----------------------------------------------------------------

    /// `self -> rhs` — the field or element, as JSON text. SQLite 3.38 and later.
    ///
    /// Spelled the same as PostgreSQL's, and means something slightly different:
    /// SQLite always yields a JSON representation, where PostgreSQL's `->` yields
    /// `json`/`jsonb` and preserves the input type.
    #[must_use]
    fn json_get(self, rhs: impl IntoExpr) -> Self {
        self.op("->", rhs)
    }

    /// `self ->> rhs` — the field or element as a SQL text, integer or real.
    #[must_use]
    fn json_get_text(self, rhs: impl IntoExpr) -> Self {
        self.op("->>", rhs)
    }

    // -- null-safe comparison ------------------------------------------------

    /// `self IS rhs` — like `=`, except that two nulls compare equal.
    ///
    /// SQLite's own spelling, and much older than the standard
    /// `IS NOT DISTINCT FROM` that [`Chain::is_not_distinct_from`] writes; the two
    /// mean the same thing here.
    #[must_use]
    fn is_(self, rhs: impl IntoExpr) -> Self {
        self.op("IS", rhs)
    }

    /// `self IS NOT rhs` — like `<>`, except that two nulls compare equal.
    #[must_use]
    fn is_not(self, rhs: impl IntoExpr) -> Self {
        self.op("IS NOT", rhs)
    }

    // -- misc ----------------------------------------------------------------

    /// `self COLLATE "name"` — compare with a named collating sequence.
    ///
    /// SQLite's built-in sequences are `BINARY`, `NOCASE` and `RTRIM`.
    #[must_use]
    fn collate(self, name: impl Into<Cow<'static, str>>) -> Self {
        let name = name.into();
        self.step(move |lhs| Expr::join((lhs, Expr::raw("COLLATE"), Expr::ident(name))))
    }
}

impl<T: Chain> SqliteOps for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Sqlite, arg, quote, s};
    use keelson_core::build;

    fn sql(e: Expr) -> String {
        build(&Sqlite, &e).expect("render").0
    }

    /// Spellings from <https://www.sqlite.org/lang_expr.html>. `regexp` is checked
    /// here rather than in a statement test because the engine tier has no
    /// `regexp()` function to resolve it against.
    #[test]
    fn every_operator_renders_with_one_set_of_parentheses() {
        let cases = [
            (quote("a").glob(arg("x")), r#"("a" GLOB ?1)"#),
            (quote("a").not_glob(arg("x")), r#"("a" NOT GLOB ?1)"#),
            (quote("a").regexp(arg("x")), r#"("a" REGEXP ?1)"#),
            (quote("a").not_regexp(arg("x")), r#"("a" NOT REGEXP ?1)"#),
            (quote("a").match_(arg("x")), r#"("a" MATCH ?1)"#),
            (quote("a").not_match(arg("x")), r#"("a" NOT MATCH ?1)"#),
            (quote("a").not_like(arg("x")), r#"("a" NOT LIKE ?1)"#),
            (quote("a").json_get(s("$.b")), r#"("a" -> '$.b')"#),
            (quote("a").json_get_text(s("$.b")), r#"("a" ->> '$.b')"#),
            (quote("a").is_(quote("b")), r#"("a" IS "b")"#),
            (quote("a").is_not(quote("b")), r#"("a" IS NOT "b")"#),
        ];
        for (e, expected) in cases {
            assert_eq!(sql(e), expected);
        }
    }

    /// `expr LIKE pattern ESCAPE expr` — one production with three operands.
    #[test]
    fn escape_is_a_third_operand_of_like() {
        assert_eq!(
            sql(quote("a").like_escape(s("100\\%"), s("\\"))),
            r#"("a" LIKE '100\%' ESCAPE '\')"#
        );
        assert_eq!(
            sql(quote("a").not_like_escape(s("100\\%"), s("\\"))),
            r#"("a" NOT LIKE '100\%' ESCAPE '\')"#
        );
    }

    #[test]
    fn collate_quotes_its_sequence_name() {
        assert_eq!(
            sql(quote("a").collate("NOCASE")),
            r#"("a" COLLATE "NOCASE")"#
        );
    }

    #[test]
    fn a_sqlite_operator_still_applies_after_a_core_one() {
        assert_eq!(
            sql(quote("a").is_null().or(quote("b").glob(s("x*")))),
            r#"(("a" IS NULL) OR ("b" GLOB 'x*'))"#
        );
    }
}
