use std::any::{Any, TypeId};

use super::arg::{Args, arg, arg_group, args, placeholders};
use super::case::CaseChain;
use super::cast::Cast;
use super::constants::NOT;
use super::group::Group;
use super::operators::Join;
use super::quote::{QuoteParts, Quoted, quote};
use super::raw::{Clause, Raw, RawArg};
use super::string::RawString;
use crate::value::ToValue;
use crate::writer::{DynExpr, Expression, dyn_expr};

/// The dialect's own expression type: what every operator method returns.
///
/// bob parameterises `Chain[T, B]` and `Builder[T, B]` on this so each dialect
/// can add operators of its own (`ILIKE` for PostgreSQL, `BETWEEN SYMMETRIC`)
/// while inheriting the shared ones. Here the same job is done by implementing
/// this trait and dereferencing to [`Chain`](super::Chain):
///
/// ```
/// use std::ops::Deref;
///
/// use keelson_core::expr::{Chain, ExprBuilder, Join, x};
/// use keelson_core::{DynExpr, Expression, Result, SqlWriter, dyn_expr};
///
/// #[derive(Debug, Clone)]
/// pub struct Expr(Chain<Expr>);
///
/// impl ExprBuilder for Expr {
///     fn new(base: DynExpr) -> Self {
///         Expr(Chain::new(base))
///     }
/// }
///
/// impl Expression for Expr {
///     fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
///         w.write_expr(&self.0)
///     }
/// }
///
/// // Deref is what Go gets from struct embedding: `eq`, `in_`, `between` and
/// // the rest arrive for free and still return `Expr`.
/// impl Deref for Expr {
///     type Target = Chain<Expr>;
///     fn deref(&self) -> &Chain<Expr> {
///         &self.0
///     }
/// }
///
/// impl Expr {
///     /// A dialect-only operator, written exactly as the shared ones are.
///     pub fn ilike(&self, target: impl Expression + 'static) -> Expr {
///         x(Join::new([self.base.clone(), dyn_expr("ILIKE"), dyn_expr(target)]))
///     }
/// }
/// ```
pub trait ExprBuilder: Expression + Sized + 'static {
    /// Wrap an already-erased expression.
    ///
    /// The only method a dialect has to write; everything below is derived.
    fn new(base: DynExpr) -> Self;

    /// Wrap an expression, parenthesising it if [`x`] would.
    fn from_expr(e: impl Expression + 'static) -> Self {
        x(e)
    }

    /// `NOT expr`.
    fn not(e: impl Expression + 'static) -> Self {
        not(e)
    }

    /// `(a OR b OR c)`.
    fn or(exprs: impl IntoIterator<Item = DynExpr>) -> Self {
        x(Join::with_sep(exprs, " OR "))
    }

    /// `(a AND b AND c)`.
    fn and(exprs: impl IntoIterator<Item = DynExpr>) -> Self {
        x(Join::with_sep(exprs, " AND "))
    }

    /// `(a || b || c)`.
    fn concat(exprs: impl IntoIterator<Item = DynExpr>) -> Self {
        x(Join::with_sep(exprs, " || "))
    }

    /// A single-quoted string literal.
    fn s(literal: impl Into<String>) -> Self {
        x(RawString::new(literal))
    }

    /// One bound argument.
    fn arg(v: impl ToValue) -> Self {
        x(arg(v))
    }

    /// `$1, $2, $3`.
    fn args<V: ToValue>(vals: impl IntoIterator<Item = V>) -> Self {
        x(args(vals))
    }

    /// `($1, $2, $3)`.
    fn arg_group<V: ToValue>(vals: impl IntoIterator<Item = V>) -> Self {
        x(arg_group(vals))
    }

    /// `n` placeholders bound to `NULL`.
    fn placeholders(n: usize) -> Self {
        x(placeholders(n))
    }

    /// Raw SQL with `?` placeholders and no arguments.
    fn raw(query: impl Into<String>) -> Self {
        x(Clause::new(query))
    }

    /// Raw SQL with `?` placeholders and their replacements.
    fn raw_with(query: impl Into<String>, args: impl IntoIterator<Item = RawArg>) -> Self {
        x(Clause::new(query).with_args(args))
    }

    /// `(a, b)`.
    fn group(exprs: impl IntoIterator<Item = DynExpr>) -> Self {
        x(Group::new(exprs))
    }

    /// `"users"."id"`.
    fn quote(parts: impl QuoteParts) -> Self {
        x(quote(parts))
    }

    /// `CAST(e AS type_name)`.
    fn cast(e: impl Expression + 'static, type_name: impl Into<String>) -> Self {
        x(Cast::new(e, type_name))
    }

    /// The start of a `CASE WHEN ... END`.
    fn case() -> CaseChain<Self> {
        CaseChain::new()
    }
}

/// Whether [`x`] leaves an expression of type `E` unparenthesised.
///
/// bob's `X` is a type switch, and the set of arms is the whole reason bob's
/// output has parentheses exactly where it does. Six kinds print as themselves:
///
/// - [`Raw`] and [`Clause`] — the author wrote the SQL, we do not second-guess
///   its shape.
/// - [`RawString`] and [`Quoted`] — a literal and an identifier are already
///   atomic.
/// - [`Args`] — usually lands somewhere that supplies parentheses of its own,
///   such as `VALUES (...)`.
/// - [`Group`] — has its own parentheses already.
///
/// `&'static str` and `String` are here too, which bob has no equivalent of:
/// keelson renders a bare string verbatim (`impl Expression for str`), making it
/// exactly a [`Raw`]. Treating the two differently would mean `x(Raw::new(sql))`
/// and `x(sql)` disagreed about parentheses, which is a trap.
///
/// Note that this is a decision about the *static* type. An expression already
/// erased into a [`DynExpr`] reads as "some expression" and gets parenthesised;
/// call [`x_raw`] if you know better.
pub fn is_self_contained<E: 'static>() -> bool {
    let id = TypeId::of::<E>();
    id == TypeId::of::<Raw>()
        || id == TypeId::of::<Clause>()
        || id == TypeId::of::<RawString>()
        || id == TypeId::of::<Quoted>()
        || id == TypeId::of::<Args>()
        || id == TypeId::of::<Group>()
        || id == TypeId::of::<&'static str>()
        || id == TypeId::of::<String>()
}

/// Wrap an expression as the dialect's own expression type, parenthesising it
/// unless it prints as itself — bob's `X`.
///
/// This is the single place precedence is dealt with: operator constructors
/// build a bare `a OP b` and hand it here, and the parentheses that make
/// `WHERE ("age" >= $1)` come out of the default arm. See
/// [`is_self_contained`] for the exceptions.
///
/// An expression that is *already* the target type is returned untouched, so
/// re-wrapping cannot nest groups.
pub fn x<T: ExprBuilder, E: Expression + 'static>(e: E) -> T {
    if TypeId::of::<E>() == TypeId::of::<T>() {
        let erased: Box<dyn Any> = Box::new(e);
        return *erased
            .downcast::<T>()
            .expect("type ids matched, so the downcast cannot fail");
    }

    if is_self_contained::<E>() {
        T::new(dyn_expr(e))
    } else {
        T::new(dyn_expr(Group::of(e)))
    }
}

/// [`x`] over several expressions, space-joined first — bob's variadic
/// `X(exp, others...)`.
///
/// The head stays concrete rather than becoming a `DynExpr` so that an empty
/// `others` can hand it to [`x`] with its type still visible; otherwise
/// `x_all(quote("a"), [])` would come back parenthesised.
pub fn x_all<T: ExprBuilder, E: Expression + 'static>(
    head: E,
    others: impl IntoIterator<Item = DynExpr>,
) -> T {
    let others: Vec<DynExpr> = others.into_iter().collect();
    if others.is_empty() {
        return x(head);
    }

    let mut exprs = Vec::with_capacity(others.len() + 1);
    exprs.push(dyn_expr(head));
    exprs.extend(others);
    x(Join::new(exprs))
}

/// [`x`] without the type inspection: never parenthesises.
pub fn x_raw<T: ExprBuilder>(e: impl Expression + 'static) -> T {
    T::new(dyn_expr(e))
}

/// [`x`] without the type inspection: always parenthesises.
pub fn x_group<T: ExprBuilder>(e: impl Expression + 'static) -> T {
    T::new(dyn_expr(Group::of(e)))
}

/// `NOT expr`.
///
/// The `NOT` and the expression are joined directly rather than passed through
/// [`x`], so the result is `NOT (a = b)` and not `(NOT (a = b))`.
pub fn not<T: ExprBuilder>(e: impl Expression + 'static) -> T {
    let inner: T = x(e);
    T::new(dyn_expr(Join::new([dyn_expr(NOT), dyn_expr(inner)])))
}

#[cfg(test)]
pub(crate) mod tests {
    use std::ops::Deref;

    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::error::Result;
    use crate::expr::Chain;
    use crate::writer::{SqlWriter, build};

    /// A stand-in for a dialect's expression type, wired up exactly as a real
    /// dialect crate will wire its own.
    #[derive(Debug, Clone)]
    pub(crate) struct Expr(pub Chain<Expr>);

    impl ExprBuilder for Expr {
        fn new(base: DynExpr) -> Self {
            Expr(Chain::new(base))
        }
    }

    impl Expression for Expr {
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

    pub(crate) fn sql(e: &impl Expression) -> String {
        build(&Numbered, e).unwrap().0
    }

    #[test]
    fn self_contained_kinds_are_not_parenthesised() {
        assert_eq!(
            sql(&x::<Expr, _>(Quoted::new(["users".into()]))),
            r#""users""#
        );
        assert_eq!(sql(&x::<Expr, _>(Raw::new("COUNT(*)"))), "COUNT(*)");
        assert_eq!(sql(&x::<Expr, _>(RawString::new("a"))), "'a'");
        assert_eq!(sql(&x::<Expr, _>(args([1, 2]))), "$1, $2");
        assert_eq!(sql(&x::<Expr, _>(arg_group([1, 2]))), "($1, $2)");
        assert_eq!(sql(&x::<Expr, _>(Clause::new("a = b"))), "a = b");
        assert_eq!(sql(&x::<Expr, _>(Group::of("a"))), "(a)");
    }

    #[test]
    fn a_bare_string_counts_as_raw_sql() {
        assert_eq!(sql(&x::<Expr, _>("a = b")), "a = b");
        assert_eq!(sql(&x::<Expr, _>(String::from("a = b"))), "a = b");
    }

    #[test]
    fn everything_else_is_parenthesised() {
        assert_eq!(
            sql(&x::<Expr, _>(super::super::op("=", "a", "b"))),
            "(a = b)"
        );
        assert_eq!(
            sql(&x::<Expr, _>(Join::with_sep(
                [dyn_expr("a"), dyn_expr("b")],
                " OR "
            ))),
            "(a OR b)"
        );
        assert_eq!(
            sql(&x::<Expr, _>(Cast::new("a", "int"))),
            "(CAST(a AS int))"
        );
    }

    #[test]
    fn wrapping_the_target_type_again_is_a_no_op() {
        let once: Expr = x(super::super::op("=", "a", "b"));
        assert_eq!(sql(&once), "(a = b)");
        let twice: Expr = x(once);
        assert_eq!(sql(&twice), "(a = b)", "no second group");
    }

    #[test]
    fn x_all_joins_only_when_there_is_something_to_join() {
        let one: Expr = x_all(Quoted::new(["a".into()]), []);
        assert_eq!(sql(&one), r#""a""#, "a lone expression is not joined");

        let three: Expr = x_all("a", [dyn_expr("="), dyn_expr("b")]);
        assert_eq!(sql(&three), "(a = b)");
    }

    #[test]
    fn the_escape_hatches_override_the_rule() {
        assert_eq!(
            sql(&x_raw::<Expr>(super::super::op("=", "a", "b"))),
            "a = b"
        );
        assert_eq!(sql(&x_group::<Expr>(Raw::new("a = b"))), "(a = b)");
    }

    #[test]
    fn erasing_before_wrapping_loses_the_exemption() {
        // Documented consequence of dispatching on the static type: `x` cannot
        // see through a `DynExpr`.
        let erased: DynExpr = dyn_expr(Quoted::new(["a".into()]));
        assert_eq!(sql(&x::<Expr, _>(erased)), r#"("a")"#);
    }

    #[test]
    fn not_does_not_add_an_outer_group() {
        let e: Expr = not(super::super::op("=", "a", "b"));
        assert_eq!(sql(&e), "NOT (a = b)");

        let e: Expr = Expr::not("true");
        assert_eq!(sql(&e), "NOT true");
    }

    #[test]
    fn the_builder_starters_agree_with_the_bare_constructors() {
        assert_eq!(sql(&Expr::s("a string")), "'a string'");
        assert_eq!(sql(&Expr::quote(("users", "id"))), r#""users"."id""#);
        assert_eq!(sql(&Expr::arg(21)), "$1");
        assert_eq!(sql(&Expr::args([1, 2, 3])), "$1, $2, $3");
        assert_eq!(sql(&Expr::arg_group([1, 2])), "($1, $2)");
        assert_eq!(sql(&Expr::placeholders(2)), "$1, $2");
        assert_eq!(sql(&Expr::raw("a = b")), "a = b");
        assert_eq!(sql(&Expr::cast("a", "int")), "(CAST(a AS int))");
        assert_eq!(
            sql(&Expr::or([dyn_expr("a"), dyn_expr("b"), dyn_expr("c")])),
            "(a OR b OR c)"
        );
        assert_eq!(sql(&Expr::and([dyn_expr("a"), dyn_expr("b")])), "(a AND b)");
        assert_eq!(
            sql(&Expr::concat([dyn_expr("a"), dyn_expr("b")])),
            "(a || b)"
        );
        assert_eq!(sql(&Expr::group([dyn_expr("a"), dyn_expr("b")])), "(a, b)");
    }

    #[test]
    fn raw_with_binds_its_replacements() {
        let e: Expr = Expr::raw_with("a = ? AND b = ?", [RawArg::value(1), RawArg::value(2)]);
        let (sql, vals) = build(&Numbered, &e).unwrap();
        assert_eq!(sql, "a = $1 AND b = $2");
        assert_eq!(vals.len(), 2);
    }

    #[test]
    fn a_builder_error_propagates() {
        // Too few replacements: the count check inside `Clause` still fires
        // through the wrapper.
        let e: Expr = Expr::raw("a = ?");
        assert!(build(&Numbered, &e).is_err());
    }
}
