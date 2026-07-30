use crate::error::Result;
use crate::writer::{DynExpr, Expression, SqlWriter};

/// The source of the rows an `INSERT` adds.
///
/// Three mutually exclusive shapes, in priority order: a query to insert from, a
/// list of value rows, or nothing at all — which means `DEFAULT VALUES`.
#[derive(Debug, Clone, Default)]
pub struct Values {
    /// `INSERT INTO t SELECT …`. Takes priority over [`vals`](Self::vals).
    pub query: Option<DynExpr>,
    /// One [`ValuesRow`] per row.
    pub vals: Vec<ValuesRow>,
}

impl Values {
    /// Append one row. An empty row is ignored, because `VALUES ()` is not valid
    /// anywhere and an insert with no columns wants `DEFAULT VALUES` instead.
    pub fn append_values(&mut self, vals: impl IntoIterator<Item = DynExpr>) {
        let row: Vec<DynExpr> = vals.into_iter().collect();
        if row.is_empty() {
            return;
        }
        self.vals.push(ValuesRow(row));
    }
}

impl Expression for Values {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        if let Some(query) = &self.query {
            return w.write_expr(query);
        }

        if !self.vals.is_empty() {
            return w.write_slice(&self.vals, "VALUES ", ", ", "");
        }

        w.push_str("DEFAULT VALUES");
        Ok(())
    }
}

/// An insert statement with a row source.
pub trait HasValues {
    fn values_mut(&mut self) -> &mut Values;
}

/// One parenthesised row of an insert's `VALUES`.
///
/// Named `ValuesRow` rather than bob's `Value` so that it cannot be confused with
/// [`Value`](crate::Value), the bound-argument enum.
#[derive(Debug, Clone, Default)]
pub struct ValuesRow(pub Vec<DynExpr>);

impl ValuesRow {
    pub fn new(exprs: impl IntoIterator<Item = DynExpr>) -> Self {
        ValuesRow(exprs.into_iter().collect())
    }
}

impl Expression for ValuesRow {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        w.write_slice(&self.0, "(", ", ", ")")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::writer::{build, dyn_expr, expr_fn};

    fn arg(v: i32) -> DynExpr {
        dyn_expr(expr_fn(move |w: &mut SqlWriter<'_>| {
            w.push_arg(v);
            Ok(())
        }))
    }

    #[test]
    fn no_rows_and_no_query_means_default_values() {
        assert_eq!(
            build(&Numbered, &Values::default()).unwrap().0,
            "DEFAULT VALUES"
        );
    }

    #[test]
    fn rows_are_parenthesised_and_numbered_across_the_whole_list() {
        let mut v = Values::default();
        v.append_values([arg(1), arg(2)]);
        v.append_values([arg(3), arg(4)]);

        let (sql, args) = build(&Numbered, &v).unwrap();
        assert_eq!(sql, "VALUES ($1, $2), ($3, $4)");
        assert_eq!(args.len(), 4);
    }

    #[test]
    fn an_empty_row_is_dropped_rather_than_written_as_empty_parens() {
        let mut v = Values::default();
        v.append_values(Vec::new());
        assert!(v.vals.is_empty());
        assert_eq!(build(&Numbered, &v).unwrap().0, "DEFAULT VALUES");
    }

    #[test]
    fn a_query_wins_over_recorded_rows() {
        let mut v = Values::default();
        v.append_values([arg(1)]);
        v.query = Some(dyn_expr("SELECT * FROM tmp_films"));
        assert_eq!(
            build(&Numbered, &v).unwrap().0,
            "SELECT * FROM tmp_films",
            "no VALUES keyword and no arguments from the dropped rows"
        );
        assert!(build(&Numbered, &v).unwrap().1.is_empty());
    }
}
