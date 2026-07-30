//! The SQLite driver: [`Pool`] implements
//! [`Executor`](keelson_exec::Executor), [`Begin`](keelson_exec::Begin) and
//! [`StreamExecutor`](keelson_exec::StreamExecutor).
//!
//! SQLite has no native storage class for the mapped types, so every one of
//! them binds as its pinned text form from `docs/type-mappings.md`, rendered
//! here by hand — byte-identical to the forms `Value` serialises to, which is
//! what `FromValue`'s text acceptance reads back.

use std::sync::Arc;

use keelson_core::Value;
use keelson_exec::{
    Begin, Column, ExecError, ExecFuture, ExecResult, Executor, Family, RawConnection, Row,
    RowStream, Statement, StreamExecutor, Transaction,
};
use sqlx::sqlite::{SqliteArguments, SqliteRow};
use sqlx::{Column as _, Row as _, Sqlite, TypeInfo as _, ValueRef as _};

use crate::common::{decode_err, unhandled};

/// A SQLite connection pool.
#[derive(Debug, Clone)]
pub struct Pool {
    inner: sqlx::SqlitePool,
}

impl Pool {
    /// Connect to `url` (`sqlite://path/to.db`, or `sqlite::memory:`).
    ///
    /// The database file is created if missing — for the no-server engine
    /// that is almost always what a caller means.
    pub async fn connect(url: &str) -> Result<Self, ExecError> {
        use std::str::FromStr as _;
        let opts = sqlx::sqlite::SqliteConnectOptions::from_str(url)
            .map_err(ExecError::driver)?
            .create_if_missing(true);
        let inner = sqlx::sqlite::SqlitePoolOptions::new()
            .connect_with(opts)
            .await
            .map_err(ExecError::driver)?;
        Ok(Pool { inner })
    }

    /// Wrap an existing sqlx pool.
    pub fn from_pool(inner: sqlx::SqlitePool) -> Self {
        Pool { inner }
    }

    /// The wrapped sqlx pool.
    pub fn inner(&self) -> &sqlx::SqlitePool {
        &self.inner
    }
}

impl Executor for Pool {
    fn family(&self) -> Family {
        Family::Sqlite
    }

    fn fetch(&self, stmt: Statement) -> ExecFuture<'_, Result<Vec<Row>, ExecError>> {
        Box::pin(async move {
            let Statement { sql, args, .. } = stmt;
            do_fetch(&self.inner, &sql, args).await
        })
    }

    fn execute(&self, stmt: Statement) -> ExecFuture<'_, Result<ExecResult, ExecError>> {
        Box::pin(async move {
            let Statement { sql, args, .. } = stmt;
            do_execute(&self.inner, &sql, args).await
        })
    }
}

impl Begin for Pool {
    fn begin(&self) -> ExecFuture<'_, Result<Transaction, ExecError>> {
        Box::pin(async move {
            let conn = self.inner.acquire().await.map_err(ExecError::driver)?;
            Transaction::begin_on(Box::new(RawConn { conn })).await
        })
    }
}

impl StreamExecutor for Pool {
    fn fetch_stream(&self, stmt: Statement) -> ExecFuture<'_, Result<RowStream, ExecError>> {
        Box::pin(async move {
            let pool = self.inner.clone();
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<Row, ExecError>>(64);
            tokio::spawn(async move {
                use futures_util::StreamExt as _;
                let Statement { sql, args, .. } = stmt;
                let q = match bind_args(&sql, args) {
                    Ok(q) => q,
                    Err(e) => {
                        let _ = tx.send(Err(e)).await;
                        return;
                    }
                };
                let mut native = q.fetch(&pool);
                let mut header: Option<Arc<[Column]>> = None;
                while let Some(next) = native.next().await {
                    let msg = match next {
                        Ok(row) => decode_row(&row, &mut header),
                        Err(e) => Err(ExecError::driver(e)),
                    };
                    let stop = msg.is_err();
                    if tx.send(msg).await.is_err() || stop {
                        return;
                    }
                }
            });
            Ok(RowStream::new(rx))
        })
    }
}

/// One checked-out connection, exclusively held by a [`Transaction`].
#[derive(Debug)]
struct RawConn {
    conn: sqlx::pool::PoolConnection<Sqlite>,
}

impl RawConnection for RawConn {
    fn family(&self) -> Family {
        Family::Sqlite
    }

    fn fetch<'a>(
        &'a mut self,
        sql: &'a str,
        args: Vec<Value>,
    ) -> ExecFuture<'a, Result<Vec<Row>, ExecError>> {
        Box::pin(async move { do_fetch(&mut *self.conn, sql, args).await })
    }

    fn execute<'a>(
        &'a mut self,
        sql: &'a str,
        args: Vec<Value>,
    ) -> ExecFuture<'a, Result<ExecResult, ExecError>> {
        Box::pin(async move { do_execute(&mut *self.conn, sql, args).await })
    }

    fn abandon(self: Box<Self>) {
        let _ = self.conn.detach();
    }
}

async fn do_fetch<'e, E>(exec: E, sql: &str, args: Vec<Value>) -> Result<Vec<Row>, ExecError>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    let rows = bind_args(sql, args)?
        .fetch_all(exec)
        .await
        .map_err(ExecError::driver)?;
    let mut header: Option<Arc<[Column]>> = None;
    rows.iter().map(|r| decode_row(r, &mut header)).collect()
}

async fn do_execute<'e, E>(exec: E, sql: &str, args: Vec<Value>) -> Result<ExecResult, ExecError>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    // Zero-argument statements go over the driver's plain (unprepared) path:
    // MySQL refuses transaction control (`BEGIN`, `SAVEPOINT …`) in the
    // prepared-statement protocol, and nothing is gained by preparing an
    // argument-less statement anyway.
    let done = if args.is_empty() {
        exec.execute(sql).await.map_err(ExecError::driver)?
    } else {
        bind_args(sql, args)?
            .execute(exec)
            .await
            .map_err(ExecError::driver)?
    };
    let last = Some(done.last_insert_rowid()).filter(|id| *id != 0);
    Ok(ExecResult::new(done.rows_affected(), last))
}

/// The total map `Value` → SQLite parameter. Everything the engine has a
/// storage class for binds natively; every mapped type binds as its pinned
/// text form (the forms are the ones `Value` serialises to — see
/// `docs/type-mappings.md`).
fn bind_args<'q>(
    sql: &'q str,
    args: Vec<Value>,
) -> Result<sqlx::query::Query<'q, Sqlite, SqliteArguments<'q>>, ExecError> {
    let mut q = sqlx::query(sql);
    for v in args {
        q = match v {
            Value::Null => q.bind(Option::<String>::None),
            Value::Bool(x) => q.bind(x),
            Value::I8(x) => q.bind(i64::from(x)),
            Value::I16(x) => q.bind(i64::from(x)),
            Value::I32(x) => q.bind(i64::from(x)),
            Value::I64(x) => q.bind(x),
            Value::U8(x) => q.bind(i64::from(x)),
            Value::U16(x) => q.bind(i64::from(x)),
            Value::U32(x) => q.bind(i64::from(x)),
            Value::U64(x) => {
                q.bind(i64::try_from(x).map_err(|_| unsupported_value("u64 out of i64 range"))?)
            }
            // SQLite REAL is 8-byte; f32 widens losslessly.
            Value::F32(x) => q.bind(f64::from(x)),
            Value::F64(x) => q.bind(x),
            Value::Text(x) => q.bind(x),
            Value::Bytes(x) => q.bind(x),
            #[cfg(feature = "chrono")]
            Value::Date(x) => q.bind(x.format("%Y-%m-%d").to_string()),
            #[cfg(feature = "chrono")]
            Value::Time(x) => q.bind(x.format("%H:%M:%S%.f").to_string()),
            #[cfg(feature = "chrono")]
            Value::DateTime(x) => q.bind(x.format("%Y-%m-%dT%H:%M:%S%.f").to_string()),
            #[cfg(feature = "chrono")]
            Value::TimestampTz(x) => q.bind(x.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true)),
            #[cfg(feature = "uuid")]
            Value::Uuid(x) => q.bind(x.hyphenated().to_string()),
            // `Decimal::to_string` preserves scale — `1.10` stays `1.10`,
            // which TEXT storage round-trips exactly.
            #[cfg(feature = "decimal")]
            Value::Decimal(x) => q.bind(x.to_string()),
            #[cfg(feature = "json")]
            Value::Json(x) => q.bind(
                serde_json::to_string(&x).map_err(|_| unsupported_value("unserialisable json"))?,
            ),
            other => return Err(unsupported_value(other.type_name())),
        };
    }
    Ok(q)
}

fn unsupported_value(type_name: &'static str) -> ExecError {
    ExecError::UnsupportedValue {
        type_name,
        family: Family::Sqlite,
    }
}

fn decode_row(row: &SqliteRow, header: &mut Option<Arc<[Column]>>) -> Result<Row, ExecError> {
    let columns = header
        .get_or_insert_with(|| {
            row.columns()
                .iter()
                .map(|c| Column::new(c.name()))
                .collect::<Vec<_>>()
                .into()
        })
        .clone();
    let mut values = Vec::with_capacity(row.columns().len());
    for i in 0..row.columns().len() {
        values.push(decode_value(row, i)?);
    }
    Ok(Row::new(columns, values))
}

/// SQLite values decode by storage class — `INTEGER`/`REAL`/`TEXT`/`BLOB` —
/// with one nicety: a column *declared* `BOOLEAN` reads as a real `bool`
/// (core's `FromValue` for `bool` does not guess from integers, on purpose).
/// A mapped type stored as `TEXT` comes back as `Value::Text`, and
/// `FromValue`'s documented text acceptance turns it into the Rust type at
/// the edge — the round-trip suite is what keeps that contract honest.
fn decode_value(row: &SqliteRow, i: usize) -> Result<Value, ExecError> {
    let col = &row.columns()[i];
    let name = col.name();
    let raw = row.try_get_raw(i).map_err(|e| decode_err(name, e))?;
    if raw.is_null() {
        return Ok(Value::Null);
    }

    macro_rules! take {
        ($t:ty) => {
            row.try_get::<$t, _>(i).map_err(|e| decode_err(name, e))?
        };
    }

    // The declared type when the column has one (it names BOOLEAN and the
    // date-ish declarations); the value's own storage class otherwise
    // (expressions, RETURNING, aggregates).
    let decl = col.type_info();
    let decl = decl.name();
    let ty = if decl == "NULL" {
        let vt = raw.type_info();
        vt.name().to_owned()
    } else {
        decl.to_owned()
    };

    Ok(match ty.as_str() {
        "BOOLEAN" => Value::Bool(take!(bool)),
        "INTEGER" | "INT8" => Value::I64(take!(i64)),
        "REAL" => Value::F64(take!(f64)),
        // Declared temporal/decimal columns are TEXT under the mappings
        // table; SQLite's own extended declarations decode as text too and
        // resolve at the `FromValue` edge.
        "TEXT" | "DATE" | "TIME" | "DATETIME" | "NUMERIC" => Value::Text(take!(String)),
        "BLOB" => Value::Bytes(take!(Vec<u8>)),
        other => return Err(unhandled(name, other)),
    })
}
