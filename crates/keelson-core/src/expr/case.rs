use std::fmt;
use std::marker::PhantomData;

use super::builder::{ExprBuilder, x};
use crate::error::{Error, Result};
use crate::writer::{DynExpr, Expression, SqlWriter, dyn_expr};

/// `CASE WHEN cond THEN val ... ELSE val END`.
///
/// Built through [`CaseChain`], which is what makes the `WHEN`s accumulate.
#[derive(Debug, Clone, Default)]
pub struct CaseExpr {
    whens: Vec<When>,
    else_expr: Option<DynExpr>,
}

/// One `WHEN cond THEN val` branch.
#[derive(Debug, Clone)]
struct When {
    condition: DynExpr,
    then: DynExpr,
}

impl CaseExpr {
    /// A `CASE` with no branches yet. Rendering one is an error.
    pub fn new() -> Self {
        CaseExpr::default()
    }

    /// Add a `WHEN condition THEN then` branch.
    pub fn when(
        mut self,
        condition: impl Expression + 'static,
        then: impl Expression + 'static,
    ) -> Self {
        self.whens.push(When {
            condition: dyn_expr(condition),
            then: dyn_expr(then),
        });
        self
    }

    /// Set the `ELSE` branch, replacing any previous one.
    pub fn or_else(mut self, then: impl Expression + 'static) -> Self {
        self.else_expr = Some(dyn_expr(then));
        self
    }

    /// How many `WHEN` branches there are.
    pub fn branches(&self) -> usize {
        self.whens.len()
    }
}

impl Expression for CaseExpr {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        if self.whens.is_empty() {
            return Err(Error::other("case must have at least one when expression"));
        }

        w.push_str("CASE");
        for when in &self.whens {
            w.push_str(" WHEN ");
            w.write_expr(&when.condition)?;
            w.push_str(" THEN ");
            w.write_expr(&when.then)?;
        }

        if let Some(else_expr) = &self.else_expr {
            w.push_str(" ELSE ");
            w.write_expr(else_expr)?;
        }
        w.push_str(" END");

        Ok(())
    }
}

/// A `CASE` under construction, typed with the dialect's expression type so that
/// [`else_`](Self::else_) and [`end`](Self::end) can close it into one.
///
/// The chain is an [`Expression`] itself, so a half-built `CASE` still renders —
/// as an error if it has no branches, matching bob.
pub struct CaseChain<T> {
    case: CaseExpr,
    _target: PhantomData<fn() -> T>,
}

impl<T> CaseChain<T> {
    /// An empty `CASE`.
    pub fn new() -> Self {
        CaseChain {
            case: CaseExpr::new(),
            _target: PhantomData,
        }
    }

    /// Add a `WHEN condition THEN then` branch.
    pub fn when(
        self,
        condition: impl Expression + 'static,
        then: impl Expression + 'static,
    ) -> Self {
        CaseChain {
            case: self.case.when(condition, then),
            _target: PhantomData,
        }
    }
}

impl<T: ExprBuilder> CaseChain<T> {
    /// Close the `CASE` with an `ELSE` branch.
    pub fn else_(self, then: impl Expression + 'static) -> T {
        x(self.case.or_else(then))
    }

    /// Close the `CASE` without an `ELSE` branch.
    pub fn end(self) -> T {
        x(self.case)
    }
}

impl<T> Default for CaseChain<T> {
    fn default() -> Self {
        CaseChain::new()
    }
}

impl<T> Clone for CaseChain<T> {
    fn clone(&self) -> Self {
        CaseChain {
            case: self.case.clone(),
            _target: PhantomData,
        }
    }
}

impl<T> fmt::Debug for CaseChain<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("CaseChain").field(&self.case).finish()
    }
}

impl<T> Expression for CaseChain<T> {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        w.write_expr(&self.case)
    }
}

#[cfg(test)]
mod tests {
    use super::super::builder::tests::{Expr, sql};
    use super::super::s;
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::writer::build;

    /// bob's `case with else` fixture, minus the surrounding SELECT.
    #[test]
    fn a_closed_case_is_parenthesised_like_any_other_expression() {
        let e = Expr::case()
            .when(Expr::quote("id").eq(Expr::s("1")), s("A"))
            .else_(s("B"));
        assert_eq!(sql(&e), r#"(CASE WHEN ("id" = '1') THEN 'A' ELSE 'B' END)"#);
    }

    /// bob's `case without else` fixture.
    #[test]
    fn end_closes_without_an_else() {
        let e = Expr::case()
            .when(Expr::quote("id").eq(Expr::s("1")), s("A"))
            .end();
        assert_eq!(sql(&e), r#"(CASE WHEN ("id" = '1') THEN 'A' END)"#);
    }

    #[test]
    fn a_closed_case_can_be_aliased() {
        let e = Expr::case()
            .when(Expr::quote("id").eq(Expr::s("1")), s("A"))
            .end()
            .as_("C");
        assert_eq!(sql(&e), r#"(CASE WHEN ("id" = '1') THEN 'A' END) AS "C""#);
    }

    #[test]
    fn branches_accumulate_in_order() {
        let e = Expr::case()
            .when("a", s("1"))
            .when("b", s("2"))
            .else_(s("3"));
        assert_eq!(
            sql(&e),
            "(CASE WHEN a THEN '1' WHEN b THEN '2' ELSE '3' END)"
        );
    }

    #[test]
    fn args_are_numbered_across_the_branches() {
        let e = Expr::case()
            .when(
                Expr::quote("a").eq(super::super::arg(1)),
                super::super::arg(2),
            )
            .else_(super::super::arg(3));
        let (sql, vals) = build(&Numbered, &e).unwrap();
        assert_eq!(sql, r#"(CASE WHEN ("a" = $1) THEN $2 ELSE $3 END)"#);
        assert_eq!(vals.len(), 3);
    }

    #[test]
    fn a_case_with_no_branches_is_an_error() {
        let e: CaseChain<Expr> = CaseChain::new();
        let err = build(&Numbered, &e).unwrap_err();
        assert_eq!(
            err.to_string(),
            "case must have at least one when expression"
        );
    }

    #[test]
    fn a_half_built_case_still_renders() {
        let e: CaseChain<Expr> = CaseChain::new().when("a", s("1"));
        assert_eq!(sql(&e), "CASE WHEN a THEN '1' END");
    }
}
