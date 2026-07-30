use crate::error::Result;
use crate::writer::{DynExpr, Expression, SqlWriter};

pub const LOCK_STRENGTH_UPDATE: &str = "UPDATE";
pub const LOCK_STRENGTH_NO_KEY_UPDATE: &str = "NO KEY UPDATE";
pub const LOCK_STRENGTH_SHARE: &str = "SHARE";
pub const LOCK_STRENGTH_KEY_SHARE: &str = "KEY SHARE";

pub const LOCK_WAIT_NO_WAIT: &str = "NOWAIT";
pub const LOCK_WAIT_SKIP_LOCKED: &str = "SKIP LOCKED";

/// Every `FOR …` locking clause on one statement.
///
/// A statement may carry several, one per table, so they are newline-separated.
/// The clause writes no keyword of its own — each [`Lock`] starts with `FOR` —
/// so a query renders it as `write_if(!locks.is_empty(), "\n", &locks, "")`.
#[derive(Debug, Clone, Default)]
pub struct Locks {
    pub locks: Vec<DynExpr>,
}

impl Locks {
    pub fn append_lock(&mut self, lock: DynExpr) {
        self.locks.push(lock);
    }

    pub fn is_empty(&self) -> bool {
        self.locks.is_empty()
    }
}

impl Expression for Locks {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        w.write_slice(&self.locks, "", "\n", "")
    }
}

/// A query that can be locked.
pub trait HasLocks {
    fn locks_mut(&mut self) -> &mut Locks;
}

/// `FOR <strength> [OF <tables>] [NOWAIT | SKIP LOCKED]`
#[derive(Debug, Clone, Default)]
pub struct Lock {
    /// One of the `LOCK_STRENGTH_*` constants. Empty means no lock at all.
    pub strength: String,
    /// Table names, written verbatim.
    pub tables: Vec<String>,
    /// [`LOCK_WAIT_NO_WAIT`] or [`LOCK_WAIT_SKIP_LOCKED`].
    pub wait: String,
}

impl Expression for Lock {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        if self.strength.is_empty() {
            return Ok(());
        }

        w.push_str("FOR ");
        w.push_str(&self.strength);
        w.push_str(" ");

        w.write_slice(&self.tables, "OF ", ", ", "")?;

        if !self.wait.is_empty() {
            w.push_str(" ");
            w.push_str(&self.wait);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::writer::{build, dyn_expr};

    #[test]
    fn a_lock_without_a_strength_writes_nothing() {
        assert_eq!(build(&Numbered, &Lock::default()).unwrap().0, "");
    }

    #[test]
    fn strength_tables_and_wait_render_in_order() {
        let l = Lock {
            strength: LOCK_STRENGTH_UPDATE.into(),
            tables: vec!["users".into()],
            wait: LOCK_WAIT_SKIP_LOCKED.into(),
        };
        assert_eq!(
            build(&Numbered, &l).unwrap().0,
            "FOR UPDATE OF users SKIP LOCKED"
        );
    }

    #[test]
    fn without_tables_the_trailing_space_after_the_strength_remains() {
        // bob writes "FOR <strength> " unconditionally; the OF list is what is
        // optional. The fixture SQL contains that space.
        let l = Lock {
            strength: LOCK_STRENGTH_KEY_SHARE.into(),
            ..Lock::default()
        };
        assert_eq!(build(&Numbered, &l).unwrap().0, "FOR KEY SHARE ");
    }

    #[test]
    fn several_locks_are_newline_separated() {
        let mut locks = Locks::default();
        locks.append_lock(dyn_expr(Lock {
            strength: LOCK_STRENGTH_UPDATE.into(),
            tables: vec!["a".into()],
            ..Lock::default()
        }));
        locks.append_lock(dyn_expr(Lock {
            strength: LOCK_STRENGTH_SHARE.into(),
            tables: vec!["b".into()],
            ..Lock::default()
        }));
        assert_eq!(
            build(&Numbered, &locks).unwrap().0,
            "FOR UPDATE OF a\nFOR SHARE OF b"
        );
    }
}
