use keelson_core::clause::{
    GroupBy, HasGroupBy, HasHaving, HasJoins, HasLimit, HasOffset, HasOrderBy, HasSelectList,
    HasTableRef, HasValues, HasWhere, HasWindows, HasWith, Having, Join, Limit, Offset, OrderBy,
    SelectList, TableRef, Values, Where, Windows, With,
};
use keelson_core::expr::{Expr, IntoExpr, IntoExprList};
use keelson_core::{Dialect, Error, Expression, Mod, Query, QueryExtensions, QueryType, SqlWriter};

use super::{HasExtraTables, write_from_list};
use crate::Sqlite;
use crate::extras::{Compounds, HasCompounds};

/// A SQLite `SELECT`.
///
/// The field order is the clause order of <https://www.sqlite.org/lang_select.html>:
///
/// ```text
/// [ WITH [ RECURSIVE ] common-table-expression [, ...] ]
/// select-core [ compound-operator select-core ]*
/// [ ORDER BY ordering-term [, ...] ]
/// [ LIMIT expr [ ( OFFSET | , ) expr ] ]
///
/// select-core:
///     SELECT [ DISTINCT | ALL ] result-column [, ...]
///         [ FROM table-or-subquery [, ...] | join-clause ]
///         [ WHERE expr ]
///         [ GROUP BY expr [, ...] [ HAVING expr ] ]
///         [ WINDOW window-name AS window-defn [, ...] ]
///   | VALUES ( expr [, ...] ) [, ...]
/// ```
///
/// Three things in that grammar are unlike PostgreSQL's, and all three are visible
/// in this type.
///
/// **A compound operand is a bare `select-core`.** There are no parentheses around
/// it and none may be added — a parenthesised select is a `table-or-subquery` in
/// SQLite, never a compound operand. So the `ORDER BY` and `LIMIT` after the last
/// operand are the *only* ones there can be, they always apply to the whole
/// compound, and this type therefore has one set of them rather than PostgreSQL's
/// two. See [`Compound`](crate::Compound).
///
/// **`OFFSET` lives inside the `LIMIT` production.** `SELECT … OFFSET 5` with no
/// `LIMIT` is a syntax error, so an offset without a limit is a recorded
/// [`Error::Incomplete`] rather than SQL that will be rejected later.
///
/// **`VALUES (…), (…)` is a `select-core` in its own right.** [`values`](Self::values)
/// is that alternative: when it is non-empty the statement *is* a `VALUES`
/// statement, and the clauses that only a `SELECT` core can carry are a recorded
/// failure rather than silently dropped. A real SQLite additionally refuses
/// `ORDER BY`/`LIMIT` when the **last** core is a `VALUES`, which its own parser
/// accepts; that one is left to the engine, since whether it holds depends on what
/// is compounded after the `VALUES`.
#[derive(Debug, Clone, Default)]
pub struct SelectQuery {
    /// `WITH …`.
    pub with: With,
    /// `SELECT DISTINCT`. `false` is `ALL`, the default, which adds nothing.
    pub distinct: bool,
    /// The result columns. Empty renders `*`.
    pub select_list: SelectList,
    /// The `VALUES (…), (…)` alternative to the whole `SELECT` core.
    pub values: Values,
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
    /// The compound operands, applied left to right.
    pub compounds: Compounds,
    /// `ORDER BY …`, which belongs to the whole compound when there is one.
    pub order_by: OrderBy,
    /// `LIMIT …`.
    pub limit: Limit,
    /// `OFFSET …`, which cannot stand without a [`limit`](Self::limit).
    pub offset: Offset,
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

    /// The first clause set on this query that only a `SELECT` core can carry, if
    /// any — the check a `VALUES` core has to make before it renders.
    fn select_only_clause(&self) -> Option<&'static str> {
        if self.distinct {
            Some("DISTINCT")
        } else if !self.select_list.is_empty() {
            Some("a result-column list")
        } else if !self.from.is_empty() || !self.extra_from.is_empty() {
            Some("a FROM clause")
        } else if !self.where_.is_empty() {
            Some("a WHERE clause")
        } else if !self.group_by.is_empty() {
            Some("a GROUP BY clause")
        } else if !self.having.is_empty() {
            Some("a HAVING clause")
        } else if !self.windows.is_empty() {
            Some("a WINDOW clause")
        } else {
            None
        }
    }

    /// Write the `select-core`: either the `VALUES` alternative or the `SELECT` one.
    fn write_core(&self, w: &mut SqlWriter<'_>) {
        if !self.values.is_empty() {
            if let Some(clause) = self.select_only_clause() {
                w.record_error(Error::other(format!(
                    "a VALUES statement cannot carry {clause}"
                )));
                return;
            }
            w.write_expr(&self.values);
            return;
        }

        w.push_str("SELECT ");
        if self.distinct {
            w.push_str("DISTINCT ");
        }
        // The one clause whose absent rendering is not empty: `*`.
        w.write_expr(&self.select_list);

        write_from_list(
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
    }
}

impl Expression for SelectQuery {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.write_if(!self.with.is_empty(), "", &self.with, " ");

        self.write_core(w);

        w.write_if(!self.compounds.is_empty(), " ", &self.compounds, "");
        w.write_if(!self.order_by.is_empty(), " ", &self.order_by, "");

        if self.limit.is_empty() && !self.offset.is_empty() {
            // `LIMIT expr [ ( OFFSET | , ) expr ]`: the offset hangs off the limit
            // and there is no production that lets it stand alone.
            w.record_error(Error::Incomplete("the LIMIT that an OFFSET belongs to"));
            return;
        }
        w.write_if(!self.limit.is_empty(), " ", &self.limit, "");
        w.write_if(!self.offset.is_empty(), " ", &self.offset, "");
    }
}

impl Query for SelectQuery {
    fn query_type(&self) -> QueryType {
        QueryType::Select
    }

    fn dialect(&self) -> &dyn Dialect {
        &Sqlite
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

impl HasValues for SelectQuery {
    fn values_mut(&mut self) -> &mut Values {
        &mut self.values
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

impl HasCompounds for SelectQuery {
    fn compounds_mut(&mut self) -> &mut Compounds {
        &mut self.compounds
    }
}
