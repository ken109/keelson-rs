use crate::error::Result;
use crate::writer::{DynExpr, Expression, SqlWriter};

/// `LIMIT n`
///
/// The count is an expression, not a number: SQLite accepts an arbitrary
/// expression and PostgreSQL takes a bound argument. Restricting it to a literal
/// is a dialect mod's job.
#[derive(Debug, Clone, Default)]
pub struct Limit {
    pub count: Option<DynExpr>,
}

impl Limit {
    pub fn set_limit(&mut self, count: DynExpr) {
        self.count = Some(count);
    }

    pub fn is_empty(&self) -> bool {
        self.count.is_none()
    }
}

impl Expression for Limit {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        if let Some(count) = &self.count {
            w.push_str("LIMIT ");
            w.write_expr(count)?;
        }
        Ok(())
    }
}

/// A query with a `LIMIT`.
pub trait HasLimit {
    fn limit_mut(&mut self) -> &mut Limit;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::writer::{build, dyn_expr, expr_fn};

    #[test]
    fn an_unset_limit_writes_nothing() {
        assert_eq!(build(&Numbered, &Limit::default()).unwrap().0, "");
        assert!(Limit::default().is_empty());
    }

    #[test]
    fn the_count_can_be_a_literal_or_an_argument() {
        let mut l = Limit::default();
        l.set_limit(dyn_expr("10"));
        assert_eq!(build(&Numbered, &l).unwrap().0, "LIMIT 10");

        l.set_limit(dyn_expr(expr_fn(|w: &mut SqlWriter<'_>| {
            w.push_arg(10i64);
            Ok(())
        })));
        let (sql, args) = build(&Numbered, &l).unwrap();
        assert_eq!(sql, "LIMIT $1");
        assert_eq!(args.len(), 1);
    }
}
