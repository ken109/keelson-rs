use crate::error::Result;
use crate::writer::{DynExpr, Expression, SqlWriter};

/// `FETCH NEXT n ROWS ONLY` — the SQL-standard spelling of `LIMIT`.
#[derive(Debug, Clone, Default)]
pub struct Fetch {
    pub count: Option<DynExpr>,
    /// `ROWS WITH TIES` instead of `ROWS ONLY`.
    pub with_ties: bool,
}

impl Fetch {
    pub fn set_fetch(&mut self, fetch: Fetch) {
        *self = fetch;
    }

    pub fn is_empty(&self) -> bool {
        self.count.is_none()
    }
}

impl Expression for Fetch {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        // `WITH TIES` alone is meaningless, so the count gates the whole clause.
        let Some(count) = &self.count else {
            return Ok(());
        };

        w.push_str("FETCH NEXT ");
        w.write_expr(count)?;
        w.push_str(if self.with_ties {
            " ROWS WITH TIES"
        } else {
            " ROWS ONLY"
        });
        Ok(())
    }
}

/// A query with a `FETCH` clause.
pub trait HasFetch {
    fn fetch_mut(&mut self) -> &mut Fetch;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::writer::{build, dyn_expr};

    #[test]
    fn with_ties_without_a_count_writes_nothing() {
        let f = Fetch {
            with_ties: true,
            count: None,
        };
        assert_eq!(build(&Numbered, &f).unwrap().0, "");
    }

    #[test]
    fn the_suffix_switches_on_with_ties() {
        let mut f = Fetch {
            count: Some(dyn_expr("3")),
            with_ties: false,
        };
        assert_eq!(build(&Numbered, &f).unwrap().0, "FETCH NEXT 3 ROWS ONLY");

        f.with_ties = true;
        assert_eq!(
            build(&Numbered, &f).unwrap().0,
            "FETCH NEXT 3 ROWS WITH TIES"
        );
    }
}
