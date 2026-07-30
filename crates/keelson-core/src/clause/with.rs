use crate::error::Result;
use crate::writer::{DynExpr, Expression, SqlWriter};

/// `WITH [RECURSIVE] <cte>, <cte>, …`
///
/// The keyword and the separator live here; the query supplies the leading
/// newline, so an absent `WITH` costs nothing.
#[derive(Debug, Clone, Default)]
pub struct With {
    pub recursive: bool,
    pub ctes: Vec<DynExpr>,
}

impl With {
    pub fn append_cte(&mut self, cte: DynExpr) {
        self.ctes.push(cte);
    }

    pub fn set_recursive(&mut self, recursive: bool) {
        self.recursive = recursive;
    }

    pub fn is_empty(&self) -> bool {
        self.ctes.is_empty()
    }
}

impl Expression for With {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        let prefix = if self.recursive {
            "WITH RECURSIVE\n"
        } else {
            "WITH\n"
        };
        w.write_slice(&self.ctes, prefix, ",\n", "")
    }
}

/// A query that accepts common table expressions.
pub trait HasWith {
    fn with_mut(&mut self) -> &mut With;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::writer::{build, dyn_expr};

    #[test]
    fn an_empty_with_writes_nothing_not_even_the_keyword() {
        let (sql, _) = build(&Numbered, &With::default()).unwrap();
        assert_eq!(sql, "");
    }

    #[test]
    fn ctes_are_comma_newline_separated() {
        let mut with = With::default();
        with.append_cte(dyn_expr("a AS (SELECT 1)"));
        with.append_cte(dyn_expr("b AS (SELECT 2)"));

        let (sql, _) = build(&Numbered, &with).unwrap();
        assert_eq!(sql, "WITH\na AS (SELECT 1),\nb AS (SELECT 2)");

        with.set_recursive(true);
        let (sql, _) = build(&Numbered, &with).unwrap();
        assert!(sql.starts_with("WITH RECURSIVE\n"));
    }
}
