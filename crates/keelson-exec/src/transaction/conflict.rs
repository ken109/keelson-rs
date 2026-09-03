//! The conflicts worth retrying, and how a backend reports one.
//!
//! Every variant means the same thing to a caller — run the whole
//! transaction again — which is why the classification is a type rather than
//! message text, and why it reaches [`ExecError::Conflict`] as a variant a
//! `match` can see.

use std::fmt;

use crate::error::ExecError;

/// A concurrency conflict the engine raised: this transaction lost, and the
/// only correct response is to run the whole thing again.
///
/// The point of the type is that it is *matchable*. Serialization failures
/// arrive as engine-specific codes on engine-specific error types; without a
/// classification a caller ends up matching on message text, which is a bug
/// waiting for a locale or a version bump. A backend that reports one of
/// these constructs a [`TxConflictError`]; a caller asks [`TxConflict::of`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TxConflict {
    /// The engine could not serialize this transaction against a concurrent
    /// one — PostgreSQL `40001` (`could not serialize access …`).
    Serialization,
    /// A deadlock was detected and this transaction was picked as the victim
    /// — PostgreSQL `40P01`, MySQL `1213` (`ER_LOCK_DEADLOCK`, SQLSTATE
    /// `40001`: InnoDB reports its serialization failures this way).
    Deadlock,
    /// A lock wait timed out — PostgreSQL `55P03`, MySQL `1205`
    /// (`ER_LOCK_WAIT_TIMEOUT`).
    LockTimeout,
    /// SQLite could not take the lock it needed — `SQLITE_BUSY` /
    /// `SQLITE_LOCKED`, which is how SQLite says "serialize elsewhere".
    Busy,
}

impl TxConflict {
    /// A short stable name, for logs and assertions.
    pub fn as_str(self) -> &'static str {
        match self {
            TxConflict::Serialization => "serialization failure",
            TxConflict::Deadlock => "deadlock",
            TxConflict::LockTimeout => "lock timeout",
            TxConflict::Busy => "database busy",
        }
    }

    /// Classify a PostgreSQL `SQLSTATE`.
    ///
    /// `40001` serialization_failure, `40P01` deadlock_detected, `55P03`
    /// lock_not_available (what `lock_timeout` and `NOWAIT` raise). Anything
    /// else stays an opaque driver error.
    ///
    /// Here rather than in a backend because there is more than one
    /// PostgreSQL backend, and the two disagreeing about what counts as a
    /// conflict would mean the same workload retried on one and gave up on
    /// the other. Both used to carry this table, with a comment in each
    /// saying it matched the other one. The classification is made from what
    /// the *server* sent, so nothing driver-specific belongs in it.
    pub fn from_postgres_sqlstate(code: &str) -> Option<TxConflict> {
        match code {
            "40001" => Some(TxConflict::Serialization),
            "40P01" => Some(TxConflict::Deadlock),
            "55P03" => Some(TxConflict::LockTimeout),
            _ => None,
        }
    }

    /// Classify an error: `Some` when a backend reported a concurrency
    /// conflict, `None` otherwise. Every variant means the same thing to a
    /// caller — retry the transaction from the top.
    pub fn of(e: &ExecError) -> Option<TxConflict> {
        match e {
            ExecError::Conflict(c) => Some(c.kind),
            _ => None,
        }
    }
}

impl fmt::Display for TxConflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The error a backend reports for a [`TxConflict`], carrying the engine's own
/// code and message and (usually) the driver error as its `source`.
///
/// Backend-facing: construct one, then [`TxConflictError::into_exec_error`].
#[derive(Debug)]
pub struct TxConflictError {
    kind: TxConflict,
    code: String,
    message: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl TxConflictError {
    /// A conflict of `kind`, as the engine reported it (`code` is the
    /// engine's own: a SQLSTATE, an error number, a SQLite result code).
    pub fn new(kind: TxConflict, code: impl Into<String>, message: impl Into<String>) -> Self {
        TxConflictError {
            kind,
            code: code.into(),
            message: message.into(),
            source: None,
        }
    }

    /// Keep the driver error underneath, so nothing is lost by classifying.
    pub fn with_source(mut self, e: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(e));
        self
    }

    /// Which conflict this is.
    pub fn kind(&self) -> TxConflict {
        self.kind
    }

    /// The engine's own code.
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Wrap into the error the executor traits speak.
    ///
    /// [`ExecError::Conflict`], not a boxed driver error: a caller decides
    /// whether to retry by matching, and used to have to reach for
    /// `downcast_ref` to ask the one question this error exists to answer.
    pub fn into_exec_error(self) -> ExecError {
        ExecError::Conflict(self)
    }
}

impl fmt::Display for TxConflictError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} [{}]: {}", self.kind, self.code, self.message)
    }
}

impl std::error::Error for TxConflictError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|e| &**e as &(dyn std::error::Error + 'static))
    }
}
