use std::borrow::Cow;

use crate::error::Result;
use crate::writer::{DynExpr, Expression, SqlWriter, dyn_expr};

/// An infix operator: `left OP right`, with a single space either side.
///
/// Nothing is parenthesised here. Precedence is handled one level up, by
/// [`x`](super::x) wrapping the result in a [`Group`](super::Group).
#[derive(Debug, Clone)]
pub struct LeftRight {
    left: DynExpr,
    operator: Cow<'static, str>,
    right: DynExpr,
}

impl LeftRight {
    /// `left OP right`.
    pub fn new(
        left: impl Expression + 'static,
        operator: impl Into<Cow<'static, str>>,
        right: impl Expression + 'static,
    ) -> Self {
        LeftRight {
            left: dyn_expr(left),
            operator: operator.into(),
            right: dyn_expr(right),
        }
    }

    /// `left OP right` from already-erased sides, to avoid a second `Arc`.
    pub fn from_dyn(left: DynExpr, operator: impl Into<Cow<'static, str>>, right: DynExpr) -> Self {
        LeftRight {
            left,
            operator: operator.into(),
            right,
        }
    }

    /// The operator, without its surrounding spaces.
    pub fn operator(&self) -> &str {
        &self.operator
    }
}

/// `left OP right` — bob's `OP`.
pub fn op(
    operator: impl Into<Cow<'static, str>>,
    left: impl Expression + 'static,
    right: impl Expression + 'static,
) -> LeftRight {
    LeftRight::new(left, operator, right)
}

impl Expression for LeftRight {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        w.write_expr(&self.left)?;
        w.push_str(" ");
        w.push_str(&self.operator);
        w.push_str(" ");
        w.write_expr(&self.right)
    }
}

/// Several expressions joined by a separator, with nothing added around them.
///
/// The separator carries its own spacing (`" OR "`, `" || "`), and an empty one
/// means a single space — that is how `BETWEEN a AND b` is built out of five
/// fragments.
#[derive(Debug, Clone, Default)]
pub struct Join {
    exprs: Vec<DynExpr>,
    sep: Cow<'static, str>,
}

impl Join {
    /// Space-separated.
    pub fn new(exprs: impl IntoIterator<Item = DynExpr>) -> Self {
        Join {
            exprs: exprs.into_iter().collect(),
            sep: Cow::Borrowed(" "),
        }
    }

    /// Separated by `sep`; an empty `sep` means a single space.
    pub fn with_sep(
        exprs: impl IntoIterator<Item = DynExpr>,
        sep: impl Into<Cow<'static, str>>,
    ) -> Self {
        Join {
            exprs: exprs.into_iter().collect(),
            sep: sep.into(),
        }
    }

    /// The joined expressions.
    pub fn exprs(&self) -> &[DynExpr] {
        &self.exprs
    }

    /// The separator actually used, with the empty-means-space rule applied.
    pub fn separator(&self) -> &str {
        if self.sep.is_empty() { " " } else { &self.sep }
    }
}

/// Space-separated expressions.
pub fn join(exprs: impl IntoIterator<Item = DynExpr>) -> Join {
    Join::new(exprs)
}

/// Expressions separated by `sep`.
pub fn join_with(
    exprs: impl IntoIterator<Item = DynExpr>,
    sep: impl Into<Cow<'static, str>>,
) -> Join {
    Join::with_sep(exprs, sep)
}

impl Expression for Join {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        let sep = if self.sep.is_empty() {
            " "
        } else {
            self.sep.as_ref()
        };
        w.write_slice(&self.exprs, "", sep, "")
    }
}

#[cfg(test)]
mod tests {
    use super::super::{arg, quote};
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::writer::build;

    #[test]
    fn an_operator_is_spaced_on_both_sides() {
        let e = op(">=", quote("age"), arg(21));
        let (sql, vals) = build(&Numbered, &e).unwrap();
        assert_eq!(sql, r#""age" >= $1"#);
        assert_eq!(vals.len(), 1);
    }

    #[test]
    fn operands_are_rendered_left_to_right_so_args_stay_in_order() {
        let e = op("=", arg(1), arg(2));
        let (sql, vals) = build(&Numbered, &e).unwrap();
        assert_eq!(sql, "$1 = $2");
        assert_eq!(vals.len(), 2);
    }

    #[test]
    fn a_join_uses_its_separator() {
        let e = join_with([dyn_expr("a"), dyn_expr("b"), dyn_expr("c")], " OR ");
        let (sql, _) = build(&Numbered, &e).unwrap();
        assert_eq!(sql, "a OR b OR c");
    }

    #[test]
    fn an_empty_separator_means_one_space() {
        let e = Join::with_sep([dyn_expr("a"), dyn_expr("b")], "");
        assert_eq!(e.separator(), " ");
        let (sql, _) = build(&Numbered, &e).unwrap();
        assert_eq!(sql, "a b");
    }

    #[test]
    fn an_empty_join_writes_nothing() {
        let (sql, _) = build(&Numbered, &Join::default()).unwrap();
        assert_eq!(sql, "");
    }
}
