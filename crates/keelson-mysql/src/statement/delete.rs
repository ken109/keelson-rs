use std::borrow::Cow;

use keelson_core::clause::{
    HasJoins, HasLimit, HasOrderBy, HasTableRef, HasWhere, HasWith, Join, Limit, OrderBy, TableRef,
    Where, With,
};
use keelson_core::expr::{Expr, IntoExpr, IntoExprList};
use keelson_core::{Dialect, Error, Expression, Mod, Query, QueryExtensions, QueryType, SqlWriter};

use super::{HasDeleteTables, HasExtraTables, write_hints_and_modifiers, write_table_list};
use crate::Mysql;
use crate::extras::{HasHints, HasModifiers, Hints, Modifiers};

/// A MySQL `DELETE`.
///
/// From <https://dev.mysql.com/doc/refman/8.4/en/delete.html>, taking the
/// single-table form and the `FROM … USING` spelling of the multiple-table one:
///
/// ```text
/// [WITH [RECURSIVE] with_query [, ...]]
/// DELETE [hint_comment] [LOW_PRIORITY] [QUICK] [IGNORE]
///     FROM tbl_name[.*] [[AS] tbl_alias] [, tbl_name[.*]] ...
///     [PARTITION (partition_name [, partition_name] ...)]
///     [USING table_references]
///     [WHERE where_condition]
///     [ORDER BY ...]
///     [LIMIT row_count]
/// ```
///
/// MySQL spells the multiple-table delete two ways —
/// `DELETE t1, t2 FROM refs` and `DELETE FROM t1, t2 USING refs`. Only the second
/// is built here, because it is also the single-table form with the `USING` left
/// out, so one shape covers both and `DELETE FROM t` is what an empty `USING`
/// gives.
///
/// # `PARTITION` sits in an odd place
///
/// This is the one statement where MySQL writes `PARTITION` *after* the alias.
/// [`TableRef`] puts it before, which is right everywhere else, so
/// [`HasDeleteTables`] carries a partition slot of its own and the
/// `delete::from(..).partition(..)` chain moves its list into it.
///
/// `ORDER BY` and `LIMIT`, like in `UPDATE`, are single-table only.
#[derive(Debug, Clone, Default)]
pub struct DeleteQuery {
    /// `WITH …`.
    pub with: With,
    /// `/*+ … */`.
    pub hints: Hints,
    /// `LOW_PRIORITY`, `QUICK` and `IGNORE`.
    pub modifiers: Modifiers,
    /// The tables rows are deleted from — the `FROM` list.
    pub tables: Vec<TableRef>,
    /// `PARTITION (…)`, written after the `FROM` list.
    pub partitions: Vec<Cow<'static, str>>,
    /// The first `USING` item, with its joins.
    pub using: TableRef,
    /// Further comma-separated `USING` items.
    pub extra_using: Vec<TableRef>,
    /// `WHERE …`.
    pub where_: Where,
    /// `ORDER BY …`. Single-table form only.
    pub order_by: OrderBy,
    /// `LIMIT row_count`. Single-table form only, and there is no `OFFSET`.
    pub limit: Limit,
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

        let mut tables = self.tables.iter().filter(|t| !t.is_empty());
        let Some(first) = tables.next() else {
            w.record_error(Error::Incomplete("the table of a DELETE"));
            return;
        };

        w.push_str("DELETE ");
        write_hints_and_modifiers(w, &self.hints, &self.modifiers);
        w.push_str("FROM ");
        w.write_expr(first);
        for table in tables {
            w.push_str(", ");
            w.write_expr(table);
        }

        if !self.partitions.is_empty() {
            w.push_str(" PARTITION (");
            for (i, partition) in self.partitions.iter().enumerate() {
                if i > 0 {
                    w.push_str(", ");
                }
                w.push_quoted(&[partition]);
            }
            w.push_str(")");
        }

        write_table_list(
            w,
            " USING ",
            &self.using,
            &self.extra_using,
            "the USING item its joins attach to",
        );

        w.write_if(!self.where_.is_empty(), " ", &self.where_, "");
        w.write_if(!self.order_by.is_empty(), " ", &self.order_by, "");
        w.write_if(!self.limit.is_empty(), " ", &self.limit, "");
    }
}

impl Query for DeleteQuery {
    fn query_type(&self) -> QueryType {
        QueryType::Delete
    }

    fn dialect(&self) -> &dyn Dialect {
        &Mysql
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
    HasWith      => with_mut:      With      = with,
    HasHints     => hints_mut:     Hints     = hints,
    HasModifiers => modifiers_mut: Modifiers = modifiers,
});

impl HasDeleteTables for DeleteQuery {
    fn delete_tables_mut(&mut self) -> &mut Vec<TableRef> {
        &mut self.tables
    }

    fn delete_partitions_mut(&mut self) -> &mut Vec<Cow<'static, str>> {
        &mut self.partitions
    }
}

keelson_core::impl_clause_accessors!(DeleteQuery {
    HasTableRef    => table_ref_mut:    TableRef      = using,
    HasExtraTables => extra_tables_mut: Vec<TableRef> = extra_using,
    HasJoins       => joins_mut:        Vec<Join>     = using.joins,
    HasWhere       => where_mut:        Where         = where_,
    HasOrderBy     => order_by_mut:     OrderBy       = order_by,
    HasLimit       => limit_mut:        Limit         = limit,
});
