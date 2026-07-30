use crate::error::Result;
use crate::writer::{DynExpr, Expression, SqlWriter};

/// `RETURNING a, b`
///
/// Whether the clause is present decides whether a mutation runs as a query or
/// as an exec, so [`has_returning`](Self::has_returning) is read by the
/// execution layer, not only by the writer.
#[derive(Debug, Clone, Default)]
pub struct Returning {
    pub expressions: Vec<DynExpr>,
}

impl Returning {
    pub fn has_returning(&self) -> bool {
        !self.expressions.is_empty()
    }

    pub fn append_returning(&mut self, columns: impl IntoIterator<Item = DynExpr>) {
        self.expressions.extend(columns);
    }

    pub fn is_empty(&self) -> bool {
        self.expressions.is_empty()
    }
}

impl Expression for Returning {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        w.write_slice(&self.expressions, "RETURNING ", ", ", "")
    }
}

/// A mutation that can return rows.
pub trait HasReturning {
    fn returning_mut(&mut self) -> &mut Returning;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::writer::{build, dyn_expr};

    #[test]
    fn empty_writes_nothing_and_reports_no_returning() {
        let r = Returning::default();
        assert_eq!(build(&Numbered, &r).unwrap().0, "");
        assert!(!r.has_returning());
    }

    #[test]
    fn columns_are_comma_separated() {
        let mut r = Returning::default();
        r.append_returning([dyn_expr("id"), dyn_expr("name")]);
        assert_eq!(build(&Numbered, &r).unwrap().0, "RETURNING id, name");
        assert!(r.has_returning());
    }
}
