use keelson_core::clause::{
    Combines, Fetch, HasCombines, HasFetch, HasLimit, HasLocks, HasOffset, HasOrderBy, HasTableRef,
    HasWith, Limit, Locks, Offset, OrderBy, TableRef, With,
};
use keelson_core::expr::{Expr, IntoExpr, IntoExprList};
use keelson_core::{Dialect, Error, Expression, Mod, Query, QueryExtensions, QueryType, SqlWriter};

use crate::Psql;

/// The PostgreSQL `TABLE` command — `TABLE name` is `SELECT * FROM name`.
///
/// From <https://www.postgresql.org/docs/17/sql-select.html> (the `TABLE`
/// section): `TABLE [ ONLY ] table_name [ * ]`, and
///
/// > it can be used as a top-level command or as a space-saving syntax variant
/// > in parts of complex queries. Only the `WITH`, `UNION`, `INTERSECT`,
/// > `EXCEPT`, `ORDER BY`, `LIMIT`, `OFFSET`, `FETCH` and `FOR` locking clauses
/// > can be used with `TABLE`; the `WHERE` clause and any form of aggregation
/// > cannot be used.
///
/// The struct is that sentence: exactly those clauses, and no others — a
/// `table::where_` is a compile error because `HasWhere` is not implemented.
#[derive(Debug, Clone, Default)]
pub struct TableQuery {
    /// `WITH …`.
    pub with: With,
    /// The table: `TABLE [ ONLY ] name`. Grammar takes a name only — no alias,
    /// no column list.
    pub table: TableRef,
    /// `ORDER BY …` — this statement's own.
    pub order_by: OrderBy,
    /// `LIMIT …` — this statement's own.
    pub limit: Limit,
    /// `OFFSET …` — this statement's own.
    pub offset: Offset,
    /// `FETCH …` — this statement's own.
    pub fetch: Fetch,
    /// `FOR UPDATE …`.
    pub locks: Locks,
    /// The set operations, and the trailing clauses that belong to their result.
    pub combines: Combines,
}

impl TableQuery {
    /// A `TABLE` with no table yet — which does not build until it has one.
    pub fn new() -> TableQuery {
        TableQuery::default()
    }

    /// Apply more mods to an existing query.
    pub fn apply(&mut self, mods: impl Mod<TableQuery>) {
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
            || !self.locks.is_empty()
    }
}

impl Expression for TableQuery {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        // One production, two spellings — same rule as SELECT.
        if !self.limit.is_empty() && !self.fetch.is_empty() {
            w.record_error(Error::conflicting_clauses("LIMIT", "FETCH"));
            return;
        }
        if self.table.is_empty() {
            w.record_error(Error::Incomplete("the table of a TABLE statement"));
            return;
        }

        w.write_if(!self.with.is_empty(), "", &self.with, " ");

        let parens = self
            .combines
            .parenthesises_leading_query(self.has_tail_clauses());
        if parens {
            w.push_str("(");
        }

        w.push_str("TABLE ");
        w.write_expr(&self.table);

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

impl Query for TableQuery {
    fn query_type(&self) -> QueryType {
        // `TABLE name` is `SELECT * FROM name`, rows and all.
        QueryType::Select
    }

    fn dialect(&self) -> &dyn Dialect {
        &Psql
    }
}

impl<H, L, M> QueryExtensions<H, L, M> for TableQuery {}

impl IntoExpr for TableQuery {
    fn into_expr(self) -> Expr {
        crate::query(self)
    }
}

impl IntoExprList for TableQuery {
    fn into_expr_list(self) -> Vec<Expr> {
        vec![self.into_expr()]
    }
}

impl HasWith for TableQuery {
    fn with_mut(&mut self) -> &mut With {
        &mut self.with
    }
}

impl HasTableRef for TableQuery {
    fn table_ref_mut(&mut self) -> &mut TableRef {
        &mut self.table
    }
}

impl HasOrderBy for TableQuery {
    fn order_by_mut(&mut self) -> &mut OrderBy {
        &mut self.order_by
    }
}

impl HasLimit for TableQuery {
    fn limit_mut(&mut self) -> &mut Limit {
        &mut self.limit
    }
}

impl HasOffset for TableQuery {
    fn offset_mut(&mut self) -> &mut Offset {
        &mut self.offset
    }
}

impl HasFetch for TableQuery {
    fn fetch_mut(&mut self) -> &mut Fetch {
        &mut self.fetch
    }
}

impl HasLocks for TableQuery {
    fn locks_mut(&mut self) -> &mut Locks {
        &mut self.locks
    }
}

impl HasCombines for TableQuery {
    fn combines_mut(&mut self) -> &mut Combines {
        &mut self.combines
    }
}
