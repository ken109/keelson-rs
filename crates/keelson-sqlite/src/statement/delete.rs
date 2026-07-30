use keelson_core::clause::{HasReturning, HasWhere, HasWith, Returning, TableRef, Where, With};
use keelson_core::expr::{Expr, IntoExpr, IntoExprList};
use keelson_core::{Dialect, Error, Expression, Mod, Query, QueryExtensions, QueryType, SqlWriter};

use super::HasTargetTable;
use crate::Sqlite;

/// A SQLite `DELETE`.
///
/// From <https://www.sqlite.org/lang_delete.html>:
///
/// ```text
/// [ WITH [ RECURSIVE ] common-table-expression [, ...] ]
/// DELETE FROM qualified-table-name
///     [ WHERE expr ]
///     [ RETURNING result-column [, ...] ]
/// ```
///
/// That is the whole statement — the shortest of the four by a wide margin.
/// PostgreSQL's `USING` has no counterpart, so there is no from-item, no join and
/// no `HasTableRef` here; a delete driven by another table is written with a
/// sub-query in the `WHERE`, which is what SQLite gives instead. There is also no
/// `OR` clause: SQLite's `conflict-clause` is offered on `INSERT` and `UPDATE`
/// only, since a delete cannot violate a uniqueness constraint.
#[derive(Debug, Clone, Default)]
pub struct DeleteQuery {
    /// `WITH …`.
    pub with: With,
    /// The table rows are deleted from.
    pub table: TableRef,
    /// `WHERE …`.
    pub where_: Where,
    /// `RETURNING …`. SQLite 3.35 and later.
    pub returning: Returning,
}

impl DeleteQuery {
    /// A `DELETE` with nothing set yet.
    pub fn new() -> DeleteQuery {
        DeleteQuery::default()
    }

    /// Apply more mods to an existing query.
    pub fn apply(&mut self, mods: impl Mod<DeleteQuery>) {
        mods.apply(self);
    }
}

impl Expression for DeleteQuery {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.write_if(!self.with.is_empty(), "", &self.with, " ");

        if self.table.is_empty() {
            w.record_error(Error::Incomplete("the table of a DELETE"));
            return;
        }

        w.push_str("DELETE FROM ");
        w.write_expr(&self.table);

        w.write_if(!self.where_.is_empty(), " ", &self.where_, "");
        w.write_if(!self.returning.is_empty(), " ", &self.returning, "");
    }
}

impl Query for DeleteQuery {
    fn query_type(&self) -> QueryType {
        QueryType::Delete
    }

    fn dialect(&self) -> &dyn Dialect {
        &Sqlite
    }
}

impl<H, L, M> QueryExtensions<H, L, M> for DeleteQuery {}

impl IntoExpr for DeleteQuery {
    fn into_expr(self) -> Expr {
        crate::query(self)
    }
}

impl IntoExprList for DeleteQuery {
    fn into_expr_list(self) -> Vec<Expr> {
        vec![self.into_expr()]
    }
}

impl HasWith for DeleteQuery {
    fn with_mut(&mut self) -> &mut With {
        &mut self.with
    }
}

impl HasTargetTable for DeleteQuery {
    fn target_table_mut(&mut self) -> &mut TableRef {
        &mut self.table
    }
}

impl HasWhere for DeleteQuery {
    fn where_mut(&mut self) -> &mut Where {
        &mut self.where_
    }
}

impl HasReturning for DeleteQuery {
    fn returning_mut(&mut self) -> &mut Returning {
        &mut self.returning
    }
}
