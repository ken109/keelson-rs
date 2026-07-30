use super::convert::IntoExpr;
use super::node::Expr;

/// A `CASE` expression under construction — bob's `CaseChain`.
///
/// ```
/// use keelson_core::expr::{case, literal, quote, Chain, arg};
///
/// // (CASE WHEN ("id" = $1) THEN 'A' ELSE 'B' END)
/// let e = case()
///     .when(quote("id").eq(arg(1i32)), literal("A"))
///     .else_(literal("B"));
/// ```
///
/// [`end`](Self::end) and [`else_`](Self::else_) both apply the parenthesisation
/// rule, so the result is `(CASE .. END)` — self-delimiting, and safe to drop into
/// an operand slot or alias with
/// [`Chain::as_`](crate::expr::Chain::as_).
#[derive(Debug, Clone, Default)]
pub struct CaseBuilder {
    whens: Vec<(Expr, Expr)>,
}

impl CaseBuilder {
    /// An empty `CASE`. At least one [`when`](Self::when) is required before it
    /// can render.
    pub fn new() -> CaseBuilder {
        CaseBuilder::default()
    }

    /// Add a `WHEN condition THEN result` branch.
    #[must_use]
    pub fn when(mut self, condition: impl IntoExpr, then: impl IntoExpr) -> CaseBuilder {
        self.whens.push((condition.into_expr(), then.into_expr()));
        self
    }

    /// Finish with an `ELSE` branch.
    #[must_use]
    pub fn else_(self, then: impl IntoExpr) -> Expr {
        Expr::Case {
            whens: self.whens,
            else_: Some(Box::new(then.into_expr())),
        }
        .grouped()
    }

    /// Finish without an `ELSE` branch.
    #[must_use]
    pub fn end(self) -> Expr {
        Expr::Case {
            whens: self.whens,
            else_: None,
        }
        .grouped()
    }
}

#[cfg(test)]
mod tests {
    use super::super::{arg, case, literal, quote};
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::expr::Chain;
    use crate::writer::build;

    fn sql(e: Expr) -> String {
        build(&Numbered, &e).expect("render").0
    }

    /// The `case with else` fixture, in all three dialects, is this shape aliased
    /// into a select list.
    #[test]
    fn a_case_with_an_else_branch() {
        let e = case()
            .when(quote("id").eq(literal("1")), literal("A"))
            .else_(literal("B"))
            .as_("C");
        assert_eq!(
            sql(e),
            r#"(CASE WHEN ("id" = '1') THEN 'A' ELSE 'B' END) AS "C""#
        );
    }

    /// The `case without else` fixture.
    #[test]
    fn a_case_without_an_else_branch() {
        let e = case()
            .when(quote("id").eq(literal("1")), literal("A"))
            .end()
            .as_("C");
        assert_eq!(sql(e), r#"(CASE WHEN ("id" = '1') THEN 'A' END) AS "C""#);
    }

    #[test]
    fn branches_render_in_the_order_they_were_added() {
        let e = case()
            .when(quote("a"), arg(1i32))
            .when(quote("b"), arg(2i32))
            .else_(arg(3i32));
        let (s, args) = build(&Numbered, &e).unwrap();
        assert_eq!(s, r#"(CASE WHEN "a" THEN $1 WHEN "b" THEN $2 ELSE $3 END)"#);
        assert_eq!(args.len(), 3);
    }

    #[test]
    fn a_case_with_no_branches_refuses_to_build() {
        assert!(build(&Numbered, &CaseBuilder::new().end()).is_err());
    }
}
