//! The MySQL driver: [`Pool`] implements
//! [`Executor`], [`Begin`], [`BeginWith`] and [`StreamExecutor`].
//!
//! [`Pool::connect`] pins `time_zone = '+00:00'` on **every** connection it
//! establishes — the `docs/type-mappings.md` requirement that makes
//! `TIMESTAMP` an instant rather than a session-relative reading. No
//! user-visible surface depends on remembering to do this.

use std::sync::Arc;

use keelson_core::Value;
use keelson_exec::{
    Begin, BeginWith, Column, ExecError, ExecFuture, ExecResult, Executor, Family, RawConnection,
    Row, RowStream, Statement, StreamExecutor, Transaction, TxConflict, TxConflictError, TxOptions,
};
use sqlx::mysql::{MySqlArguments, MySqlRow};
use sqlx::{Column as _, MySql, Row as _, TypeInfo as _, ValueRef as _};

use crate::common::{decode_err, unhandled};

/// A MySQL connection pool, with the session zone pinned to UTC on every
/// connection.
#[derive(Debug, Clone)]
pub struct Pool {
    inner: sqlx::MySqlPool,
}

impl Pool {
    /// Connect to `url` (`mysql://user:pass@host:port/db`).
    pub async fn connect(url: &str) -> Result<Self, ExecError> {
        let inner = sqlx::mysql::MySqlPoolOptions::new()
            .after_connect(|conn, _meta| {
                Box::pin(async move {
                    // The type-mappings session-zone requirement, applied at
                    // the only place it cannot be forgotten.
                    sqlx::Executor::execute(&mut *conn, "SET time_zone = '+00:00'").await?;
                    Ok(())
                })
            })
            .connect(url)
            .await
            .map_err(ExecError::driver)?;
        Ok(Pool { inner })
    }

    /// Wrap an existing sqlx pool.
    ///
    /// The caller then owns the session-zone pin: connections this pool
    /// establishes are **not** set to `time_zone = '+00:00'` unless its own
    /// `after_connect` does so, and `TIMESTAMP` round-trips are wrong without
    /// it. Prefer [`Pool::connect`].
    pub fn from_pool(inner: sqlx::MySqlPool) -> Self {
        Pool { inner }
    }

    /// The wrapped sqlx pool.
    pub fn inner(&self) -> &sqlx::MySqlPool {
        &self.inner
    }
}

impl Executor for Pool {
    fn family(&self) -> Family {
        Family::MySql
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

impl BeginWith for Pool {
    fn begin_with(&self, opts: TxOptions) -> ExecFuture<'_, Result<Transaction, ExecError>> {
        Box::pin(async move {
            // Refuse before taking a connection out of the pool.
            opts.check(Family::MySql)?;
            let conn = self.inner.acquire().await.map_err(ExecError::driver)?;
            Transaction::begin_on_with(Box::new(RawConn { conn }), opts).await
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
    conn: sqlx::pool::PoolConnection<MySql>,
}

impl RawConnection for RawConn {
    fn family(&self) -> Family {
        Family::MySql
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

/// A driver failure, with concurrency conflicts classified out of it.
///
/// MySQL uses SQLSTATE as a coarse category and the error *number* as the
/// precise one, so the number is what is matched: `1213` `ER_LOCK_DEADLOCK`
/// (SQLSTATE `40001` — a deadlock is how InnoDB reports a serialization
/// failure) and `1205` `ER_LOCK_WAIT_TIMEOUT`. Everything else stays an
/// opaque driver error.
fn driver_err(e: sqlx::Error) -> ExecError {
    if let sqlx::Error::Database(db) = &e
        && let Some(my) = db.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>()
    {
        let kind = match my.number() {
            1213 => Some(TxConflict::Deadlock),
            1205 => Some(TxConflict::LockTimeout),
            _ => None,
        };
        if let Some(kind) = kind {
            let code = my.number().to_string();
            let message = my.message().to_owned();
            return TxConflictError::new(kind, code, message)
                .with_source(e)
                .into_exec_error();
        }
    }
    ExecError::driver(e)
}

async fn do_fetch<'e, E>(exec: E, sql: &str, args: Vec<Value>) -> Result<Vec<Row>, ExecError>
where
    E: sqlx::Executor<'e, Database = MySql>,
{
    let rows = bind_args(sql, args)?
        .fetch_all(exec)
        .await
        .map_err(driver_err)?;
    let mut header: Option<Arc<[Column]>> = None;
    rows.iter().map(|r| decode_row(r, &mut header)).collect()
}

async fn do_execute<'e, E>(exec: E, sql: &str, args: Vec<Value>) -> Result<ExecResult, ExecError>
where
    E: sqlx::Executor<'e, Database = MySql>,
{
    // Zero-argument statements go over the driver's plain (unprepared) path:
    // MySQL refuses transaction control (`BEGIN`, `SAVEPOINT …`) in the
    // prepared-statement protocol, and nothing is gained by preparing an
    // argument-less statement anyway.
    let done = if args.is_empty() {
        exec.execute(sql).await.map_err(driver_err)?
    } else {
        bind_args(sql, args)?
            .execute(exec)
            .await
            .map_err(driver_err)?
    };
    let last = i64::try_from(done.last_insert_id())
        .ok()
        .filter(|id| *id != 0);
    Ok(ExecResult::new(done.rows_affected(), last))
}

/// The total map `Value` → MySQL parameter, per the "binds as" column of
/// `docs/type-mappings.md`. Notably: `Uuid` binds as hyphenated lowercase
/// text (the `CHAR(36)` mapping), and `TimestampTz` relies on the session
/// zone pinned by [`Pool::connect`].
fn bind_args<'q>(
    sql: &'q str,
    args: Vec<Value>,
) -> Result<sqlx::query::Query<'q, MySql, MySqlArguments>, ExecError> {
    let mut q = sqlx::query(sql);
    for v in args {
        q = match v {
            Value::Null => q.bind(Option::<String>::None),
            Value::Bool(x) => q.bind(x),
            Value::I8(x) => q.bind(x),
            Value::I16(x) => q.bind(x),
            Value::I32(x) => q.bind(x),
            Value::I64(x) => q.bind(x),
            Value::U8(x) => q.bind(x),
            Value::U16(x) => q.bind(x),
            Value::U32(x) => q.bind(x),
            Value::U64(x) => q.bind(x),
            Value::F32(x) => q.bind(x),
            Value::F64(x) => q.bind(x),
            Value::Text(x) => q.bind(x),
            Value::Bytes(x) => q.bind(x),
            #[cfg(feature = "chrono")]
            Value::Date(x) => q.bind(x),
            #[cfg(feature = "chrono")]
            Value::Time(x) => q.bind(x),
            #[cfg(feature = "chrono")]
            Value::DateTime(x) => q.bind(x),
            #[cfg(feature = "chrono")]
            Value::TimestampTz(x) => q.bind(x),
            #[cfg(feature = "uuid")]
            Value::Uuid(x) => q.bind(x.hyphenated().to_string()),
            #[cfg(feature = "decimal")]
            Value::Decimal(x) => q.bind(x),
            #[cfg(feature = "json")]
            Value::Json(x) => q.bind(x),
            other => {
                return Err(ExecError::UnsupportedValue {
                    type_name: other.type_name(),
                    family: Family::MySql,
                });
            }
        };
    }
    Ok(q)
}

fn decode_row(row: &MySqlRow, header: &mut Option<Arc<[Column]>>) -> Result<Row, ExecError> {
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

fn decode_value(row: &MySqlRow, i: usize) -> Result<Value, ExecError> {
    let col = &row.columns()[i];
    let name = col.name();
    let raw = row.try_get_raw(i).map_err(|e| decode_err(name, e))?;
    if raw.is_null() {
        return Ok(Value::Null);
    }
    let ty = raw.type_info();
    let ty = ty.name();

    macro_rules! take {
        ($t:ty) => {
            row.try_get::<$t, _>(i).map_err(|e| decode_err(name, e))?
        };
    }

    Ok(match ty {
        // TINYINT(1); what the mappings table means by a boolean column.
        "BOOLEAN" => Value::Bool(take!(bool)),
        "TINYINT" => Value::I8(take!(i8)),
        "SMALLINT" => Value::I16(take!(i16)),
        "MEDIUMINT" | "INT" => Value::I32(take!(i32)),
        "BIGINT" => Value::I64(take!(i64)),
        "TINYINT UNSIGNED" => Value::U8(take!(u8)),
        "SMALLINT UNSIGNED" => Value::U16(take!(u16)),
        "MEDIUMINT UNSIGNED" | "INT UNSIGNED" => Value::U32(take!(u32)),
        "BIGINT UNSIGNED" => Value::U64(take!(u64)),
        "FLOAT" => Value::F32(take!(f32)),
        "DOUBLE" => Value::F64(take!(f64)),
        "CHAR" | "VARCHAR" | "TEXT" | "TINYTEXT" | "MEDIUMTEXT" | "LONGTEXT" | "ENUM" => {
            Value::Text(take!(String))
        }
        "BINARY" | "VARBINARY" | "BLOB" | "TINYBLOB" | "MEDIUMBLOB" | "LONGBLOB" => {
            Value::Bytes(take!(Vec<u8>))
        }
        #[cfg(feature = "chrono")]
        "DATE" => Value::Date(take!(chrono::NaiveDate)),
        #[cfg(feature = "chrono")]
        "TIME" => Value::Time(take!(chrono::NaiveTime)),
        #[cfg(feature = "chrono")]
        "DATETIME" => Value::DateTime(take!(chrono::NaiveDateTime)),
        // With the session zone pinned to +00:00, a TIMESTAMP comes back as
        // the instant it stores.
        #[cfg(feature = "chrono")]
        "TIMESTAMP" => Value::TimestampTz(take!(chrono::DateTime<chrono::Utc>)),
        #[cfg(not(feature = "chrono"))]
        "DATE" | "TIME" | "DATETIME" | "TIMESTAMP" => {
            return Err(crate::common::need_feature(name, ty, "chrono"));
        }
        #[cfg(feature = "decimal")]
        "DECIMAL" => Value::Decimal(take!(rust_decimal::Decimal)),
        #[cfg(not(feature = "decimal"))]
        "DECIMAL" => return Err(crate::common::need_feature(name, ty, "decimal")),
        #[cfg(feature = "json")]
        "JSON" => Value::Json(take!(serde_json::Value)),
        #[cfg(not(feature = "json"))]
        "JSON" => return Err(crate::common::need_feature(name, ty, "json")),
        other => return Err(unhandled(name, other)),
    })
}
