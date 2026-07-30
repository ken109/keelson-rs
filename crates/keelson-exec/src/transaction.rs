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
/// backend re-implements (or drifts on) transaction semantics. Isolation
/// levels and access modes are a later, additive `begin_with(opts)`.
pub struct Transaction {
    conn: Mutex<Option<Box<dyn RawConnection>>>,
    family: Family,
    finished: AtomicBool,
    depth: AtomicU32,
}

impl fmt::Debug for Transaction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Transaction")
            .field("family", &self.family)
            .field("finished", &self.finished.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl Transaction {
    /// Open a transaction on an exclusively-owned connection. Backend-facing:
    /// a backend's [`Begin`] impl checks a connection out and hands it here.
    pub async fn begin_on(mut conn: Box<dyn RawConnection>) -> Result<Self, ExecError> {
        let family = conn.family();
        if let Err(e) = conn.execute("BEGIN", Vec::new()).await {
            conn.abandon();
            return Err(e);
        }
        #[cfg(feature = "tracing")]
        tracing::debug!(target: "keelson", family = family.as_str(), "transaction begun");
        Ok(Transaction {
            conn: Mutex::new(Some(conn)),
            family,
            finished: AtomicBool::new(false),
            depth: AtomicU32::new(0),
        })
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
    #[derive(Debug, Default)]
    struct Script {
        log: Arc<StdMutex<Vec<String>>>,
        abandoned: Arc<StdMutex<bool>>,
    }

    impl RawConnection for Script {
        fn family(&self) -> Family {
            Family::Sqlite
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
            self.log.lock().unwrap().push(sql.to_owned());
            Box::pin(async { Ok(ExecResult::default()) })
        }

        fn abandon(self: Box<Self>) {
            *self.abandoned.lock().unwrap() = true;
        }
    }

    type Log = Arc<StdMutex<Vec<String>>>;
    type Abandoned = Arc<StdMutex<bool>>;

    fn script() -> (Script, Log, Abandoned) {
        let s = Script::default();
        (
            Script {
                log: s.log.clone(),
                abandoned: s.abandoned.clone(),
            },
            s.log,
            s.abandoned,
        )
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
