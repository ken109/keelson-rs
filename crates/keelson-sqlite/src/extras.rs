use std::borrow::Cow;

use keelson_core::clause::ConflictClause;
use keelson_core::expr::{Expr, IntoExpr};
use keelson_core::{Error, Expression, Query, SqlWriter};

// ---------------------------------------------------------------------------
// OR <conflict-algorithm>
// ---------------------------------------------------------------------------

/// The `conflict-clause` of an `INSERT` or `UPDATE`: `INSERT OR REPLACE INTO …`,
/// `UPDATE OR IGNORE …`.
///
/// From <https://www.sqlite.org/lang_insert.html> and
/// <https://www.sqlite.org/lang_update.html>:
///
/// ```text
/// INSERT OR { ROLLBACK | ABORT | REPLACE | FAIL | IGNORE } INTO …
/// UPDATE OR { ROLLBACK | ABORT | REPLACE | FAIL | IGNORE } …
/// ```
///
/// `ABORT` is the default and there is no reason to write it, but it *is* one of
/// the five keywords the grammar lists rather than an absence, so it is
/// representable — unlike PostgreSQL's `ALL` on a `SELECT`, which adds nothing at
/// all. A `DELETE` has no such clause, which is why nothing in
/// [`delete`](crate::delete) mentions one.
///
/// SQLite's standalone `REPLACE INTO t …` is exactly `INSERT OR REPLACE INTO t …`;
/// only the longer spelling is produced, because the two are the same statement
/// and one spelling is enough.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Or {
    /// `OR ROLLBACK` — abort the whole transaction.
    Rollback,
    /// `OR ABORT` — the default: abort this statement, keep the transaction.
    Abort,
    /// `OR REPLACE` — delete the rows that conflict, then insert.
    Replace,
    /// `OR FAIL` — stop, but keep the changes already made by this statement.
    Fail,
    /// `OR IGNORE` — skip the offending row and carry on.
    Ignore,
}

impl Or {
    /// The keyword, as written after `OR`.
    pub fn as_str(self) -> &'static str {
        match self {
            Or::Rollback => "ROLLBACK",
            Or::Abort => "ABORT",
            Or::Replace => "REPLACE",
            Or::Fail => "FAIL",
            Or::Ignore => "IGNORE",
        }
    }
}

/// A statement that takes a `conflict-clause`: an `INSERT` or an `UPDATE`.
///
/// A `DELETE` does not implement this, which is how "a delete cannot violate a
/// constraint" is said — `delete::or_replace` does not exist to be misapplied.
pub trait HasOr {
    /// The conflict algorithm to modify.
    fn or_mut(&mut self) -> &mut Option<Or>;
}

impl HasOr for Option<Or> {
    fn or_mut(&mut self) -> &mut Option<Or> {
        self
    }
}

// ---------------------------------------------------------------------------
// Compound SELECTs
// ---------------------------------------------------------------------------

/// A `compound-operator` — the four SQLite has, and no more.
///
/// From <https://www.sqlite.org/syntax/compound-operator.html>:
///
/// ```text
/// UNION | UNION ALL | INTERSECT | EXCEPT
/// ```
///
/// `ALL` belongs to the operator here rather than being a separate flag, because
/// SQLite offers it on `UNION` alone: `INTERSECT ALL` and `EXCEPT ALL` are
/// rejected by SQLite's own parser as well as by the engine. Folding it into the
/// enum is how "there is no `INTERSECT ALL`" is said in a way that cannot be
/// written by accident — the shape PostgreSQL needs, a `SetOp` plus an `all` flag,
/// would make it representable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompoundOp {
    /// `UNION` — rows of either, duplicates removed.
    Union,
    /// `UNION ALL` — rows of either, duplicates kept.
    UnionAll,
    /// `INTERSECT` — rows of both.
    Intersect,
    /// `EXCEPT` — rows of the left that are not in the right.
    Except,
}

impl CompoundOp {
    /// The operator, as written.
    pub fn as_str(self) -> &'static str {
        match self {
            CompoundOp::Union => "UNION",
            CompoundOp::UnionAll => "UNION ALL",
            CompoundOp::Intersect => "INTERSECT",
            CompoundOp::Except => "EXCEPT",
        }
    }
}

/// One operand of a compound `SELECT`: `UNION ALL <select-core>`.
///
/// **The operand is not parenthesised**, and that is the whole reason this type
/// exists instead of [`Combine`](keelson_core::clause::Combine). SQLite's
/// `compound-select-stmt` is a sequence of bare `select-core`s:
///
/// ```text
/// select-core ( compound-operator select-core )*
/// ```
///
/// A parenthesised select is only a *table-or-subquery* in SQLite, never a
/// compound operand, so `(SELECT 1) UNION (SELECT 2)` is a syntax error — verified
/// against SQLite's own parser and against a real SQLite. PostgreSQL parenthesises
/// every operand so that one may carry its own `ORDER BY`/`LIMIT`; SQLite cannot
/// express that at all, and correspondingly has no need for the `_combined` mods
/// [`keelson_psql`] carries.
#[derive(Debug, Clone, Default)]
pub struct Compound {
    /// Which operator. `None` is how a default-constructed operand stays absent.
    pub op: Option<CompoundOp>,
    /// The operand, rendered bare.
    pub query: Option<Expr>,
}

impl Compound {
    /// A compound operand joined by `op`.
    pub fn new(op: CompoundOp, query: impl IntoExpr) -> Compound {
        Compound {
            op: Some(op),
            query: Some(query.into_expr()),
        }
    }

    /// Whether this operand is absent.
    pub fn is_empty(&self) -> bool {
        self.op.is_none() && self.query.is_none()
    }
}

impl Expression for Compound {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        if self.is_empty() {
            return;
        }
        // Half-filled is a caller error rather than an absent clause, and there is
        // no rendering that could be right.
        let Some(op) = self.op else {
            w.record_error(Error::Incomplete("the operator of a compound SELECT"));
            return;
        };
        let Some(query) = &self.query else {
            w.record_error(Error::Incomplete("the operand of a compound SELECT"));
            return;
        };

        w.push_str(op.as_str());
        w.push_str(" ");
        w.write_expr(query);
    }
}

/// Every compound operand chained onto one `SELECT`.
///
/// Unlike [`Combines`](keelson_core::clause::Combines) this holds *only* the
/// operands. SQLite's `ORDER BY` and `LIMIT` sit after the last operand and always
/// belong to the whole compound — there is no way to give one operand its own —
/// so the statement's single `ORDER BY`/`LIMIT`/`OFFSET` is already the
/// combination's, and no second set is needed.
#[derive(Debug, Clone, Default)]
pub struct Compounds {
    /// The operands, applied left to right.
    pub operands: Vec<Compound>,
}

impl Compounds {
    /// Append one operand.
    pub fn append_compound(&mut self, compound: Compound) {
        self.operands.push(compound);
    }

    /// Whether nothing is compounded onto the statement.
    ///
    /// A list of nothing but *absent* operands counts as empty, so the enclosing
    /// statement does not write the separator in front of a clause that renders
    /// nothing. A half-filled operand is not absent — it records a failure instead.
    pub fn is_empty(&self) -> bool {
        self.operands.iter().all(Compound::is_empty)
    }
}

impl Expression for Compounds {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        write_spaced(w, self.operands.iter().filter(|c| !c.is_empty()));
    }
}

/// A `SELECT` other `SELECT`s can be compounded onto.
pub trait HasCompounds {
    /// The compound operands to modify.
    fn compounds_mut(&mut self) -> &mut Compounds;
}

impl HasCompounds for Compounds {
    fn compounds_mut(&mut self) -> &mut Compounds {
        self
    }
}

/// An `INSERT`'s `upsert-clause` list.
///
/// SQLite 3.35 and later accept several, tried in order:
///
/// ```text
/// INSERT … ON CONFLICT (a) DO UPDATE SET … ON CONFLICT DO NOTHING
/// ```
///
/// with the rule that only the last may omit its conflict target. PostgreSQL has
/// exactly one `ON CONFLICT`, which is why
/// [`Conflict`](keelson_core::clause::Conflict) is a single slot and this is a list
/// instead. The clause itself is core's
/// [`ConflictClause`](keelson_core::clause::ConflictClause) — SQLite's
/// `ON CONFLICT (cols) [WHERE …] DO { NOTHING | UPDATE SET … [WHERE …] }` is that
/// shape exactly, minus the `ON CONSTRAINT` target, for which no mod is exported.
pub trait HasUpserts {
    /// The upsert clauses to modify.
    fn upserts_mut(&mut self) -> &mut Vec<ConflictClause>;
}

impl HasUpserts for Vec<ConflictClause> {
    fn upserts_mut(&mut self) -> &mut Vec<ConflictClause> {
        self
    }
}

/// Write a sequence of possibly-absent items, single-space separated, writing
/// nothing at all when every one of them is absent.
///
/// `SqlWriter::write_iter` cannot be used: it would put a separator either side of
/// an item that renders nothing. Core has this helper too, privately, for exactly
/// the same reason.
pub(crate) fn write_spaced<'a, E: Expression + 'a>(
    w: &mut SqlWriter<'_>,
    items: impl IntoIterator<Item = &'a E>,
) {
    let mut written = false;
    for item in items {
        if written {
            w.push_str(" ");
        }
        w.write_expr(item);
        written = true;
    }
}

// ---------------------------------------------------------------------------
// Sub-queries and upsert values
// ---------------------------------------------------------------------------

/// A whole query standing in an expression slot, rendered in **its own** dialect.
///
/// [`SqlWriter::write_with_dialect`] keeps one shared argument list and
/// placeholder counter, so a sub-query re-indexes into its container for free.
#[derive(Debug)]
struct QueryExpr<Q>(Q);

impl<Q: Query> Expression for QueryExpr<Q> {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.write_with_dialect(self.0.dialect(), &self.0);
    }
}

/// A query as an expression, **not** parenthesised.
///
/// The form for slots that supply their own parentheses — a `WITH` body,
/// `INSERT … SELECT` — *and* for a compound operand, which in SQLite must have no
/// parentheses at all. Use [`subquery`] where the parentheses belong to the
/// sub-query itself, as in a `FROM` item or a scalar sub-expression.
pub fn query(q: impl Query + 'static) -> Expr {
    Expr::custom(QueryExpr(q))
}

/// A parenthesised sub-query: `(SELECT …)`.
///
/// What a `table-or-subquery` or a scalar sub-expression needs. Unlike PostgreSQL,
/// SQLite does not require an alias on a `FROM` sub-query.
pub fn subquery(q: impl Query + 'static) -> Expr {
    Expr::group(query(q))
}

/// `excluded."col"` — the row that would have been inserted, inside
/// `ON CONFLICT … DO UPDATE`.
///
/// SQLite spells the pseudo-table in lower case
/// (<https://www.sqlite.org/lang_upsert.html>); the name is not quoted, because
/// `"excluded"` would be read as an ordinary table name.
pub fn excluded(column: impl Into<Cow<'static, str>>) -> Expr {
    Expr::join_with("", (Expr::raw("excluded."), Expr::ident(column.into())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Sqlite;
    use keelson_core::build;

    fn sql(e: impl Expression) -> String {
        build(&Sqlite, &e).expect("render").0
    }

    /// <https://www.sqlite.org/syntax/compound-operator.html>
    #[test]
    fn a_compound_operand_carries_its_operator_and_no_parentheses() {
        assert_eq!(
            sql(Compound::new(CompoundOp::UnionAll, Expr::raw("SELECT 1"))),
            "UNION ALL SELECT 1"
        );
        assert_eq!(
            sql(Compound::new(CompoundOp::Intersect, Expr::raw("SELECT 1"))),
            "INTERSECT SELECT 1"
        );
        assert_eq!(
            sql(Compound::new(CompoundOp::Except, Expr::raw("SELECT 1"))),
            "EXCEPT SELECT 1"
        );
        assert_eq!(
            sql(Compound::new(CompoundOp::Union, Expr::raw("SELECT 1"))),
            "UNION SELECT 1"
        );
    }

    #[test]
    fn an_absent_operand_takes_its_separator_with_it() {
        let mut cs = Compounds::default();
        assert!(cs.is_empty());
        assert_eq!(sql(Compounds::default()), "");

        cs.append_compound(Compound::default());
        assert!(
            cs.is_empty(),
            "a list of nothing but absent operands is an absent clause, or the \
             statement writes the separator in front of nothing"
        );

        cs.append_compound(Compound::new(CompoundOp::Union, Expr::raw("SELECT 1")));
        cs.append_compound(Compound::default());
        assert!(!cs.is_empty());
        assert_eq!(sql(cs), "UNION SELECT 1");
    }

    #[test]
    fn a_half_filled_operand_is_a_recorded_failure() {
        let no_op = Compound {
            query: Some(Expr::raw("SELECT 1")),
            ..Compound::default()
        };
        assert_eq!(
            build(&Sqlite, &no_op).unwrap_err().to_string(),
            "query is missing the operator of a compound SELECT"
        );

        let no_query = Compound {
            op: Some(CompoundOp::Union),
            ..Compound::default()
        };
        assert_eq!(
            build(&Sqlite, &no_query).unwrap_err().to_string(),
            "query is missing the operand of a compound SELECT"
        );
    }

    /// <https://www.sqlite.org/lang_upsert.html>: the pseudo-table is `excluded`,
    /// unquoted and lower case.
    #[test]
    fn excluded_qualifies_the_column_with_the_pseudo_table() {
        assert_eq!(sql(excluded("email")), r#"excluded."email""#);
    }

    #[test]
    fn every_conflict_algorithm_has_its_keyword() {
        assert_eq!(Or::Rollback.as_str(), "ROLLBACK");
        assert_eq!(Or::Abort.as_str(), "ABORT");
        assert_eq!(Or::Replace.as_str(), "REPLACE");
        assert_eq!(Or::Fail.as_str(), "FAIL");
        assert_eq!(Or::Ignore.as_str(), "IGNORE");
    }
}
