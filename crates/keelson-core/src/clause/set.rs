use crate::error::Result;
use crate::writer::{DynExpr, Expression, SqlWriter};

/// The assignment list of an `UPDATE`, or of `DO UPDATE` / `ON DUPLICATE KEY
/// UPDATE`.
///
/// The `SET` keyword is *not* written here: an update statement puts it right
/// after the table, while a conflict clause puts it after `DO UPDATE`, so the
/// caller supplies it. Assignments are separated by `,\n`.
#[derive(Debug, Clone, Default)]
pub struct Set {
    pub exprs: Vec<DynExpr>,
}

impl Set {
    pub fn append_set(&mut self, exprs: impl IntoIterator<Item = DynExpr>) {
        self.exprs.extend(exprs);
    }

    pub fn is_empty(&self) -> bool {
        self.exprs.is_empty()
    }
}

impl Expression for Set {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        w.write_slice(&self.exprs, "", ",\n", "")
    }
}

/// A query — or a [`ConflictClause`](super::ConflictClause) — with assignments.
pub trait HasSet {
    fn set_mut(&mut self) -> &mut Set;
}

impl HasSet for Set {
    fn set_mut(&mut self) -> &mut Set {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::writer::{build, dyn_expr};

    #[test]
    fn assignments_are_comma_newline_separated_without_a_keyword() {
        let mut s = Set::default();
        s.append_set([dyn_expr(r#""a" = 1"#), dyn_expr(r#""b" = 2"#)]);
        assert_eq!(build(&Numbered, &s).unwrap().0, "\"a\" = 1,\n\"b\" = 2");
    }

    #[test]
    fn an_empty_set_writes_nothing() {
        assert_eq!(build(&Numbered, &Set::default()).unwrap().0, "");
    }
}
