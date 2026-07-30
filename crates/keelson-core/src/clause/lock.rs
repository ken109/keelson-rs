use std::borrow::Cow;

use crate::writer::{Expression, SqlWriter};

use super::{MaybeAbsent, write_present, write_quoted_list};

/// Every `FOR …` locking clause on one statement.
///
/// A statement may carry several, one per table, so this is a list. It writes no
/// keyword of its own — each [`Lock`] starts with `FOR` — so a query renders it as
/// `w.write_if(!locks.is_empty(), " ", &locks, "")`.
#[derive(Debug, Clone, Default)]
pub struct Locks {
    /// The locking clauses, in order.
    pub locks: Vec<Lock>,
}

impl Locks {
    /// Append a locking clause.
    pub fn append_lock(&mut self, lock: Lock) {
        self.locks.push(lock);
    }

    /// Whether the clause is absent.
    pub fn is_empty(&self) -> bool {
        self.locks.is_empty()
    }
}

impl Expression for Locks {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        write_present(w, &self.locks, "", " ", "");
    }
}

/// A statement that can take a row lock.
pub trait HasLocks {
    /// The locking clauses to modify.
    fn locks_mut(&mut self) -> &mut Locks;
}

impl HasLocks for Locks {
    fn locks_mut(&mut self) -> &mut Locks {
        self
    }
}

/// `FOR <strength> [OF table, …] [NOWAIT | SKIP LOCKED]`
///
/// From PostgreSQL 17:
///
/// ```text
/// FOR { UPDATE | NO KEY UPDATE | SHARE | KEY SHARE }
///     [ OF table_name [, ...] ] [ NOWAIT | SKIP LOCKED ]
/// ```
///
/// Never contributes a bound argument: the `OF` list is table names.
#[derive(Debug, Clone, Default)]
pub struct Lock {
    /// How strong a lock. `None` is how a default-constructed lock stays absent —
    /// there is no `FOR` clause without a strength.
    pub strength: Option<LockStrength>,
    /// Restrict the lock to these tables of the statement. Quoted.
    pub tables: Vec<Cow<'static, str>>,
    /// What to do when a row is already locked. The default is to wait.
    pub wait: Option<LockWait>,
}

impl Lock {
    /// A lock of `strength` over every table in the statement.
    pub fn new(strength: LockStrength) -> Self {
        Lock {
            strength: Some(strength),
            ..Lock::default()
        }
    }

    /// Restrict the lock to these tables.
    pub fn append_table(&mut self, tables: impl IntoIterator<Item = impl Into<Cow<'static, str>>>) {
        self.tables.extend(tables.into_iter().map(Into::into));
    }

    /// Whether the clause is absent.
    pub fn is_empty(&self) -> bool {
        self.strength.is_none()
    }
}

impl Expression for Lock {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        let Some(strength) = &self.strength else {
            return;
        };

        w.push_str("FOR ");
        w.push_str(strength.as_str());

        write_quoted_list(w, &self.tables, " OF ", ", ", "");

        if let Some(wait) = &self.wait {
            w.push_str(" ");
            w.push_str(wait.as_str());
        }
    }
}

/// How strong a row lock is, weakest last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockStrength {
    /// `FOR UPDATE`.
    Update,
    /// `FOR NO KEY UPDATE` — PostgreSQL only; weaker than `UPDATE`, and does not
    /// block a foreign-key reference.
    NoKeyUpdate,
    /// `FOR SHARE`.
    Share,
    /// `FOR KEY SHARE` — PostgreSQL only; the weakest.
    KeyShare,
}

impl LockStrength {
    /// The keyword, as written.
    pub fn as_str(self) -> &'static str {
        match self {
            LockStrength::Update => "UPDATE",
            LockStrength::NoKeyUpdate => "NO KEY UPDATE",
            LockStrength::Share => "SHARE",
            LockStrength::KeyShare => "KEY SHARE",
        }
    }
}

/// What to do about a row someone else has already locked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockWait {
    /// `NOWAIT` — fail rather than wait.
    NoWait,
    /// `SKIP LOCKED` — leave the row out of the result.
    SkipLocked,
}

impl LockWait {
    /// The keyword, as written.
    pub fn as_str(self) -> &'static str {
        match self {
            LockWait::NoWait => "NOWAIT",
            LockWait::SkipLocked => "SKIP LOCKED",
        }
    }
}

impl MaybeAbsent for Lock {
    fn is_absent(&self) -> bool {
        self.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::writer::build;

    #[test]
    fn a_lock_without_a_strength_writes_nothing() {
        assert_eq!(build(&Numbered, &Lock::default()).unwrap().0, "");
        assert_eq!(build(&Numbered, &Locks::default()).unwrap().0, "");
        assert!(Lock::default().is_empty());
        assert!(Locks::default().is_empty());
    }

    #[test]
    fn a_bare_lock_is_just_for_and_the_strength() {
        // No trailing space: bob writes `FOR KEY SHARE ` because it pads before
        // the optional OF list rather than inside it.
        assert_eq!(
            build(&Numbered, &Lock::new(LockStrength::KeyShare))
                .unwrap()
                .0,
            "FOR KEY SHARE"
        );
    }

    #[test]
    fn strength_tables_and_wait_render_in_grammar_order() {
        // PostgreSQL 17: FOR strength [ OF table … ] [ NOWAIT | SKIP LOCKED ]
        let mut l = Lock::new(LockStrength::Update);
        l.append_table(["users", "posts"]);
        l.wait = Some(LockWait::SkipLocked);

        let (sql, args) = build(&Numbered, &l).unwrap();
        assert_eq!(sql, r#"FOR UPDATE OF "users", "posts" SKIP LOCKED"#);
        assert!(args.is_empty(), "table names are identifiers");
    }

    #[test]
    fn every_strength_and_wait_has_its_spelling() {
        for (strength, keyword) in [
            (LockStrength::Update, "FOR UPDATE"),
            (LockStrength::NoKeyUpdate, "FOR NO KEY UPDATE"),
            (LockStrength::Share, "FOR SHARE"),
            (LockStrength::KeyShare, "FOR KEY SHARE"),
        ] {
            assert_eq!(build(&Numbered, &Lock::new(strength)).unwrap().0, keyword);
        }

        let mut l = Lock::new(LockStrength::Share);
        l.wait = Some(LockWait::NoWait);
        assert_eq!(build(&Numbered, &l).unwrap().0, "FOR SHARE NOWAIT");
    }

    #[test]
    fn several_locks_are_space_separated() {
        let mut locks = Locks::default();
        let mut first = Lock::new(LockStrength::Update);
        first.append_table(["users"]);
        let mut second = Lock::new(LockStrength::Share);
        second.append_table(["posts"]);
        locks.append_lock(first);
        locks.append_lock(second);

        assert_eq!(
            build(&Numbered, &locks).unwrap().0,
            r#"FOR UPDATE OF "users" FOR SHARE OF "posts""#
        );
    }
}
