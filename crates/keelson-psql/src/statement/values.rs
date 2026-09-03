use keelson_core::clause::{
    Combines, Fetch, HasCombines, HasFetch, HasLimit, HasOffset, HasOrderBy, HasValues, HasWith,
    Limit, Offset, OrderBy, Values, With,
};
use keelson_core::expr::{Expr, IntoExpr, IntoExprList};
use keelson_core::{Dialect, Error, Expression, Mod, Query, QueryExtensions, QueryType, SqlWriter};

use crate::Psql;

/// A standalone PostgreSQL `VALUES` statement.
///
/// From <https://www.postgresql.org/docs/17/sql-values.html>:
///
/// ```text
/// VALUES ( expression [, ...] ) [, ...]
///     [ ORDER BY sort_expression [ ASC | DESC | USING operator ] [, ...] ]
///     [ LIMIT { count | ALL } ]
///     [ OFFSET start [ ROW | ROWS ] ]
///     [ FETCH { FIRST | NEXT } [ count ] { ROW | ROWS } { ONLY | WITH TIES } ]
/// ```
///
/// `VALUES` is a `simple_select` alternative in `gram.y`, so it also takes a
/// leading `WITH` and participates in set operations — `VALUES (1) UNION
/// SELECT …` — and the tail clauses of a combination live in [`Combines`], as
/// they do for a `SELECT`. The columns of the result are named `column1`,
/// `column2`, …, which is what an `ORDER BY` here refers to (or `ORDER BY 1`).
///
/// It has no `FOR UPDATE`: PostgreSQL rejects a locking clause on `VALUES`, so
/// `HasLocks` is not implemented and `values::for_update` does not exist.
///
/// The rows are the same [`Values`] clause an `INSERT` holds, minus its
/// query alternative: a standalone `VALUES` *is* rows, so a
/// [`Values::query`] set here is a recorded build error rather than something to
/// render around.
#[derive(Debug, Clone, Default)]
pub struct ValuesQuery {
    /// `WITH …`.
    pub with: With,
    /// The rows.
    pub values: Values,
    /// `ORDER BY …` — this statement's own.
    pub order_by: OrderBy,
    /// `LIMIT …` — this statement's own.
    pub limit: Limit,
    /// `OFFSET …` — this statement's own.
    pub offset: Offset,
    /// `FETCH …` — this statement's own.
    pub fetch: Fetch,
    /// The set operations, and the trailing clauses that belong to their result.
    pub combines: Combines,
}

impl ValuesQuery {
    /// A `VALUES` with no rows yet — which does not build until it has one.
    pub fn new() -> ValuesQuery {
        ValuesQuery::default()
    }

    /// Apply more mods to an existing query.
    pub fn apply(&mut self, mods: impl Mod<ValuesQuery>) {
        mods.apply(self);
    }

    /// Whether this statement carries a clause a set operation would silently
    /// steal — the condition [`Combines::parenthesises_leading_query`] asks
    /// about.
    fn has_tail_clauses(&self) -> bool {
        !self.order_by.is_empty()
            || !self.limit.is_empty()
            || !self.offset.is_empty()
            || !self.fetch.is_empty()
    }
}

impl Expression for ValuesQuery {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        // One production, two spellings — same rule as SELECT.
        if !self.limit.is_empty() && !self.fetch.is_empty() {
            w.record_error(Error::conflicting_clauses("LIMIT", "FETCH"));
            return;
        }
        // The query alternative of the shared clause belongs to INSERT; a
        // standalone VALUES has no slot for it, and silently dropping either
        // part would render a statement the caller did not write.
        if self.values.query.is_some() {
            w.record_error(Error::other(
                "a standalone VALUES statement takes rows; a source query belongs to INSERT",
            ));
            return;
        }
        if self.values.rows.is_empty() {
            w.record_error(Error::Incomplete("the rows of a VALUES statement"));
            return;
        }

        w.write_if(!self.with.is_empty(), "", &self.with, " ");

        let parens = self
            .combines
            .parenthesises_leading_query(self.has_tail_clauses());
        if parens {
            w.push_str("(");
        }

        w.write_expr(&self.values);

        w.write_if(!self.order_by.is_empty(), " ", &self.order_by, "");
        w.write_if(!self.limit.is_empty(), " ", &self.limit, "");
        w.write_if(!self.offset.is_empty(), " ", &self.offset, "");
        w.write_if(!self.fetch.is_empty(), " ", &self.fetch, "");

        if parens {
            w.push_str(")");
        }

        w.write_if(!self.combines.is_empty(), " ", &self.combines, "");
    }
}

impl Query for ValuesQuery {
    fn query_type(&self) -> QueryType {
        // Rows come back, exactly as from a SELECT — and keelson-sqlite reads
        // its VALUES select-core the same way.
        QueryType::Select
    }

    fn dialect(&self) -> &dyn Dialect {
        &Psql
    }
}

impl<H, L, M> QueryExtensions<H, L, M> for ValuesQuery {}

impl IntoExpr for ValuesQuery {
    fn into_expr(self) -> Expr {
        crate::query(self)
    }
}

impl IntoExprList for ValuesQuery {
    fn into_expr_list(self) -> Vec<Expr> {
        vec![self.into_expr()]
    }
}

keelson_core::impl_clause_accessors!(ValuesQuery {
    HasWith     => with_mut:     With     = with,
    HasValues   => values_mut:   Values   = values,
    HasOrderBy  => order_by_mut: OrderBy  = order_by,
    HasLimit    => limit_mut:    Limit    = limit,
    HasOffset   => offset_mut:   Offset   = offset,
    HasFetch    => fetch_mut:    Fetch    = fetch,
    HasCombines => combines_mut: Combines = combines,
});
