use keelson_core::clause::{HasSet, HasTableRef, HasValues, Set, TableRef, Values};
use keelson_core::expr::{Expr, IntoExpr, IntoExprList};
use keelson_core::{Dialect, Error, Expression, Mod, Query, QueryExtensions, QueryType, SqlWriter};

use super::write_hints_and_modifiers;
use crate::Mysql;
use crate::extras::{
    HasDuplicateKeyUpdate, HasHints, HasModifiers, HasRowAlias, Hints, Modifiers, RowAlias,
};

/// A MySQL `INSERT`.
///
/// From <https://dev.mysql.com/doc/refman/8.4/en/insert.html>, with the three row
/// sources folded together:
///
/// ```text
/// INSERT [hint_comment] [LOW_PRIORITY | DELAYED | HIGH_PRIORITY] [IGNORE]
///     [INTO] tbl_name
///     [PARTITION (partition_name [, partition_name] ...)]
///     [(col_name [, col_name] ...)]
///     { {VALUES | VALUE} (value_list) [, (value_list)] ...
///     | SET assignment_list
///     | [(col_name [, col_name] ...)] { SELECT ... | TABLE tbl | VALUES row_list } }
///     [AS row_alias[(col_alias [, col_alias] ...)]]
///     [ON DUPLICATE KEY UPDATE assignment_list]
/// ```
///
/// `INTO` is written unconditionally even though the grammar makes it optional:
/// the shorter form saves four characters and costs a reader a double-take.
///
/// # Three row sources, one field each
///
/// `VALUES`, a query, and `SET` are alternatives. [`Values`] holds the first two;
/// [`set`](Self::set) is the third, and it **wins** when both are present, because
/// a half-and-half rendering is not a statement. With none of them, `VALUES ()` is
/// written — MySQL's spelling of "every column takes its default", which
/// PostgreSQL spells `DEFAULT VALUES`.
///
/// Choosing `SET` also drops the insert column list, because the `SET` production
/// does not have one: `INSERT INTO t (a) SET b = 1` is a syntax error, not a
/// statement with a redundant clause. `PARTITION` is in both productions and stays.
///
/// # No `WITH`
///
/// MySQL permits a CTE only immediately before the `SELECT` of an
/// `INSERT … SELECT`, never in front of the `INSERT` (*15.2.20*). So there is no
/// `with` field and no `insert::with` mod; put the `WITH` on the sub-query given
/// to [`insert::query`](crate::insert::query).
#[derive(Debug, Clone, Default)]
pub struct InsertQuery {
    /// `/*+ … */`.
    pub hints: Hints,
    /// `LOW_PRIORITY` / `HIGH_PRIORITY` / `DELAYED`, and `IGNORE`.
    pub modifiers: Modifiers,
    /// The target: table, partitions, and the insert column list.
    pub table: TableRef,
    /// The rows, or the query to insert the results of.
    pub values: Values,
    /// `SET a = 1, b = 2`, the assignment-list row source.
    pub set: Set,
    /// `AS row_alias [(cols)]`.
    pub row_alias: RowAlias,
    /// `ON DUPLICATE KEY UPDATE …`.
    pub duplicate_key_update: Set,
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
        if self.table.is_empty() {
            // Unlike a `SELECT`, which is a statement without a table, an `INSERT`
            // has nowhere to put the rows.
            w.record_error(Error::Incomplete("the target table of an INSERT"));
            return;
        }

        w.push_str("INSERT ");
        write_hints_and_modifiers(w, &self.hints, &self.modifiers);
        w.push_str("INTO ");
        write_target_and_row_source(w, &self.table, &self.set, &self.values);

        w.write_if(!self.row_alias.is_empty(), " ", &self.row_alias, "");
        w.write_if(
            !self.duplicate_key_update.is_empty(),
            " ON DUPLICATE KEY UPDATE ",
            &self.duplicate_key_update,
            "",
        );
    }
}

/// The target and the row source, shared by `INSERT` and `REPLACE`.
///
/// They are written together because the choice of row source decides whether the
/// target carries its column list: `SET` wins over `VALUES`, and the `SET`
/// production has no `(col_name, …)`. An absent source is `VALUES ()` — see
/// [`InsertQuery`].
pub(super) fn write_target_and_row_source(
    w: &mut SqlWriter<'_>,
    table: &TableRef,
    set: &Set,
    values: &Values,
) {
    if set.is_empty() {
        w.write_expr(table);
        if values.is_empty() {
            w.push_str(" VALUES ()");
        } else {
            w.push_str(" ");
            w.write_expr(values);
        }
        return;
    }

    // Only the SET path pays for the clone, and only to drop a list the production
    // it selected does not have.
    if table.columns.is_empty() {
        w.write_expr(table);
    } else {
        w.write_expr(&TableRef {
            columns: Vec::new(),
            ..table.clone()
        });
    }
    w.push_str(" SET ");
    w.write_expr(set);
}

impl Query for InsertQuery {
    fn query_type(&self) -> QueryType {
        QueryType::Insert
    }

    fn dialect(&self) -> &dyn Dialect {
        &Mysql
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
    HasHints     => hints_mut:     Hints     = hints,
    HasModifiers => modifiers_mut: Modifiers = modifiers,
    HasTableRef  => table_ref_mut: TableRef  = table,
    HasValues    => values_mut:    Values    = values,
});

/// The `INSERT … SET` row source, *not* the `ON DUPLICATE KEY UPDATE` list —
/// see [`HasDuplicateKeyUpdate`].
impl HasSet for InsertQuery {
    fn set_mut(&mut self) -> &mut Set {
        &mut self.set
    }
}

keelson_core::impl_clause_accessors!(InsertQuery {
    HasRowAlias           => row_alias_mut:            RowAlias = row_alias,
    HasDuplicateKeyUpdate => duplicate_key_update_mut: Set      = duplicate_key_update,
});
