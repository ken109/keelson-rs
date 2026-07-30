use super::constants::NULL;
use crate::error::Result;
use crate::writer::{DynExpr, Expression, SqlWriter, dyn_expr};

/// Several expressions parenthesised and comma-joined: `(a, b)`.
///
/// [`x`](super::x) wraps in a `Group` by default, which is why a comparison
/// comes out as `("age" >= $1)`, and why a `Group` is one of the few types `x`
/// leaves alone — it already has its parentheses.
#[derive(Debug, Clone, Default)]
pub struct Group(Vec<DynExpr>);

impl Group {
    /// A group of already-erased expressions.
    pub fn new(exprs: impl IntoIterator<Item = DynExpr>) -> Self {
        Group(exprs.into_iter().collect())
    }

    /// A group of one, the shape [`x`](super::x) produces.
    pub fn of(e: impl Expression + 'static) -> Self {
        Group(vec![dyn_expr(e)])
    }

    /// The grouped expressions.
    pub fn exprs(&self) -> &[DynExpr] {
        &self.0
    }
}

/// Parenthesise and comma-join expressions.
pub fn group(exprs: impl IntoIterator<Item = DynExpr>) -> Group {
    Group::new(exprs)
}

impl Expression for Group {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        if self.0.is_empty() {
            return w.write_if(true, "(", &NULL, ")");
        }

        w.write_slice(&self.0, "(", ", ", ")")
    }
}

#[cfg(test)]
mod tests {
    use super::super::quote;
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::writer::build;

    #[test]
    fn members_are_comma_joined_inside_parentheses() {
        let g = group([dyn_expr(quote("id")), dyn_expr(quote("employee_id"))]);
        let (sql, _) = build(&Numbered, &g).unwrap();
        assert_eq!(sql, r#"("id", "employee_id")"#);
    }

    #[test]
    fn an_empty_group_is_null() {
        let (sql, args) = build(&Numbered, &Group::default()).unwrap();
        assert_eq!(sql, "(NULL)");
        assert!(args.is_empty());
    }

    #[test]
    fn a_group_of_one_has_no_separator() {
        let (sql, _) = build(&Numbered, &Group::of("a = 1")).unwrap();
        assert_eq!(sql, "(a = 1)");
    }
}
