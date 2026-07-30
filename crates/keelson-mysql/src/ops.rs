use std::borrow::Cow;

use keelson_core::expr::{Chain, Expr, IntoExpr, IntoExprList};

/// The operators MySQL has and the other two dialects do not.
///
/// An extension trait over [`Chain`] with a blanket impl, which is the shape
/// `keelson_core::expr::chain` documents for exactly this: nothing in core changes,
/// and the operators are reachable only where this trait is imported.
///
/// ```
/// use keelson_mysql::{MysqlOps, arg, quote};
///
/// let e = quote("name").regexp(arg("^a"));
/// ```
///
/// Every method finishes through [`Chain::step`] or [`Chain::op`], so the
/// parenthesisation rule is applied for it and a result never accumulates redundant
/// parentheses.
///
/// # A caution about the shared operators
///
/// [`Chain`] carries `is_distinct_from` and `is_not_distinct_from` for every
/// dialect, and **MySQL has neither**. Its null-safe comparison is
/// [`null_safe_eq`](MysqlOps::null_safe_eq), the `<=>` operator; the SQL-standard
/// spelling does not parse here. That one cannot be hidden, because the method lives
/// on the shared trait.
// The `is_*` predicates take `self` by value like every other operator: they are SQL
// keywords being spelled out, not Rust predicates returning `bool`.
#[allow(clippy::wrong_self_convention)]
pub trait MysqlOps: Chain {
    // -- pattern matching ----------------------------------------------------

    /// `self NOT LIKE rhs`.
    #[must_use]
    fn not_like(self, rhs: impl IntoExpr) -> Self {
        self.op("NOT LIKE", rhs)
    }

    /// `self LIKE pattern ESCAPE escape` — name the escape character.
    #[must_use]
    fn like_escape(self, pattern: impl IntoExpr, escape: impl IntoExpr) -> Self {
        self.step(move |lhs| {
            Expr::join((lhs, Expr::raw("LIKE"), pattern, Expr::raw("ESCAPE"), escape))
        })
    }

    /// `self REGEXP rhs` — extended regular-expression match (*14.8.2*).
    #[must_use]
    fn regexp(self, rhs: impl IntoExpr) -> Self {
        self.op("REGEXP", rhs)
    }

    /// `self NOT REGEXP rhs`.
    #[must_use]
    fn not_regexp(self, rhs: impl IntoExpr) -> Self {
        self.op("NOT REGEXP", rhs)
    }

    /// `self RLIKE rhs`, MySQL's synonym for `REGEXP`.
    #[must_use]
    fn rlike(self, rhs: impl IntoExpr) -> Self {
        self.op("RLIKE", rhs)
    }

    /// `self SOUNDS LIKE rhs` — equal under `SOUNDEX`.
    #[must_use]
    fn sounds_like(self, rhs: impl IntoExpr) -> Self {
        self.op("SOUNDS LIKE", rhs)
    }

    // -- comparison ----------------------------------------------------------

    /// `self <=> rhs` — the null-safe equality operator, which is what MySQL has
    /// instead of `IS NOT DISTINCT FROM`.
    #[must_use]
    fn null_safe_eq(self, rhs: impl IntoExpr) -> Self {
        self.op("<=>", rhs)
    }

    /// `self != rhs`. MySQL accepts both this and the standard `<>`, which is
    /// [`Chain::ne`].
    #[must_use]
    fn bang_eq(self, rhs: impl IntoExpr) -> Self {
        self.op("!=", rhs)
    }

    // -- logic ---------------------------------------------------------------

    /// `self XOR rhs` — logical exclusive or.
    #[must_use]
    fn xor(self, rhs: impl IntoExpr) -> Self {
        self.op("XOR", rhs)
    }

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

    /// `self IS UNKNOWN` — true when the operand is `NULL`.
    #[must_use]
    fn is_unknown(self) -> Self {
        self.step(|lhs| Expr::postfix(lhs, "IS UNKNOWN"))
    }

    /// `self IS NOT UNKNOWN`.
    #[must_use]
    fn is_not_unknown(self) -> Self {
        self.step(|lhs| Expr::postfix(lhs, "IS NOT UNKNOWN"))
    }

    // -- arithmetic and bits -------------------------------------------------

    /// `self * rhs`.
    #[must_use]
    fn times(self, rhs: impl IntoExpr) -> Self {
        self.op("*", rhs)
    }

    /// `self / rhs` — floating-point division.
    #[must_use]
    fn divide(self, rhs: impl IntoExpr) -> Self {
        self.op("/", rhs)
    }

    /// `self DIV rhs` — integer division.
    #[must_use]
    fn div(self, rhs: impl IntoExpr) -> Self {
        self.op("DIV", rhs)
    }

    /// `self MOD rhs`. `%` is the same operator; this is the keyword spelling.
    #[must_use]
    fn modulo(self, rhs: impl IntoExpr) -> Self {
        self.op("MOD", rhs)
    }

    /// `self & rhs` — bitwise and.
    #[must_use]
    fn bit_and(self, rhs: impl IntoExpr) -> Self {
        self.op("&", rhs)
    }

    /// `self | rhs` — bitwise or.
    #[must_use]
    fn bit_or(self, rhs: impl IntoExpr) -> Self {
        self.op("|", rhs)
    }

    /// `self ^ rhs` — bitwise exclusive or. Not exponentiation, which MySQL spells
    /// `POW`.
    #[must_use]
    fn bit_xor(self, rhs: impl IntoExpr) -> Self {
        self.op("^", rhs)
    }

    /// `self << rhs` — left shift.
    #[must_use]
    fn shift_left(self, rhs: impl IntoExpr) -> Self {
        self.op("<<", rhs)
    }

    /// `self >> rhs` — right shift.
    #[must_use]
    fn shift_right(self, rhs: impl IntoExpr) -> Self {
        self.op(">>", rhs)
    }

    // -- JSON ----------------------------------------------------------------

    /// `self -> path` — `JSON_EXTRACT`, keeping the JSON quoting.
    #[must_use]
    fn json_get(self, path: impl IntoExpr) -> Self {
        self.op("->", path)
    }

    /// `self ->> path` — `JSON_UNQUOTE(JSON_EXTRACT(…))`.
    #[must_use]
    fn json_get_text(self, path: impl IntoExpr) -> Self {
        self.op("->>", path)
    }

    /// `self MEMBER OF (array)` — whether this value is an element of a JSON array
    /// (MySQL 8.0.17).
    #[must_use]
    fn member_of(self, array: impl IntoExpr) -> Self {
        self.step(move |lhs| Expr::binary(lhs, "MEMBER OF", Expr::group(array.into_expr())))
    }

    // -- quantified comparison ------------------------------------------------

    /// `self = ANY (subquery)`.
    #[must_use]
    fn eq_any(self, subquery: impl IntoExprList) -> Self {
        self.any("=", subquery)
    }

    /// `self <> ALL (subquery)`.
    #[must_use]
    fn ne_all(self, subquery: impl IntoExprList) -> Self {
        self.all("<>", subquery)
    }

    /// `self <op> ANY (subquery)` — true if the comparison holds for some row.
    #[must_use]
    fn any(self, op: &'static str, subquery: impl IntoExprList) -> Self {
        self.step(move |lhs| {
            Expr::join((
                Expr::binary(lhs, op, Expr::raw("ANY")),
                Expr::group(subquery),
            ))
        })
    }

    /// `self <op> ALL (subquery)` — true if the comparison holds for every row.
    #[must_use]
    fn all(self, op: &'static str, subquery: impl IntoExprList) -> Self {
        self.step(move |lhs| {
            Expr::join((
                Expr::binary(lhs, op, Expr::raw("ALL")),
                Expr::group(subquery),
            ))
        })
    }

    // -- decorations ---------------------------------------------------------

    /// `self COLLATE \`name\`` — compare or sort under a named collation.
    #[must_use]
    fn collate(self, name: impl Into<Cow<'static, str>>) -> Self {
        let name = name.into();
        self.step(move |lhs| Expr::join((lhs, Expr::raw("COLLATE"), Expr::ident(name))))
    }

    /// `BINARY self` — compare as a binary string, which is how MySQL is made
    /// case-sensitive without naming a collation.
    #[must_use]
    fn binary(self) -> Self {
        self.step(|lhs| Expr::prefix("BINARY", lhs))
    }
}

impl<C: Chain> MysqlOps for C {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Mysql, arg, quote, s};
    use keelson_core::build;

    fn sql(e: Expr) -> String {
        build(&Mysql, &e).expect("render").0
    }

    /// One set of parentheses per operator, from the chain's own rule.
    #[test]
    fn every_operator_renders_with_one_set_of_parentheses() {
        let col = || quote("name");
        for (produced, expected) in [
            (col().not_like(arg("a%")), "(`name` NOT LIKE ?)"),
            (col().regexp(arg("^a")), "(`name` REGEXP ?)"),
            (col().not_regexp(arg("^a")), "(`name` NOT REGEXP ?)"),
            (col().rlike(arg("^a")), "(`name` RLIKE ?)"),
            (col().sounds_like(arg("robert")), "(`name` SOUNDS LIKE ?)"),
            (col().null_safe_eq(arg("a")), "(`name` <=> ?)"),
            (col().bang_eq(arg("a")), "(`name` != ?)"),
            (col().xor(arg(true)), "(`name` XOR ?)"),
            (col().is_true(), "(`name` IS TRUE)"),
            (col().is_not_true(), "(`name` IS NOT TRUE)"),
            (col().is_false(), "(`name` IS FALSE)"),
            (col().is_not_false(), "(`name` IS NOT FALSE)"),
            (col().is_unknown(), "(`name` IS UNKNOWN)"),
            (col().is_not_unknown(), "(`name` IS NOT UNKNOWN)"),
            (col().times(2i32), "(`name` * 2)"),
            (col().divide(2i32), "(`name` / 2)"),
            (col().div(2i32), "(`name` DIV 2)"),
            (col().modulo(2i32), "(`name` MOD 2)"),
            (col().bit_and(3i32), "(`name` & 3)"),
            (col().bit_or(3i32), "(`name` | 3)"),
            (col().bit_xor(3i32), "(`name` ^ 3)"),
            (col().shift_left(1i32), "(`name` << 1)"),
            (col().shift_right(1i32), "(`name` >> 1)"),
            (col().json_get(s("$.a")), "(`name` -> '$.a')"),
            (col().json_get_text(s("$.a")), "(`name` ->> '$.a')"),
            (col().binary(), "(BINARY `name`)"),
        ] {
            assert_eq!(sql(produced), expected);
        }
    }

    /// The multi-token forms, where the shape is the thing worth pinning.
    #[test]
    fn the_multi_token_operators_keep_their_shape() {
        assert_eq!(
            sql(quote("name").like_escape(arg("a!_b"), s("!"))),
            "(`name` LIKE ? ESCAPE '!')"
        );
        assert_eq!(
            sql(arg(3i32).member_of(quote("body"))),
            "(? MEMBER OF (`body`))"
        );
        assert_eq!(
            sql(quote("id").eq_any(quote("sub"))),
            "(`id` = ANY (`sub`))"
        );
        assert_eq!(
            sql(quote("id").ne_all(quote("sub"))),
            "(`id` <> ALL (`sub`))"
        );
        assert_eq!(
            sql(quote("id").any(">", quote("sub"))),
            "(`id` > ANY (`sub`))"
        );
        assert_eq!(
            sql(quote("name").collate("utf8mb4_bin")),
            "(`name` COLLATE `utf8mb4_bin`)"
        );
    }
}
