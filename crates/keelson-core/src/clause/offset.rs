use crate::expr::{Expr, IntoExpr};
use crate::writer::{Expression, SqlWriter};

/// The `ROW`/`ROWS` synonym pair, spelled the way the human reading the SQL
/// expects.
///
/// The two spellings mean the same thing everywhere the grammar offers them —
/// `OFFSET n { ROW | ROWS }` and `FETCH { FIRST | NEXT } n { ROW | ROWS }` — but
/// unlike a grammar *default* such as `SELECT ALL`, writing one is not writing
/// nothing: the keyword shows up whenever generated SQL is read or diffed against
/// a hand-written query, so both spellings are representable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RowsKeyword {
    /// `ROW`, the singular.
    Row,
    /// `ROWS`.
    #[default]
    Rows,
}

impl RowsKeyword {
    /// The keyword itself.
    pub fn as_str(self) -> &'static str {
        match self {
            RowsKeyword::Row => "ROW",
            RowsKeyword::Rows => "ROWS",
        }
    }
}

/// `OFFSET n [ ROW | ROWS ]`
///
/// Like [`Limit`](super::Limit) the count is an expression: SQLite accepts one, and
/// every dialect accepts a bound argument.
///
/// The trailing keyword is the SQL-standard spelling and PostgreSQL accepts it;
/// `None` writes the bare historical `OFFSET n`, which every dialect takes.
#[derive(Debug, Clone, Default)]
pub struct Offset {
    /// How many rows to skip.
    pub count: Option<Expr>,
    /// The optional trailing `ROW`/`ROWS`. `None` writes neither.
    pub rows: Option<RowsKeyword>,
}

impl Offset {
    /// Set the count.
    pub fn set_offset(&mut self, count: impl IntoExpr) {
        self.count = Some(count.into_expr());
    }

    /// Whether the clause is absent. The keyword rides on the count, so it does
    /// not make an otherwise-empty clause present.
    pub fn is_empty(&self) -> bool {
        self.count.is_none()
    }
}

impl Expression for Offset {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        let Some(count) = &self.count else {
            return;
        };

        w.push_str("OFFSET ");
        w.write_expr(count);
        if let Some(rows) = self.rows {
            w.push_str(" ");
            w.push_str(rows.as_str());
        }
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
    use keelson_sqlcheck::testing::assert_frag_sql;

    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::expr::arg;
    use crate::value::Value;
    use crate::writer::build;

    const FRAME: &str = r#"SELECT "id" FROM users {}"#;

    #[test]
    fn an_unset_offset_writes_nothing() {
        assert_frag_sql(FRAME, &build(&Numbered, &Offset::default()).unwrap().0, "");
        assert!(Offset::default().is_empty());
    }

    #[test]
    fn the_count_is_written_after_the_keyword() {
        let mut o = Offset::default();
        o.set_offset(5i64);
        assert_frag_sql(FRAME, &build(&Numbered, &o).unwrap().0, "OFFSET 5");

        o.set_offset(arg(5i64));
        let (sql, args) = build(&Numbered, &o).unwrap();
        assert_frag_sql(FRAME, &sql, "OFFSET $1");
        assert_eq!(args, vec![Value::I64(5)]);
    }

    /// PostgreSQL 17, `sql-select.html`: `[ OFFSET start [ ROW | ROWS ] ]` — the
    /// keyword follows the count and either number is legal whatever the count.
    #[test]
    fn the_rows_keyword_follows_the_count_in_either_number() {
        let mut o = Offset::default();
        o.set_offset(5i64);
        o.rows = Some(RowsKeyword::Rows);
        assert_frag_sql(FRAME, &build(&Numbered, &o).unwrap().0, "OFFSET 5 ROWS");

        o.rows = Some(RowsKeyword::Row);
        assert_frag_sql(FRAME, &build(&Numbered, &o).unwrap().0, "OFFSET 5 ROW");
    }

    /// With the keyword, the count is `gram.y`'s `select_fetch_first_value` — a
    /// `c_expr` — and a placeholder is one, so binding still works.
    #[test]
    fn the_count_may_be_bound_with_the_keyword_present() {
        let mut o = Offset::default();
        o.set_offset(arg(5i64));
        o.rows = Some(RowsKeyword::Rows);
        let (sql, args) = build(&Numbered, &o).unwrap();
        assert_frag_sql(FRAME, &sql, "OFFSET $1 ROWS");
        assert_eq!(args, vec![Value::I64(5)]);
    }

    /// The keyword rides on the count: without one there is nothing for it to
    /// follow, so it does not write a dangling `OFFSET ROWS`.
    #[test]
    fn the_keyword_alone_writes_nothing() {
        let o = Offset {
            count: None,
            rows: Some(RowsKeyword::Rows),
        };
        assert_frag_sql(FRAME, &build(&Numbered, &o).unwrap().0, "");
        assert!(o.is_empty());
    }
}
