use crate::error::Result;
use crate::writer::{DynExpr, Expression, SqlWriter};

/// `WHERE a AND b AND c`
///
/// Conditions accumulate and are always joined with `AND`; `OR` is built inside
/// a single condition expression, not here.
#[derive(Debug, Clone, Default)]
pub struct Where {
    pub conditions: Vec<DynExpr>,
}

impl Where {
    pub fn append_where(&mut self, condition: DynExpr) {
        self.conditions.push(condition);
    }

    pub fn is_empty(&self) -> bool {
        self.conditions.is_empty()
    }
}

impl Expression for Where {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        w.write_slice(&self.conditions, "WHERE ", " AND ", "")
    }
}

/// A query with a `WHERE` clause.
///
/// Also implemented by [`ConflictClause`](super::ConflictClause), so a `where_`
/// mod written once works inside `ON CONFLICT … DO UPDATE` too.
pub trait HasWhere {
    fn where_mut(&mut self) -> &mut Where;
}

impl HasWhere for Where {
    fn where_mut(&mut self) -> &mut Where {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::writer::{build, dyn_expr, expr_fn};

    #[test]
    fn an_empty_where_writes_nothing() {
        let (sql, _) = build(&Numbered, &Where::default()).unwrap();
        assert_eq!(sql, "");
    }

    #[test]
    fn conditions_are_and_joined_and_number_in_order() {
        let mut wh = Where::default();
        for v in [1i32, 2, 3] {
            wh.append_where(dyn_expr(expr_fn(move |w: &mut SqlWriter<'_>| {
                w.push_str("(x = ");
                w.push_arg(v);
                w.push_str(")");
                Ok(())
            })));
        }
        let (sql, args) = build(&Numbered, &wh).unwrap();
        assert_eq!(sql, "WHERE (x = $1) AND (x = $2) AND (x = $3)");
        assert_eq!(args.len(), 3);
    }
}
