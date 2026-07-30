use crate::error::Result;
use crate::writer::{DynExpr, Expression, SqlWriter};

use super::set::{HasSet, Set};
use super::where_::{HasWhere, Where};

pub const CONFLICT_DO_NOTHING: &str = "NOTHING";
pub const CONFLICT_DO_UPDATE: &str = "UPDATE";

/// The slot an insert statement keeps for its conflict handling.
///
/// It holds an erased expression rather than a [`ConflictClause`] because MySQL
/// spells the same idea `ON DUPLICATE KEY UPDATE` and SQLite adds `OR REPLACE`;
/// each dialect puts its own expression here.
#[derive(Debug, Clone, Default)]
pub struct Conflict {
    pub expression: Option<DynExpr>,
}

impl Conflict {
    pub fn set_conflict(&mut self, conflict: DynExpr) {
        self.expression = Some(conflict);
    }

    pub fn is_empty(&self) -> bool {
        self.expression.is_none()
    }
}

impl Expression for Conflict {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        if let Some(e) = &self.expression {
            w.write_expr(e)?;
        }
        Ok(())
    }
}

/// An insert statement with conflict handling.
pub trait HasConflict {
    fn conflict_mut(&mut self) -> &mut Conflict;
}

/// `ON CONFLICT <target> DO <NOTHING | UPDATE SET … WHERE …>`
///
/// Implements [`HasSet`] and [`HasWhere`], so the assignment and condition mods
/// written for statements work here too.
#[derive(Debug, Clone, Default)]
pub struct ConflictClause {
    /// [`CONFLICT_DO_NOTHING`] or [`CONFLICT_DO_UPDATE`].
    pub do_: String,
    pub target: ConflictTarget,
    pub set: Set,
    pub where_: Where,
}

impl Expression for ConflictClause {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        w.push_str("ON CONFLICT");
        w.write_expr(&self.target)?;

        w.push_str(" DO ");
        w.push_str(&self.do_);

        w.write_if(!self.set.is_empty(), " SET\n", &self.set, "")?;
        w.write_if(!self.where_.is_empty(), "\n", &self.where_, "")?;

        Ok(())
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

/// What the conflict is detected on: a named constraint, or an index inference
/// from a column list and an optional predicate.
///
/// A constraint name wins outright — PostgreSQL forbids combining the two.
#[derive(Debug, Clone, Default)]
pub struct ConflictTarget {
    pub constraint: String,
    pub columns: Vec<DynExpr>,
    pub where_: Vec<DynExpr>,
}

impl Expression for ConflictTarget {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        if !self.constraint.is_empty() {
            w.push_str(" ON CONSTRAINT ");
            w.push_str(&self.constraint);
            return Ok(());
        }

        w.write_slice(&self.columns, " (", ", ", ")")?;
        w.write_slice(&self.where_, " WHERE ", " AND ", "")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::writer::{build, dyn_expr};

    #[test]
    fn do_nothing_needs_no_target() {
        let c = ConflictClause {
            do_: CONFLICT_DO_NOTHING.into(),
            ..ConflictClause::default()
        };
        assert_eq!(build(&Numbered, &c).unwrap().0, "ON CONFLICT DO NOTHING");
    }

    #[test]
    fn a_column_target_precedes_the_action() {
        let c = ConflictClause {
            do_: CONFLICT_DO_UPDATE.into(),
            target: ConflictTarget {
                columns: vec![dyn_expr("did")],
                ..ConflictTarget::default()
            },
            ..ConflictClause::default()
        };
        assert_eq!(
            build(&Numbered, &c).unwrap().0,
            "ON CONFLICT (did) DO UPDATE"
        );
    }

    #[test]
    fn a_constraint_name_beats_the_column_list() {
        let t = ConflictTarget {
            constraint: "distributors_pkey".into(),
            columns: vec![dyn_expr("did")],
            where_: vec![dyn_expr("x")],
        };
        assert_eq!(
            build(&Numbered, &t).unwrap().0,
            " ON CONSTRAINT distributors_pkey"
        );
    }

    #[test]
    fn a_partial_index_target_carries_its_own_where() {
        let t = ConflictTarget {
            columns: vec![dyn_expr("did"), dyn_expr("dname")],
            where_: vec![dyn_expr("a"), dyn_expr("b")],
            ..ConflictTarget::default()
        };
        assert_eq!(
            build(&Numbered, &t).unwrap().0,
            " (did, dname) WHERE a AND b"
        );
    }

    #[test]
    fn set_and_where_hang_off_do_update() {
        let mut c = ConflictClause {
            do_: CONFLICT_DO_UPDATE.into(),
            target: ConflictTarget {
                columns: vec![dyn_expr("did")],
                ..ConflictTarget::default()
            },
            ..ConflictClause::default()
        };
        c.set_mut()
            .append_set([dyn_expr(r#""dname" = EXCLUDED."dname""#)]);
        c.where_mut()
            .append_where(dyn_expr(r#"("d"."zip" <> '1')"#));

        assert_eq!(
            build(&Numbered, &c).unwrap().0,
            "ON CONFLICT (did) DO UPDATE SET\n\"dname\" = EXCLUDED.\"dname\"\nWHERE (\"d\".\"zip\" <> '1')"
        );
    }

    #[test]
    fn the_conflict_slot_is_transparent() {
        let mut slot = Conflict::default();
        assert_eq!(build(&Numbered, &slot).unwrap().0, "");

        slot.set_conflict(dyn_expr("ON DUPLICATE KEY UPDATE\n`a` = 1"));
        assert_eq!(
            build(&Numbered, &slot).unwrap().0,
            "ON DUPLICATE KEY UPDATE\n`a` = 1"
        );
    }
}
