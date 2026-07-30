use std::borrow::Cow;

use crate::error::Error;
use crate::expr::{Expr, IntoExpr, IntoExprList};
use crate::writer::{Expression, SqlWriter};

use super::set::{HasSet, Set};
use super::where_::{HasWhere, Where};

/// The slot an `INSERT` keeps for its conflict handling.
///
/// It holds an erased expression rather than a [`ConflictClause`] because the three
/// dialects do not spell this the same way: PostgreSQL and SQLite have
/// `ON CONFLICT`, MySQL has `ON DUPLICATE KEY UPDATE` and SQLite also has
/// `INSERT OR REPLACE`. Each dialect puts its own expression here — usually a
/// `ConflictClause`, reaching core through
/// [`Expr::Custom`](crate::expr::Expr::Custom).
#[derive(Debug, Clone, Default)]
pub struct Conflict {
    /// The whole conflict clause, whatever this dialect's shape for one is.
    pub expression: Option<Expr>,
}

impl Conflict {
    /// Set the conflict clause.
    pub fn set_conflict(&mut self, conflict: impl IntoExpr) {
        self.expression = Some(conflict.into_expr());
    }

    /// Whether the clause is absent.
    pub fn is_empty(&self) -> bool {
        self.expression.is_none()
    }
}

impl Expression for Conflict {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.write_if_some(self.expression.as_ref(), "", "");
    }
}

/// An `INSERT` with conflict handling.
pub trait HasConflict {
    /// The conflict slot to modify.
    fn conflict_mut(&mut self) -> &mut Conflict;
}

impl HasConflict for Conflict {
    fn conflict_mut(&mut self) -> &mut Conflict {
        self
    }
}

/// `ON CONFLICT [<target>] DO NOTHING | DO UPDATE SET … [WHERE …]`
///
/// From PostgreSQL 17, <https://www.postgresql.org/docs/17/sql-insert.html>:
///
/// ```text
/// ON CONFLICT [ conflict_target ] conflict_action
///
/// conflict_target: ( { index_column_name | ( index_expression ) } [ COLLATE … ] [ opclass ] [, ...] )
///                    [ WHERE index_predicate ]
///                | ON CONSTRAINT constraint_name
/// conflict_action: DO NOTHING
///                | DO UPDATE SET { … } [, ...] [ WHERE condition ]
/// ```
///
/// The two halves are easy to conflate and behave nothing alike. The **target** is
/// an index inference — which unique index the conflict is detected on — and its
/// `WHERE` is the *index's* predicate, matched against the index rather than
/// evaluated per row. The **action**'s `WHERE` filters which conflicting rows get
/// updated. Both are `WHERE`s in the same clause, and both are reachable through
/// [`HasWhere`] here: on [`ConflictTarget`] for the first, on `ConflictClause` for
/// the second.
///
/// `DO UPDATE` also requires at least one assignment, which is checked rather than
/// rendered into a syntax error.
#[derive(Debug, Clone, Default)]
pub struct ConflictClause {
    /// Which conflicts this handles. Absent means any.
    pub target: ConflictTarget,
    /// What to do. `None` is how a default-constructed clause stays absent: there
    /// is no `ON CONFLICT` without an action.
    pub action: Option<ConflictAction>,
    /// The assignments of `DO UPDATE`.
    pub set: Set,
    /// Which conflicting rows `DO UPDATE` applies to.
    pub where_: Where,
}

impl ConflictClause {
    /// `ON CONFLICT … DO NOTHING`.
    pub fn do_nothing() -> Self {
        ConflictClause {
            action: Some(ConflictAction::Nothing),
            ..ConflictClause::default()
        }
    }

    /// `ON CONFLICT … DO UPDATE SET …`. The assignments still have to be added.
    pub fn do_update() -> Self {
        ConflictClause {
            action: Some(ConflictAction::Update),
            ..ConflictClause::default()
        }
    }

    /// Whether the clause is absent.
    pub fn is_empty(&self) -> bool {
        self.action.is_none()
    }
}

impl Expression for ConflictClause {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        let Some(action) = &self.action else {
            return;
        };
        if matches!(action, ConflictAction::Update) && self.set.is_empty() {
            w.record_error(Error::Incomplete(
                "the assignments of ON CONFLICT DO UPDATE",
            ));
            return;
        }

        w.push_str("ON CONFLICT");
        w.write_if(!self.target.is_empty(), " ", &self.target, "");
        w.push_str(" DO ");
        w.push_str(action.as_str());

        // The keyword belongs here rather than to `Set`: MySQL's
        // `ON DUPLICATE KEY UPDATE` takes the same list without one.
        w.write_if(!self.set.is_empty(), " SET ", &self.set, "");
        w.write_if(!self.where_.is_empty(), " ", &self.where_, "");
    }
}

impl HasSet for ConflictClause {
    fn set_mut(&mut self) -> &mut Set {
        &mut self.set
    }
}

impl HasWhere for ConflictClause {
    fn where_mut(&mut self) -> &mut Where {
        &mut self.where_
    }
}

/// Anything with a conflict clause of the `ON CONFLICT` shape, so that the target
/// and action mods can be written once.
pub trait HasConflictClause {
    /// The conflict clause to modify.
    fn conflict_clause_mut(&mut self) -> &mut ConflictClause;
}

impl HasConflictClause for ConflictClause {
    fn conflict_clause_mut(&mut self) -> &mut ConflictClause {
        self
    }
}

/// What the conflict is detected on: a named constraint, or an index inferred from
/// a column list and an optional predicate.
///
/// A constraint name wins outright, because PostgreSQL forbids combining the two —
/// `ON CONSTRAINT` names an index directly and leaves nothing to infer.
#[derive(Debug, Clone, Default)]
pub struct ConflictTarget {
    /// `ON CONSTRAINT <name>`. Quoted on output.
    pub constraint: Option<Cow<'static, str>>,
    /// The columns or expressions the unique index is over.
    pub columns: Vec<Expr>,
    /// The partial index's predicate — matched against the index definition, not
    /// evaluated against rows.
    pub where_: Where,
}

impl ConflictTarget {
    /// Infer the index from `columns`.
    pub fn on_columns(columns: impl IntoExprList) -> Self {
        ConflictTarget {
            columns: columns.into_expr_list(),
            ..ConflictTarget::default()
        }
    }

    /// Name the constraint directly.
    pub fn on_constraint(name: impl Into<Cow<'static, str>>) -> Self {
        ConflictTarget {
            constraint: Some(name.into()),
            ..ConflictTarget::default()
        }
    }

    /// Whether the target is absent, so that any conflict is handled.
    pub fn is_empty(&self) -> bool {
        self.constraint.is_none() && self.columns.is_empty() && self.where_.is_empty()
    }
}

impl Expression for ConflictTarget {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        if let Some(constraint) = &self.constraint {
            w.push_str("ON CONSTRAINT ");
            w.push_quoted(&[constraint]);
            return;
        }

        if self.columns.is_empty() {
            // PostgreSQL's gram.y:
            //   opt_conf_expr: '(' index_params ')' where_clause
            //                | ON CONSTRAINT name | /*EMPTY*/
            // The predicate hangs off the parenthesised column list and cannot
            // stand without it — `ON CONFLICT WHERE …` is a syntax error, verified
            // against libpg_query — so a predicate on its own is refused rather
            // than written or silently dropped.
            if !self.where_.is_empty() {
                w.record_error(Error::Incomplete(
                    "the column list an ON CONFLICT index predicate belongs to",
                ));
            }
            return;
        }

        w.write_slice(&self.columns, "(", ", ", ")");
        w.write_if(!self.where_.is_empty(), " ", &self.where_, "");
    }
}

impl HasWhere for ConflictTarget {
    fn where_mut(&mut self) -> &mut Where {
        &mut self.where_
    }
}

/// What to do about a conflicting row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictAction {
    /// `DO NOTHING` — skip the row.
    Nothing,
    /// `DO UPDATE` — the upsert. Requires assignments.
    Update,
}

impl ConflictAction {
    /// The keyword, as written after `DO`.
    pub fn as_str(self) -> &'static str {
        match self {
            ConflictAction::Nothing => "NOTHING",
            ConflictAction::Update => "UPDATE",
        }
    }
}

#[cfg(test)]
mod tests {
    use keelson_sqlcheck::testing::assert_frag_sql;

    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::expr::{Chain, arg, quote};
    use crate::value::Value;
    use crate::writer::build;

    /// `ON CONFLICT` only exists on an `INSERT`, so that is the frame. `users`'
    /// primary key is what the column targets below infer, because a target that
    /// matches no unique index is a semantic error rather than a syntactic one —
    /// the class of mistake only the engine tier sees.
    const FRAME: &str = r#"INSERT INTO users ("id", "name") VALUES (1, 'kubo') {}"#;
    /// For a bare [`ConflictTarget`], which is the part between the keyword and the
    /// action.
    const TARGET_FRAME: &str =
        r#"INSERT INTO tags ("id", "name") VALUES (1, 'rust') ON CONFLICT {} DO NOTHING"#;

    fn sql(e: &impl Expression) -> String {
        build(&Numbered, e).expect("render").0
    }

    #[test]
    fn an_actionless_clause_writes_nothing() {
        assert_frag_sql(FRAME, &sql(&ConflictClause::default()), "");
        assert!(ConflictClause::default().is_empty());
        assert_frag_sql(FRAME, &sql(&Conflict::default()), "");
        assert!(Conflict::default().is_empty());
    }

    #[test]
    fn do_nothing_needs_no_target() {
        // PostgreSQL 17: `ON CONFLICT [ conflict_target ] conflict_action`, and
        // DO NOTHING is the one action that works with no target at all.
        assert_frag_sql(
            FRAME,
            &sql(&ConflictClause::do_nothing()),
            "ON CONFLICT DO NOTHING",
        );
    }

    #[test]
    fn a_column_target_precedes_the_action() {
        let c = ConflictClause {
            target: ConflictTarget::on_columns(quote("id")),
            ..ConflictClause::do_nothing()
        };
        assert_frag_sql(FRAME, &sql(&c), r#"ON CONFLICT ("id") DO NOTHING"#);
    }

    #[test]
    fn a_constraint_name_beats_the_column_list() {
        // The two forms of conflict_target are alternatives, so a target holding
        // both renders the one PostgreSQL would accept. `tags_name_key` is the
        // constraint the shared schema's `name text NOT NULL UNIQUE` creates.
        let mut t = ConflictTarget::on_constraint("tags_name_key");
        t.columns = vec![quote("name")];
        t.where_.append_where("id IS NOT NULL");
        assert_frag_sql(TARGET_FRAME, &sql(&t), r#"ON CONSTRAINT "tags_name_key""#);
    }

    #[test]
    fn a_partial_index_target_carries_the_indexs_own_predicate() {
        // This WHERE belongs to the *index*: it is how PostgreSQL is told which
        // partial unique index to infer.
        //
        // Not framed. The engine tier resolves a conflict target against the
        // indexes that exist, and the shared schema has no partial unique index
        // for this to match — inventing one there would change a fixture five
        // other test binaries share, to check a rendering rule. What the grammar
        // says (`'(' index_params ')' where_clause`) is pinned by the psql crate,
        // which owns that syntax; here it is only the order of the two parts.
        let mut t = ConflictTarget::on_columns((quote("email"), quote("tenant_id")));
        t.where_.append_where("deleted_at IS NULL");
        assert_eq!(
            build(&Numbered, &t).unwrap().0,
            r#"("email", "tenant_id") WHERE deleted_at IS NULL"#
        );
        assert!(!t.is_empty());
    }

    #[test]
    fn an_empty_target_writes_nothing() {
        assert_frag_sql(TARGET_FRAME, &sql(&ConflictTarget::default()), "");
        assert!(ConflictTarget::default().is_empty());
    }

    #[test]
    fn an_index_predicate_without_a_column_list_is_a_recorded_failure() {
        // `ON CONFLICT WHERE …` does not parse: in gram.y the where_clause follows
        // `'(' index_params ')'`, so there is nothing for the predicate to qualify.
        let mut t = ConflictTarget::default();
        t.where_mut().append_where("deleted_at IS NULL");
        assert!(!t.is_empty());
        assert_eq!(
            build(&Numbered, &t).unwrap_err().to_string(),
            "query is missing the column list an ON CONFLICT index predicate belongs to"
        );
    }

    #[test]
    fn do_update_carries_the_set_keyword_and_its_own_where() {
        // The action's WHERE filters rows; the target's matched an index. Both
        // appear here, in that order, which is the shape most easily got wrong.
        let mut c = ConflictClause {
            target: ConflictTarget::on_columns(quote("id")),
            ..ConflictClause::do_update()
        };
        c.set_mut()
            .append_set(Expr::raw(r#""name" = EXCLUDED."name""#));
        c.where_mut()
            .append_where(quote(("users", "id")).gt(arg(0i32)));

        let (rendered, args) = build(&Numbered, &c).unwrap();
        assert_frag_sql(
            FRAME,
            &rendered,
            r#"ON CONFLICT ("id") DO UPDATE SET "name" = EXCLUDED."name" WHERE ("users"."id" > $1)"#,
        );
        assert_eq!(args, vec![Value::I32(0)]);
    }

    #[test]
    fn do_update_without_assignments_is_a_recorded_failure() {
        // `DO UPDATE` with no SET does not parse, so it is refused rather than
        // written.
        assert_eq!(
            build(&Numbered, &ConflictClause::do_update())
                .unwrap_err()
                .to_string(),
            "query is missing the assignments of ON CONFLICT DO UPDATE"
        );
    }

    #[test]
    fn the_two_nested_wheres_are_independent() {
        let mut c = ConflictClause::do_update();
        c.set_mut().append_set(Expr::raw("a = 1"));
        c.target.where_mut().append_where("index_pred");
        c.where_mut().append_where("row_pred");
        c.target.columns = vec![quote("id")];

        // Not framed, for the reason given in
        // `a_partial_index_target_carries_the_indexs_own_predicate`: the index
        // predicate has no matching index in the shared schema. What is asserted
        // is that the two WHEREs land on opposite sides of the action and neither
        // borrows the other's conditions.
        assert_eq!(
            build(&Numbered, &c).unwrap().0,
            r#"ON CONFLICT ("id") WHERE index_pred DO UPDATE SET a = 1 WHERE row_pred"#
        );
    }

    #[test]
    fn the_slot_is_transparent_to_whatever_a_dialect_puts_in_it() {
        // MySQL's spelling has no ON CONFLICT and no SET, which is exactly why the
        // slot holds an expression rather than a ConflictClause. Not framed: the
        // psql judge would reject it, and rightly. MySQL's own crate checks it
        // against MySQL.
        let mut slot = Conflict::default();
        slot.set_conflict(Expr::raw("ON DUPLICATE KEY UPDATE `a` = 1"));
        assert_eq!(
            build(&Numbered, &slot).unwrap().0,
            "ON DUPLICATE KEY UPDATE `a` = 1"
        );

        let mut slot = Conflict::default();
        slot.set_conflict(Expr::custom(ConflictClause::do_nothing()));
        assert_frag_sql(FRAME, &sql(&slot), "ON CONFLICT DO NOTHING");
    }
}
