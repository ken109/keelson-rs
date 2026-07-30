use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use keelson_core::{Query, QueryType, Value};

use crate::error::ExecError;
use crate::row::Row;

/// The boxed future every trait method returns.
///
/// Plain `std`, no futures-crate dependency. `'a` is the transient borrow of
/// the executor for the duration of one call — the same class of lifetime as
/// `SqlWriter<'_>`, and the only one this crate's public surface has.
pub type ExecFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Which engine family an executor talks to.
///
/// Metadata, not dispatch: it feeds observability (`db.system`) and the
/// round-trip harness. Backends are crates; nothing branches on this at run
/// time to change behaviour.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Family {
    /// PostgreSQL.
    Postgres,
    /// MySQL.
    MySql,
    /// SQLite.
    Sqlite,
}

impl Family {
    /// The OTel `db.system` value for this family.
    pub fn as_str(self) -> &'static str {
        match self {
            Family::Postgres => "postgresql",
            Family::MySql => "mysql",
            Family::Sqlite => "sqlite",
        }
    }
}

impl fmt::Display for Family {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What crosses the executor boundary: exactly what `build()` produces, plus
/// the statement-kind hint core already carries.
///
/// Owned, so an executor's future borrows nothing from the caller's query.
/// Constructing one by hand — [`Statement::new`] — is the raw-SQL escape
/// hatch, mirroring core's "the `build()` seam is always open": the SQL is
/// passed to the driver verbatim, in the backend's own placeholder syntax,
/// with the arguments bound per `docs/type-mappings.md`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Statement {
    /// The SQL text, placeholders included.
    pub sql: String,
    /// The arguments, one per placeholder.
    pub args: Vec<Value>,
    /// Which statement this is. Feeds tracing; never policed against the SQL.
    pub query_type: QueryType,
}

impl Statement {
    /// A raw statement. `sql` is sent to the driver verbatim.
    pub fn new(sql: impl Into<String>, args: Vec<Value>) -> Self {
        Statement {
            sql: sql.into(),
            args,
            query_type: QueryType::Unknown,
        }
    }

    /// Build a query into a statement. This is the only path the
    /// [`Execute`](crate::Execute) verbs use.
    pub fn from_query(q: &(impl Query + ?Sized)) -> Result<Self, ExecError> {
        let (sql, args) = q.build()?;
        Ok(Statement {
            sql,
            args,
            query_type: q.query_type(),
        })
    }
}

/// What a side-effect statement reports back.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExecResult {
    /// How many rows the statement changed.
    pub rows_affected: u64,
    /// The auto-increment id of the inserted row — MySQL and SQLite only.
    /// PostgreSQL answers `None`: use `RETURNING` there, which is the honest
    /// cross-engine story rather than a pretend-portable one.
    pub last_insert_id: Option<i64>,
}

impl ExecResult {
    /// Assemble a result. Backend-facing (the struct is `#[non_exhaustive]`).
    pub fn new(rows_affected: u64, last_insert_id: Option<i64>) -> Self {
        ExecResult {
            rows_affected,
            last_insert_id,
        }
    }
}

/// Anything that can run a built statement: a pool, a connection, a
/// [`Transaction`](crate::Transaction).
///
/// Object-safe on purpose — `&dyn Executor` is the currency application code,
/// generated models and hooks trade in — and `&self` on purpose: exclusivity
/// is the *executor's* problem (a pool checks out per call; a transaction
/// serialises behind a lock), not every call site's. The promise is
/// deliberately weak: each call runs on *some* connection. Only a connection
/// or a transaction strengthens that to *the same* connection, which is why
/// session state (`SET`, temp tables, advisory locks) through a bare pool is
/// a bug.
///
/// Backends implement these three methods and nothing else here; every
/// ergonomic path ([`Execute`](crate::Execute)) funnels into them. New
/// capabilities arrive as new opt-in traits ([`StreamExecutor`] is the
/// template), never as added methods — adding a method here breaks every
/// backend.
pub trait Executor: Send + Sync + fmt::Debug {
    /// Which engine family this executor talks to.
    fn family(&self) -> Family;

    /// Run a statement and collect every row — `SELECT`, or any mutation with
    /// `RETURNING`.
    fn fetch(&self, stmt: Statement) -> ExecFuture<'_, Result<Vec<Row>, ExecError>>;

    /// Run a statement for its side effect.
    fn execute(&self, stmt: Statement) -> ExecFuture<'_, Result<ExecResult, ExecError>>;
}

impl<E: Executor + ?Sized> Executor for &E {
    fn family(&self) -> Family {
        (**self).family()
    }

    fn fetch(&self, stmt: Statement) -> ExecFuture<'_, Result<Vec<Row>, ExecError>> {
        (**self).fetch(stmt)
    }

    fn execute(&self, stmt: Statement) -> ExecFuture<'_, Result<ExecResult, ExecError>> {
        (**self).execute(stmt)
    }
}

impl<E: Executor + ?Sized> Executor for Arc<E> {
    fn family(&self) -> Family {
        (**self).family()
    }

    fn fetch(&self, stmt: Statement) -> ExecFuture<'_, Result<Vec<Row>, ExecError>> {
        (**self).fetch(stmt)
    }

    fn execute(&self, stmt: Statement) -> ExecFuture<'_, Result<ExecResult, ExecError>> {
        (**self).execute(stmt)
    }
}

impl<E: Executor + ?Sized> Executor for Box<E> {
    fn family(&self) -> Family {
        (**self).family()
    }

    fn fetch(&self, stmt: Statement) -> ExecFuture<'_, Result<Vec<Row>, ExecError>> {
        (**self).fetch(stmt)
    }

    fn execute(&self, stmt: Statement) -> ExecFuture<'_, Result<ExecResult, ExecError>> {
        (**self).execute(stmt)
    }
}

/// Opt-in streaming. A backend that can stream implements it; nothing
/// requires it, because drivers differ too much here for it to belong in the
/// minimum contract (native streams borrow their connection; ours must not).
pub trait StreamExecutor: Executor {
    /// Run a statement and hand rows back incrementally.
    ///
    /// The returned [`RowStream`] is owned — dropping it cancels the producer
    /// and releases whatever connection it was riding.
    fn fetch_stream(&self, stmt: Statement) -> ExecFuture<'_, Result<RowStream, ExecError>>;
}

/// An owned stream of rows (house rule: no lifetime parameter).
///
/// A bounded channel fed by a producer the backend runs; dropping the stream
/// closes the channel, which the producer observes as its signal to stop and
/// release its connection. Deliberately a concrete struct rather than
/// `impl Stream`, so the futures crate stays out of the public API; a `Stream`
/// impl can be added behind a feature later without breaking anything.
pub struct RowStream {
    rx: tokio::sync::mpsc::Receiver<Result<Row, ExecError>>,
}

impl fmt::Debug for RowStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RowStream").finish_non_exhaustive()
    }
}

impl RowStream {
    /// Wrap a channel a backend feeds. Backend-facing.
    pub fn new(rx: tokio::sync::mpsc::Receiver<Result<Row, ExecError>>) -> Self {
        RowStream { rx }
    }

    /// The next row, or `None` when the result set is exhausted.
    pub async fn next(&mut self) -> Option<Result<Row, ExecError>> {
        self.rx.recv().await
    }
}
