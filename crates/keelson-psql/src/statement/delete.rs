use keelson_core::clause::{
    HasJoins, HasReturning, HasTableRef, HasWhere, HasWith, Join, Returning, TableRef, Where, With,
};
use keelson_core::expr::{Expr, IntoExpr, IntoExprList};
use keelson_core::{Dialect, Error, Expression, Mod, Query, QueryExtensions, QueryType, SqlWriter};

use super::{HasExtraTables, HasTargetTable, write_from_list};
use crate::Psql;

/// A PostgreSQL `DELETE`.
///
/// From <https://www.postgresql.org/docs/17/sql-delete.html>:
///
/// ```text
/// [ WITH [ RECURSIVE ] with_query [, ...] ]
/// DELETE FROM [ ONLY ] table_name [ * ] [ [ AS ] alias ]
///     [ USING from_item [, ...] ]
///     [ WHERE condition | WHERE CURRENT OF cursor_name ]
///     [ RETURNING … ]
/// ```
///
/// `USING` is a `FROM` list under another keyword, which is why it is the
/// [`HasTableRef`] slot here and the deleted-from table is the
/// [`HasTargetTable`](super::HasTargetTable) one.
#[derive(Debug, Clone, Default)]
pub struct DeleteQuery {
    /// `WITH …`.
    pub with: With,
    /// The table rows are deleted from.
    pub table: TableRef,
    /// The first `USING` item, with its joins.
    pub using: TableRef,
    /// Further comma-separated `USING` items.
    pub extra_using: Vec<TableRef>,
    /// `WHERE …`.
    pub where_: Where,
    /// `RETURNING …`.
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

        write_from_list(
            w,
            " USING ",
            &self.using,
            &self.extra_using,
            "the USING item its joins attach to",
        );

        w.write_if(!self.where_.is_empty(), " ", &self.where_, "");
        w.write_if(!self.returning.is_empty(), " ", &self.returning, "");
    }
}

impl Query for DeleteQuery {
    fn query_type(&self) -> QueryType {
        QueryType::Delete
    }

    fn dialect(&self) -> &dyn Dialect {
        &Psql
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

keelson_core::impl_clause_accessors!(DeleteQuery {
    HasWith        => with_mut:         With          = with,
    HasTargetTable => target_table_mut: TableRef      = table,
    HasTableRef    => table_ref_mut:    TableRef      = using,
    HasExtraTables => extra_tables_mut: Vec<TableRef> = extra_using,
    HasJoins       => joins_mut:        Vec<Join>     = using.joins,
    HasWhere       => where_mut:        Where         = where_,
    HasReturning   => returning_mut:    Returning     = returning,
});
