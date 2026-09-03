use keelson_core::clause::{
    HasJoins, HasLimit, HasOrderBy, HasSet, HasWhere, HasWith, Join, Limit, OrderBy, Set, TableRef,
    Where, With,
};
use keelson_core::expr::{Expr, IntoExpr, IntoExprList};
use keelson_core::{Dialect, Error, Expression, Mod, Query, QueryExtensions, QueryType, SqlWriter};

use super::{HasExtraTables, HasTargetTable, write_hints_and_modifiers, write_table_list};
use crate::Mysql;
use crate::extras::{HasHints, HasModifiers, Hints, Modifiers};

/// A MySQL `UPDATE`.
///
/// From <https://dev.mysql.com/doc/refman/8.4/en/update.html>, with the
/// single-table and multiple-table forms folded together:
///
/// ```text
/// [WITH [RECURSIVE] with_query [, ...]]
/// UPDATE [hint_comment] [LOW_PRIORITY] [IGNORE] table_references
///     SET assignment_list
///     [WHERE where_condition]
///     [ORDER BY ...]
///     [LIMIT row_count]
/// ```
///
/// **There is no `UPDATE … FROM`.** MySQL's target *is* a `table_references`, so a
/// join or a comma joins tables that are all updatable, and `SET` may assign to any
/// of them. That is the one structural difference from PostgreSQL, and it is why
/// this type implements [`HasTargetTable`] rather than
/// [`HasTableRef`](keelson_core::clause::HasTableRef) — `update::inner_join(..)`
/// lands on the updated table list, because there is nowhere else for it to go.
///
/// `ORDER BY` and `LIMIT` belong to the single-table form only; MySQL rejects them
/// once more than one table is named. Nothing here prevents it — the shape is
/// legal and the server is what says no.
#[derive(Debug, Clone, Default)]
pub struct UpdateQuery {
    /// `WITH …`.
    pub with: With,
    /// `/*+ … */`.
    pub hints: Hints,
    /// `LOW_PRIORITY` and `IGNORE`.
    pub modifiers: Modifiers,
    /// The first table reference, with its joins.
    pub table: TableRef,
    /// Further comma-separated table references.
    pub extra_tables: Vec<TableRef>,
    /// `SET …`.
    pub set: Set,
    /// `WHERE …`.
    pub where_: Where,
    /// `ORDER BY …`. Single-table form only.
    pub order_by: OrderBy,
    /// `LIMIT row_count`. Single-table form only, and there is no `OFFSET`.
    pub limit: Limit,
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
        write_hints_and_modifiers(w, &self.hints, &self.modifiers);
        // The guard inside cannot fire here: an empty target was already an
        // `Incomplete` above, and MySQL's joins legitimately live on it.
        write_table_list(
            w,
            "",
            &self.table,
            &self.extra_tables,
            "the table of an UPDATE",
        );

        // `Set` writes no keyword of its own — `ON DUPLICATE KEY UPDATE` takes the
        // same list bare — so the `SET` belongs here.
        w.push_str(" SET ");
        w.write_expr(&self.set);

        w.write_if(!self.where_.is_empty(), " ", &self.where_, "");
        w.write_if(!self.order_by.is_empty(), " ", &self.order_by, "");
        w.write_if(!self.limit.is_empty(), " ", &self.limit, "");
    }
}

impl Query for UpdateQuery {
    fn query_type(&self) -> QueryType {
        QueryType::Update
    }

    fn dialect(&self) -> &dyn Dialect {
        &Mysql
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

keelson_core::impl_clause_accessors!(UpdateQuery {
    HasWith        => with_mut:         With          = with,
    HasHints       => hints_mut:        Hints         = hints,
    HasModifiers   => modifiers_mut:    Modifiers     = modifiers,
    HasTargetTable => target_table_mut: TableRef      = table,
    HasExtraTables => extra_tables_mut: Vec<TableRef> = extra_tables,
    HasJoins       => joins_mut:        Vec<Join>     = table.joins,
    HasSet         => set_mut:          Set           = set,
    HasWhere       => where_mut:        Where         = where_,
    HasOrderBy     => order_by_mut:     OrderBy       = order_by,
    HasLimit       => limit_mut:        Limit         = limit,
});
