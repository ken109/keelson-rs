use keelson_core::clause::{HasLimit, HasOrderBy, HasValues, Limit, OrderBy, Values};
use keelson_core::expr::{Expr, IntoExpr, IntoExprList};
use keelson_core::{Dialect, Error, Expression, Mod, Query, QueryExtensions, QueryType, SqlWriter};

use crate::Mysql;

/// The MySQL `VALUES` statement (MySQL 8.0.19+).
///
/// From <https://dev.mysql.com/doc/refman/8.4/en/values.html>:
///
/// ```text
/// VALUES row_constructor_list [ORDER BY column_designator] [LIMIT number]
///
/// row_constructor_list: ROW(value_list) [, ROW(value_list)] ...
/// ```
///
/// Unlike an `INSERT`'s `VALUES` list, every row here is spelled with the `ROW`
/// keyword — that is the grammar, not a flourish — and the result's columns are
/// named `column_0`, `column_1`, …, which is what an `ORDER BY` refers to.
///
/// The rows are the shared [`Values`] clause, whose `VALUES (…), (…)` rendering
/// is the `INSERT` spelling; this statement writes the `ROW` form itself. The
/// clause's query alternative belongs to `INSERT` and is a recorded build error
/// here.
#[derive(Debug, Clone, Default)]
pub struct ValuesQuery {
    /// The rows.
    pub values: Values,
    /// `ORDER BY column_designator`.
    pub order_by: OrderBy,
    /// `LIMIT number`.
    pub limit: Limit,
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
}

impl Expression for ValuesQuery {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        // The query alternative of the shared clause belongs to INSERT; a
        // standalone VALUES has no slot for it.
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

        w.push_str("VALUES ");
        for (i, row) in self.values.rows.iter().enumerate() {
            if i > 0 {
                w.push_str(", ");
            }
            // `ROW` welded to the row's own parentheses: `ROW(?, ?)`, the
            // manual's spelling.
            w.push_str("ROW");
            w.write_expr(row);
        }

        w.write_if(!self.order_by.is_empty(), " ", &self.order_by, "");
        w.write_if(!self.limit.is_empty(), " ", &self.limit, "");
    }
}

impl Query for ValuesQuery {
    fn query_type(&self) -> QueryType {
        // Rows come back, exactly as from a SELECT.
        QueryType::Select
    }

    fn dialect(&self) -> &dyn Dialect {
        &Mysql
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

impl HasValues for ValuesQuery {
    fn values_mut(&mut self) -> &mut Values {
        &mut self.values
    }
}

impl HasOrderBy for ValuesQuery {
    fn order_by_mut(&mut self) -> &mut OrderBy {
        &mut self.order_by
    }
}

impl HasLimit for ValuesQuery {
    fn limit_mut(&mut self) -> &mut Limit {
        &mut self.limit
    }
}
