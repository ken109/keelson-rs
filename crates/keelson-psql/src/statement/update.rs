use keelson_core::clause::{
    HasJoins, HasReturning, HasSet, HasTableRef, HasWhere, HasWith, Join, Returning, Set, TableRef,
    Where, With,
};
use keelson_core::expr::{Expr, IntoExpr, IntoExprList};
use keelson_core::{Dialect, Error, Expression, Mod, Query, QueryExtensions, QueryType, SqlWriter};

use super::{HasExtraTables, HasTargetTable, write_from_list};
use crate::Psql;

/// A PostgreSQL `UPDATE`.
///
/// From <https://www.postgresql.org/docs/17/sql-update.html>:
///
/// ```text
/// [ WITH [ RECURSIVE ] with_query [, ...] ]
/// UPDATE [ ONLY ] table_name [ * ] [ [ AS ] alias ]
///     SET { column_name = { expression | DEFAULT }
///         | ( column_name [, ...] ) = [ ROW ] ( { expression | DEFAULT } [, ...] )
///         | ( column_name [, ...] ) = ( sub-SELECT ) } [, ...]
///     [ FROM from_item [, ...] ]
///     [ WHERE condition | WHERE CURRENT OF cursor_name ]
///     [ RETURNING … ]
/// ```
///
/// `ONLY` lives on [`TableRef::only`], because that is where the shared clause puts
/// it and because the same flag decorates a `FROM` item.
///
/// The assignment list is not optional: `UPDATE t` with no `SET` is not a statement,
/// so an empty [`Set`] is a recorded [`Error::Incomplete`] rather than a clause that
/// renders nothing.
#[derive(Debug, Clone, Default)]
pub struct UpdateQuery {
    /// `WITH …`.
    pub with: With,
    /// The table being updated.
    pub table: TableRef,
    /// `SET …`.
    pub set: Set,
    /// The first `FROM` item, with its joins.
    pub from: TableRef,
    /// Further comma-separated `FROM` items.
    pub extra_from: Vec<TableRef>,
    /// `WHERE …`.
    pub where_: Where,
    /// `RETURNING …`.
    pub returning: Returning,
}

impl UpdateQuery {
    /// An `UPDATE` with nothing set yet.
    pub fn new() -> UpdateQuery {
        UpdateQuery::default()
    }

    /// Apply more mods to an existing query.
    pub fn apply(&mut self, mods: impl Mod<UpdateQuery>) {
        mods.apply(self);
    }
}

impl Expression for UpdateQuery {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.write_if(!self.with.is_empty(), "", &self.with, " ");

        if self.table.is_empty() {
            w.record_error(Error::Incomplete("the table of an UPDATE"));
            return;
        }
        if self.set.is_empty() {
            w.record_error(Error::Incomplete("the assignments of an UPDATE"));
            return;
        }

        w.push_str("UPDATE ");
        w.write_expr(&self.table);
        // `Set` writes no keyword of its own — MySQL's ON DUPLICATE KEY UPDATE
        // takes the same list bare — so the `SET` belongs here.
        w.push_str(" SET ");
        w.write_expr(&self.set);

        write_from_list(
            w,
            " FROM ",
            &self.from,
            &self.extra_from,
            "the FROM item its joins attach to",
        );

        w.write_if(!self.where_.is_empty(), " ", &self.where_, "");
        w.write_if(!self.returning.is_empty(), " ", &self.returning, "");
    }
}

impl Query for UpdateQuery {
    fn query_type(&self) -> QueryType {
        QueryType::Update
    }

    fn dialect(&self) -> &dyn Dialect {
        &Psql
    }
}

impl<H, L, M> QueryExtensions<H, L, M> for UpdateQuery {}

impl IntoExpr for UpdateQuery {
    fn into_expr(self) -> Expr {
        crate::query(self)
    }
}

impl IntoExprList for UpdateQuery {
    fn into_expr_list(self) -> Vec<Expr> {
        vec![self.into_expr()]
    }
}

impl HasWith for UpdateQuery {
    fn with_mut(&mut self) -> &mut With {
        &mut self.with
    }
}

impl HasTargetTable for UpdateQuery {
    fn target_table_mut(&mut self) -> &mut TableRef {
        &mut self.table
    }
}

impl HasSet for UpdateQuery {
    fn set_mut(&mut self) -> &mut Set {
        &mut self.set
    }
}

impl HasTableRef for UpdateQuery {
    fn table_ref_mut(&mut self) -> &mut TableRef {
        &mut self.from
    }
}

impl HasExtraTables for UpdateQuery {
    fn extra_tables_mut(&mut self) -> &mut Vec<TableRef> {
        &mut self.extra_from
    }
}

impl HasJoins for UpdateQuery {
    fn joins_mut(&mut self) -> &mut Vec<Join> {
        &mut self.from.joins
    }
}

impl HasWhere for UpdateQuery {
    fn where_mut(&mut self) -> &mut Where {
        &mut self.where_
    }
}

impl HasReturning for UpdateQuery {
    fn returning_mut(&mut self) -> &mut Returning {
        &mut self.returning
    }
}
