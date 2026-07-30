use std::ops::{Deref, DerefMut};

use keelson_core::{Expression, Mod, QueryType, Result, SqlWriter, Value, build_from};

use crate::dialect::PSQL;

/// A statement plus what it needs in order to be built and to nest — bob's
/// `BaseQuery`.
///
/// The wrapper exists because a statement renders two ways. On its own, and
/// wherever a clause supplies its own parentheses (a CTE body, the right-hand
/// side of a `UNION`), it renders bare. Anywhere else — a `FROM` item, an `IN`
/// list, a scalar column — it is a sub-query and has to be parenthesised. bob
/// splits those two along `WriteQuery` and `WriteSQL`; keelson splits them along
/// `Query` (parenthesised) and [`Bare`].
///
/// It also pins the dialect, so a PostgreSQL sub-query embedded in a MySQL
/// statement still writes `$1` placeholders while sharing the outer argument
/// list.
#[derive(Debug, Clone)]
pub struct Query<Q> {
    /// The statement itself: a bag of clauses that mods write into.
    pub query: Q,
    query_type: QueryType,
}

impl<Q> Query<Q> {
    /// Wrap a statement.
    pub fn new(query: Q, query_type: QueryType) -> Self {
        Query { query, query_type }
    }

    /// `SELECT`, `INSERT`, … — what the execution layer dispatches on.
    pub fn query_type(&self) -> QueryType {
        self.query_type
    }

    /// Apply mods after the fact.
    ///
    /// The declarative form is `psql::select((a, b))`; this is for the cases
    /// where a condition is easier to write as a statement than as an
    /// `Option<Mod>`.
    pub fn apply<M: Mod<Q>>(&mut self, m: M) -> &mut Self {
        m.apply(&mut self.query);
        self
    }

    /// The statement without the parentheses a sub-query gets.
    ///
    /// What a CTE body or a `UNION` operand is stored as, since those clauses
    /// write the parentheses themselves.
    pub fn into_bare(self) -> Bare<Q> {
        Bare(self.query)
    }

    /// Unwrap the statement.
    pub fn into_inner(self) -> Q {
        self.query
    }
}

impl<Q: Expression> Query<Q> {
    /// Render to SQL and arguments, numbering placeholders from 1.
    pub fn build(&self) -> Result<(String, Vec<Value>)> {
        self.build_from(1)
    }

    /// [`build`](Self::build) with a different first placeholder position.
    ///
    /// # Panics
    /// If `start` is 0.
    pub fn build_from(&self, start: usize) -> Result<(String, Vec<Value>)> {
        build_from(&PSQL, start, &self.query)
    }
}

/// Parenthesised: a statement standing in for a value.
impl<Q: Expression> Expression for Query<Q> {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        w.push_str("(");
        w.write_with_dialect(&PSQL, &self.query)?;
        w.push_str(")");
        Ok(())
    }
}

impl<Q> Deref for Query<Q> {
    type Target = Q;

    fn deref(&self) -> &Q {
        &self.query
    }
}

impl<Q> DerefMut for Query<Q> {
    fn deref_mut(&mut self) -> &mut Q {
        &mut self.query
    }
}

/// A statement rendered without the parentheses a sub-query gets.
///
/// See [`Query::into_bare`].
#[derive(Debug, Clone)]
pub struct Bare<Q>(pub Q);

impl<Q: Expression> Expression for Bare<Q> {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        w.write_with_dialect(&PSQL, &self.0)
    }
}
