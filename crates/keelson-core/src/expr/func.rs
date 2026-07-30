use std::borrow::Cow;

use crate::writer::{Expression, SqlWriter};

use super::convert::{IntoExpr, IntoExprList};
use super::node::Expr;

/// A function call under construction: `f("row_number", ()).over(window)`.
///
/// [`Expr::Func`] carries the optional `OVER` window, but `OVER` is only
/// meaningful on a function call, so the method that sets it lives here rather
/// than on [`Expr`] — where it would have to either silently do nothing or panic
/// for the fifteen other variants.
///
/// A `FuncExpr` is an [`Expression`] and an [`IntoExpr`], so it can be used
/// wherever an expression is expected without being finished first. Call
/// [`IntoExpr::into_expr`] to get the [`Expr`] and continue with the operator
/// chain.
///
/// The richer function forms — `DISTINCT`, `FILTER (WHERE ..)`,
/// `WITHIN GROUP (..)`, column definition lists — differ per dialect, and belong
/// to that dialect's own function builder. It reaches core as
/// [`Expr::Custom`](Expr::Custom).
#[derive(Debug, Clone)]
pub struct FuncExpr {
    name: Cow<'static, str>,
    args: Vec<Expr>,
}

impl FuncExpr {
    /// A function call: `name(args..)`.
    pub fn new(name: impl Into<Cow<'static, str>>, args: impl IntoExprList) -> FuncExpr {
        FuncExpr {
            name: name.into(),
            args: args.into_expr_list(),
        }
    }

    /// Attach a window: `name(args..) OVER (window)`.
    ///
    /// The window may be a definition or the name of one declared in a `WINDOW`
    /// clause; both render inside the same parentheses. An empty expression gives
    /// the valid and occasionally useful `OVER ()`.
    ///
    /// Ends the builder, because there is nothing further to configure.
    #[must_use]
    pub fn over(self, window: impl IntoExpr) -> Expr {
        Expr::Func {
            name: self.name,
            args: self.args,
            over: Some(Box::new(window.into_expr())),
        }
    }
}

impl IntoExpr for FuncExpr {
    fn into_expr(self) -> Expr {
        Expr::Func {
            name: self.name,
            args: self.args,
            over: None,
        }
    }
}

impl IntoExprList for FuncExpr {
    fn into_expr_list(self) -> Vec<Expr> {
        vec![self.into_expr()]
    }
}

impl Expression for FuncExpr {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        // Cheap enough to go through the enum: cloning the parts costs one Vec
        // and one Cow, and it keeps the rendering rules in exactly one place.
        w.write_expr(&self.clone().into_expr());
    }
}

#[cfg(test)]
mod tests {
    use super::super::{f, quote};
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::expr::Chain;
    use crate::writer::build;

    fn sql(e: Expr) -> String {
        build(&Numbered, &e).expect("render").0
    }

    #[test]
    fn a_call_with_no_arguments_still_has_its_parentheses() {
        assert_eq!(sql(f("NOW", ()).into_expr()), "NOW()");
    }

    #[test]
    fn arguments_are_comma_separated_and_may_be_anything() {
        assert_eq!(
            sql(f("LEAD", ("created_date", 1, f("NOW", ()))).into_expr()),
            "LEAD(created_date, 1, NOW())"
        );
    }

    /// The `psql` fixtures pin both of these: a window by name and an empty one.
    #[test]
    fn a_window_may_be_a_definition_a_name_or_empty() {
        assert_eq!(sql(f("avg", "salary").over("w")), "avg(salary) OVER (w)");
        assert_eq!(sql(f("row_number", ()).over("")), "row_number() OVER ()");
        assert_eq!(
            sql(f("LEAD", ("created_date", 1)).over("PARTITION BY presale_id")),
            "LEAD(created_date, 1) OVER (PARTITION BY presale_id)"
        );
    }

    #[test]
    fn a_windowed_call_continues_into_the_operator_chain() {
        let e = f("LEAD", ("created_date", 1))
            .over("PARTITION BY presale_id")
            .minus(quote("created_date"))
            .as_("difference");
        assert_eq!(
            sql(e),
            r#"(LEAD(created_date, 1) OVER (PARTITION BY presale_id) - "created_date") AS "difference""#
        );
    }

    #[test]
    fn a_call_can_be_used_as_an_expression_directly() {
        // Without finishing the builder: the writer takes `&impl Expression`.
        let (s, args) = build(&Numbered, &f("count", "*")).unwrap();
        assert_eq!(s, "count(*)");
        assert!(args.is_empty());
    }

    #[test]
    fn arguments_inside_a_call_are_numbered_in_order() {
        let e = f("coalesce", (Expr::arg(1i32), Expr::arg(2i32))).into_expr();
        let (s, args) = build(&Numbered, &e).unwrap();
        assert_eq!(s, "coalesce($1, $2)");
        assert_eq!(args.len(), 2);
    }
}
