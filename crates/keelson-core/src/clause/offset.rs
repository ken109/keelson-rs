use crate::error::Result;
use crate::writer::{DynExpr, Expression, SqlWriter};

/// `OFFSET n`
///
/// Like [`Limit`](super::Limit) the count is an expression, since SQLite accepts
/// one and every dialect accepts a bound argument.
#[derive(Debug, Clone, Default)]
pub struct Offset {
    pub count: Option<DynExpr>,
}

impl Offset {
    pub fn set_offset(&mut self, count: DynExpr) {
        self.count = Some(count);
    }

    pub fn is_empty(&self) -> bool {
        self.count.is_none()
    }
}

impl Expression for Offset {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        if let Some(count) = &self.count {
            w.push_str("OFFSET ");
            w.write_expr(count)?;
        }
        Ok(())
    }
}

/// A query with an `OFFSET`.
pub trait HasOffset {
    fn offset_mut(&mut self) -> &mut Offset;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::writer::{build, dyn_expr};

    #[test]
    fn unset_writes_nothing_and_set_writes_the_keyword() {
        assert_eq!(build(&Numbered, &Offset::default()).unwrap().0, "");

        let mut o = Offset::default();
        o.set_offset(dyn_expr("5"));
        assert_eq!(build(&Numbered, &o).unwrap().0, "OFFSET 5");
    }
}
