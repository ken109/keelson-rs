use keelson_core::clause::{
    Conflict, HasConflict, HasReturning, HasTableRef, HasValues, HasWith, Returning, TableRef,
    Values, With,
};
use keelson_core::expr::{Expr, IntoExpr, IntoExprList};
use keelson_core::{Dialect, Error, Expression, Mod, Query, QueryExtensions, QueryType, SqlWriter};

use crate::Psql;
use crate::extras::Overriding;

/// A PostgreSQL `INSERT`.
///
/// From <https://www.postgresql.org/docs/17/sql-insert.html>:
///
/// ```text
/// [ WITH [ RECURSIVE ] with_query [, ...] ]
/// INSERT INTO table_name [ AS alias ] [ ( column_name [, ...] ) ]
///     [ OVERRIDING { SYSTEM | USER } VALUE ]
///     { DEFAULT VALUES | VALUES ( { expression | DEFAULT } [, ...] ) [, ...] | query }
///     [ ON CONFLICT [ conflict_target ] conflict_action ]
///     [ RETURNING … ]
/// ```
///
/// The row source is one of three alternatives, and `DEFAULT VALUES` is the one
/// [`Values`] cannot represent — an absent clause has to render nothing, and the
/// spelling is not shared with MySQL anyway. So an empty `Values` is what
/// `DEFAULT VALUES` *is* here, and this query writes the keywords itself.
#[derive(Debug, Clone, Default)]
pub struct InsertQuery {
    /// `WITH …`.
    pub with: With,
    /// The target: table, optional alias, and the insert column list.
    pub table: TableRef,
    /// `OVERRIDING … VALUE`.
    pub overriding: Option<Overriding>,
    /// The rows, or the query to insert the results of. Empty means
    /// `DEFAULT VALUES`.
    pub values: Values,
    /// `ON CONFLICT …`.
    pub conflict: Conflict,
    /// `RETURNING …`.
    pub returning: Returning,
}

impl InsertQuery {
    /// An `INSERT` with nothing set yet.
    pub fn new() -> InsertQuery {
        InsertQuery::default()
    }

    /// Apply more mods to an existing query.
    pub fn apply(&mut self, mods: impl Mod<InsertQuery>) {
        mods.apply(self);
    }
}

impl Expression for InsertQuery {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.write_if(!self.with.is_empty(), "", &self.with, " ");

        if self.table.is_empty() {
            // Unlike a `SELECT`, which is a statement without a table, an `INSERT`
            // has nowhere to put the rows.
            w.record_error(Error::Incomplete("the target table of an INSERT"));
            return;
        }
        w.push_str("INSERT INTO ");
        w.write_expr(&self.table);

        if let Some(overriding) = &self.overriding {
            w.push_str(" OVERRIDING ");
            w.push_str(overriding.as_str());
            w.push_str(" VALUE");
        }

        if self.values.is_empty() {
            w.push_str(" DEFAULT VALUES");
        } else {
            w.push_str(" ");
            w.write_expr(&self.values);
        }

        w.write_if(!self.conflict.is_empty(), " ", &self.conflict, "");
        w.write_if(!self.returning.is_empty(), " ", &self.returning, "");
    }
}

impl Query for InsertQuery {
    fn query_type(&self) -> QueryType {
        QueryType::Insert
    }

    fn dialect(&self) -> &dyn Dialect {
        &Psql
    }
}

impl<H, L, M> QueryExtensions<H, L, M> for InsertQuery {}

impl IntoExpr for InsertQuery {
    fn into_expr(self) -> Expr {
        crate::query(self)
    }
}

impl IntoExprList for InsertQuery {
    fn into_expr_list(self) -> Vec<Expr> {
        vec![self.into_expr()]
    }
}

keelson_core::impl_clause_accessors!(InsertQuery {
    HasWith      => with_mut:      With      = with,
    HasTableRef  => table_ref_mut: TableRef  = table,
    HasValues    => values_mut:    Values    = values,
    HasConflict  => conflict_mut:  Conflict  = conflict,
    HasReturning => returning_mut: Returning = returning,
});
