use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use keelson_core::Value;
use tokio::sync::Mutex;

use crate::error::ExecError;
use crate::executor::{ExecFuture, ExecResult, Executor, Family, Statement};
use crate::row::Row;

/// One raw connection, exclusively held. The seam a backend implements.
///
/// keelson-sqlx implements this once per driver; a future backend implements
/// it once. Everything else — [`Transaction`], the transaction SQL, the verb
/// layer — is written once in this crate against it, which is what keeps
/// transaction semantics identical across backends.
pub trait RawConnection: Send + fmt::Debug {
    /// Which engine family this connection talks to.
    fn family(&self) -> Family;

    /// Run a statement and collect every row.
    fn fetch<'a>(
        &'a mut self,
        sql: &'a str,
        args: Vec<Value>,
    ) -> ExecFuture<'a, Result<Vec<Row>, ExecError>>;

    /// Run a statement for its side effect.
    fn execute<'a>(
        &'a mut self,
        sql: &'a str,
        args: Vec<Value>,
    ) -> ExecFuture<'a, Result<ExecResult, ExecError>>;

    /// Dispose of a connection whose server-side state is unknown — an
    /// abandoned transaction. Must **not** return the connection to a pool as
    /// reusable: close it (the server then discards the open transaction).
    fn abandon(self: Box<Self>);
}

/// An open transaction. Owned, lifetime-free, and an [`Executor`] — any
/// function written as `fn f(db: &dyn Executor)` accepts it, which is what
/// lets model hooks run inside the caller's transaction without knowing one
/// exists.
///
/// `commit` and `rollback` consume `self`: using a finished transaction is a
/// compile error, not a runtime one. Dropping without either **abandons the
/// connection** — it is closed rather than returned to the pool, and the
/// server rolls the transaction back. The lazy path is therefore always
/// *safe* and merely expensive; `commit` is the only way to keep work. Prefer
/// [`BeginExt::within`], where neither a drop nor a forgotten commit can
/// happen at all.
///
/// This crate itself issues the transaction vocabulary — `BEGIN`, `COMMIT`,
/// `ROLLBACK`, `SAVEPOINT n` / `RELEASE SAVEPOINT n` / `ROLLBACK TO SAVEPOINT
/// n` — which is identical across PostgreSQL, MySQL and SQLite, so no
/// backend re-implements (or drifts on) transaction semantics. Where the
/// engines stop agreeing — isolation levels, access modes — the vocabulary
/// is still written here, once, but per family and with the disagreements
/// spelled out: see [`TxOptions`] and [`BeginWith`].
pub struct Transaction {
    conn: Mutex<Option<Box<dyn RawConnection>>>,
    family: Family,
    opts: TxOptions,
    finished: AtomicBool,
    depth: AtomicU32,
}

impl fmt::Debug for Transaction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Transaction")
            .field("family", &self.family)
            .field("options", &self.opts)
            .field("finished", &self.finished.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl Transaction {
    /// Open a transaction on an exclusively-owned connection. Backend-facing:
    /// a backend's [`Begin`] impl checks a connection out and hands it here.
    pub async fn begin_on(conn: Box<dyn RawConnection>) -> Result<Self, ExecError> {
        Transaction::begin_on_with(conn, TxOptions::new()).await
    }

    /// Open a transaction with explicit [`TxOptions`]. Backend-facing, the
    /// counterpart of [`BeginWith::begin_with`].
    ///
    /// The options are turned into statements by [`TxOptions::plan`] *before*
    /// anything is sent, so an option this engine cannot honour is refused
    /// with the connection untouched — it goes back to its pool clean rather
    /// than being abandoned. Every statement of the plan runs on this one
    /// connection, ahead of any statement the caller issues; if one of them
    /// fails the connection is abandoned rather than returned, because a
    /// half-applied plan (MySQL's `SET TRANSACTION` having landed without its
    /// `START TRANSACTION`) would otherwise leak into whatever transaction
    /// this pooled connection served next.
    pub async fn begin_on_with(
        mut conn: Box<dyn RawConnection>,
        opts: TxOptions,
    ) -> Result<Self, ExecError> {
        let family = conn.family();
        let plan = opts.plan(family)?;
        for sql in &plan {
            if let Err(e) = conn.execute(sql, Vec::new()).await {
                conn.abandon();
                return Err(e);
            }
        }
        #[cfg(feature = "tracing")]
        tracing::debug!(
            target: "keelson",
            family = family.as_str(),
            isolation = opts.get_isolation().map(Isolation::as_sql),
            "transaction begun"
        );
        Ok(Transaction {
            conn: Mutex::new(Some(conn)),
            family,
            opts,
            finished: AtomicBool::new(false),
            depth: AtomicU32::new(0),
        })
    }

    /// The options this transaction was opened with — [`TxOptions::new`]'s
    /// defaults for one begun by [`Begin::begin`].
    pub fn options(&self) -> TxOptions {
        self.opts
    }

    /// Commit. Consumes the transaction; on success the connection goes back
    /// to wherever it came from.
    pub async fn commit(self) -> Result<(), ExecError> {
        self.end("COMMIT").await
    }

    /// Roll back explicitly. Consumes the transaction; cheaper than dropping,
    /// because the connection is returned cleanly instead of closed.
    pub async fn rollback(self) -> Result<(), ExecError> {
        self.end("ROLLBACK").await
    }

    async fn end(&self, sql: &str) -> Result<(), ExecError> {
        self.finished.store(true, Ordering::Relaxed);
        let mut guard = self.conn.lock().await;
        let mut conn = guard
            .take()
            .ok_or_else(|| ExecError::other("transaction connection missing"))?;
        let res = conn.execute(sql, Vec::new()).await.map(|_| ());
        #[cfg(feature = "tracing")]
        tracing::debug!(
            target: "keelson",
            family = self.family.as_str(),
            outcome = if sql == "COMMIT" { "commit" } else { "rollback" },
            "transaction finished"
        );
        if res.is_err() {
            // The server-side state is unknown; the connection must not be
            // reused.
            conn.abandon();
        }
        res
    }

    /// A nested transaction, via `SAVEPOINT`.
    ///
    /// The savepoint has no handle to leak: `Ok(_)` releases it, `Err(_)`
    /// rolls back to it, and the outer transaction lives on either way. The
    /// closure receives this same transaction, so every `&dyn Executor`-taking
    /// helper works unchanged inside. Nesting is unbounded; depth-numbered
    /// names never collide.
    pub async fn savepoint<T, E, F>(&self, f: F) -> Result<T, E>
    where
        F: AsyncFnOnce(&Transaction) -> Result<T, E>,
        E: From<ExecError>,
    {
        let level = self.depth.fetch_add(1, Ordering::Relaxed) + 1;
        let name = format!("keelson_sp_{level}");
        if let Err(e) = self.raw(&format!("SAVEPOINT {name}")).await {
            self.depth.fetch_sub(1, Ordering::Relaxed);
            return Err(E::from(e));
        }
        let out = f(self).await;
        let cleanup = match &out {
            Ok(_) => self.raw(&format!("RELEASE SAVEPOINT {name}")).await,
            // ROLLBACK TO leaves the savepoint in place on every engine we
            // target, so it is released afterwards to keep names reusable.
            Err(_) => match self.raw(&format!("ROLLBACK TO SAVEPOINT {name}")).await {
                Ok(()) => self.raw(&format!("RELEASE SAVEPOINT {name}")).await,
                Err(e) => Err(e),
            },
        };
        self.depth.fetch_sub(1, Ordering::Relaxed);
        match (out, cleanup) {
            (Ok(v), Ok(())) => Ok(v),
            (Ok(_), Err(e)) => Err(E::from(e)),
            // The closure's own error wins; if cleanup also failed the
            // transaction is suspect and the eventual COMMIT will refuse.
            (Err(e), _) => Err(e),
        }
    }

    /// Run transaction-control SQL on the held connection.
    async fn raw(&self, sql: &str) -> Result<(), ExecError> {
        let mut guard = self.conn.lock().await;
        let conn = guard
            .as_mut()
            .ok_or_else(|| ExecError::other("transaction already finished"))?;
        conn.execute(sql, Vec::new()).await.map(|_| ())
    }
}

impl Executor for Transaction {
    fn family(&self) -> Family {
        self.family
    }

    fn fetch(&self, stmt: Statement) -> ExecFuture<'_, Result<Vec<Row>, ExecError>> {
        Box::pin(async move {
            let Statement { sql, args, .. } = stmt;
            let mut guard = self.conn.lock().await;
            let conn = guard
                .as_mut()
                .ok_or_else(|| ExecError::other("transaction already finished"))?;
            conn.fetch(&sql, args).await
        })
    }

    fn execute(&self, stmt: Statement) -> ExecFuture<'_, Result<ExecResult, ExecError>> {
        Box::pin(async move {
            let Statement { sql, args, .. } = stmt;
            let mut guard = self.conn.lock().await;
            let conn = guard
                .as_mut()
                .ok_or_else(|| ExecError::other("transaction already finished"))?;
            conn.execute(&sql, args).await
        })
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        if self.finished.load(Ordering::Relaxed) {
            return;
        }
        // No runtime is assumed here, so nothing async can run: the
        // connection is abandoned (closed), and the server rolls back. Safe,
        // merely expensive — which is the right way round.
        if let Ok(mut guard) = self.conn.try_lock()
            && let Some(conn) = guard.take()
        {
            conn.abandon();
            #[cfg(feature = "tracing")]
            tracing::debug!(
                target: "keelson",
                family = self.family.as_str(),
                outcome = "abandoned",
                "transaction dropped without commit; connection abandoned"
            );
        }
    }
}

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

    /// Classify an error: `Some` when a backend reported a concurrency
    /// conflict, `None` otherwise. Every variant means the same thing to a
    /// caller — retry the transaction from the top.
    pub fn of(e: &ExecError) -> Option<TxConflict> {
        match e {
            ExecError::Driver(d) => d.downcast_ref::<TxConflictError>().map(|c| c.kind),
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

    /// Wrap into the error the executor traits speak. This is the one
    /// construction [`TxConflict::of`] recognises.
    pub fn into_exec_error(self) -> ExecError {
        ExecError::Driver(Box::new(self))
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

/// Something a transaction can be begun on: a pool or a connection.
///
/// Deliberately **not** implemented by [`Transaction`] — nesting is spelled
/// [`Transaction::savepoint`], so "did I open a transaction or a savepoint?"
/// cannot be confused at a call site.
pub trait Begin: Executor {
    /// Open a transaction on a connection this executor owns or checks out.
    fn begin(&self) -> ExecFuture<'_, Result<Transaction, ExecError>>;
}

impl<B: Begin + ?Sized> Begin for &B {
    fn begin(&self) -> ExecFuture<'_, Result<Transaction, ExecError>> {
        (**self).begin()
    }
}

impl<B: Begin + ?Sized> Begin for Arc<B> {
    fn begin(&self) -> ExecFuture<'_, Result<Transaction, ExecError>> {
        (**self).begin()
    }
}

/// Opt-in transaction options: [`begin_with`](BeginWith::begin_with) beside
/// [`begin`](Begin::begin).
///
/// A separate trait rather than a second method on [`Begin`], following the
/// [`StreamExecutor`](crate::StreamExecutor) template: the core traits never
/// grow methods, so a backend that has no answer for isolation levels stays
/// compiling and stays honest by *not* implementing this.
///
/// ```ignore
/// let tx = db.begin_with(Isolation::Serializable.into()).await?;
/// let tx = db.begin_with(TxOptions::new().isolation(Isolation::RepeatableRead).read_only()).await?;
/// ```
///
/// Implementing it is three lines: refuse the options for this family
/// ([`TxOptions::check`]) *before* taking a connection, then hand the
/// connection to [`Transaction::begin_on_with`], which owns the SQL.
pub trait BeginWith: Begin {
    /// Open a transaction with explicit options.
    ///
    /// Options this engine cannot honour are an error — never a silent
    /// downgrade, never a no-op. [`TxOptions::plan`] is the table of what
    /// each engine accepts and why.
    fn begin_with(&self, opts: TxOptions) -> ExecFuture<'_, Result<Transaction, ExecError>>;
}

impl<B: BeginWith + ?Sized> BeginWith for &B {
    fn begin_with(&self, opts: TxOptions) -> ExecFuture<'_, Result<Transaction, ExecError>> {
        (**self).begin_with(opts)
    }
}

impl<B: BeginWith + ?Sized> BeginWith for Arc<B> {
    fn begin_with(&self, opts: TxOptions) -> ExecFuture<'_, Result<Transaction, ExecError>> {
        (**self).begin_with(opts)
    }
}

/// [`BeginExt::within`] with options.
pub trait BeginWithExt: BeginWith {
    /// Run `f` inside a fresh transaction opened with `opts`.
    fn within_with<T, E, F>(
        &self,
        opts: TxOptions,
        f: F,
    ) -> impl std::future::Future<Output = Result<T, E>>
    where
        F: AsyncFnOnce(&Transaction) -> Result<T, E>,
        E: From<ExecError>,
    {
        async move {
            let tx = self.begin_with(opts).await.map_err(E::from)?;
            match f(&tx).await {
                Ok(v) => {
                    tx.commit().await.map_err(E::from)?;
                    Ok(v)
                }
                Err(e) => {
                    let _ = tx.rollback().await;
                    Err(e)
                }
            }
        }
    }
}

impl<B: BeginWith + ?Sized> BeginWithExt for B {}

/// The closure form of a transaction: commit on `Ok`, roll back on `Err`.
///
/// The closure receives `&Transaction` and so *cannot* commit or consume it —
/// `within` owns the outcome. Neither a dropped transaction nor a forgotten
/// commit is expressible here, which is why this is the recommended shape.
pub trait BeginExt: Begin {
    /// Run `f` inside a fresh transaction.
    ///
    /// ```ignore
    /// let order = db.within(async |tx| {
    ///     let order: Order = insert_order.fetch_one(tx).await?;
    ///     reserve_stock(tx, &order).await?;
    ///     Ok(order)
    /// }).await?;
    /// ```
    fn within<T, E, F>(&self, f: F) -> impl std::future::Future<Output = Result<T, E>>
    where
        F: AsyncFnOnce(&Transaction) -> Result<T, E>,
        E: From<ExecError>,
    {
        async move {
            let tx = self.begin().await.map_err(E::from)?;
            match f(&tx).await {
                Ok(v) => {
                    tx.commit().await.map_err(E::from)?;
                    Ok(v)
                }
                Err(e) => {
                    // Best-effort: the closure's error is what the caller
                    // needs to see even if the rollback also failed.
                    let _ = tx.rollback().await;
                    Err(e)
                }
            }
        }
    }
}

impl<B: Begin + ?Sized> BeginExt for B {}

/// **All-or-nothing here, wherever "here" turns out to be**: a transaction
/// when nothing is open, a savepoint when a transaction already is.
///
/// This is the trait a *reusable* unit of work is written against. Without it
/// a helper has to pick, and both choices are wrong somewhere:
///
/// ```text
/// async fn transfer(db: &dyn Executor) -> …   // composes, but cannot be atomic
/// async fn transfer(tx: &Transaction) -> …    // atomic, but demands its caller open one
/// async fn transfer(db: &(impl Atomic + ?Sized)) -> …   // both
/// ```
///
/// The third accepts a pool, a connection, a `&dyn Begin` and a
/// [`Transaction`], and is atomic in all of them:
///
/// ```ignore
/// async fn transfer(db: &(impl Atomic + ?Sized), from: i64, to: i64) -> Result<(), ExecError> {
///     db.atomic(async |tx| {
///         debit(tx, from).await?;
///         credit(tx, to).await
///     })
///     .await
/// }
///
/// transfer(&pool, a, b).await?;                       // BEGIN … COMMIT
/// pool.within(async |tx| {                            // BEGIN …
///     transfer(tx, a, b).await?;                      //   SAVEPOINT … RELEASE
///     audit(tx).await                                 // … COMMIT
/// })
/// .await?;
/// ```
///
/// # What "atomic" promises, and what it does not
///
/// The *block* is all-or-nothing. What an `Err` discards is not:
///
/// - at the top, the block is the transaction, so an error rolls back
///   everything;
/// - nested, an error rolls back to the savepoint and **the caller's
///   transaction survives** — the caller decides whether its own work still
///   makes sense.
///
/// That is the useful reading of a nested unit of work, and it is why this is
/// not merely a shorthand for "open a transaction if you can".
///
/// A **retry loop belongs at the transaction boundary, not here.** Rolling
/// back to a savepoint does recover a transaction from an error, but a
/// serialization failure ([`TxConflict`]) will recur against the same
/// snapshot; only re-running the whole transaction can win. Retry where the
/// transaction begins.
///
/// # The cost, stated
///
/// The method is generic, so `Atomic` is **not object-safe**: `&dyn Atomic`
/// does not exist and the erased currency stays `&dyn Executor`. Take
/// `&(impl Atomic + ?Sized)` — the `?Sized` is what keeps `&dyn Begin`
/// acceptable — and pass it on as `&dyn Executor` for everything that is not
/// a scope.
///
/// [`Begin`] is still deliberately *not* implemented by [`Transaction`], so
/// `begin`/`within` and [`savepoint`](Transaction::savepoint) stay
/// distinguishable wherever the distinction matters. `atomic` is how a call
/// site says the other thing on purpose: *I do not care which of the two this
/// is; I care that it is atomic.*
///
/// There is no `atomic_with`. Isolation level and access mode are properties
/// of the outermost transaction and cannot be changed by a nested scope, so a
/// nested `atomic_with` could only ignore them — and keelson does not accept
/// options it would have to ignore. Ask for them where the transaction is
/// opened: [`BeginWith::begin_with`] or [`BeginWithExt::within_with`].
pub trait Atomic: Executor {
    /// Run `f` as one all-or-nothing block, nesting if the receiver is
    /// already a transaction.
    fn atomic<T, E, F>(&self, f: F) -> impl std::future::Future<Output = Result<T, E>>
    where
        F: AsyncFnOnce(&Transaction) -> Result<T, E>,
        E: From<ExecError>;
}

/// Anything a transaction can be begun on: `atomic` *is* [`BeginExt::within`].
impl<B: Begin + ?Sized> Atomic for B {
    fn atomic<T, E, F>(&self, f: F) -> impl std::future::Future<Output = Result<T, E>>
    where
        F: AsyncFnOnce(&Transaction) -> Result<T, E>,
        E: From<ExecError>,
    {
        self.within(f)
    }
}

/// Inside a transaction: `atomic` *is* [`Transaction::savepoint`].
///
/// Not an overlapping impl, and not by luck: [`Transaction`] does not
/// implement [`Begin`], which is the design decision above holding the two
/// impls apart.
impl Atomic for Transaction {
    fn atomic<T, E, F>(&self, f: F) -> impl std::future::Future<Output = Result<T, E>>
    where
        F: AsyncFnOnce(&Transaction) -> Result<T, E>,
        E: From<ExecError>,
    {
        self.savepoint(f)
    }
}

/// The hook payload the execution layer fixes
/// [`QueryExtensions`](keelson_core::QueryExtensions)' `Hook` parameter to:
/// an async function of `&dyn Executor`.
///
/// A hook receives exactly the executor the caller passed in — so it runs
/// inside the caller's transaction when there is one — and it receives it as
/// `&dyn Executor`, not `&Transaction`, so a hook *cannot* end a transaction
/// it did not open.
pub type ExecHook =
    Arc<dyn for<'a> Fn(&'a dyn Executor) -> ExecFuture<'a, Result<(), ExecError>> + Send + Sync>;

/// The loader payload: like [`ExecHook`], plus the rows the query produced.
pub type ExecLoader = Arc<
    dyn for<'a> Fn(&'a dyn Executor, &'a [Row]) -> ExecFuture<'a, Result<(), ExecError>>
        + Send
        + Sync,
>;

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use super::*;

    /// A RawConnection that records every statement and whether it was
    /// abandoned — transaction semantics are testable without a database.
    #[derive(Debug)]
    struct Script {
        family: Family,
        log: Arc<StdMutex<Vec<String>>>,
        abandoned: Arc<StdMutex<bool>>,
        fail_from: Option<usize>,
    }

    impl RawConnection for Script {
        fn family(&self) -> Family {
            self.family
        }

        fn fetch<'a>(
            &'a mut self,
            sql: &'a str,
            _args: Vec<Value>,
        ) -> ExecFuture<'a, Result<Vec<Row>, ExecError>> {
            self.log.lock().unwrap().push(sql.to_owned());
            Box::pin(async { Ok(Vec::new()) })
        }

        fn execute<'a>(
            &'a mut self,
            sql: &'a str,
            _args: Vec<Value>,
        ) -> ExecFuture<'a, Result<ExecResult, ExecError>> {
            let n = {
                let mut log = self.log.lock().unwrap();
                log.push(sql.to_owned());
                log.len()
            };
            let fails = self.fail_from.is_some_and(|from| n > from);
            Box::pin(async move {
                if fails {
                    Err(ExecError::other("statement refused"))
                } else {
                    Ok(ExecResult::default())
                }
            })
        }

        fn abandon(self: Box<Self>) {
            *self.abandoned.lock().unwrap() = true;
        }
    }

    type Log = Arc<StdMutex<Vec<String>>>;
    type Abandoned = Arc<StdMutex<bool>>;

    fn script_of(family: Family, fail_from: Option<usize>) -> (Script, Log, Abandoned) {
        let log: Log = Arc::default();
        let abandoned: Abandoned = Arc::default();
        (
            Script {
                family,
                log: log.clone(),
                abandoned: abandoned.clone(),
                fail_from,
            },
            log,
            abandoned,
        )
    }

    fn script() -> (Script, Log, Abandoned) {
        script_of(Family::Sqlite, None)
    }

    #[tokio::test]
    async fn commit_speaks_begin_then_commit_and_keeps_the_connection() {
        let (conn, log, abandoned) = script();
        let tx = Transaction::begin_on(Box::new(conn)).await.unwrap();
        tx.execute(Statement::new("INSERT 1", vec![]))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["BEGIN", "INSERT 1", "COMMIT"]);
        assert!(!*abandoned.lock().unwrap());
    }

    #[tokio::test]
    async fn drop_without_commit_abandons_the_connection() {
        let (conn, log, abandoned) = script();
        let tx = Transaction::begin_on(Box::new(conn)).await.unwrap();
        drop(tx);
        assert_eq!(*log.lock().unwrap(), vec!["BEGIN"]);
        assert!(
            *abandoned.lock().unwrap(),
            "the connection must not be reused"
        );
    }

    /// A pool that hands out its one scripted connection: enough to have a
    /// `Begin` on this side of the driver seam.
    #[derive(Debug)]
    struct Handle(StdMutex<Option<Box<dyn RawConnection>>>);

    impl Executor for Handle {
        fn family(&self) -> Family {
            Family::Sqlite
        }

        fn fetch(&self, _: Statement) -> ExecFuture<'_, Result<Vec<Row>, ExecError>> {
            Box::pin(async { Err(ExecError::other("not the point of this fixture")) })
        }

        fn execute(&self, _: Statement) -> ExecFuture<'_, Result<ExecResult, ExecError>> {
            Box::pin(async { Err(ExecError::other("not the point of this fixture")) })
        }
    }

    impl Begin for Handle {
        fn begin(&self) -> ExecFuture<'_, Result<Transaction, ExecError>> {
            let conn = self.0.lock().unwrap().take();
            Box::pin(async move {
                Transaction::begin_on(conn.ok_or_else(|| ExecError::other("checked out twice"))?)
                    .await
            })
        }
    }

    /// One helper, written once, atomic at both levels — the whole point of
    /// [`Atomic`].
    async fn unit_of_work(db: &(impl Atomic + ?Sized), fail: bool) -> Result<(), ExecError> {
        db.atomic(async |tx| {
            tx.execute(Statement::new("WORK", vec![])).await?;
            if fail {
                return Err(ExecError::other("no"));
            }
            Ok(())
        })
        .await
    }

    #[tokio::test]
    async fn one_helper_is_a_transaction_at_the_top_and_a_savepoint_inside_one() {
        let (conn, log, _) = script();
        let pool = Handle(StdMutex::new(Some(Box::new(conn))));
        unit_of_work(&pool, false).await.unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["BEGIN", "WORK", "COMMIT"]);

        let (conn, log, _) = script();
        let tx = Transaction::begin_on(Box::new(conn)).await.unwrap();
        unit_of_work(&tx, false).await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            *log.lock().unwrap(),
            vec![
                "BEGIN",
                "SAVEPOINT keelson_sp_1",
                "WORK",
                "RELEASE SAVEPOINT keelson_sp_1",
                "COMMIT",
            ]
        );
    }

    #[tokio::test]
    async fn a_nested_failure_costs_the_block_and_not_the_callers_transaction() {
        let (conn, log, _) = script();
        let tx = Transaction::begin_on(Box::new(conn)).await.unwrap();
        let err = unit_of_work(&tx, true).await.unwrap_err();
        assert_eq!(err.to_string(), "no");
        // The caller's transaction is still usable, and still commits.
        tx.execute(Statement::new("AFTER", vec![])).await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            *log.lock().unwrap(),
            vec![
                "BEGIN",
                "SAVEPOINT keelson_sp_1",
                "WORK",
                "ROLLBACK TO SAVEPOINT keelson_sp_1",
                "RELEASE SAVEPOINT keelson_sp_1",
                "AFTER",
                "COMMIT",
            ]
        );
    }

    #[tokio::test]
    async fn savepoints_release_on_ok_and_roll_back_on_err() {
        let (conn, log, _) = script();
        let tx = Transaction::begin_on(Box::new(conn)).await.unwrap();

        tx.savepoint(async |sp| {
            sp.execute(Statement::new("GOOD", vec![])).await?;
            Ok::<_, ExecError>(())
        })
        .await
        .unwrap();

        let err = tx
            .savepoint(async |sp| {
                sp.execute(Statement::new("BAD", vec![])).await?;
                Err::<(), _>(ExecError::other("boom"))
            })
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "boom");

        // Nested: the inner savepoint numbers deeper.
        tx.savepoint(async |sp| {
            sp.savepoint(async |sp2| {
                sp2.execute(Statement::new("DEEP", vec![])).await?;
                Ok::<_, ExecError>(())
            })
            .await
        })
        .await
        .unwrap();

        tx.commit().await.unwrap();
        assert_eq!(
            *log.lock().unwrap(),
            vec![
                "BEGIN",
                "SAVEPOINT keelson_sp_1",
                "GOOD",
                "RELEASE SAVEPOINT keelson_sp_1",
                "SAVEPOINT keelson_sp_1",
                "BAD",
                "ROLLBACK TO SAVEPOINT keelson_sp_1",
                "RELEASE SAVEPOINT keelson_sp_1",
                "SAVEPOINT keelson_sp_1",
                "SAVEPOINT keelson_sp_2",
                "DEEP",
                "RELEASE SAVEPOINT keelson_sp_2",
                "RELEASE SAVEPOINT keelson_sp_1",
                "COMMIT",
            ]
        );
    }

    // ---- transaction options -------------------------------------------
    //
    // The statement text is derived from each engine's own grammar:
    // PostgreSQL `BEGIN [ transaction_mode … ]`, MySQL `SET TRANSACTION
    // transaction_characteristic` + `START TRANSACTION [ transaction_
    // characteristic ]`, SQLite `BEGIN [ DEFERRED | IMMEDIATE | EXCLUSIVE ]`.
    // What these tests pin is the *shape and order*; that the engines accept
    // it, and that it changes their behaviour, is proved against real servers
    // in keelson-sqlx's tests/transactions.rs.

    #[test]
    fn default_options_send_exactly_what_a_plain_begin_sends() {
        for family in [Family::Postgres, Family::MySql, Family::Sqlite] {
            assert_eq!(TxOptions::new().plan(family).unwrap(), vec!["BEGIN"]);
        }
    }

    #[test]
    fn postgres_puts_the_modes_on_begin_itself() {
        assert_eq!(
            TxOptions::from(Isolation::Serializable)
                .plan(Family::Postgres)
                .unwrap(),
            vec!["BEGIN ISOLATION LEVEL SERIALIZABLE"]
        );
        assert_eq!(
            TxOptions::new()
                .isolation(Isolation::RepeatableRead)
                .read_only()
                .plan(Family::Postgres)
                .unwrap(),
            vec!["BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY"]
        );
    }

    #[test]
    fn mysql_sets_the_level_first_then_starts() {
        assert_eq!(
            TxOptions::from(Isolation::ReadCommitted)
                .plan(Family::MySql)
                .unwrap(),
            vec!["SET TRANSACTION ISOLATION LEVEL READ COMMITTED", "BEGIN"]
        );
        assert_eq!(
            TxOptions::new()
                .isolation(Isolation::Serializable)
                .read_only()
                .plan(Family::MySql)
                .unwrap(),
            vec![
                "SET TRANSACTION ISOLATION LEVEL SERIALIZABLE",
                "START TRANSACTION READ ONLY",
            ]
        );
    }

    #[test]
    fn sqlite_gets_begin_modes_and_nothing_pretending_to_be_a_level() {
        assert_eq!(
            TxOptions::from(SqliteBegin::Immediate)
                .plan(Family::Sqlite)
                .unwrap(),
            vec!["BEGIN IMMEDIATE"]
        );
        // Serializable is accepted because it is literally what SQLite runs.
        assert_eq!(
            TxOptions::from(Isolation::Serializable)
                .plan(Family::Sqlite)
                .unwrap(),
            vec!["BEGIN"]
        );
    }

    #[test]
    fn every_level_an_engine_would_only_pretend_to_honour_is_refused() {
        // PostgreSQL accepts READ UNCOMMITTED and runs READ COMMITTED.
        let e = TxOptions::from(Isolation::ReadUncommitted)
            .check(Family::Postgres)
            .unwrap_err()
            .to_string();
        assert!(e.contains("READ UNCOMMITTED"), "{e}");
        assert!(e.contains("READ COMMITTED"), "{e}");

        // SQLite cannot weaken below serializable.
        for level in [
            Isolation::ReadUncommitted,
            Isolation::ReadCommitted,
            Isolation::RepeatableRead,
        ] {
            let e = TxOptions::from(level)
                .check(Family::Sqlite)
                .unwrap_err()
                .to_string();
            assert!(e.contains(level.as_sql()), "{e}");
            assert!(e.contains("sqlite_begin"), "{e}");
        }

        // SQLite has no per-transaction read-only mode.
        let e = TxOptions::new()
            .read_only()
            .check(Family::Sqlite)
            .unwrap_err()
            .to_string();
        assert!(e.contains("query_only"), "{e}");

        // SQLite's begin modes are not portable vocabulary.
        for family in [Family::Postgres, Family::MySql] {
            let e = TxOptions::from(SqliteBegin::Exclusive)
                .check(family)
                .unwrap_err()
                .to_string();
            assert!(e.contains("Exclusive"), "{e}");
            assert!(e.contains(family.as_str()), "{e}");
        }

        // MySQL is the one engine that implements all four.
        for level in [
            Isolation::ReadUncommitted,
            Isolation::ReadCommitted,
            Isolation::RepeatableRead,
            Isolation::Serializable,
        ] {
            TxOptions::from(level).check(Family::MySql).unwrap();
        }
    }

    #[tokio::test]
    async fn a_refused_option_never_reaches_the_wire_and_keeps_the_connection() {
        let (conn, log, abandoned) = script_of(Family::Sqlite, None);
        let err = Transaction::begin_on_with(Box::new(conn), Isolation::ReadCommitted.into())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("one isolation level"));
        assert!(log.lock().unwrap().is_empty(), "nothing may be sent");
        assert!(
            !*abandoned.lock().unwrap(),
            "an untouched connection goes back to its pool"
        );
    }

    #[tokio::test]
    async fn the_whole_plan_runs_on_the_transactions_own_connection_before_any_statement() {
        let (conn, log, _) = script_of(Family::MySql, None);
        let tx = Transaction::begin_on_with(
            Box::new(conn),
            TxOptions::new()
                .isolation(Isolation::Serializable)
                .read_only(),
        )
        .await
        .unwrap();
        assert_eq!(
            tx.options(),
            TxOptions::new()
                .isolation(Isolation::Serializable)
                .access(Access::ReadOnly)
        );
        tx.execute(Statement::new("SELECT 1", vec![]))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        // One connection, one log: the level is set on it, ahead of the
        // caller's first statement, and inside the same checkout.
        assert_eq!(
            *log.lock().unwrap(),
            vec![
                "SET TRANSACTION ISOLATION LEVEL SERIALIZABLE",
                "START TRANSACTION READ ONLY",
                "SELECT 1",
                "COMMIT",
            ]
        );
    }

    #[tokio::test]
    async fn a_half_applied_plan_abandons_the_connection() {
        // MySQL's SET lands, START TRANSACTION fails: the pending
        // next-transaction characteristic would otherwise ride this
        // connection back into the pool.
        let (conn, log, abandoned) = script_of(Family::MySql, Some(1));
        let err = Transaction::begin_on_with(Box::new(conn), Isolation::Serializable.into())
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "statement refused");
        assert_eq!(
            *log.lock().unwrap(),
            vec!["SET TRANSACTION ISOLATION LEVEL SERIALIZABLE", "BEGIN"]
        );
        assert!(
            *abandoned.lock().unwrap(),
            "the connection carries state nobody can see; it must not be reused"
        );
    }

    #[test]
    fn conflicts_are_matchable_rather_than_stringly_typed() {
        let e = TxConflictError::new(
            TxConflict::Serialization,
            "40001",
            "could not serialize access due to concurrent update",
        )
        .with_source(ExecError::other("driver"))
        .into_exec_error();
        assert_eq!(TxConflict::of(&e), Some(TxConflict::Serialization));
        assert!(e.to_string().contains("40001"), "{e}");
        assert!(std::error::Error::source(&e).is_some());
        assert_eq!(TxConflict::of(&ExecError::RowNotFound), None);
    }

    #[test]
    fn a_transaction_is_a_dyn_executor() {
        // The design's center of gravity, checked at compile time: a hook
        // signature accepts a transaction as a plain &dyn Executor.
        fn takes(_: &dyn Executor) {}
        fn prove(tx: &Transaction) {
            takes(tx);
        }
        let _ = prove;
    }
}
