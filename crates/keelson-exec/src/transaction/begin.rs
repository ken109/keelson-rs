//! The traits a caller reaches a transaction through.
//!
//! Four of them, in a ladder: [`Begin`] opens one, [`BeginWith`] opens one
//! with options, their `Ext` twins add the closure forms, and [`Atomic`] is
//! the one that does not care whether it is opening a transaction or taking a
//! savepoint. Split from [`mod`](super) because a caller reads this to find
//! out what it may ask for, and that module to find out what a transaction
//! then does.

use std::sync::Arc;

use crate::error::ExecError;
use crate::executor::{ExecFuture, Executor};
use crate::row::Row;
use crate::transaction::Transaction;
use crate::transaction::options::TxOptions;

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
///
/// # `within` or [`Atomic::atomic`]
///
/// On a pool they are the same call: `atomic` for a [`Begin`] *is* `within`.
/// What differs is what the receiver may be, which is to say what the code
/// claims:
///
/// - `within` takes a [`Begin`] and nothing else — **a transaction begins
///   here**. Handing it a [`Transaction`] does not compile.
/// - `atomic` takes either — *I do not care which of the two this is, I care
///   that it is atomic.*
///
/// The weaker claim is not the safer default. Anything whose correctness
/// depends on where the transaction *ends* needs `within`:
///
/// - **A retry loop.** Re-running a savepoint cannot clear a serialization
///   failure — the snapshot is unchanged, so the conflict recurs. A retry
///   written against `atomic` would spin when its caller happened to be
///   inside a transaction; written against `within`, that call does not
///   compile.
/// - **Isolation and access mode**, which are properties of the outermost
///   transaction and so are only reachable through [`BeginWith::begin_with`]
///   and [`BeginWithExt::within_with`].
///
/// Rule of thumb: `within` when the extent of the transaction is part of what
/// the code is saying, `atomic` for a reusable unit of work that only needs
/// its own block to be all-or-nothing.
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
/// async fn transfer(db: impl Atomic) -> …     // both
/// ```
///
/// The third accepts a pool, a connection, a `&dyn Begin` and a
/// [`Transaction`], and is atomic in all of them:
///
/// ```ignore
/// async fn transfer(db: impl Atomic, from: i64, to: i64) -> Result<(), ExecError> {
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
/// # How to spell the parameter
///
/// `db: impl Atomic`, by value and with no bound beyond the trait. Every
/// receiver you would want is accepted, because `Atomic` follows [`Executor`]
/// in being implemented for handles as well as values:
///
/// ```text
/// f(&pool)      f(pool)      f(Arc::clone(&pool))      f(&dyn Begin)      f(tx)
/// ```
///
/// The last is the `&Transaction` a scope closure hands you, which is what
/// makes these functions nest into each other. Writing `&impl Atomic` instead
/// would *reject* `&dyn Begin` (a trait object is not `Sized`), so the bare
/// form is the wider one as well as the shorter one.
///
/// # The cost, stated
///
/// The method is generic, so `Atomic` is **not object-safe**: `&dyn Atomic`
/// does not exist and the erased currency stays `&dyn Executor`, which a
/// scope parameter is passed on as for everything that is not itself a
/// scope.
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

/// A *reference* to a transaction — what makes the bare `impl Atomic`
/// parameter form work, since that is the type a scope closure hands you.
///
/// [`Executor`] is implemented for `&E` and `Arc<E>` for the same reason: a
/// caller should not have to know whether what it holds is the value or a
/// handle to it. Coherent with the blanket impl for the same reason
/// [`Transaction`]'s own impl is — `&Transaction` is not [`Begin`] either.
impl Atomic for &Transaction {
    fn atomic<T, E, F>(&self, f: F) -> impl std::future::Future<Output = Result<T, E>>
    where
        F: AsyncFnOnce(&Transaction) -> Result<T, E>,
        E: From<ExecError>,
    {
        (**self).savepoint(f)
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
