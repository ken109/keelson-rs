use std::future::Future;

use keelson_core::{FromValue, Query};

use crate::error::ExecError;
use crate::executor::{ExecResult, Executor, Statement};
use crate::row::{FromRow, Row};

/// The ergonomic verbs, hung on every [`Query`].
///
/// Blanket-implemented — `use keelson_exec::Execute` is the one import that
/// makes `q.fetch_all(&db)` compile, and `db` is anything that implements
/// [`Executor`]: a pool, a connection, a transaction, or a `&dyn Executor`.
/// The methods build the query synchronously (the query knows its own
/// dialect), so the returned future borrows only the executor.
///
/// This is also the funnel observability lives in (feature `tracing`): every
/// verb passes through one pair of functions, so no backend can ship
/// uninstrumented and no two backends can drift. Calling [`Executor::fetch`]
/// directly bypasses the sugar and the spans together; that path is the
/// escape hatch and is documented as such.
pub trait Execute: Query {
    /// Every row, mapped to `T`.
    fn fetch_all<T: FromRow>(
        &self,
        db: &(impl Executor + ?Sized),
    ) -> impl Future<Output = Result<Vec<T>, ExecError>> + Send {
        let stmt = Statement::from_query(self);
        async move {
            let rows = run_fetch(db, stmt?).await?;
            rows.into_iter().map(|mut r| T::from_row(&mut r)).collect()
        }
    }

    /// Exactly one row. Zero rows is [`ExecError::RowNotFound`]; a second row
    /// is [`ExecError::TooManyRows`] — "one" means one.
    fn fetch_one<T: FromRow>(
        &self,
        db: &(impl Executor + ?Sized),
    ) -> impl Future<Output = Result<T, ExecError>> + Send {
        let stmt = Statement::from_query(self);
        async move {
            let mut rows = run_fetch(db, stmt?).await?;
            match rows.len() {
                0 => Err(ExecError::RowNotFound),
                1 => T::from_row(&mut rows[0]),
                _ => Err(ExecError::TooManyRows),
            }
        }
    }

    /// At most one row. A second row is still [`ExecError::TooManyRows`].
    fn fetch_optional<T: FromRow>(
        &self,
        db: &(impl Executor + ?Sized),
    ) -> impl Future<Output = Result<Option<T>, ExecError>> + Send {
        let stmt = Statement::from_query(self);
        async move {
            let mut rows = run_fetch(db, stmt?).await?;
            match rows.len() {
                0 => Ok(None),
                1 => T::from_row(&mut rows[0]).map(Some),
                _ => Err(ExecError::TooManyRows),
            }
        }
    }

    /// The first column of the single row — `SELECT count(*)`, or an
    /// `INSERT … RETURNING id`.
    ///
    /// A separate verb rather than a blanket `FromRow for T: FromValue`,
    /// which would collide with a type implementing both; the verb is clearer
    /// at the call site anyway.
    fn fetch_scalar<T: FromValue>(
        &self,
        db: &(impl Executor + ?Sized),
    ) -> impl Future<Output = Result<T, ExecError>> + Send {
        let stmt = Statement::from_query(self);
        async move {
            let mut rows = run_fetch(db, stmt?).await?;
            match rows.len() {
                0 => Err(ExecError::RowNotFound),
                1 => rows[0].take_at(0),
                _ => Err(ExecError::TooManyRows),
            }
        }
    }

    /// The first column of every row.
    fn fetch_scalars<T: FromValue>(
        &self,
        db: &(impl Executor + ?Sized),
    ) -> impl Future<Output = Result<Vec<T>, ExecError>> + Send {
        let stmt = Statement::from_query(self);
        async move {
            let rows = run_fetch(db, stmt?).await?;
            rows.into_iter().map(|mut r| r.take_at(0)).collect()
        }
    }

    /// Run for the side effect.
    fn execute(
        &self,
        db: &(impl Executor + ?Sized),
    ) -> impl Future<Output = Result<ExecResult, ExecError>> + Send {
        let stmt = Statement::from_query(self);
        async move { run_execute(db, stmt?).await }
    }
}

impl<Q: Query + ?Sized> Execute for Q {}

/// The one place a row-returning statement passes on its way to a backend.
pub(crate) async fn run_fetch<E: Executor + ?Sized>(
    db: &E,
    stmt: Statement,
) -> Result<Vec<Row>, ExecError> {
    #[cfg(feature = "tracing")]
    {
        use tracing::Instrument as _;
        let span = query_span(db, &stmt);
        // Recorded on the handle, not on `Span::current()`: the latter needs
        // the subscriber to track span entry, which not every subscriber does.
        let res = db.fetch(stmt).instrument(span.clone()).await;
        match &res {
            Ok(rows) => span.record("keelson.rows", rows.len() as u64),
            Err(e) => span.record("error", tracing::field::display(e)),
        };
        res
    }
    #[cfg(not(feature = "tracing"))]
    {
        db.fetch(stmt).await
    }
}

/// The one place a side-effect statement passes on its way to a backend.
pub(crate) async fn run_execute<E: Executor + ?Sized>(
    db: &E,
    stmt: Statement,
) -> Result<ExecResult, ExecError> {
    #[cfg(feature = "tracing")]
    {
        use tracing::Instrument as _;
        let span = query_span(db, &stmt);
        let res = db.execute(stmt).instrument(span.clone()).await;
        match &res {
            Ok(done) => span.record("keelson.rows_affected", done.rows_affected),
            Err(e) => span.record("error", tracing::field::display(e)),
        };
        res
    }
    #[cfg(not(feature = "tracing"))]
    {
        db.execute(stmt).await
    }
}

/// The per-statement span. Field names follow the OTel database semconv so
/// existing dashboards light up.
///
/// `db.query.text` is the full SQL, untruncated — it is parameterized text and
/// safe by construction (placeholders, never values; the sole way user data
/// reaches SQL text is `expr::literal`, documented there), and a truncated
/// query is the one you cannot paste into `EXPLAIN`. The *arguments* are never
/// recorded, at any level, on any field: they are the PII channel. Only their
/// count is, so "did the IN-list explode" stays answerable. Pinned by test in
/// `tests/tracing.rs`.
#[cfg(feature = "tracing")]
fn query_span<E: Executor + ?Sized>(db: &E, stmt: &Statement) -> tracing::Span {
    tracing::info_span!(
        "keelson.query",
        db.system = db.family().as_str(),
        db.query.text = %stmt.sql,
        keelson.query_type = %stmt.query_type,
        keelson.args.count = stmt.args.len() as u64,
        keelson.rows = tracing::field::Empty,
        keelson.rows_affected = tracing::field::Empty,
        error = tracing::field::Empty,
    )
}
