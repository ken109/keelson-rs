use crate::error::Result;
use crate::writer::{DynExpr, Expression, SqlWriter};

/// `HAVING a AND b`
#[derive(Debug, Clone, Default)]
pub struct Having {
    pub conditions: Vec<DynExpr>,
}

impl Having {
    pub fn append_having(&mut self, condition: DynExpr) {
        self.conditions.push(condition);
    }

    pub fn is_empty(&self) -> bool {
        self.conditions.is_empty()
    }
}

impl Expression for Having {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        w.write_slice(&self.conditions, "HAVING ", " AND ", "")
    }
}

/// A query with a `HAVING` clause.
pub trait HasHaving {
    fn having_mut(&mut self) -> &mut Having;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::writer::{build, dyn_expr};

    #[test]
    fn empty_writes_nothing_and_conditions_are_and_joined() {
        assert_eq!(build(&Numbered, &Having::default()).unwrap().0, "");

        let mut h = Having::default();
        h.append_having(dyn_expr("count(*) > 1"));
        h.append_having(dyn_expr("sum(x) < 9"));
        assert_eq!(
            build(&Numbered, &h).unwrap().0,
            "HAVING count(*) > 1 AND sum(x) < 9"
        );
    }
}
