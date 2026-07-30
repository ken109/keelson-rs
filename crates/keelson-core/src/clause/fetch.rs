use super::offset::RowsKeyword;
use crate::expr::{Expr, IntoExpr};
use crate::writer::{Expression, SqlWriter};

/// `FETCH { FIRST | NEXT } n { ROW | ROWS } { ONLY | WITH TIES }` — the
/// SQL-standard spelling of `LIMIT`, and the only one that can ask for ties.
///
/// From PostgreSQL 17:
///
/// ```text
/// [ FETCH { FIRST | NEXT } [ count ] { ROW | ROWS } { ONLY | WITH TIES } ]
/// ```
///
/// `FIRST`/`NEXT` and `ROW`/`ROWS` are pure synonyms in the grammar, but not on
/// the page: which one a query says is visible whenever generated SQL is read or
/// diffed against a hand-written statement, so every spelling is representable.
/// The defaults write `FETCH NEXT n ROWS`. `WITH TIES` requires an `ORDER BY`,
/// which is the statement's business rather than this clause's.
#[derive(Debug, Clone, Default)]
pub struct Fetch {
    /// How many rows.
    pub count: Option<Expr>,
    /// `FETCH FIRST` rather than `FETCH NEXT`.
    pub first_or_next: FirstOrNext,
    /// `ROW` or `ROWS`, either legal whatever the count.
    pub rows: RowsKeyword,
    /// `WITH TIES` rather than `ONLY`: also return rows that tie with
    /// the last one under the `ORDER BY`.
    pub with_ties: bool,
}

/// The `FIRST`/`NEXT` synonym pair of a `FETCH` — `gram.y` calls the production
/// `first_or_next`. Synonyms in the grammar, distinct on the page, exactly as
/// [`RowsKeyword`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FirstOrNext {
    /// `FETCH FIRST`.
    First,
    /// `FETCH NEXT`.
    #[default]
    Next,
}

impl FirstOrNext {
    /// The keyword, as written after `FETCH`.
    pub fn as_str(self) -> &'static str {
        match self {
            FirstOrNext::First => "FIRST",
            FirstOrNext::Next => "NEXT",
        }
    }
}

impl Fetch {
    /// Fetch `count` rows, without ties, in the default spelling.
    pub fn new(count: impl IntoExpr) -> Self {
        Fetch {
            count: Some(count.into_expr()),
            ..Fetch::default()
        }
    }

    /// Set the count.
    pub fn set_fetch(&mut self, count: impl IntoExpr) {
        self.count = Some(count.into_expr());
    }

    /// Whether the clause is absent.
    pub fn is_empty(&self) -> bool {
        self.count.is_none()
    }
}

impl Expression for Fetch {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        // The spelling choices on their own say nothing, so the count gates the
        // clause. The count is formally optional in the grammar — `FETCH NEXT
        // ROWS ONLY` means one row — but that form is a trap, and asking for it
        // explicitly with `Fetch::new(1)` is clearer than making the field's
        // absence mean two different things.
        let Some(count) = &self.count else {
            return;
        };

        w.push_str("FETCH ");
        w.push_str(self.first_or_next.as_str());
        w.push_str(" ");
        w.write_expr(count);
        w.push_str(" ");
        w.push_str(self.rows.as_str());
        w.push_str(if self.with_ties {
            " WITH TIES"
        } else {
            " ONLY"
        });
    }
}

/// A statement with a `FETCH` clause.
pub trait HasFetch {
    /// The `FETCH` clause to modify.
    fn fetch_mut(&mut self) -> &mut Fetch;
}

impl HasFetch for Fetch {
    fn fetch_mut(&mut self) -> &mut Fetch {
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

    /// The frame carries an `ORDER BY` because PostgreSQL refuses `WITH TIES`
    /// without one — the ties are ties *in the sort order*.
    const FRAME: &str = r#"SELECT "id" FROM users ORDER BY "id" {}"#;

    fn sql(f: &Fetch) -> String {
        build(&Numbered, f).expect("render").0
    }

    #[test]
    fn an_unset_fetch_writes_nothing_even_with_spellings_asked_for() {
        let f = Fetch {
            count: None,
            first_or_next: FirstOrNext::First,
            rows: RowsKeyword::Row,
            with_ties: true,
        };
        assert_frag_sql(FRAME, &sql(&f), "");
        assert!(Fetch::default().is_empty());
    }

    #[test]
    fn the_suffix_switches_on_with_ties() {
        let mut f = Fetch::new(3i64);
        assert_frag_sql(FRAME, &sql(&f), "FETCH NEXT 3 ROWS ONLY");

        f.with_ties = true;
        assert_frag_sql(FRAME, &sql(&f), "FETCH NEXT 3 ROWS WITH TIES");
    }

    /// PostgreSQL 17, `sql-select.html`: `FETCH { FIRST | NEXT } [ count ]
    /// { ROW | ROWS } …` — the spellings vary independently, and either number
    /// is legal whatever the count.
    #[test]
    fn first_and_row_are_the_other_spellings() {
        let mut f = Fetch::new(3i64);
        f.first_or_next = FirstOrNext::First;
        assert_frag_sql(FRAME, &sql(&f), "FETCH FIRST 3 ROWS ONLY");

        f.rows = RowsKeyword::Row;
        assert_frag_sql(FRAME, &sql(&f), "FETCH FIRST 3 ROW ONLY");

        f.first_or_next = FirstOrNext::Next;
        f.set_fetch(1i64);
        assert_frag_sql(FRAME, &sql(&f), "FETCH NEXT 1 ROW ONLY");
    }

    #[test]
    fn the_count_may_be_bound() {
        let mut f = Fetch::default();
        f.set_fetch(arg(3i64));
        let (rendered, args) = build(&Numbered, &f).unwrap();
        assert_frag_sql(FRAME, &rendered, "FETCH NEXT $1 ROWS ONLY");
        assert_eq!(args, vec![Value::I64(3)]);
    }

    /// The spellings compose with a bound count and with ties: `FIRST` is a
    /// synonym, not a different clause.
    #[test]
    fn first_composes_with_a_bound_count_and_ties() {
        let mut f = Fetch::default();
        f.set_fetch(arg(3i64));
        f.first_or_next = FirstOrNext::First;
        f.with_ties = true;
        let (rendered, args) = build(&Numbered, &f).unwrap();
        assert_frag_sql(FRAME, &rendered, "FETCH FIRST $1 ROWS WITH TIES");
        assert_eq!(args, vec![Value::I64(3)]);
    }
}
