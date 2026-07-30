use crate::expr::{Expr, IntoExpr, IntoExprList};
use crate::writer::{Expression, SqlWriter};

use super::{MaybeAbsent, write_present};

/// Where the rows an `INSERT` adds come from.
///
/// Two shapes, in priority order — a query to insert from, or a list of rows:
///
/// ```text
/// INSERT INTO t (cols) VALUES ( expr [, ...] ) [, ...]
/// INSERT INTO t (cols) query
/// ```
///
/// bob has a third: with neither it writes `DEFAULT VALUES`. That is not done here,
/// because an absent clause has to render nothing, and because the spelling is not
/// shared — PostgreSQL and SQLite say `DEFAULT VALUES`, MySQL says `VALUES ()` or
/// `() VALUES ()`. An `INSERT` query type checks [`is_empty`](Self::is_empty) and
/// writes its own dialect's spelling.
#[derive(Debug, Clone, Default)]
pub struct Values {
    /// A query to insert the results of. Takes priority over
    /// [`rows`](Self::rows), because `INSERT … VALUES … SELECT …` is not a thing:
    /// the two are alternatives, and a query having been set is the more
    /// deliberate act.
    pub query: Option<Expr>,
    /// One entry per row.
    pub rows: Vec<ValuesRow>,
}

impl Values {
    /// Insert the results of `query`.
    pub fn from_query(query: impl IntoExpr) -> Self {
        Values {
            query: Some(query.into_expr()),
            rows: Vec::new(),
        }
    }

    /// Append one row.
    ///
    /// An empty row is ignored: `VALUES ()` is not valid in PostgreSQL or SQLite,
    /// and an insert with no values wants its dialect's "default row" spelling
    /// instead.
    pub fn append_values(&mut self, values: impl IntoExprList) {
        let row = values.into_expr_list();
        if row.is_empty() {
            return;
        }
        self.rows.push(ValuesRow(row));
    }

    /// Whether there is no row source at all.
    pub fn is_empty(&self) -> bool {
        self.query.is_none() && self.rows.is_empty()
    }
}

impl Expression for Values {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        if let Some(query) = &self.query {
            w.write_expr(query);
            return;
        }
        write_present(w, &self.rows, "VALUES ", ", ", "");
    }
}

/// An `INSERT` with a row source.
pub trait HasValues {
    /// The row source to modify.
    fn values_mut(&mut self) -> &mut Values;
}

impl HasValues for Values {
    fn values_mut(&mut self) -> &mut Values {
        self
    }
}

/// One parenthesised row of a `VALUES` list.
///
/// Named `ValuesRow` rather than bob's `Value` so that it cannot be confused with
/// [`Value`](crate::Value), the bound-argument enum.
#[derive(Debug, Clone, Default)]
pub struct ValuesRow(
    /// The cells, in column order.
    pub Vec<Expr>,
);

impl ValuesRow {
    /// A row from any expression list.
    pub fn new(cells: impl IntoExprList) -> Self {
        ValuesRow(cells.into_expr_list())
    }

    /// Whether the row has no cells.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Expression for ValuesRow {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.write_slice(&self.0, "(", ", ", ")");
    }
}

impl MaybeAbsent for ValuesRow {
    fn is_absent(&self) -> bool {
        self.is_empty()
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
    fn no_query_and_no_rows_writes_nothing() {
        assert_eq!(build(&Numbered, &Values::default()).unwrap().0, "");
        assert!(Values::default().is_empty());
    }

    #[test]
    fn rows_are_parenthesised_and_numbered_across_the_whole_list() {
        let mut v = Values::default();
        v.append_values((arg(1i32), arg("a")));
        v.append_values((arg(2i32), arg("b")));

        let (sql, args) = build(&Numbered, &v).unwrap();
        assert_eq!(sql, "VALUES ($1, $2), ($3, $4)");
        assert_eq!(
            args,
            vec![
                Value::I32(1),
                Value::Text("a".into()),
                Value::I32(2),
                Value::Text("b".into())
            ]
        );
    }

    #[test]
    fn a_cell_may_be_a_literal_a_keyword_or_a_sub_select() {
        let mut v = Values::default();
        v.append_values((
            arg(1i32),
            "DEFAULT",
            Expr::group(Expr::raw("SELECT max(id) FROM users")),
        ));
        assert_eq!(
            build(&Numbered, &v).unwrap().0,
            "VALUES ($1, DEFAULT, (SELECT max(id) FROM users))"
        );
    }

    #[test]
    fn an_empty_row_is_dropped_rather_than_written_as_empty_parentheses() {
        let mut v = Values::default();
        v.append_values(());
        assert!(v.rows.is_empty());
        assert_eq!(build(&Numbered, &v).unwrap().0, "");
        assert!(ValuesRow::default().is_empty());
    }

    #[test]
    fn a_query_wins_over_recorded_rows_and_writes_no_values_keyword() {
        let mut v = Values::default();
        v.append_values(arg(1i32));
        v.query = Some(Expr::raw("SELECT id FROM staging"));

        let (sql, args) = build(&Numbered, &v).unwrap();
        assert_eq!(sql, "SELECT id FROM staging");
        assert!(
            args.is_empty(),
            "the dropped rows must not leave their arguments behind"
        );

        assert_eq!(
            build(&Numbered, &Values::from_query(Expr::raw("SELECT 1")))
                .unwrap()
                .0,
            "SELECT 1"
        );
    }
}
