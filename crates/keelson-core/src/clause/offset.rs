use crate::expr::{Expr, IntoExpr};
use crate::writer::{Expression, SqlWriter};

/// `OFFSET n`
///
/// Like [`Limit`](super::Limit) the count is an expression: SQLite accepts one, and
/// every dialect accepts a bound argument.
#[derive(Debug, Clone, Default)]
pub struct Offset {
    /// How many rows to skip.
    pub count: Option<Expr>,
}

impl Offset {
    /// Set the count.
    pub fn set_offset(&mut self, count: impl IntoExpr) {
        self.count = Some(count.into_expr());
    }

    /// Whether the clause is absent.
    pub fn is_empty(&self) -> bool {
        self.count.is_none()
    }
}

impl Expression for Offset {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.write_if_some(self.count.as_ref(), "OFFSET ", "");
    }
}

/// A statement with an `OFFSET`.
pub trait HasOffset {
    /// The `OFFSET` clause to modify.
    fn offset_mut(&mut self) -> &mut Offset;
}

impl HasOffset for Offset {
    fn offset_mut(&mut self) -> &mut Offset {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::expr::arg;
    use crate::value::Value;
    use crate::writer::build;

    #[test]
    fn an_unset_offset_writes_nothing() {
        assert_eq!(build(&Numbered, &Offset::default()).unwrap().0, "");
        assert!(Offset::default().is_empty());
    }

    #[test]
    fn the_count_is_written_after_the_keyword() {
        let mut o = Offset::default();
        o.set_offset(5i64);
        assert_eq!(build(&Numbered, &o).unwrap().0, "OFFSET 5");

        o.set_offset(arg(5i64));
        let (sql, args) = build(&Numbered, &o).unwrap();
        assert_eq!(sql, "OFFSET $1");
        assert_eq!(args, vec![Value::I64(5)]);
    }
}
