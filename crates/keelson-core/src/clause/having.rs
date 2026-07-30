use crate::expr::{Expr, IntoExpr};
use crate::writer::{Expression, SqlWriter};

/// `HAVING a AND b`
///
/// Structurally identical to [`Where`](super::Where) and deliberately a separate
/// type: a statement has at most one of each, and a `having` mod must not be
/// accepted where only a `WHERE` exists.
#[derive(Debug, Clone, Default)]
pub struct Having {
    /// The conditions, in the order they were appended.
    pub conditions: Vec<Expr>,
}

impl Having {
    /// Append one condition.
    pub fn append_having(&mut self, condition: impl IntoExpr) {
        self.conditions.push(condition.into_expr());
    }

    /// Whether the clause is absent.
    pub fn is_empty(&self) -> bool {
        self.conditions.is_empty()
    }
}

impl Expression for Having {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.write_slice(&self.conditions, "HAVING ", " AND ", "");
    }
}

/// A statement with a `HAVING` clause.
pub trait HasHaving {
    /// The `HAVING` clause to modify.
    fn having_mut(&mut self) -> &mut Having;
}

impl HasHaving for Having {
    fn having_mut(&mut self) -> &mut Having {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::expr::{Chain, arg};
    use crate::value::Value;
    use crate::writer::build;

    #[test]
    fn an_empty_having_writes_nothing() {
        assert_eq!(build(&Numbered, &Having::default()).unwrap().0, "");
        assert!(Having::default().is_empty());
    }

    #[test]
    fn conditions_are_and_joined() {
        let mut h = Having::default();
        h.append_having(Expr::func("count", "1").gt(arg(1i32)));
        h.append_having("sum(x) < 9");
        let (sql, args) = build(&Numbered, &h).unwrap();
        assert_eq!(sql, "HAVING (count(1) > $1) AND sum(x) < 9");
        assert_eq!(args, vec![Value::I32(1)]);
    }
}
