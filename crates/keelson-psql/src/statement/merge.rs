use std::borrow::Cow;

use keelson_core::clause::{
    HasJoins, HasReturning, HasTableRef, HasWith, Join, Returning, Set, TableRef, With,
};
use keelson_core::expr::{Expr, IntoExpr, IntoExprList};
use keelson_core::{Dialect, Error, Expression, Mod, Query, QueryExtensions, QueryType, SqlWriter};

use super::HasTargetTable;
use crate::Psql;
use crate::extras::Overriding;

/// A PostgreSQL `MERGE` (PostgreSQL 15+).
///
/// From <https://www.postgresql.org/docs/17/sql-merge.html>:
///
/// ```text
/// [ WITH with_query [, ...] ]
/// MERGE INTO [ ONLY ] target_table_name [ * ] [ [ AS ] target_alias ]
///     USING data_source ON join_condition
///     when_clause [...]
///     [ RETURNING … ]
///
/// when_clause:
///     WHEN MATCHED [ AND condition ] THEN { merge_update | merge_delete | DO NOTHING }
///   | WHEN NOT MATCHED BY SOURCE [ AND condition ] THEN
///         { merge_update | merge_delete | DO NOTHING }
///   | WHEN NOT MATCHED [ BY TARGET ] [ AND condition ] THEN
///         { merge_insert | DO NOTHING }
/// ```
///
/// Three parts of the production are newer than 15 and are marked where the API
/// produces them: `WHEN NOT MATCHED BY SOURCE`, the explicit `BY TARGET`
/// spelling, and `RETURNING` are all PostgreSQL 17+.
///
/// The target lives in [`HasTargetTable`] — like an `UPDATE`'s table — and the
/// `USING` source in [`HasTableRef`], like a `DELETE`'s `USING` item, which is
/// what lets one [`TableChain`](crate::shared::TableChain) serve both slots.
/// The source also carries [`HasJoins`]: gram.y's `MergeStmt` reads
/// `USING table_ref ON a_expr`, and a `table_ref` may be a `joined_table`, so a
/// joined source is grammatical. The target is a `relation_expr_opt_alias`,
/// which admits no joins — the same split an `UPDATE` has.
///
/// Which actions a `WHEN` clause may take depends on which `WHEN` it is, and that
/// is enforced by the chain types in [`crate::merge`] rather than re-checked
/// here: a [`MergeWhen`] holds whatever it was built with.
#[derive(Debug, Clone, Default)]
pub struct MergeQuery {
    /// `WITH …`. `MERGE` takes a plain `WITH`; PostgreSQL rejects
    /// `WITH RECURSIVE` on it at analysis time, which is why
    /// [`crate::merge`] does not re-export `recursive`.
    pub with: With,
    /// The target: `MERGE INTO [ ONLY ] table [ * ] [ AS alias ]`.
    pub target: TableRef,
    /// The data source: a table or a parenthesised query, with an alias.
    pub source: TableRef,
    /// The `ON` join condition. Several entries are `AND`-joined, as in a join's
    /// `ON`.
    pub on: Vec<Expr>,
    /// The `WHEN` clauses, applied in order — the grammar requires at least one.
    pub whens: Vec<MergeWhen>,
    /// `RETURNING …` (PostgreSQL 17+).
    pub returning: Returning,
}

impl MergeQuery {
    /// A `MERGE` with nothing set yet.
    pub fn new() -> MergeQuery {
        MergeQuery::default()
    }

    /// Apply more mods to an existing query.
    pub fn apply(&mut self, mods: impl Mod<MergeQuery>) {
        mods.apply(self);
    }
}

impl Expression for MergeQuery {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.write_if(!self.with.is_empty(), "", &self.with, " ");

        // Every one of these is grammatically required — sql-merge.html has no
        // brackets around USING, ON or the when-clause list — so an absent one
        // is a recorded failure, never a shorter statement.
        if self.target.is_empty() {
            w.record_error(Error::Incomplete("the target table of a MERGE"));
            return;
        }
        if self.source.is_empty() {
            w.record_error(Error::Incomplete("the USING source of a MERGE"));
            return;
        }
        if self.on.is_empty() {
            w.record_error(Error::Incomplete("the ON condition of a MERGE"));
            return;
        }
        if self.whens.is_empty() {
            w.record_error(Error::Incomplete("the WHEN clauses of a MERGE"));
            return;
        }

        w.push_str("MERGE INTO ");
        w.write_expr(&self.target);
        w.push_str(" USING ");
        w.write_expr(&self.source);
        w.write_slice(&self.on, " ON ", " AND ", "");
        w.write_slice(&self.whens, " ", " ", "");
        w.write_if(!self.returning.is_empty(), " ", &self.returning, "");
    }
}

impl Query for MergeQuery {
    fn query_type(&self) -> QueryType {
        QueryType::Merge
    }

    fn dialect(&self) -> &dyn Dialect {
        &Psql
    }
}

impl<H, L, M> QueryExtensions<H, L, M> for MergeQuery {}

impl IntoExpr for MergeQuery {
    fn into_expr(self) -> Expr {
        crate::query(self)
    }
}

impl IntoExprList for MergeQuery {
    fn into_expr_list(self) -> Vec<Expr> {
        vec![self.into_expr()]
    }
}

impl HasWith for MergeQuery {
    fn with_mut(&mut self) -> &mut With {
        &mut self.with
    }
}

impl HasTargetTable for MergeQuery {
    fn target_table_mut(&mut self) -> &mut TableRef {
        &mut self.target
    }
}

impl HasTableRef for MergeQuery {
    fn table_ref_mut(&mut self) -> &mut TableRef {
        &mut self.source
    }
}

impl HasJoins for MergeQuery {
    fn joins_mut(&mut self) -> &mut Vec<Join> {
        // The joins belong to the USING source — the one slot of a MERGE that
        // is a full `table_ref` in gram.y. They cannot be dropped silently: an
        // absent source is already recorded as Incomplete before rendering
        // reaches the point where its joins would have been written.
        &mut self.source.joins
    }
}

impl HasReturning for MergeQuery {
    fn returning_mut(&mut self) -> &mut Returning {
        &mut self.returning
    }
}

/// One `WHEN … THEN …` clause of a [`MergeQuery`].
#[derive(Debug, Clone)]
pub struct MergeWhen {
    /// Which of the three `WHEN` forms this is.
    pub kind: MergeMatchKind,
    /// The `AND condition` refinement. Several entries are `AND`-joined.
    pub condition: Vec<Expr>,
    /// What `THEN` does.
    pub action: MergeAction,
}

impl Expression for MergeWhen {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.push_str(self.kind.as_str());
        w.write_slice(&self.condition, " AND ", " AND ", "");
        w.push_str(" THEN ");
        match &self.action {
            MergeAction::Update(set) => {
                if set.is_empty() {
                    // `UPDATE SET` with nothing after it is not a merge_update,
                    // and the keywords are already half-written by the time an
                    // empty list would render — so this is recorded, as every
                    // unfillable clause is.
                    w.record_error(Error::Incomplete("the assignments of a MERGE UPDATE"));
                    return;
                }
                w.push_str("UPDATE SET ");
                w.write_expr(set);
            }
            MergeAction::Delete => w.push_str("DELETE"),
            MergeAction::DoNothing => w.push_str("DO NOTHING"),
            MergeAction::Insert(insert) => w.write_expr(insert),
        }
    }
}

/// Which `WHEN` form a [`MergeWhen`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeMatchKind {
    /// `WHEN MATCHED` — the source row found a target row.
    Matched,
    /// `WHEN NOT MATCHED [ BY TARGET ]` — the source row found no target row.
    ///
    /// `BY TARGET` spells out the default; it exists (PostgreSQL 17+) to read
    /// well next to `BY SOURCE`, and is written only when asked for —
    /// see the `OFFSET`/`FETCH` entry in `docs/sql-rendering.md` for the rule.
    NotMatched {
        /// Whether to write the explicit `BY TARGET` (PostgreSQL 17+).
        by_target: bool,
    },
    /// `WHEN NOT MATCHED BY SOURCE` — the target row has no source row
    /// (PostgreSQL 17+).
    NotMatchedBySource,
}

impl MergeMatchKind {
    /// The clause head, as written.
    pub fn as_str(self) -> &'static str {
        match self {
            MergeMatchKind::Matched => "WHEN MATCHED",
            MergeMatchKind::NotMatched { by_target: false } => "WHEN NOT MATCHED",
            MergeMatchKind::NotMatched { by_target: true } => "WHEN NOT MATCHED BY TARGET",
            MergeMatchKind::NotMatchedBySource => "WHEN NOT MATCHED BY SOURCE",
        }
    }
}

/// What a [`MergeWhen`]'s `THEN` does.
#[derive(Debug, Clone)]
pub enum MergeAction {
    /// `UPDATE SET …` — a matched arm. The `Set` is the same assignment list an
    /// `UPDATE` carries, keyword supplied here.
    Update(Set),
    /// `DELETE` — a matched arm.
    Delete,
    /// `DO NOTHING` — any arm.
    DoNothing,
    /// `INSERT …` — a not-matched arm.
    Insert(MergeInsert),
}

/// The `merge_insert` production:
///
/// ```text
/// INSERT [( column_name [, ...] )]
///     [ OVERRIDING { SYSTEM | USER } VALUE ]
///     { VALUES ( { expression | DEFAULT } [, ...] ) | DEFAULT VALUES }
/// ```
///
/// One row only — unlike an `INSERT` statement's `VALUES` list — and no source
/// query, which is why this is its own shape rather than a reuse of
/// [`Values`](keelson_core::clause::Values). An empty row is `DEFAULT VALUES`,
/// the same reading [`InsertQuery`](super::InsertQuery) gives an empty row
/// source.
#[derive(Debug, Clone, Default)]
pub struct MergeInsert {
    /// The insert column list. Quoted.
    pub columns: Vec<Cow<'static, str>>,
    /// `OVERRIDING … VALUE`.
    pub overriding: Option<Overriding>,
    /// The single row's cells. Empty means `DEFAULT VALUES`.
    pub row: Vec<Expr>,
}

impl Expression for MergeInsert {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.push_str("INSERT");
        if !self.columns.is_empty() {
            w.push_str(" (");
            for (i, column) in self.columns.iter().enumerate() {
                if i > 0 {
                    w.push_str(", ");
                }
                w.push_quoted(&[column]);
            }
            w.push_str(")");
        }
        if let Some(overriding) = &self.overriding {
            w.push_str(" OVERRIDING ");
            w.push_str(overriding.as_str());
            w.push_str(" VALUE");
        }
        if self.row.is_empty() {
            w.push_str(" DEFAULT VALUES");
        } else {
            w.write_slice(&self.row, " VALUES (", ", ", ")");
        }
    }
}
