use crate::error::Result;
use crate::writer::{DynExpr, Expression, SqlWriter, dyn_expr};

/// `CAST(expr AS type)`.
#[derive(Debug, Clone)]
pub struct Cast {
    e: DynExpr,
    type_name: String,
}

impl Cast {
    /// `CAST(e AS type_name)`.
    pub fn new(e: impl Expression + 'static, type_name: impl Into<String>) -> Self {
        Cast {
            e: dyn_expr(e),
            type_name: type_name.into(),
        }
    }

    /// The target type, as written.
    pub fn type_name(&self) -> &str {
        &self.type_name
    }
}

/// `CAST(e AS type_name)`.
pub fn cast(e: impl Expression + 'static, type_name: impl Into<String>) -> Cast {
    Cast::new(e, type_name)
}

impl Expression for Cast {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        w.push_str("CAST(");
        w.write_expr(&self.e)?;
        w.push_str(" AS ");
        w.push_str(&self.type_name);
        w.push_str(")");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::arg;
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::writer::build;

    #[test]
    fn a_cast_wraps_its_operand() {
        let (sql, _) = build(&Numbered, &cast("a", "int")).unwrap();
        assert_eq!(sql, "CAST(a AS int)");
    }

    #[test]
    fn a_cast_operand_may_bind_args() {
        let (sql, vals) = build(&Numbered, &cast(arg("2020-01-01"), "date")).unwrap();
        assert_eq!(sql, "CAST($1 AS date)");
        assert_eq!(vals.len(), 1);
    }
}
