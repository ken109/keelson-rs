use keelson_core::clause::{
    Combines, Fetch, GroupBy, HasCombines, HasFetch, HasGroupBy, HasHaving, HasJoins, HasLimit,
    HasLocks, HasOffset, HasOrderBy, HasSelectList, HasTableRef, HasWhere, HasWindows, HasWith,
    Having, Join, Limit, Locks, Offset, OrderBy, SelectList, TableRef, Where, Windows, With,
};
use keelson_core::expr::{Expr, IntoExpr, IntoExprList};
use keelson_core::{Dialect, Expression, Mod, Query, QueryExtensions, QueryType, SqlWriter};

use super::{HasExtraTables, write_from_list};
use crate::Psql;
use crate::extras::Distinct;

/// A PostgreSQL `SELECT`.
///
/// The field order is the clause order of
/// <https://www.postgresql.org/docs/17/sql-select.html>:
///
/// ```text
/// [ WITH [ RECURSIVE ] with_query [, ...] ]
/// SELECT [ ALL | DISTINCT [ ON ( expression [, ...] ) ] ]
///     [ * | expression [ [ AS ] output_name ] [, ...] ]
///     [ FROM from_item [, ...] ]
///     [ WHERE condition ]
///     [ GROUP BY [ ALL | DISTINCT ] grouping_element [, ...] ]
///     [ HAVING condition ]
///     [ WINDOW window_name AS ( window_definition ) [, ...] ]
///     [ { UNION | INTERSECT | EXCEPT } [ ALL | DISTINCT ] select ]
///     [ ORDER BY expression [ ASC | DESC | USING operator ] [ NULLS { FIRST | LAST } ] [, ...] ]
///     [ LIMIT { count | ALL } ]
///     [ OFFSET start [ ROW | ROWS ] ]
///     [ FETCH { FIRST | NEXT } [ count ] { ROW | ROWS } { ONLY | WITH TIES } ]
///     [ FOR { UPDATE | NO KEY UPDATE | SHARE | KEY SHARE } [ OF table_name [, ...] ]
///       [ NOWAIT | SKIP LOCKED ] [...] ]
/// ```
///
/// The set-operation line is the one the layout does not mirror, and deliberately:
/// the trailing clauses after it belong to the **combination**, not to this query,
/// and PostgreSQL says so —
///
/// > Without parentheses, these clauses will be taken to apply to the result of the
/// > `UNION`, not to its right-hand input expression.
///
/// — so this query's own `ORDER BY`/`LIMIT`/`OFFSET`/`FETCH`/`FOR` render where the
/// fields sit and the whole thing is parenthesised when something is combined onto
/// it, while the combination's live inside [`Combines`].
#[derive(Debug, Clone, Default)]
pub struct SelectQuery {
    /// `WITH …`.
    pub with: With,
    /// `DISTINCT` / `DISTINCT ON (…)`. `None` is `ALL`, the default.
    pub distinct: Option<Distinct>,
    /// The projection. Empty renders `*`.
    pub select_list: SelectList,
    /// The first `FROM` item, with its joins.
    pub from: TableRef,
    /// Further comma-separated `FROM` items.
    pub extra_from: Vec<TableRef>,
    /// `WHERE …`.
    pub where_: Where,
    /// `GROUP BY …`.
    pub group_by: GroupBy,
    /// `HAVING …`.
    pub having: Having,
    /// `WINDOW …`.
    pub windows: Windows,
    /// `ORDER BY …` — this query's own.
    pub order_by: OrderBy,
    /// `LIMIT …` — this query's own.
    pub limit: Limit,
    /// `OFFSET …` — this query's own.
    pub offset: Offset,
    /// `FETCH …` — this query's own.
    pub fetch: Fetch,
    /// `FOR UPDATE …`.
    pub locks: Locks,
    /// The set operations, and the trailing clauses that belong to their result.
    pub combines: Combines,
}

impl SelectQuery {
    /// An empty `SELECT`, which renders `SELECT *`.
    pub fn new() -> SelectQuery {
        SelectQuery::default()
    }

    /// Apply more mods to an existing query.
    pub fn apply(&mut self, mods: impl Mod<SelectQuery>) {
        mods.apply(self);
    }

    /// Whether this query carries a clause that a set operation would silently
    /// steal — the condition [`Combines::parenthesises_leading_query`] asks about.
    fn has_tail_clauses(&self) -> bool {
        !self.order_by.is_empty()
            || !self.limit.is_empty()
            || !self.offset.is_empty()
            || !self.fetch.is_empty()
            || !self.locks.is_empty()
    }
}

impl Expression for SelectQuery {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.write_if(!self.with.is_empty(), "", &self.with, " ");

        let parens = self
            .combines
            .parenthesises_leading_query(self.has_tail_clauses());
        if parens {
            w.push_str("(");
        }

        w.push_str("SELECT ");
        if let Some(distinct) = &self.distinct {
            w.write_expr(distinct);
            w.push_str(" ");
        }
        // The one clause whose absent rendering is not empty: `*`.
        w.write_expr(&self.select_list);

        write_from_list(w, " FROM ", &self.from, &self.extra_from);

        w.write_if(!self.where_.is_empty(), " ", &self.where_, "");
        w.write_if(!self.group_by.is_empty(), " ", &self.group_by, "");
        w.write_if(!self.having.is_empty(), " ", &self.having, "");
        w.write_if(!self.windows.is_empty(), " ", &self.windows, "");
        w.write_if(!self.order_by.is_empty(), " ", &self.order_by, "");
        w.write_if(!self.limit.is_empty(), " ", &self.limit, "");
        w.write_if(!self.offset.is_empty(), " ", &self.offset, "");
        w.write_if(!self.fetch.is_empty(), " ", &self.fetch, "");
        w.write_if(!self.locks.is_empty(), " ", &self.locks, "");

        if parens {
            w.push_str(")");
        }

        w.write_if(!self.combines.is_empty(), " ", &self.combines, "");
    }
}

impl Query for SelectQuery {
    fn query_type(&self) -> QueryType {
        QueryType::Select
    }

    fn dialect(&self) -> &dyn Dialect {
        &Psql
    }
}

impl<H, L, M> QueryExtensions<H, L, M> for SelectQuery {}

impl IntoExpr for SelectQuery {
    fn into_expr(self) -> Expr {
        crate::query(self)
    }
}

impl IntoExprList for SelectQuery {
    fn into_expr_list(self) -> Vec<Expr> {
        vec![self.into_expr()]
    }
}

impl HasWith for SelectQuery {
    fn with_mut(&mut self) -> &mut With {
        &mut self.with
    }
}

impl HasSelectList for SelectQuery {
    fn select_list_mut(&mut self) -> &mut SelectList {
        &mut self.select_list
    }
}

impl HasTableRef for SelectQuery {
    fn table_ref_mut(&mut self) -> &mut TableRef {
        &mut self.from
    }
}

impl HasExtraTables for SelectQuery {
    fn extra_tables_mut(&mut self) -> &mut Vec<TableRef> {
        &mut self.extra_from
    }
}

impl HasJoins for SelectQuery {
    fn joins_mut(&mut self) -> &mut Vec<Join> {
        &mut self.from.joins
    }
}

impl HasWhere for SelectQuery {
    fn where_mut(&mut self) -> &mut Where {
        &mut self.where_
    }
}

impl HasGroupBy for SelectQuery {
    fn group_by_mut(&mut self) -> &mut GroupBy {
        &mut self.group_by
    }
}

impl HasHaving for SelectQuery {
    fn having_mut(&mut self) -> &mut Having {
        &mut self.having
    }
}

impl HasWindows for SelectQuery {
    fn windows_mut(&mut self) -> &mut Windows {
        &mut self.windows
    }
}

impl HasOrderBy for SelectQuery {
    fn order_by_mut(&mut self) -> &mut OrderBy {
        &mut self.order_by
    }
}

impl HasLimit for SelectQuery {
    fn limit_mut(&mut self) -> &mut Limit {
        &mut self.limit
    }
}

impl HasOffset for SelectQuery {
    fn offset_mut(&mut self) -> &mut Offset {
        &mut self.offset
    }
}

impl HasFetch for SelectQuery {
    fn fetch_mut(&mut self) -> &mut Fetch {
        &mut self.fetch
    }
}

impl HasLocks for SelectQuery {
    fn locks_mut(&mut self) -> &mut Locks {
        &mut self.locks
    }
}

impl HasCombines for SelectQuery {
    fn combines_mut(&mut self) -> &mut Combines {
        &mut self.combines
    }
}
