mod begin;
mod conflict;
mod options;

pub use begin::{Atomic, Begin, BeginExt, BeginWith, BeginWithExt, ExecHook, ExecLoader};
pub use conflict::{TxConflict, TxConflictError};
pub use options::{Access, Isolation, SqliteBegin, TxOptions};

use std::fmt;
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
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
    async fn unit_of_work(db: impl Atomic, fail: bool) -> Result<(), ExecError> {
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
        // The question the error exists to answer, asked by matching rather
        // than by downcasting a boxed driver error.
        let retry = matches!(
            e,
            ExecError::Conflict(ref c) if c.kind() == TxConflict::Serialization
        );
        assert!(retry, "a conflict must be reachable with a plain match");
        // The driver error it was classified out of is still underneath.
        assert!(matches!(e, ExecError::Conflict(ref c) if c.code() == "40001"));
    }

    /// Both PostgreSQL backends read this one table, so the codes it names
    /// are the definition of "retry" rather than something two crates keep in
    /// step by hand.
    #[test]
    fn the_postgres_sqlstates_that_mean_retry() {
        for (code, want) in [
            ("40001", TxConflict::Serialization),
            ("40P01", TxConflict::Deadlock),
            ("55P03", TxConflict::LockTimeout),
        ] {
            assert_eq!(
                TxConflict::from_postgres_sqlstate(code),
                Some(want),
                "{code}"
            );
        }
        // A unique violation is a bug in the workload, not contention: it
        // fails again on every retry.
        assert_eq!(TxConflict::from_postgres_sqlstate("23505"), None);
        assert_eq!(TxConflict::from_postgres_sqlstate(""), None);
    }

    #[test]
    fn every_way_of_holding_a_scope_is_atomic() {
        // Checked at compile time: this is what lets a unit of work say
        // `db: impl Atomic` and mean "a pool or a transaction, however you
        // happen to be holding it".
        fn takes(_: impl Atomic) {}
        fn prove(pool: Handle, shared: Arc<Handle>, erased: &dyn Begin, tx: &Transaction) {
            takes(&pool);
            takes(shared);
            takes(erased);
            // The one a scope closure hands you, and the reason `&Transaction`
            // has an impl of its own.
            takes(tx);
            takes(pool);
        }
        let _ = prove;
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
