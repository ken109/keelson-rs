use crate::expr::{Expr, IntoExpr};
use crate::writer::{Expression, SqlWriter};

/// `FETCH NEXT n ROWS ONLY` — the SQL-standard spelling of `LIMIT`, and the only
/// one that can ask for ties.
///
/// From PostgreSQL 17:
///
/// ```text
/// [ FETCH { FIRST | NEXT } [ count ] { ROW | ROWS } { ONLY | WITH TIES } ]
/// ```
///
/// `FIRST`/`NEXT` and `ROW`/`ROWS` are pure synonyms, so only one spelling of each
/// is representable. `WITH TIES` requires an `ORDER BY`, which is the statement's
/// business rather than this clause's.
#[derive(Debug, Clone, Default)]
pub struct Fetch {
    /// How many rows.
    pub count: Option<Expr>,
    /// `ROWS WITH TIES` rather than `ROWS ONLY`: also return rows that tie with
    /// the last one under the `ORDER BY`.
    pub with_ties: bool,
}

impl Fetch {
    /// Fetch `count` rows, without ties.
    pub fn new(count: impl IntoExpr) -> Self {
        Fetch {
            count: Some(count.into_expr()),
            with_ties: false,
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
        // `WITH TIES` on its own says nothing, so the count gates the clause. The
        // count is formally optional in the grammar — `FETCH NEXT ROWS ONLY` means
        // one row — but that form is a trap, and asking for it explicitly with
        // `Fetch::new(1)` is clearer than making the field's absence mean two
        // different things.
        let Some(count) = &self.count else {
            return;
        };

        w.push_str("FETCH NEXT ");
        w.write_expr(count);
        w.push_str(if self.with_ties {
            " ROWS WITH TIES"
        } else {
            " ROWS ONLY"
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
    fn an_unset_fetch_writes_nothing_even_with_ties_asked_for() {
        let f = Fetch {
            count: None,
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

    #[test]
    fn the_count_may_be_bound() {
        let mut f = Fetch::default();
        f.set_fetch(arg(3i64));
        let (rendered, args) = build(&Numbered, &f).unwrap();
        assert_frag_sql(FRAME, &rendered, "FETCH NEXT $1 ROWS ONLY");
        assert_eq!(args, vec![Value::I64(3)]);
    }
}
