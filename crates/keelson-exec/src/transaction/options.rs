//! What a transaction is *asked for*, and what each engine will actually run.
//!
//! Split from [`mod`](super) because it answers a different question: that
//! module is a transaction's lifetime, this one is the per-engine table of
//! what an isolation level, an access mode or a SQLite begin-mode means —
//! and, more often, which of them an engine refuses. keelson accepts a level
//! on an engine only when that engine runs the transaction at it; it never
//! substitutes a neighbouring one and calls it success.

use std::fmt;

use crate::error::ExecError;
use crate::executor::Family;

/// A SQL-standard isolation level, as *asked for*.
///
/// keelson accepts a level on an engine only when that engine actually runs
/// the transaction at it. It never substitutes a neighbouring level and calls
/// it success — see [`TxOptions::plan`] for the per-engine table and the
/// refusals.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Isolation {
    /// Dirty reads permitted. **MySQL only** here: PostgreSQL accepts the
    /// syntax and runs READ COMMITTED, SQLite has nothing weaker than
    /// serializable, and both of those are refused rather than faked.
    ReadUncommitted,
    /// Each statement sees rows committed before *it* began. PostgreSQL's
    /// default; MySQL supports it; SQLite cannot offer it.
    ReadCommitted,
    /// A transaction-wide snapshot. MySQL/InnoDB's default. **Same name, two
    /// semantics**: PostgreSQL raises a serialization failure when a
    /// transaction writes a row changed since its snapshot, while InnoDB's
    /// consistent read silently coexists with locking reads and `UPDATE`
    /// taking the *current* row — the classic lost-update shape.
    RepeatableRead,
    /// PostgreSQL: true serializability (predicate locks, `40001` on
    /// conflict). MySQL: `REPEATABLE READ` with every plain `SELECT` promoted
    /// to a locking read. SQLite: what you always get.
    Serializable,
}

impl Isolation {
    /// The SQL spelling, per each engine's `SET TRANSACTION` grammar (they
    /// agree on the four names).
    pub fn as_sql(self) -> &'static str {
        match self {
            Isolation::ReadUncommitted => "READ UNCOMMITTED",
            Isolation::ReadCommitted => "READ COMMITTED",
            Isolation::RepeatableRead => "REPEATABLE READ",
            Isolation::Serializable => "SERIALIZABLE",
        }
    }
}

impl fmt::Display for Isolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_sql())
    }
}

/// A transaction's access mode.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Access {
    /// The default everywhere; stating it is allowed and explicit.
    ReadWrite,
    /// PostgreSQL `BEGIN … READ ONLY`, MySQL `START TRANSACTION READ ONLY`.
    /// Refused on SQLite, which has no per-transaction read-only mode.
    ReadOnly,
}

impl Access {
    /// The SQL spelling (identical on PostgreSQL and MySQL).
    pub fn as_sql(self) -> &'static str {
        match self {
            Access::ReadWrite => "READ WRITE",
            Access::ReadOnly => "READ ONLY",
        }
    }
}

/// SQLite's begin modes — **not** isolation levels, and named so nobody can
/// mistake them for a portable knob.
///
/// SQLite has one isolation level (serializable); what it lets you choose is
/// *when* the transaction takes its locks. Asking for one of these on
/// PostgreSQL or MySQL is an error, not a no-op.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SqliteBegin {
    /// Locks are taken at the first statement that needs them. The default.
    Deferred,
    /// The write lock is taken at `BEGIN`, so a write-write conflict surfaces
    /// as [`TxConflict::Busy`] immediately instead of mid-transaction.
    Immediate,
    /// As `IMMEDIATE`, and other connections cannot read either (outside WAL).
    Exclusive,
}

impl SqliteBegin {
    /// The keyword this mode contributes to `BEGIN`.
    pub fn as_sql(self) -> &'static str {
        match self {
            SqliteBegin::Deferred => "DEFERRED",
            SqliteBegin::Immediate => "IMMEDIATE",
            SqliteBegin::Exclusive => "EXCLUSIVE",
        }
    }
}

/// What a transaction is opened *with*: the parts of transaction control the
/// three engines do not agree on.
///
/// Defaults to "whatever the engine's own default is" on every axis, so
/// [`Transaction::begin_on_with`] with a default `TxOptions` sends exactly
/// what [`Transaction::begin_on`] sends.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct TxOptions {
    isolation: Option<Isolation>,
    access: Option<Access>,
    sqlite_begin: Option<SqliteBegin>,
}

impl TxOptions {
    /// No option set: the engine's own defaults.
    pub const fn new() -> Self {
        TxOptions {
            isolation: None,
            access: None,
            sqlite_begin: None,
        }
    }

    /// Ask for an isolation level.
    pub const fn isolation(mut self, level: Isolation) -> Self {
        self.isolation = Some(level);
        self
    }

    /// Ask for an access mode.
    pub const fn access(mut self, mode: Access) -> Self {
        self.access = Some(mode);
        self
    }

    /// Shorthand for [`Access::ReadOnly`].
    pub const fn read_only(self) -> Self {
        self.access(Access::ReadOnly)
    }

    /// SQLite's begin mode. An error on any other family.
    pub const fn sqlite_begin(mut self, mode: SqliteBegin) -> Self {
        self.sqlite_begin = Some(mode);
        self
    }

    /// The isolation level asked for, if any.
    pub const fn get_isolation(self) -> Option<Isolation> {
        self.isolation
    }

    /// The access mode asked for, if any.
    pub const fn get_access(self) -> Option<Access> {
        self.access
    }

    /// The SQLite begin mode asked for, if any.
    pub const fn get_sqlite_begin(self) -> Option<SqliteBegin> {
        self.sqlite_begin
    }

    /// Can this engine honour these options? Answers without opening a
    /// transaction — backends call it before checking a connection out, and
    /// callers can pre-flight a configuration with it.
    pub fn check(&self, family: Family) -> Result<(), ExecError> {
        self.plan(family).map(|_| ())
    }

    /// The exact statements [`Transaction::begin_on_with`] will run, in
    /// order, on the transaction's own connection — or the error explaining
    /// why this engine cannot be asked for this.
    ///
    /// Nothing here is inferred at run time from a server version; it is the
    /// documented grammar of each engine, and it is public so that "what does
    /// keelson actually send?" is answerable without a packet capture.
    ///
    /// | | PostgreSQL | MySQL / InnoDB | SQLite |
    /// |---|---|---|---|
    /// | READ UNCOMMITTED | **refused** (accepted by the server, run as READ COMMITTED) | yes | **refused** |
    /// | READ COMMITTED | yes (default) | yes | **refused** |
    /// | REPEATABLE READ | yes | yes (default) | **refused** |
    /// | SERIALIZABLE | yes | yes | yes — SQLite's only level |
    /// | READ ONLY | yes | yes | **refused** (`PRAGMA query_only` is connection state) |
    /// | [`SqliteBegin`] | **refused** | **refused** | yes |
    ///
    /// The rule behind every refusal: a level is accepted only when the
    /// engine runs the transaction at *that* level. Substituting a
    /// neighbouring level would satisfy the SQL standard (it permits running
    /// stricter than asked) and would still be a lie to the caller, who asked
    /// in order to get particular behaviour.
    ///
    /// The match on [`Family`] is exhaustive on purpose: a new family cannot
    /// be added to this crate without deciding what it does here.
    pub fn plan(&self, family: Family) -> Result<Vec<String>, ExecError> {
        match family {
            Family::Postgres => self.plan_postgres(),
            Family::MySql => self.plan_mysql(),
            Family::Sqlite => self.plan_sqlite(),
        }
    }

    /// PostgreSQL puts everything in the `BEGIN` itself:
    /// `BEGIN [ ISOLATION LEVEL … ] [ READ WRITE | READ ONLY ]`.
    fn plan_postgres(&self) -> Result<Vec<String>, ExecError> {
        self.reject_sqlite_begin(Family::Postgres)?;
        let mut sql = String::from("BEGIN");
        if let Some(level) = self.isolation {
            if level == Isolation::ReadUncommitted {
                return Err(ExecError::other(
                    "PostgreSQL accepts READ UNCOMMITTED and then runs the transaction as \
                     READ COMMITTED — it has no weaker level. keelson refuses the request \
                     rather than hand back an isolation level the engine does not \
                     implement; ask for Isolation::ReadCommitted if that is the behaviour \
                     you want.",
                ));
            }
            sql.push_str(" ISOLATION LEVEL ");
            sql.push_str(level.as_sql());
        }
        if let Some(mode) = self.access {
            sql.push(' ');
            sql.push_str(mode.as_sql());
        }
        Ok(vec![sql])
    }

    /// MySQL cannot carry an isolation level on `START TRANSACTION`, and
    /// refuses `SET TRANSACTION` once a transaction is open
    /// (`ER_CANT_CHANGE_TX_CHARACTERISTICS`). Unqualified — no `SESSION`, no
    /// `GLOBAL` — the `SET` applies to the *next* transaction on this
    /// connection and to nothing else, which is exactly the scope wanted: it
    /// expires with the transaction instead of riding the connection back
    /// into the pool.
    fn plan_mysql(&self) -> Result<Vec<String>, ExecError> {
        self.reject_sqlite_begin(Family::MySql)?;
        let mut out = Vec::with_capacity(2);
        if let Some(level) = self.isolation {
            out.push(format!(
                "SET TRANSACTION ISOLATION LEVEL {}",
                level.as_sql()
            ));
        }
        out.push(match self.access {
            // `BEGIN` when nothing is asked, so the default path is
            // byte-identical to `Transaction::begin_on`'s.
            None => "BEGIN".to_owned(),
            Some(mode) => format!("START TRANSACTION {}", mode.as_sql()),
        });
        Ok(out)
    }

    /// SQLite: `BEGIN [ DEFERRED | IMMEDIATE | EXCLUSIVE ]`, and no standard
    /// levels at all.
    fn plan_sqlite(&self) -> Result<Vec<String>, ExecError> {
        if let Some(level) = self.isolation
            && level != Isolation::Serializable
        {
            return Err(ExecError::other(format!(
                "SQLite has exactly one isolation level — serializable — and cannot be \
                 weakened to {level}: a transaction asking for it would silently run \
                 serializable, which is a different set of permitted anomalies from the \
                 one you asked for. Ask for Isolation::Serializable (SQLite's only level, \
                 and what a plain BEGIN already gives), or use TxOptions::sqlite_begin for \
                 SQLite's own DEFERRED / IMMEDIATE / EXCLUSIVE begin modes."
            )));
        }
        if self.access == Some(Access::ReadOnly) {
            return Err(ExecError::other(
                "SQLite has no per-transaction read-only mode. `PRAGMA query_only` is \
                 connection-level state, and keelson will not set connection state behind \
                 a pooled connection's back; open the database read-only instead \
                 (`sqlite://file?mode=ro`).",
            ));
        }
        Ok(vec![match self.sqlite_begin {
            None => "BEGIN".to_owned(),
            Some(mode) => format!("BEGIN {}", mode.as_sql()),
        }])
    }

    fn reject_sqlite_begin(&self, family: Family) -> Result<(), ExecError> {
        match self.sqlite_begin {
            None => Ok(()),
            Some(mode) => Err(ExecError::other(format!(
                "SqliteBegin::{mode:?} is SQLite's own begin-mode vocabulary and has no \
                 meaning on {family}; it is refused rather than ignored. Use \
                 TxOptions::isolation and TxOptions::access there."
            ))),
        }
    }
}

impl From<Isolation> for TxOptions {
    fn from(level: Isolation) -> Self {
        TxOptions::new().isolation(level)
    }
}

impl From<Access> for TxOptions {
    fn from(mode: Access) -> Self {
        TxOptions::new().access(mode)
    }
}

impl From<SqliteBegin> for TxOptions {
    fn from(mode: SqliteBegin) -> Self {
        TxOptions::new().sqlite_begin(mode)
    }
}
