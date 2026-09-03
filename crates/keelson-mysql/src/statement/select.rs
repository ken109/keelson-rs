use keelson_core::clause::{
    Combines, GroupBy, HasCombines, HasGroupBy, HasHaving, HasJoins, HasLimit, HasLocks, HasOffset,
    HasOrderBy, HasSelectList, HasTableRef, HasWhere, HasWindows, HasWith, Having, Join, Limit,
    Locks, Offset, OrderBy, SelectList, TableRef, Where, Windows, With,
};
use keelson_core::expr::{Expr, IntoExpr, IntoExprList};
use keelson_core::{Dialect, Expression, Mod, Query, QueryExtensions, QueryType, SqlWriter};

use super::{HasExtraTables, write_hints_and_modifiers, write_table_list};
use crate::Mysql;
use crate::extras::{HasHints, HasModifiers, Hints, Modifiers};

/// A MySQL `SELECT`.
///
/// The field order is the clause order of
/// <https://dev.mysql.com/doc/refman/8.4/en/select.html>:
///
/// ```text
/// [WITH [RECURSIVE] with_query [, ...]]
/// SELECT [hint_comment]
///     [ALL | DISTINCT | DISTINCTROW] [HIGH_PRIORITY] [STRAIGHT_JOIN]
///     [SQL_SMALL_RESULT] [SQL_BIG_RESULT] [SQL_BUFFER_RESULT]
///     [SQL_NO_CACHE] [SQL_CALC_FOUND_ROWS]
///     select_expr [, select_expr] ...
///     [FROM table_references]
///     [WHERE where_condition]
///     [GROUP BY {col_name | expr | position}, ... [WITH ROLLUP]]
///     [HAVING where_condition]
///     [WINDOW window_name AS (window_spec) [, ...]]
///     [ORDER BY {col_name | expr | position} [ASC | DESC], ...]
///     [LIMIT {[offset,] row_count | row_count OFFSET offset}]
///     [FOR {UPDATE | SHARE} [OF tbl_name [, ...]] [NOWAIT | SKIP LOCKED]
///       | LOCK IN SHARE MODE]
/// ```
///
/// Two differences from PostgreSQL are worth stating.
///
/// **The locking clause has two shapes, and they are alternatives.** `FOR UPDATE`
/// and `FOR SHARE` are [`Locks`]; `LOCK IN SHARE MODE` is a production of its own
/// with no `OF` list and no wait option, so it is a flag rather than a
/// [`Lock`](keelson_core::clause::Lock) strength. Setting both is a caller error
/// the server refuses.
///
/// **`LIMIT` gates `OFFSET`.** MySQL's grammar spells the pair as one clause, so
/// `OFFSET` alone does not parse. Nothing here prevents it — the server is what
/// says no.
#[derive(Debug, Clone, Default)]
pub struct SelectQuery {
    /// `WITH …`.
    pub with: With,
    /// `/*+ … */`.
    pub hints: Hints,
    /// `DISTINCT`, `HIGH_PRIORITY`, `STRAIGHT_JOIN`, …
    pub modifiers: Modifiers,
    /// The projection. Empty renders `*`.
    pub select_list: SelectList,
    /// The first `FROM` item, with its joins.
    pub from: TableRef,
    /// Further comma-separated `FROM` items.
    pub extra_from: Vec<TableRef>,
    /// `WHERE …`.
    pub where_: Where,
    /// `GROUP BY … [WITH ROLLUP]`.
    pub group_by: GroupBy,
    /// `HAVING …`.
    pub having: Having,
    /// `WINDOW …`.
    pub windows: Windows,
    /// `ORDER BY …` — this query's own.
    pub order_by: OrderBy,
    /// `LIMIT …` — this query's own.
    pub limit: Limit,
    /// `OFFSET …` — this query's own. Needs a `LIMIT` to be legal.
    pub offset: Offset,
    /// `FOR UPDATE …` / `FOR SHARE …`.
    pub locks: Locks,
    /// `LOCK IN SHARE MODE`, the pre-8.0 spelling of `FOR SHARE`.
    pub lock_in_share_mode: bool,
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

    /// Whether this query carries a clause a set operation would silently steal —
    /// the condition [`Combines::parenthesises_leading_query`] asks about.
    fn has_tail_clauses(&self) -> bool {
        !self.order_by.is_empty()
            || !self.limit.is_empty()
            || !self.offset.is_empty()
            || !self.locks.is_empty()
            || self.lock_in_share_mode
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
        write_hints_and_modifiers(w, &self.hints, &self.modifiers);
        // The one clause whose absent rendering is not empty: `*`.
        w.write_expr(&self.select_list);

        write_table_list(
            w,
            " FROM ",
            &self.from,
            &self.extra_from,
            "the FROM item its joins attach to",
        );

        w.write_if(!self.where_.is_empty(), " ", &self.where_, "");
        w.write_if(!self.group_by.is_empty(), " ", &self.group_by, "");
        w.write_if(!self.having.is_empty(), " ", &self.having, "");
        w.write_if(!self.windows.is_empty(), " ", &self.windows, "");
        w.write_if(!self.order_by.is_empty(), " ", &self.order_by, "");
        w.write_if(!self.limit.is_empty(), " ", &self.limit, "");
        w.write_if(!self.offset.is_empty(), " ", &self.offset, "");
        w.write_if(!self.locks.is_empty(), " ", &self.locks, "");
        if self.lock_in_share_mode {
            w.push_str(" LOCK IN SHARE MODE");
        }

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
        &Mysql
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

keelson_core::impl_clause_accessors!(SelectQuery {
    HasWith        => with_mut:         With          = with,
    HasHints       => hints_mut:        Hints         = hints,
    HasModifiers   => modifiers_mut:    Modifiers     = modifiers,
    HasSelectList  => select_list_mut:  SelectList    = select_list,
    HasTableRef    => table_ref_mut:    TableRef      = from,
    HasExtraTables => extra_tables_mut: Vec<TableRef> = extra_from,
    HasJoins       => joins_mut:        Vec<Join>     = from.joins,
    HasWhere       => where_mut:        Where         = where_,
    HasGroupBy     => group_by_mut:     GroupBy       = group_by,
    HasHaving      => having_mut:       Having        = having,
    HasWindows     => windows_mut:      Windows       = windows,
    HasOrderBy     => order_by_mut:     OrderBy       = order_by,
    HasLimit       => limit_mut:        Limit         = limit,
    HasOffset      => offset_mut:       Offset        = offset,
    HasLocks       => locks_mut:        Locks         = locks,
    HasCombines    => combines_mut:     Combines      = combines,
});
