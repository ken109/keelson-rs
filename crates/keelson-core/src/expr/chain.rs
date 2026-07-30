use super::convert::{IntoExpr, IntoExprList, IntoIdent};
use super::node::Expr;

/// The operator chain: `quote("age").gte(arg(21))`.
///
/// Every method builds a node around `self` and then applies the
/// parenthesisation rule ([`Expr::grouped`]), which is what bob's `expr.X` does
/// at the end of each of its chain methods. Because the rule is idempotent and
/// treats [`Expr::Group`] as atomic, a chain value is always "already
/// parenthesised or not needing it", and successive steps never pile up
/// redundant parentheses.
///
/// # How a dialect adds an operator
///
/// bob parameterises `Chain[T, B]` over the dialect's own expression type so that
/// `keelson-psql` can add `@>` without touching core. In Rust the same
/// extensibility comes from a trait with default methods, and there are two ways
/// to take it — neither needs a change to core.
///
/// **An extension trait**, which is the normal case and needs no new type:
///
/// ```
/// use keelson_core::expr::{Chain, Expr, IntoExpr, arg, quote};
///
/// // PostgreSQL-only operators, reachable only where this trait is imported.
/// trait PsqlOps: Chain {
///     /// `@>` — contains. Nothing but a symbol, so `op` is the whole story.
///     fn contains(self, rhs: impl IntoExpr) -> Self {
///         self.op("@>", rhs)
///     }
///
///     /// A shape `op` cannot express, built through `step` so that the
///     /// parenthesisation rule is still applied for us.
///     fn between_symmetric(self, a: impl IntoExpr, b: impl IntoExpr) -> Self {
///         self.step(move |lhs| {
///             Expr::join((lhs, Expr::raw("BETWEEN SYMMETRIC"), a, Expr::raw("AND"), b))
///         })
///     }
/// }
///
/// impl<T: Chain> PsqlOps for T {}
///
/// # #[derive(Debug)] struct Psql;
/// # impl keelson_core::Dialect for Psql {
/// #     fn write_arg(&self, w: &mut keelson_core::SqlWriter<'_>, position: usize) {
/// #         w.push_str("$"); w.push_str(&position.to_string());
/// #     }
/// #     fn write_quoted(&self, w: &mut keelson_core::SqlWriter<'_>, s: &str) {
/// #         w.push_str("\""); w.push_str(s); w.push_str("\"");
/// #     }
/// # }
/// let (sql, _) = keelson_core::build(&Psql, &quote("tags").contains(arg("x")))?;
/// assert_eq!(sql, r#"("tags" @> $1)"#);
/// # Ok::<_, keelson_core::Error>(())
/// ```
///
/// **A newtype**, when the dialect wants its operators to be unreachable from
/// another dialect's expressions rather than merely un-imported:
///
/// ```
/// use keelson_core::expr::{Chain, Expr, IntoExpr, IntoExprList};
///
/// #[derive(Debug, Clone)]
/// struct PsqlExpr(Expr);
///
/// impl IntoExpr for PsqlExpr {
///     fn into_expr(self) -> Expr { self.0 }
/// }
///
/// impl IntoExprList for PsqlExpr {
///     fn into_expr_list(self) -> Vec<Expr> { vec![self.0] }
/// }
///
/// impl Chain for PsqlExpr {
///     fn from_expr(e: Expr) -> Self { PsqlExpr(e) }
/// }
///
/// impl PsqlExpr {
///     fn contains(self, rhs: impl IntoExpr) -> Self { self.op("@>", rhs) }
/// }
///
/// // Core operators return `PsqlExpr`, so a dialect operator still applies after
/// // one: the chain never escapes into a type that has lost `@>`.
/// let e = PsqlExpr::from_expr(Expr::ident("tags")).contains("'{a}'").is_not_null();
/// ```
///
/// Either way [`step`](Self::step) is the only thing an added operator needs, and
/// the parenthesisation rule is applied for it.
///
/// # Why the supertraits
///
/// A chain has to be able to hand its expression back — to nest it in another
/// operator, or to store it in a clause. That is exactly what [`IntoExpr`] means,
/// so `Chain` requires it instead of declaring a second method with the same job,
/// and any chain value can therefore be passed to any slot in the library.
/// [`IntoExprList`] is required for the same reason one step out: it is what lets
/// `a.and(b)` take another chain value directly instead of the one-element tuple
/// `(b,)`. Both are one line for a dialect newtype, and requiring them here is how
/// that requirement gets stated rather than discovered.
// `is_null`, `is_not_null` and `is_distinct_from` take `self` by value like every
// other operator here. They are SQL keywords being spelled out, not Rust
// predicates returning `bool`, and renaming them to satisfy the convention would
// make the chain read less like the SQL it produces.
#[allow(clippy::wrong_self_convention)]
pub trait Chain: IntoExpr + IntoExprList + Sized {
    /// Wrap a finished expression back into the chain type.
    ///
    /// The inverse of [`IntoExpr::into_expr`]; the two together are all a dialect
    /// has to supply.
    fn from_expr(e: Expr) -> Self;

    /// One chain step: build a node from this expression, apply the
    /// parenthesisation rule, and return a chain again.
    ///
    /// Every operator below is a one-line call to this, and so is every operator
    /// a dialect adds.
    #[must_use]
    fn step(self, f: impl FnOnce(Expr) -> Expr) -> Self {
        Self::from_expr(f(self.into_expr()).grouped())
    }

    /// An arbitrary infix operator: `self op rhs`.
    ///
    /// The generic escape hatch — a dialect-specific operator that needs nothing
    /// but a symbol is this call and no new code in core.
    #[must_use]
    fn op(self, op: &'static str, rhs: impl IntoExpr) -> Self {
        self.step(move |lhs| Expr::binary(lhs, op, rhs))
    }

    /// `self = rhs`.
    #[must_use]
    fn eq(self, rhs: impl IntoExpr) -> Self {
        self.op("=", rhs)
    }

    /// `self <> rhs`. The standard spelling, which every dialect accepts.
    #[must_use]
    fn ne(self, rhs: impl IntoExpr) -> Self {
        self.op("<>", rhs)
    }

    /// `self < rhs`.
    #[must_use]
    fn lt(self, rhs: impl IntoExpr) -> Self {
        self.op("<", rhs)
    }

    /// `self <= rhs`.
    #[must_use]
    fn lte(self, rhs: impl IntoExpr) -> Self {
        self.op("<=", rhs)
    }

    /// `self > rhs`.
    #[must_use]
    fn gt(self, rhs: impl IntoExpr) -> Self {
        self.op(">", rhs)
    }

    /// `self >= rhs`.
    #[must_use]
    fn gte(self, rhs: impl IntoExpr) -> Self {
        self.op(">=", rhs)
    }

    /// `self IN (a, b, c)`.
    ///
    /// The operands are always parenthesised, so a single sub-select operand
    /// comes out as `IN (SELECT ..)` and a row list as `IN ((..), (..))`.
    #[must_use]
    fn in_(self, vals: impl IntoExprList) -> Self {
        self.step(move |lhs| Expr::binary(lhs, "IN", Expr::group(vals)))
    }

    /// `self NOT IN (a, b, c)`.
    #[must_use]
    fn not_in(self, vals: impl IntoExprList) -> Self {
        self.step(move |lhs| Expr::binary(lhs, "NOT IN", Expr::group(vals)))
    }

    /// `self IS NULL`.
    #[must_use]
    fn is_null(self) -> Self {
        self.step(|lhs| Expr::postfix(lhs, "IS NULL"))
    }

    /// `self IS NOT NULL`.
    #[must_use]
    fn is_not_null(self) -> Self {
        self.step(|lhs| Expr::postfix(lhs, "IS NOT NULL"))
    }

    /// `self IS DISTINCT FROM rhs`.
    #[must_use]
    fn is_distinct_from(self, rhs: impl IntoExpr) -> Self {
        self.op("IS DISTINCT FROM", rhs)
    }

    /// `self IS NOT DISTINCT FROM rhs`.
    #[must_use]
    fn is_not_distinct_from(self, rhs: impl IntoExpr) -> Self {
        self.op("IS NOT DISTINCT FROM", rhs)
    }

    /// `self BETWEEN a AND b`.
    #[must_use]
    fn between(self, a: impl IntoExpr, b: impl IntoExpr) -> Self {
        self.step(move |lhs| Expr::join((lhs, Expr::raw("BETWEEN"), a, Expr::raw("AND"), b)))
    }

    /// `self NOT BETWEEN a AND b`.
    #[must_use]
    fn not_between(self, a: impl IntoExpr, b: impl IntoExpr) -> Self {
        self.step(move |lhs| Expr::join((lhs, Expr::raw("NOT BETWEEN"), a, Expr::raw("AND"), b)))
    }

    /// `self LIKE rhs`.
    #[must_use]
    fn like(self, rhs: impl IntoExpr) -> Self {
        self.op("LIKE", rhs)
    }

    /// `self || a || b` — string concatenation.
    #[must_use]
    fn concat(self, others: impl IntoExprList) -> Self {
        self.step(move |lhs| Expr::join_with(" || ", prepend(lhs, others)))
    }

    /// `self AND a AND b`.
    #[must_use]
    fn and(self, others: impl IntoExprList) -> Self {
        self.step(move |lhs| Expr::join_with(" AND ", prepend(lhs, others)))
    }

    /// `self OR a OR b`.
    #[must_use]
    fn or(self, others: impl IntoExprList) -> Self {
        self.step(move |lhs| Expr::join_with(" OR ", prepend(lhs, others)))
    }

    /// `self + rhs`.
    #[must_use]
    fn plus(self, rhs: impl IntoExpr) -> Self {
        self.op("+", rhs)
    }

    /// `self - rhs`.
    #[must_use]
    fn minus(self, rhs: impl IntoExpr) -> Self {
        self.op("-", rhs)
    }

    /// `self AS "alias"`.
    ///
    /// This one ends the chain — it returns an [`Expr`], not `Self`. An alias is
    /// not an operand: nothing may be applied to `x AS "y"`, and unlike every
    /// other method here the result is deliberately *not* parenthesised, because
    /// `(x AS "y")` is a syntax error in a select list.
    fn as_(self, alias: impl IntoIdent) -> Expr {
        Expr::Binary {
            lhs: Box::new(self.into_expr()),
            op: "AS",
            rhs: Box::new(Expr::ident(alias)),
        }
    }
}

/// `self` first, then the rest — the shape every variadic chain method needs.
fn prepend(first: Expr, rest: impl IntoExprList) -> Vec<Expr> {
    let mut exprs = rest.into_expr_list();
    exprs.insert(0, first);
    exprs
}

/// An [`Expr`] is its own chain, so operators are available without any wrapper
/// type. A dialect that wants its own type implements [`Chain`] for that instead.
impl Chain for Expr {
    fn from_expr(e: Expr) -> Expr {
        e
    }
}

#[cfg(test)]
mod tests {
    use keelson_sqlcheck::testing::assert_frag_sql;

    use super::super::{arg, arg_group, literal, quote, raw};
    use super::*;
    use crate::dialect::testing::{Numbered, TestDialect};
    use crate::value::Value;
    use crate::writer::build;

    /// An operator expression is a fragment. A boolean one is judged where a
    /// condition goes and a scalar one where a value goes — putting either in the
    /// other's place is a mistake the grammar or the engine will name.
    const COND: &str = r#"SELECT "id" FROM users WHERE {}"#;
    const VALUE: &str = r#"SELECT {} FROM users"#;
    const POST_COND: &str = r#"SELECT "id" FROM posts WHERE {}"#;

    fn sql(e: Expr) -> String {
        build(&Numbered, &e).expect("render").0
    }

    #[test]
    fn a_comparison_is_parenthesised_exactly_once() {
        assert_frag_sql(COND, &sql(quote("age").gte(arg(21i32))), r#"("age" >= $1)"#);
    }

    #[test]
    fn every_comparison_operator_uses_its_standard_spelling() {
        // Chapter 9 of the PostgreSQL manual for each spelling; `<>` rather than
        // `!=` because that is the standard one.
        let conditions = [
            (quote("age").eq(raw("id")), r#"("age" = id)"#),
            (quote("age").ne(raw("id")), r#"("age" <> id)"#),
            (quote("age").lt(raw("id")), r#"("age" < id)"#),
            (quote("age").lte(raw("id")), r#"("age" <= id)"#),
            (quote("age").gt(raw("id")), r#"("age" > id)"#),
            (quote("age").gte(raw("id")), r#"("age" >= id)"#),
            (quote("name").like(literal("b%")), r#"("name" LIKE 'b%')"#),
        ];
        for (e, expected) in conditions {
            assert_frag_sql(COND, &sql(e), expected);
        }

        // Arithmetic is a value, not a condition, so it goes in the select list.
        let values = [
            (quote("age").plus(1i32), r#"("age" + 1)"#),
            (quote("age").minus(quote("id")), r#"("age" - "id")"#),
        ];
        for (e, expected) in values {
            assert_frag_sql(VALUE, &sql(e), expected);
        }

        // A dialect operator core has never heard of, reached through `op`.
        assert_frag_sql(
            COND,
            &sql(raw("ARRAY[1, 2]").op("@>", raw("ARRAY[1]"))),
            "(ARRAY[1, 2] @> ARRAY[1])",
        );
    }

    #[test]
    fn null_tests_are_postfix() {
        assert_frag_sql(COND, &sql(quote("age").is_null()), r#"("age" IS NULL)"#);
        assert_frag_sql(
            COND,
            &sql(quote("age").is_not_null()),
            r#"("age" IS NOT NULL)"#,
        );
    }

    #[test]
    fn distinct_from_is_an_infix_keyword_operator() {
        assert_frag_sql(
            COND,
            &sql(quote("age").is_distinct_from(quote("id"))),
            r#"("age" IS DISTINCT FROM "id")"#,
        );
        assert_frag_sql(
            COND,
            &sql(quote("age").is_not_distinct_from(quote("id"))),
            r#"("age" IS NOT DISTINCT FROM "id")"#,
        );
    }

    #[test]
    fn between_keeps_its_three_part_shape() {
        assert_frag_sql(
            COND,
            &sql(quote("age").between(arg(1i32), arg(2i32))),
            r#"("age" BETWEEN $1 AND $2)"#,
        );
        assert_frag_sql(
            COND,
            &sql(quote("age").not_between(arg(1i32), arg(2i32))),
            r#"("age" NOT BETWEEN $1 AND $2)"#,
        );
    }

    #[test]
    fn in_always_parenthesises_its_operands() {
        assert_frag_sql(
            POST_COND,
            &sql(quote("status").in_((literal("A"), literal("B")))),
            r#"("status" IN ('A', 'B'))"#,
        );
        assert_frag_sql(
            COND,
            &sql(quote("id").not_in(arg(1i32))),
            r#"("id" NOT IN ($1))"#,
        );
    }

    /// The shape bob's `select with grouped IN` fixture pins: a row constructor
    /// on the left, a list of row constructors on the right.
    #[test]
    fn a_row_constructor_in_a_list_of_row_constructors() {
        let e = Expr::group((quote("id"), quote("user_id")))
            .in_((arg_group([1i32, 2]), arg_group([3i32, 4])));
        let (rendered, args) = build(&Numbered, &e).unwrap();
        assert_frag_sql(
            POST_COND,
            &rendered,
            r#"(("id", "user_id") IN (($1, $2), ($3, $4)))"#,
        );
        assert_eq!(args.len(), 4);
    }

    #[test]
    fn boolean_chains_take_one_operand_or_several() {
        assert_frag_sql(
            COND,
            &sql(quote("is_active").and(raw("age > 1"))),
            r#"("is_active" AND age > 1)"#,
        );
        assert_frag_sql(
            COND,
            &sql(quote("is_active").or((raw("age > 1"), raw("age < 9")))),
            r#"("is_active" OR age > 1 OR age < 9)"#,
        );
    }

    /// The `psql upsert` fixture's `SET` value, which is where concatenation is
    /// pinned. `EXCLUDED` only exists inside `ON CONFLICT DO UPDATE`, so that is
    /// the frame.
    #[test]
    fn concat_joins_with_the_pipe_operator() {
        let e = raw(r#"EXCLUDED."name""#).concat((
            literal(" (formerly "),
            quote(("tags", "name")),
            literal(")"),
        ));
        assert_frag_sql(
            r#"INSERT INTO tags ("id", "name") VALUES (1, 'rust') ON CONFLICT ("id") DO UPDATE SET "name" = {}"#,
            &sql(e),
            r#"(EXCLUDED."name" || ' (formerly ' || "tags"."name" || ')')"#,
        );
    }

    #[test]
    fn nesting_a_chain_in_a_chain_adds_no_extra_parentheses() {
        // Each step's result is already atomic, so re-applying the rule is a
        // no-op. This is the invariant that replaces bob's "already a chain
        // value" arm.
        let e = quote("age").eq(arg(1i32)).and(quote("id").eq(arg(2i32)));
        assert_frag_sql(COND, &sql(e), r#"(("age" = $1) AND ("id" = $2))"#);
    }

    #[test]
    fn an_alias_ends_the_chain_and_is_not_parenthesised() {
        let e = quote("age").minus(quote("id")).as_("difference");
        assert_frag_sql(VALUE, &sql(e), r#"("age" - "id") AS "difference""#);
    }

    #[test]
    fn arguments_are_numbered_left_to_right_across_a_whole_chain() {
        let e = quote("age")
            .between(arg(1i32), arg(2i32))
            .and(quote("id").in_((arg(3i32), arg(4i32))));
        let (rendered, args) = build(&Numbered, &e).unwrap();
        assert_frag_sql(
            COND,
            &rendered,
            r#"(("age" BETWEEN $1 AND $2) AND ("id" IN ($3, $4)))"#,
        );
        assert_eq!(
            args,
            vec![Value::I32(1), Value::I32(2), Value::I32(3), Value::I32(4)]
        );
    }

    /// The extension shape the three dialect crates depend on: a trait of default
    /// methods over `Chain`, blanket-implemented, adding an operator core has
    /// never heard of.
    #[test]
    fn a_dialect_can_add_an_operator_without_touching_core() {
        trait PsqlOps: Chain {
            fn contains(self, rhs: impl IntoExpr) -> Self {
                self.op("@>", rhs)
            }

            fn between_symmetric(self, a: impl IntoExpr, b: impl IntoExpr) -> Self {
                self.step(move |lhs| {
                    Expr::join((lhs, Expr::raw("BETWEEN SYMMETRIC"), a, Expr::raw("AND"), b))
                })
            }
        }
        impl<T: Chain> PsqlOps for T {}

        assert_frag_sql(
            COND,
            &sql(raw("ARRAY[1, 2]").contains(raw("ARRAY[1]"))),
            "(ARRAY[1, 2] @> ARRAY[1])",
        );
        assert_frag_sql(
            COND,
            &sql(quote("age").between_symmetric(arg(1i32), arg(2i32))),
            r#"("age" BETWEEN SYMMETRIC $1 AND $2)"#,
        );
    }

    /// The other extension shape: a dialect newtype, so its operators cannot be
    /// reached from another dialect's expressions at all.
    ///
    /// Not judged: `GLOB` is SQLite's, and the dialect this renders under is
    /// SQLite-shaped (`?N`, `:name`). PostgreSQL — the judge reachable from
    /// `keelson-core` — has neither, and `keelson-sqlite` is where the operator
    /// itself is checked. What is asserted here is that the chain stays in the
    /// newtype.
    #[test]
    fn a_dialect_newtype_keeps_the_whole_chain_in_its_own_type() {
        #[derive(Debug, Clone)]
        struct SqliteExpr(Expr);

        impl IntoExpr for SqliteExpr {
            fn into_expr(self) -> Expr {
                self.0
            }
        }

        impl IntoExprList for SqliteExpr {
            fn into_expr_list(self) -> Vec<Expr> {
                vec![self.0]
            }
        }

        impl Chain for SqliteExpr {
            fn from_expr(e: Expr) -> Self {
                SqliteExpr(e)
            }
        }

        impl SqliteExpr {
            fn glob(self, rhs: impl IntoExpr) -> Self {
                self.op("GLOB", rhs)
            }
        }

        // Chaining a core operator returns the dialect's type, so a
        // dialect-specific one still applies afterwards.
        let e = SqliteExpr::from_expr(Expr::ident("name"))
            .glob(literal("a*"))
            .and(SqliteExpr::from_expr(Expr::ident("b")).is_null());
        let (s, _) = build(&TestDialect, &e.into_expr()).unwrap();
        assert_eq!(s, r#"(("name" GLOB 'a*') AND ("b" IS NULL))"#);
    }
}
