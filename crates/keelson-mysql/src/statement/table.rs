use keelson_core::clause::{
    HasLimit, HasOffset, HasOrderBy, HasTableRef, Limit, Offset, OrderBy, TableRef,
};
use keelson_core::expr::{Expr, IntoExpr, IntoExprList};
use keelson_core::{Dialect, Error, Expression, Mod, Query, QueryExtensions, QueryType, SqlWriter};

use crate::Mysql;

/// The MySQL `TABLE` statement (MySQL 8.0.19+) — `TABLE t` is
/// `SELECT * FROM t`.
///
/// From <https://dev.mysql.com/doc/refman/8.4/en/table.html>:
///
/// ```text
/// TABLE table_name [ORDER BY column_name] [LIMIT number [OFFSET number]]
/// ```
///
/// The struct is that line and nothing more: a bare table name — no alias, no
/// index hints, no `PARTITION` — and the three tail clauses. `WHERE` does not
/// exist here and does not compile.
#[derive(Debug, Clone, Default)]
pub struct TableQuery {
    /// The table. Grammar takes a bare name only.
    pub table: TableRef,
    /// `ORDER BY column_name`.
    pub order_by: OrderBy,
    /// `LIMIT number`.
    pub limit: Limit,
    /// `OFFSET number` — part of the `LIMIT` production, as in a `SELECT`.
    pub offset: Offset,
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
}

impl Expression for TableQuery {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        if self.table.is_empty() {
            w.record_error(Error::Incomplete("the table of a TABLE statement"));
            return;
        }

        w.push_str("TABLE ");
        w.write_expr(&self.table);

        w.write_if(!self.order_by.is_empty(), " ", &self.order_by, "");
        w.write_if(!self.limit.is_empty(), " ", &self.limit, "");
        w.write_if(!self.offset.is_empty(), " ", &self.offset, "");
    }
}

impl Query for TableQuery {
    fn query_type(&self) -> QueryType {
        // `TABLE t` is `SELECT * FROM t`, rows and all.
        QueryType::Select
    }

    fn dialect(&self) -> &dyn Dialect {
        &Mysql
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
