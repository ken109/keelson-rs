use crate::error::{Error, Result};
use crate::writer::{DynExpr, Expression, SqlWriter};

/// Breadth-first `SEARCH` order.
pub const SEARCH_BREADTH: &str = "BREADTH";
/// Depth-first `SEARCH` order.
pub const SEARCH_DEPTH: &str = "DEPTH";

/// One common table expression: `name(cols) AS [NOT MATERIALIZED] (query)`.
///
/// `query` is an erased expression rather than a dedicated query trait. A
/// dialect's query type renders itself with its own dialect via
/// [`SqlWriter::write_with_dialect`], which is exactly what bob's `bob.Query`
/// contract amounts to here, so nothing is lost by storing it as an expression.
///
/// Note that the column aliases are *not* quoted, while
/// [`TableRef`](super::TableRef)'s are. That asymmetry is bob's, and the
/// recorded fixtures depend on it.
#[derive(Debug, Clone, Default)]
pub struct Cte {
    pub query: Option<DynExpr>,
    pub name: String,
    pub columns: Vec<String>,
    /// `None` writes no materialisation keyword at all.
    pub materialized: Option<bool>,
    pub search: CteSearch,
    pub cycle: CteCycle,
}

impl Cte {
    pub fn new(name: impl Into<String>) -> Self {
        Cte {
            name: name.into(),
            ..Cte::default()
        }
    }
}

impl Expression for Cte {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        w.push_str(&self.name);
        w.write_slice(&self.columns, "(", ", ", ")")?;
        w.push_str(" AS ");

        match self.materialized {
            None => {}
            Some(true) => w.push_str("MATERIALIZED "),
            Some(false) => w.push_str("NOT MATERIALIZED "),
        }

        let query = self
            .query
            .as_ref()
            .ok_or(Error::Incomplete("the query of a CTE"))?;
        w.push_str("(");
        w.write_expr(query)?;
        w.push_str(")");

        w.write_if(!self.search.columns.is_empty(), "\n", &self.search, "")?;
        w.write_if(!self.cycle.columns.is_empty(), "\n", &self.cycle, "")?;

        Ok(())
    }
}

/// `SEARCH { BREADTH | DEPTH } FIRST BY <cols> SET <col>`
#[derive(Debug, Clone, Default)]
pub struct CteSearch {
    /// [`SEARCH_BREADTH`] or [`SEARCH_DEPTH`].
    pub order: String,
    pub columns: Vec<String>,
    pub set: String,
}

impl Expression for CteSearch {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        w.push_str("SEARCH ");
        w.push_str(&self.order);
        w.push_str(" FIRST BY ");
        w.write_slice(&self.columns, "", ", ", "")?;
        w.push_str(" SET ");
        w.push_str(&self.set);
        Ok(())
    }
}

/// `CYCLE <cols> SET <col> [TO <val> DEFAULT <val>] USING <col>`
#[derive(Debug, Clone, Default)]
pub struct CteCycle {
    pub columns: Vec<String>,
    pub set: String,
    pub using: String,
    pub set_val: Option<DynExpr>,
    pub default_val: Option<DynExpr>,
}

impl Expression for CteCycle {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        w.push_str("CYCLE ");
        w.write_slice(&self.columns, "", ", ", "")?;
        w.push_str(" SET ");
        w.push_str(&self.set);

        if let Some(v) = &self.set_val {
            w.push_str(" TO ");
            w.write_expr(v)?;
        }
        if let Some(v) = &self.default_val {
            w.push_str(" DEFAULT ");
            w.write_expr(v)?;
        }

        w.push_str(" USING ");
        w.push_str(&self.using);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::writer::{build, dyn_expr, expr_fn};

    fn sub() -> DynExpr {
        dyn_expr(expr_fn(|w: &mut SqlWriter<'_>| {
            w.push_str("SELECT ");
            w.push_arg(1i32);
            Ok(())
        }))
    }

    #[test]
    fn a_bare_cte_is_name_as_query() {
        let cte = Cte {
            query: Some(sub()),
            ..Cte::new("c")
        };
        let (sql, args) = build(&Numbered, &cte).unwrap();
        assert_eq!(sql, "c AS (SELECT $1)");
        assert_eq!(args.len(), 1);
    }

    #[test]
    fn column_aliases_are_written_verbatim() {
        let cte = Cte {
            query: Some(sub()),
            columns: vec!["id".into(), "data".into()],
            ..Cte::new("c")
        };
        let (sql, _) = build(&Numbered, &cte).unwrap();
        assert_eq!(sql, "c(id, data) AS (SELECT $1)");
    }

    #[test]
    fn materialisation_is_three_valued() {
        let base = Cte {
            query: Some(sub()),
            ..Cte::new("c")
        };

        let yes = Cte {
            materialized: Some(true),
            ..base.clone()
        };
        assert_eq!(
            build(&Numbered, &yes).unwrap().0,
            "c AS MATERIALIZED (SELECT $1)"
        );

        let no = Cte {
            materialized: Some(false),
            ..base.clone()
        };
        assert_eq!(
            build(&Numbered, &no).unwrap().0,
            "c AS NOT MATERIALIZED (SELECT $1)"
        );

        assert_eq!(build(&Numbered, &base).unwrap().0, "c AS (SELECT $1)");
    }

    #[test]
    fn search_and_cycle_appear_only_when_they_have_columns() {
        let mut cte = Cte {
            query: Some(sub()),
            ..Cte::new("c")
        };
        cte.search = CteSearch {
            order: SEARCH_DEPTH.into(),
            columns: vec!["id".into()],
            set: "ordercol".into(),
        };
        // No columns, so the whole CYCLE clause stays out even though Set is
        // filled in.
        cte.cycle = CteCycle {
            set: "is_cycle".into(),
            using: "path".into(),
            ..CteCycle::default()
        };

        let (sql, _) = build(&Numbered, &cte).unwrap();
        assert_eq!(
            sql,
            "c AS (SELECT $1)\nSEARCH DEPTH FIRST BY id SET ordercol"
        );

        cte.cycle.columns = vec!["id".into()];
        cte.cycle.set_val = Some(dyn_expr("true"));
        cte.cycle.default_val = Some(dyn_expr("false"));
        let (sql, _) = build(&Numbered, &cte).unwrap();
        assert!(
            sql.ends_with("\nCYCLE id SET is_cycle TO true DEFAULT false USING path"),
            "got {sql}"
        );
    }

    #[test]
    fn a_cte_without_a_query_is_an_error() {
        assert!(matches!(
            build(&Numbered, &Cte::new("c")),
            Err(Error::Incomplete(_))
        ));
    }
}
