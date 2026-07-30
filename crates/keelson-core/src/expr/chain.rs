use std::borrow::Cow;
use std::fmt;
use std::marker::PhantomData;

use super::builder::{ExprBuilder, x};
use super::constants::{
    AND, BETWEEN, IS_DISTINCT_FROM, IS_NOT_DISTINCT_FROM, IS_NOT_NULL, IS_NULL, NOT_BETWEEN,
};
use super::group::Group;
use super::operators::{Join, LeftRight};
use super::quote::quote;
use crate::error::Result;
use crate::writer::{DynExpr, Expression, SqlWriter, dyn_expr};

/// An expression plus the operators that can be applied to it.
///
/// `T` is the dialect's own expression type (see [`ExprBuilder`]), and every
/// method returns it — so a dialect gets `eq`, `in_`, `between` and the rest for
/// free by holding a `Chain<Self>` and dereferencing to it, then adds its own
/// operators as inherent methods. That is the Rust reading of Go's struct
/// embedding, which is how bob shares these thirty-odd methods across three
/// dialects.
///
/// Rendering a chain renders its base and nothing else; the operators build new
/// chains rather than mutating this one.
pub struct Chain<T> {
    /// The expression the operators apply to.
    pub base: DynExpr,
    _target: PhantomData<fn() -> T>,
}

impl<T> Chain<T> {
    /// A chain over an already-erased expression.
    pub fn new(base: DynExpr) -> Self {
        Chain {
            base,
            _target: PhantomData,
        }
    }

    /// A chain over any expression.
    pub fn of(base: impl Expression + 'static) -> Self {
        Chain::new(dyn_expr(base))
    }
}

impl<T> Clone for Chain<T> {
    fn clone(&self) -> Self {
        Chain {
            base: self.base.clone(),
            _target: PhantomData,
        }
    }
}

impl<T> fmt::Debug for Chain<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Chain").field(&self.base).finish()
    }
}

impl<T> Expression for Chain<T> {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        w.write_expr(&self.base)
    }
}

impl<T: ExprBuilder> Chain<T> {
    /// `base OP target`, for an operator with no dedicated method.
    pub fn op(
        &self,
        operator: impl Into<Cow<'static, str>>,
        target: impl Expression + 'static,
    ) -> T {
        x(LeftRight::from_dyn(
            self.base.clone(),
            operator,
            dyn_expr(target),
        ))
    }

    /// `base = target`.
    pub fn eq(&self, target: impl Expression + 'static) -> T {
        self.op("=", target)
    }

    /// `base <> target`.
    pub fn ne(&self, target: impl Expression + 'static) -> T {
        self.op("<>", target)
    }

    /// `base < target`.
    pub fn lt(&self, target: impl Expression + 'static) -> T {
        self.op("<", target)
    }

    /// `base <= target`.
    pub fn lte(&self, target: impl Expression + 'static) -> T {
        self.op("<=", target)
    }

    /// `base > target`.
    pub fn gt(&self, target: impl Expression + 'static) -> T {
        self.op(">", target)
    }

    /// `base >= target`.
    pub fn gte(&self, target: impl Expression + 'static) -> T {
        self.op(">=", target)
    }

    /// `base - target`.
    pub fn minus(&self, target: impl Expression + 'static) -> T {
        self.op("-", target)
    }

    /// `base + target`.
    pub fn plus(&self, target: impl Expression + 'static) -> T {
        self.op("+", target)
    }

    /// `base LIKE target`.
    pub fn like(&self, target: impl Expression + 'static) -> T {
        self.op("LIKE", target)
    }

    /// `base IN (vals...)`.
    ///
    /// The values are always grouped, so a single [`Args`](super::Args) becomes
    /// `IN ($1, $2, $3)` and a pair of [`arg_group`](super::arg_group)s becomes
    /// `IN (($1, $2), ($3, $4))`.
    pub fn in_(&self, vals: impl IntoIterator<Item = DynExpr>) -> T {
        self.op_grouped("IN", vals)
    }

    /// `base NOT IN (vals...)`.
    pub fn not_in(&self, vals: impl IntoIterator<Item = DynExpr>) -> T {
        self.op_grouped("NOT IN", vals)
    }

    /// `base IS NULL`.
    pub fn is_null(&self) -> T {
        self.joined([dyn_expr(IS_NULL)])
    }

    /// `base IS NOT NULL`.
    pub fn is_not_null(&self) -> T {
        self.joined([dyn_expr(IS_NOT_NULL)])
    }

    /// `base IS DISTINCT FROM target`.
    pub fn is_distinct_from(&self, target: impl Expression + 'static) -> T {
        self.joined([dyn_expr(IS_DISTINCT_FROM), dyn_expr(target)])
    }

    /// `base IS NOT DISTINCT FROM target`.
    pub fn is_not_distinct_from(&self, target: impl Expression + 'static) -> T {
        self.joined([dyn_expr(IS_NOT_DISTINCT_FROM), dyn_expr(target)])
    }

    /// `base BETWEEN a AND b`.
    pub fn between(&self, a: impl Expression + 'static, b: impl Expression + 'static) -> T {
        self.joined([dyn_expr(BETWEEN), dyn_expr(a), dyn_expr(AND), dyn_expr(b)])
    }

    /// `base NOT BETWEEN a AND b`.
    pub fn not_between(&self, a: impl Expression + 'static, b: impl Expression + 'static) -> T {
        self.joined([
            dyn_expr(NOT_BETWEEN),
            dyn_expr(a),
            dyn_expr(AND),
            dyn_expr(b),
        ])
    }

    /// `base OR targets...`.
    pub fn or(&self, targets: impl IntoIterator<Item = DynExpr>) -> T {
        self.joined_with(targets, " OR ")
    }

    /// `base AND targets...`.
    pub fn and(&self, targets: impl IntoIterator<Item = DynExpr>) -> T {
        self.joined_with(targets, " AND ")
    }

    /// `base || targets...`.
    pub fn concat(&self, targets: impl IntoIterator<Item = DynExpr>) -> T {
        self.joined_with(targets, " || ")
    }

    /// `base AS "alias"`.
    ///
    /// Unlike every other method this does not return a `T`: an alias is the end
    /// of an expression, so there is nothing left to chain onto.
    pub fn as_(&self, alias: &str) -> LeftRight {
        LeftRight::from_dyn(self.base.clone(), "AS", dyn_expr(quote(alias.to_owned())))
    }

    /// `base OP (vals...)`.
    fn op_grouped(
        &self,
        operator: impl Into<Cow<'static, str>>,
        vals: impl IntoIterator<Item = DynExpr>,
    ) -> T {
        x(LeftRight::from_dyn(
            self.base.clone(),
            operator,
            dyn_expr(Group::new(vals)),
        ))
    }

    /// The base followed by `rest`, space-separated.
    fn joined(&self, rest: impl IntoIterator<Item = DynExpr>) -> T {
        let mut exprs = vec![self.base.clone()];
        exprs.extend(rest);
        x(Join::new(exprs))
    }

    /// The base and `rest`, separated by `sep`.
    fn joined_with(
        &self,
        rest: impl IntoIterator<Item = DynExpr>,
        sep: impl Into<Cow<'static, str>>,
    ) -> T {
        let mut exprs = vec![self.base.clone()];
        exprs.extend(rest);
        x(Join::with_sep(exprs, sep))
    }
}

#[cfg(test)]
mod tests {
    use super::super::builder::tests::{Expr, sql};
    use super::super::{arg, arg_group, args, s};
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::writer::build;

    fn col() -> Expr {
        Expr::quote("age")
    }

    #[test]
    fn every_comparison_is_parenthesised() {
        assert_eq!(sql(&col().eq(arg(21))), r#"("age" = $1)"#);
        assert_eq!(sql(&col().ne(arg(21))), r#"("age" <> $1)"#);
        assert_eq!(sql(&col().lt(arg(21))), r#"("age" < $1)"#);
        assert_eq!(sql(&col().lte(arg(21))), r#"("age" <= $1)"#);
        assert_eq!(sql(&col().gt(arg(21))), r#"("age" > $1)"#);
        assert_eq!(sql(&col().gte(arg(21))), r#"("age" >= $1)"#);
        assert_eq!(sql(&col().minus(arg(1))), r#"("age" - $1)"#);
        assert_eq!(sql(&col().plus(arg(1))), r#"("age" + $1)"#);
        assert_eq!(sql(&col().like(s("a%"))), r#"("age" LIKE 'a%')"#);
        assert_eq!(sql(&col().op("@>", arg(1))), r#"("age" @> $1)"#);
    }

    #[test]
    fn null_tests_take_no_operand() {
        assert_eq!(sql(&col().is_null()), r#"("age" IS NULL)"#);
        assert_eq!(sql(&col().is_not_null()), r#"("age" IS NOT NULL)"#);
    }

    #[test]
    fn distinct_from_reads_as_three_fragments() {
        assert_eq!(
            sql(&col().is_distinct_from(arg(1))),
            r#"("age" IS DISTINCT FROM $1)"#
        );
        assert_eq!(
            sql(&col().is_not_distinct_from(arg(1))),
            r#"("age" IS NOT DISTINCT FROM $1)"#
        );
    }

    #[test]
    fn between_puts_and_between_the_bounds() {
        let (sql, vals) = build(&Numbered, &col().between(arg(1), arg(10))).unwrap();
        assert_eq!(sql, r#"("age" BETWEEN $1 AND $2)"#);
        assert_eq!(vals.len(), 2);

        assert_eq!(
            super::super::builder::tests::sql(&col().not_between(arg(1), arg(10))),
            r#"("age" NOT BETWEEN $1 AND $2)"#
        );
    }

    /// bob's `select distinct` fixture: a lone argument run is grouped by `in_`.
    #[test]
    fn in_groups_its_values() {
        let e = Expr::quote("id").in_([dyn_expr(args([100, 200, 300]))]);
        let (sql, vals) = build(&Numbered, &e).unwrap();
        assert_eq!(sql, r#"("id" IN ($1, $2, $3))"#);
        assert_eq!(vals.len(), 3);
    }

    /// bob's `select with grouped IN` fixture: a row constructor against tuples.
    #[test]
    fn in_over_a_group_of_groups() {
        let left = Expr::group([
            dyn_expr(super::super::quote("id")),
            dyn_expr(super::super::quote("employee_id")),
        ]);
        let e = left.in_([
            dyn_expr(arg_group([100, 200])),
            dyn_expr(arg_group([300, 400])),
        ]);
        let (sql, vals) = build(&Numbered, &e).unwrap();
        assert_eq!(sql, r#"(("id", "employee_id") IN (($1, $2), ($3, $4)))"#);
        assert_eq!(vals.len(), 4);
    }

    #[test]
    fn not_in_reads_the_same_way() {
        let e = Expr::quote("id").not_in([dyn_expr(args([1, 2]))]);
        assert_eq!(sql(&e), r#"("id" NOT IN ($1, $2))"#);
    }

    #[test]
    fn boolean_joins_start_from_the_base() {
        assert_eq!(
            sql(&col().or([dyn_expr("b"), dyn_expr("c")])),
            "(\"age\" OR b OR c)"
        );
        assert_eq!(sql(&col().and([dyn_expr("b")])), "(\"age\" AND b)");
        assert_eq!(sql(&col().concat([dyn_expr("b")])), "(\"age\" || b)");
    }

    #[test]
    fn an_alias_ends_the_chain() {
        assert_eq!(sql(&col().as_("a")), r#""age" AS "a""#);
    }

    #[test]
    fn chaining_nests_the_groups() {
        let inner = col().gte(arg(21));
        let e = inner.and([dyn_expr(Expr::quote("name").eq(arg("x")))]);
        let (sql, vals) = build(&Numbered, &e).unwrap();
        assert_eq!(sql, r#"(("age" >= $1) AND ("name" = $2))"#);
        assert_eq!(vals.len(), 2);
    }

    #[test]
    fn args_are_numbered_left_to_right_through_a_chain() {
        let e = Expr::quote("a")
            .eq(arg(1))
            .and([dyn_expr(Expr::quote("b").between(arg(2), arg(3)))]);
        let (sql, vals) = build(&Numbered, &e).unwrap();
        assert_eq!(sql, r#"(("a" = $1) AND ("b" BETWEEN $2 AND $3))"#);
        assert_eq!(vals.len(), 3);
    }

    #[test]
    fn a_chain_renders_as_its_base() {
        let c: Chain<Expr> = Chain::of("a = 1");
        assert_eq!(sql(&c), "a = 1");
    }

    #[test]
    fn a_raw_string_target_is_accepted_anywhere_an_expression_is() {
        assert_eq!(sql(&col().eq("21")), r#"("age" = 21)"#);
    }
}
