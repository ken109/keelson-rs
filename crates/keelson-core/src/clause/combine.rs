use crate::error::{Error, Result};
use crate::writer::{DynExpr, Expression, SqlWriter};

pub const UNION: &str = "UNION";
pub const INTERSECT: &str = "INTERSECT";
pub const EXCEPT: &str = "EXCEPT";

/// Every set operation chained onto one statement.
///
/// Newline-separated, with no prefix of its own; the query supplies the leading
/// newline the way it does for [`Locks`](super::Locks).
#[derive(Debug, Clone, Default)]
pub struct Combines {
    pub queries: Vec<Combine>,
}

impl Combines {
    pub fn append_combine(&mut self, combine: Combine) {
        self.queries.push(combine);
    }

    pub fn is_empty(&self) -> bool {
        self.queries.is_empty()
    }
}

impl Expression for Combines {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        w.write_slice(&self.queries, "", "\n", "")
    }
}

/// A query that other queries can be unioned onto.
pub trait HasCombines {
    fn combines_mut(&mut self) -> &mut Combines;
}

/// `UNION [ALL] (<query>)`
///
/// The operand is always parenthesised, so chaining several never changes how
/// the result associates.
#[derive(Debug, Clone, Default)]
pub struct Combine {
    /// [`UNION`], [`INTERSECT`] or [`EXCEPT`].
    pub strategy: String,
    pub query: Option<DynExpr>,
    pub all: bool,
}

impl Combine {
    pub fn set_combine(&mut self, combine: Combine) {
        *self = combine;
    }
}

impl Expression for Combine {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        if self.strategy.is_empty() {
            return Err(Error::other("combination strategy must be set"));
        }

        w.push_str(&self.strategy);
        w.push_str(if self.all { " ALL " } else { " " });

        let query = self
            .query
            .as_ref()
            .ok_or(Error::Incomplete("the query of a set operation"))?;
        w.push_str("(");
        w.write_expr(query)?;
        w.push_str(")");

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::writer::{build, dyn_expr, expr_fn};

    fn sub(v: i32) -> DynExpr {
        dyn_expr(expr_fn(move |w: &mut SqlWriter<'_>| {
            w.push_str("SELECT ");
            w.push_arg(v);
            Ok(())
        }))
    }

    #[test]
    fn all_switches_the_separator_not_the_parentheses() {
        let mut c = Combine {
            strategy: UNION.into(),
            query: Some(sub(1)),
            all: false,
        };
        assert_eq!(build(&Numbered, &c).unwrap().0, "UNION (SELECT $1)");

        c.all = true;
        assert_eq!(build(&Numbered, &c).unwrap().0, "UNION ALL (SELECT $1)");
    }

    #[test]
    fn a_missing_strategy_is_an_error() {
        let c = Combine {
            query: Some(sub(1)),
            ..Combine::default()
        };
        assert_eq!(
            build(&Numbered, &c).unwrap_err().to_string(),
            "combination strategy must be set"
        );
    }

    #[test]
    fn a_missing_query_is_an_error() {
        let c = Combine {
            strategy: EXCEPT.into(),
            ..Combine::default()
        };
        assert!(matches!(build(&Numbered, &c), Err(Error::Incomplete(_))));
    }

    #[test]
    fn chained_combines_keep_numbering_across_operands() {
        let mut cs = Combines::default();
        cs.append_combine(Combine {
            strategy: UNION.into(),
            query: Some(sub(1)),
            all: false,
        });
        cs.append_combine(Combine {
            strategy: INTERSECT.into(),
            query: Some(sub(2)),
            all: false,
        });

        let (sql, args) = build(&Numbered, &cs).unwrap();
        assert_eq!(sql, "UNION (SELECT $1)\nINTERSECT (SELECT $2)");
        assert_eq!(args.len(), 2);
    }
}
