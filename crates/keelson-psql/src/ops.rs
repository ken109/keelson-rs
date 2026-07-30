use std::borrow::Cow;

use keelson_core::expr::{Chain, Expr, IntoExpr, IntoExprList};

/// The operators PostgreSQL has and the other two dialects do not.
///
/// An extension trait over [`Chain`] with a blanket impl, which is the shape
/// `keelson_core::expr::chain` documents for exactly this: nothing in core changes,
/// and the operators are reachable only where this trait is imported.
///
/// ```
/// use keelson_psql::{PsqlOps, arg, quote};
///
/// let e = quote("title").ilike(arg("%rust%"));
/// ```
///
/// Every method finishes through [`Chain::step`] or [`Chain::op`], so the
/// parenthesisation rule is applied for it and a result never accumulates
/// redundant parentheses.
// The `is_*` predicates take `self` by value like every other operator: they are
// SQL keywords being spelled out, not Rust predicates returning `bool`.
#[allow(clippy::wrong_self_convention)]
pub trait PsqlOps: Chain {
    // -- pattern matching ----------------------------------------------------

    /// `self ILIKE rhs` — `LIKE`, case-insensitively.
    #[must_use]
    fn ilike(self, rhs: impl IntoExpr) -> Self {
        self.op("ILIKE", rhs)
    }

    /// `self NOT ILIKE rhs`.
    #[must_use]
    fn not_ilike(self, rhs: impl IntoExpr) -> Self {
        self.op("NOT ILIKE", rhs)
    }

    /// `self NOT LIKE rhs`.
    #[must_use]
    fn not_like(self, rhs: impl IntoExpr) -> Self {
        self.op("NOT LIKE", rhs)
    }

    /// `self SIMILAR TO rhs` — the SQL-standard regular-expression operator.
    #[must_use]
    fn similar_to(self, rhs: impl IntoExpr) -> Self {
        self.op("SIMILAR TO", rhs)
    }

    /// `self NOT SIMILAR TO rhs`.
    #[must_use]
    fn not_similar_to(self, rhs: impl IntoExpr) -> Self {
        self.op("NOT SIMILAR TO", rhs)
    }

    /// `self ~ rhs` — POSIX regular-expression match.
    #[must_use]
    fn matches(self, rhs: impl IntoExpr) -> Self {
        self.op("~", rhs)
    }

    /// `self ~* rhs` — POSIX match, case-insensitively.
    #[must_use]
    fn imatches(self, rhs: impl IntoExpr) -> Self {
        self.op("~*", rhs)
    }

    /// `self !~ rhs`.
    #[must_use]
    fn not_matches(self, rhs: impl IntoExpr) -> Self {
        self.op("!~", rhs)
    }

    /// `self !~* rhs`.
    #[must_use]
    fn not_imatches(self, rhs: impl IntoExpr) -> Self {
        self.op("!~*", rhs)
    }

    // -- ranges --------------------------------------------------------------

    /// `self BETWEEN SYMMETRIC a AND b` — the bounds may be given either way
    /// round.
    #[must_use]
    fn between_symmetric(self, a: impl IntoExpr, b: impl IntoExpr) -> Self {
        self.step(move |lhs| {
            Expr::join((lhs, Expr::raw("BETWEEN SYMMETRIC"), a, Expr::raw("AND"), b))
        })
    }

    /// `self NOT BETWEEN SYMMETRIC a AND b`.
    #[must_use]
    fn not_between_symmetric(self, a: impl IntoExpr, b: impl IntoExpr) -> Self {
        self.step(move |lhs| {
            Expr::join((
                lhs,
                Expr::raw("NOT BETWEEN SYMMETRIC"),
                a,
                Expr::raw("AND"),
                b,
            ))
        })
    }

    // -- containment, shared by arrays, ranges and jsonb ---------------------

    /// `self @> rhs` — contains.
    #[must_use]
    fn contains(self, rhs: impl IntoExpr) -> Self {
        self.op("@>", rhs)
    }

    /// `self <@ rhs` — is contained by.
    #[must_use]
    fn contained_by(self, rhs: impl IntoExpr) -> Self {
        self.op("<@", rhs)
    }

    /// `self && rhs` — overlaps.
    #[must_use]
    fn overlaps(self, rhs: impl IntoExpr) -> Self {
        self.op("&&", rhs)
    }

    /// `self @@ rhs` — full-text search match.
    #[must_use]
    fn text_search(self, rhs: impl IntoExpr) -> Self {
        self.op("@@", rhs)
    }

    // -- json / jsonb --------------------------------------------------------

    /// `self -> rhs` — the field or element, as `json`/`jsonb`.
    #[must_use]
    fn json_get(self, rhs: impl IntoExpr) -> Self {
        self.op("->", rhs)
    }

    /// `self ->> rhs` — the field or element, as `text`.
    #[must_use]
    fn json_get_text(self, rhs: impl IntoExpr) -> Self {
        self.op("->>", rhs)
    }

    /// `self #> rhs` — the value at a path, as `json`/`jsonb`.
    #[must_use]
    fn json_get_path(self, rhs: impl IntoExpr) -> Self {
        self.op("#>", rhs)
    }

    /// `self #>> rhs` — the value at a path, as `text`.
    #[must_use]
    fn json_get_path_text(self, rhs: impl IntoExpr) -> Self {
        self.op("#>>", rhs)
    }

    /// `self ? rhs` — does the top level contain this key.
    ///
    /// The `?` is written verbatim as an operator; it is never treated as a
    /// placeholder, because only [`template`](keelson_core::expr::template)
    /// rewrites those.
    #[must_use]
    fn json_has_key(self, rhs: impl IntoExpr) -> Self {
        self.op("?", rhs)
    }

    /// `self ?| rhs` — any of these keys.
    #[must_use]
    fn json_has_any_key(self, rhs: impl IntoExpr) -> Self {
        self.op("?|", rhs)
    }

    /// `self ?& rhs` — all of these keys.
    #[must_use]
    fn json_has_all_keys(self, rhs: impl IntoExpr) -> Self {
        self.op("?&", rhs)
    }

    // -- quantified comparison ----------------------------------------------

    /// `self = ANY (vals)` — true for at least one element.
    ///
    /// One operand is the usual case: an array-valued argument or a sub-query.
    #[must_use]
    fn eq_any(self, vals: impl IntoExprList) -> Self {
        self.step(move |lhs| Expr::join((lhs, Expr::raw("= ANY"), Expr::group(vals))))
    }

    /// `self <> ALL (vals)` — true for every element.
    #[must_use]
    fn ne_all(self, vals: impl IntoExprList) -> Self {
        self.step(move |lhs| Expr::join((lhs, Expr::raw("<> ALL"), Expr::group(vals))))
    }

    /// `self <op> ANY (vals)`, for an operator this trait does not name.
    #[must_use]
    fn any(self, op: &'static str, vals: impl IntoExprList) -> Self {
        self.step(move |lhs| Expr::join((lhs, Expr::raw(op), Expr::raw("ANY"), Expr::group(vals))))
    }

    /// `self <op> ALL (vals)`, for an operator this trait does not name.
    #[must_use]
    fn all(self, op: &'static str, vals: impl IntoExprList) -> Self {
        self.step(move |lhs| Expr::join((lhs, Expr::raw(op), Expr::raw("ALL"), Expr::group(vals))))
    }

    // -- three-valued boolean tests ------------------------------------------

    /// `self IS TRUE`.
    #[must_use]
    fn is_true(self) -> Self {
        self.step(|lhs| Expr::postfix(lhs, "IS TRUE"))
    }

    /// `self IS NOT TRUE`.
    #[must_use]
    fn is_not_true(self) -> Self {
        self.step(|lhs| Expr::postfix(lhs, "IS NOT TRUE"))
    }

    /// `self IS FALSE`.
    #[must_use]
    fn is_false(self) -> Self {
        self.step(|lhs| Expr::postfix(lhs, "IS FALSE"))
    }

    /// `self IS NOT FALSE`.
    #[must_use]
    fn is_not_false(self) -> Self {
        self.step(|lhs| Expr::postfix(lhs, "IS NOT FALSE"))
    }

    /// `self IS UNKNOWN`.
    #[must_use]
    fn is_unknown(self) -> Self {
        self.step(|lhs| Expr::postfix(lhs, "IS UNKNOWN"))
    }

    /// `self IS NOT UNKNOWN`.
    #[must_use]
    fn is_not_unknown(self) -> Self {
        self.step(|lhs| Expr::postfix(lhs, "IS NOT UNKNOWN"))
    }

    // -- misc ----------------------------------------------------------------

    /// `self::type_name` — PostgreSQL's cast shorthand.
    ///
    /// The type name is written verbatim, so `int`, `numeric(10, 2)` and
    /// `text[]` all work. [`cast`](crate::cast) is the portable spelling.
    #[must_use]
    fn cast_to(self, type_name: impl Into<Cow<'static, str>>) -> Self {
        let type_name = type_name.into();
        self.step(move |lhs| Expr::join_with("", (lhs, Expr::raw("::"), Expr::raw(type_name))))
    }

    /// `self COLLATE "name"`.
    #[must_use]
    fn collate(self, name: impl Into<Cow<'static, str>>) -> Self {
        let name = name.into();
        self.step(move |lhs| Expr::join((lhs, Expr::raw("COLLATE"), Expr::ident(name))))
    }

    /// `self AT TIME ZONE zone`.
    #[must_use]
    fn at_time_zone(self, zone: impl IntoExpr) -> Self {
        self.step(move |lhs| Expr::join((lhs, Expr::raw("AT TIME ZONE"), zone)))
    }
}

impl<T: Chain> PsqlOps for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Psql, arg, quote};
    use keelson_core::build;

    fn sql(e: Expr) -> String {
        build(&Psql, &e).expect("render").0
    }

    #[test]
    fn every_operator_renders_with_one_set_of_parentheses() {
        // Spellings taken from PostgreSQL 17 chapter 9 (Functions and Operators).
        let cases = [
            (quote("a").ilike(arg("x")), r#"("a" ILIKE $1)"#),
            (quote("a").not_ilike(arg("x")), r#"("a" NOT ILIKE $1)"#),
            (quote("a").not_like(arg("x")), r#"("a" NOT LIKE $1)"#),
            (quote("a").similar_to(arg("x")), r#"("a" SIMILAR TO $1)"#),
            (
                quote("a").not_similar_to(arg("x")),
                r#"("a" NOT SIMILAR TO $1)"#,
            ),
            (quote("a").matches(arg("x")), r#"("a" ~ $1)"#),
            (quote("a").imatches(arg("x")), r#"("a" ~* $1)"#),
            (quote("a").not_matches(arg("x")), r#"("a" !~ $1)"#),
            (quote("a").not_imatches(arg("x")), r#"("a" !~* $1)"#),
            (quote("a").contains(arg("x")), r#"("a" @> $1)"#),
            (quote("a").contained_by(arg("x")), r#"("a" <@ $1)"#),
            (quote("a").overlaps(arg("x")), r#"("a" && $1)"#),
            (quote("a").text_search(arg("x")), r#"("a" @@ $1)"#),
            (quote("a").json_get(arg("x")), r#"("a" -> $1)"#),
            (quote("a").json_get_text(arg("x")), r#"("a" ->> $1)"#),
            (quote("a").json_get_path(arg("x")), r#"("a" #> $1)"#),
            (quote("a").json_get_path_text(arg("x")), r#"("a" #>> $1)"#),
            (quote("a").json_has_key(arg("x")), r#"("a" ? $1)"#),
            (quote("a").json_has_any_key(arg("x")), r#"("a" ?| $1)"#),
            (quote("a").json_has_all_keys(arg("x")), r#"("a" ?& $1)"#),
            (quote("a").is_true(), r#"("a" IS TRUE)"#),
            (quote("a").is_not_true(), r#"("a" IS NOT TRUE)"#),
            (quote("a").is_false(), r#"("a" IS FALSE)"#),
            (quote("a").is_not_false(), r#"("a" IS NOT FALSE)"#),
            (quote("a").is_unknown(), r#"("a" IS UNKNOWN)"#),
            (quote("a").is_not_unknown(), r#"("a" IS NOT UNKNOWN)"#),
        ];
        for (e, expected) in cases {
            assert_eq!(sql(e), expected);
        }
    }

    #[test]
    fn the_multi_token_operators_keep_their_shape() {
        assert_eq!(
            sql(quote("a").between_symmetric(arg(1i32), arg(2i32))),
            r#"("a" BETWEEN SYMMETRIC $1 AND $2)"#
        );
        assert_eq!(
            sql(quote("a").not_between_symmetric(arg(1i32), arg(2i32))),
            r#"("a" NOT BETWEEN SYMMETRIC $1 AND $2)"#
        );
        assert_eq!(sql(quote("a").eq_any(arg(1i32))), r#"("a" = ANY ($1))"#);
        assert_eq!(sql(quote("a").ne_all(arg(1i32))), r#"("a" <> ALL ($1))"#);
        assert_eq!(sql(quote("a").any(">", arg(1i32))), r#"("a" > ANY ($1))"#);
        assert_eq!(sql(quote("a").all("<", arg(1i32))), r#"("a" < ALL ($1))"#);
    }

    #[test]
    fn cast_shorthand_has_no_spaces_and_collate_quotes_its_name() {
        assert_eq!(sql(quote("a").cast_to("int")), r#"("a"::int)"#);
        assert_eq!(sql(quote("a").collate("C")), r#"("a" COLLATE "C")"#);
        assert_eq!(
            sql(quote("a").at_time_zone(arg("UTC"))),
            r#"("a" AT TIME ZONE $1)"#
        );
    }
}
