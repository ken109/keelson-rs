use std::borrow::Cow;
use std::ops::Deref;

use keelson_core::expr::{AND, Chain, ExprBuilder, Group, Join as JoinExpr, LeftRight, Raw, x};
use keelson_core::{DynExpr, Expression as CoreExpression, Result, SqlWriter, dyn_expr};

use crate::into_expr::Exprs;

const BETWEEN_SYMMETRIC: Raw = Raw(Cow::Borrowed("BETWEEN SYMMETRIC"));
const NOT_BETWEEN_SYMMETRIC: Raw = Raw(Cow::Borrowed("NOT BETWEEN SYMMETRIC"));
const ILIKE: Raw = Raw(Cow::Borrowed("ILIKE"));

/// PostgreSQL's expression type: what every operator method returns.
///
/// The shared operators (`eq`, `in_`, `between`, …) arrive through
/// [`Deref`] to [`Chain`], which is the Rust reading of the struct embedding bob
/// uses to share them across dialects. The PostgreSQL-only ones —
/// [`ilike`](Self::ilike), [`between_symmetric`](Self::between_symmetric) — are
/// inherent methods here.
#[derive(Debug, Clone)]
pub struct Expr(Chain<Expr>);

impl ExprBuilder for Expr {
    fn new(base: DynExpr) -> Self {
        Expr(Chain::new(base))
    }
}

impl CoreExpression for Expr {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        w.write_expr(&self.0)
    }
}

impl Deref for Expr {
    type Target = Chain<Expr>;

    fn deref(&self) -> &Chain<Expr> {
        &self.0
    }
}

impl Expr {
    /// `base ILIKE val`.
    pub fn ilike(&self, target: impl CoreExpression + 'static) -> Expr {
        x(JoinExpr::new([
            self.base.clone(),
            dyn_expr(ILIKE),
            dyn_expr(target),
        ]))
    }

    /// `base BETWEEN SYMMETRIC a AND b`.
    pub fn between_symmetric(
        &self,
        a: impl CoreExpression + 'static,
        b: impl CoreExpression + 'static,
    ) -> Expr {
        x(JoinExpr::new([
            self.base.clone(),
            dyn_expr(BETWEEN_SYMMETRIC),
            dyn_expr(a),
            dyn_expr(AND),
            dyn_expr(b),
        ]))
    }

    /// `base NOT BETWEEN SYMMETRIC a AND b`.
    pub fn not_between_symmetric(
        &self,
        a: impl CoreExpression + 'static,
        b: impl CoreExpression + 'static,
    ) -> Expr {
        x(JoinExpr::new([
            self.base.clone(),
            dyn_expr(NOT_BETWEEN_SYMMETRIC),
            dyn_expr(a),
            dyn_expr(AND),
            dyn_expr(b),
        ]))
    }

    /// `base IN (vals...)`.
    ///
    /// Shadows [`Chain::in_`] so that bob's variadic `In(...)` reads the same
    /// way here: one expression, a tuple of them, or a `Vec`.
    pub fn in_(&self, vals: impl Exprs) -> Expr {
        self.grouped("IN", vals)
    }

    /// `base NOT IN (vals...)`.
    pub fn not_in(&self, vals: impl Exprs) -> Expr {
        self.grouped("NOT IN", vals)
    }

    /// `base OR targets...`.
    pub fn or(&self, targets: impl Exprs) -> Expr {
        self.joined_with(targets, " OR ")
    }

    /// `base AND targets...`.
    pub fn and(&self, targets: impl Exprs) -> Expr {
        self.joined_with(targets, " AND ")
    }

    /// `base || targets...`.
    pub fn concat(&self, targets: impl Exprs) -> Expr {
        self.joined_with(targets, " || ")
    }

    fn grouped(&self, operator: &'static str, vals: impl Exprs) -> Expr {
        x(LeftRight::from_dyn(
            self.base.clone(),
            operator,
            dyn_expr(Group::new(vals.into_exprs())),
        ))
    }

    fn joined_with(&self, targets: impl Exprs, sep: &'static str) -> Expr {
        let mut exprs = vec![self.base.clone()];
        exprs.extend(targets.into_exprs());
        x(JoinExpr::with_sep(exprs, sep))
    }
}
