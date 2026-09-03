use keelson_core::clause::{HasSet, HasTableRef, HasValues, Set, TableRef, Values};
use keelson_core::expr::{Expr, IntoExpr, IntoExprList};
use keelson_core::{Dialect, Error, Expression, Mod, Query, QueryExtensions, QueryType, SqlWriter};

use super::insert::write_target_and_row_source;
use super::write_hints_and_modifiers;
use crate::Mysql;
use crate::extras::{HasHints, HasModifiers, Hints, Modifiers};

/// A MySQL `REPLACE`.
///
/// From <https://dev.mysql.com/doc/refman/8.4/en/replace.html>:
///
/// ```text
/// REPLACE [LOW_PRIORITY | DELAYED] [INTO] tbl_name
///     [PARTITION (partition_name [, partition_name] ...)]
///     [(col_name [, col_name] ...)]
///     { {VALUES | VALUE} (value_list) [, (value_list)] ...
///     | SET assignment_list
///     | [(col_name [, col_name] ...)] SELECT ... }
/// ```
///
/// A statement type of its own rather than a flag on
/// [`InsertQuery`](super::InsertQuery), because the differences are exactly the
/// things a flag would leave reachable: `REPLACE` has **no `HIGH_PRIORITY`**, **no
/// `IGNORE`**, **no row alias** and **no `ON DUPLICATE KEY UPDATE`** — the last two
/// being meaningless when a duplicate key is what the statement is for. Not
/// implementing the traits is how that is said, so `replace::ignore()` and
/// `replace::on_duplicate_key_update(..)` simply do not exist.
///
/// Its [`query_type`](Query::query_type) is [`QueryType::Insert`]: `REPLACE` is a
/// `DELETE`-then-`INSERT`, and the layers above care only that it writes rows.
#[derive(Debug, Clone, Default)]
pub struct ReplaceQuery {
    /// `/*+ … */`.
    pub hints: Hints,
    /// `LOW_PRIORITY` or `DELAYED`, and nothing else.
    pub modifiers: Modifiers,
    /// The target: table, partitions, and the column list.
    pub table: TableRef,
    /// The rows, or the query to insert the results of.
    pub values: Values,
    /// `SET a = 1, b = 2`, the assignment-list row source.
    pub set: Set,
}

impl ReplaceQuery {
    /// A `REPLACE` with nothing set yet.
    pub fn new() -> ReplaceQuery {
        ReplaceQuery::default()
    }

    /// Apply more mods to an existing query.
    pub fn apply(&mut self, mods: impl Mod<ReplaceQuery>) {
        mods.apply(self);
    }
}

impl Expression for ReplaceQuery {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        if self.table.is_empty() {
            w.record_error(Error::Incomplete("the target table of a REPLACE"));
            return;
        }

        w.push_str("REPLACE ");
        write_hints_and_modifiers(w, &self.hints, &self.modifiers);
        w.push_str("INTO ");
        write_target_and_row_source(w, &self.table, &self.set, &self.values);
    }
}

impl Query for ReplaceQuery {
    fn query_type(&self) -> QueryType {
        QueryType::Insert
    }

    fn dialect(&self) -> &dyn Dialect {
        &Mysql
    }
}

impl<H, L, M> QueryExtensions<H, L, M> for ReplaceQuery {}

impl IntoExpr for ReplaceQuery {
    fn into_expr(self) -> Expr {
        crate::query(self)
    }
}

impl IntoExprList for ReplaceQuery {
    fn into_expr_list(self) -> Vec<Expr> {
        vec![self.into_expr()]
    }
}

keelson_core::impl_clause_accessors!(ReplaceQuery {
    HasHints     => hints_mut:     Hints     = hints,
    HasModifiers => modifiers_mut: Modifiers = modifiers,
    HasTableRef  => table_ref_mut: TableRef  = table,
    HasValues    => values_mut:    Values    = values,
    HasSet       => set_mut:       Set       = set,
});
