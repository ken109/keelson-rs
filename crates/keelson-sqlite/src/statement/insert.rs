use keelson_core::clause::{
    ConflictClause, HasReturning, HasTableRef, HasValues, HasWith, Returning, TableRef, Values,
    With,
};
use keelson_core::expr::{Expr, IntoExpr, IntoExprList};
use keelson_core::{Dialect, Error, Expression, Mod, Query, QueryExtensions, QueryType, SqlWriter};

use crate::Sqlite;
use crate::extras::{HasOr, HasUpserts, Or, write_spaced};

/// A SQLite `INSERT`.
///
/// From <https://www.sqlite.org/lang_insert.html>:
///
/// ```text
/// [ WITH [ RECURSIVE ] common-table-expression [, ...] ]
/// INSERT [ OR { ROLLBACK | ABORT | REPLACE | FAIL | IGNORE } ]
///     INTO [ schema. ] table [ AS alias ] [ ( column [, ...] ) ]
///     { VALUES ( expr [, ...] ) [, ...] [ upsert-clause ]
///     | select-stmt [ upsert-clause ]
///     | DEFAULT VALUES }
///     [ RETURNING result-column [, ...] ]
/// ```
///
/// `DEFAULT VALUES` is the alternative [`Values`] cannot represent — an absent
/// clause has to render nothing — so an empty `Values` is what `DEFAULT VALUES`
/// *is* here, and this query writes the keywords itself. Note from the grammar that
/// `DEFAULT VALUES` admits no `upsert-clause`; a row of defaults conflicts with
/// nothing worth updating, and SQLite refuses the combination.
///
/// A source `select-stmt` with an `upsert-clause` after it must have a `WHERE`,
/// even a trivial one, or the parser reads the `ON` as the start of a join
/// condition. That is SQLite's rule, not this type's, and it is left to the parser
/// to say so — with an unusually clear message.
#[derive(Debug, Clone, Default)]
pub struct InsertQuery {
    /// `WITH …`.
    pub with: With,
    /// `OR REPLACE` and friends. `None` is the default, `ABORT`.
    pub or: Option<Or>,
    /// The target: table, optional alias, and the insert column list.
    pub table: TableRef,
    /// The rows, or the query to insert the results of. Empty means
    /// `DEFAULT VALUES`.
    pub values: Values,
    /// The `upsert-clause` list. SQLite 3.35 and later accept several, tried in
    /// order, and only the last may omit its conflict target.
    pub upserts: Vec<ConflictClause>,
    /// `RETURNING …`. SQLite 3.35 and later.
    pub returning: Returning,
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
        w.write_if(!self.with.is_empty(), "", &self.with, " ");

        if self.table.is_empty() {
            // Unlike a `SELECT`, which is a statement without a table, an `INSERT`
            // has nowhere to put the rows.
            w.record_error(Error::Incomplete("the target table of an INSERT"));
            return;
        }

        w.push_str("INSERT");
        if let Some(or) = self.or {
            w.push_str(" OR ");
            w.push_str(or.as_str());
        }
        w.push_str(" INTO ");
        w.write_expr(&self.table);

        if self.values.is_empty() {
            // Read the grammar's braces: the `upsert-clause` hangs off the `VALUES`
            // and `select-stmt` alternatives only. `DEFAULT VALUES ON CONFLICT …`
            // does not parse — a row of defaults conflicts with nothing worth
            // updating — so it is refused rather than written.
            if self.upserts.iter().any(|c| !c.is_empty()) {
                w.record_error(Error::other(
                    "a DEFAULT VALUES insert cannot carry an ON CONFLICT clause",
                ));
                return;
            }
            w.push_str(" DEFAULT VALUES");
        } else {
            w.push_str(" ");
            w.write_expr(&self.values);
        }

        // An actionless clause renders nothing, so it must not bring the space in
        // front of it either — `… VALUES (?1) RETURNING *` would otherwise be
        // written with two.
        if self.upserts.iter().any(|c| !c.is_empty()) {
            w.push_str(" ");
            write_spaced(w, self.upserts.iter().filter(|c| !c.is_empty()));
        }

        w.write_if(!self.returning.is_empty(), " ", &self.returning, "");
    }
}

impl Query for InsertQuery {
    fn query_type(&self) -> QueryType {
        QueryType::Insert
    }

    fn dialect(&self) -> &dyn Dialect {
        &Sqlite
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
    HasWith      => with_mut:      With                = with,
    HasTableRef  => table_ref_mut: TableRef            = table,
    HasValues    => values_mut:    Values              = values,
    HasOr        => or_mut:        Option<Or>          = or,
    HasUpserts   => upserts_mut:   Vec<ConflictClause> = upserts,
    HasReturning => returning_mut: Returning           = returning,
});
