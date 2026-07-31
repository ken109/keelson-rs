//! The PostgreSQL driver: [`Pool`] implements
//! [`Executor`], [`Begin`], [`BeginWith`] and [`StreamExecutor`].

use std::sync::Arc;

use keelson_core::Value;
use keelson_exec::{
    Begin, BeginWith, Column, ExecError, ExecFuture, ExecResult, Executor, Family, RawConnection,
    Row, RowStream, Statement, StreamExecutor, Transaction, TxConflict, TxConflictError, TxOptions,
};
use sqlx::postgres::{PgArgumentBuffer, PgArguments, PgRow, PgTypeInfo};
use sqlx::{Column as _, Postgres, Row as _, TypeInfo as _, ValueRef as _};

use crate::common::{decode_err, unhandled};

/// A PostgreSQL connection pool. sqlx's pool is the pool; this adds nothing
/// but the keelson traits.
#[derive(Debug, Clone)]
pub struct Pool {
    inner: sqlx::PgPool,
}

impl Pool {
    /// Connect to `url` (`postgres://user:pass@host:port/db`).
    pub async fn connect(url: &str) -> Result<Self, ExecError> {
        let inner = sqlx::postgres::PgPoolOptions::new()
            .connect(url)
            .await
            .map_err(ExecError::driver)?;
        Ok(Pool { inner })
    }

    /// Wrap an existing sqlx pool.
    pub fn from_pool(inner: sqlx::PgPool) -> Self {
        Pool { inner }
    }

    /// The wrapped sqlx pool — keelson is a layer, not a jail.
    pub fn inner(&self) -> &sqlx::PgPool {
        &self.inner
    }
}

impl Executor for Pool {
    fn family(&self) -> Family {
        Family::Postgres
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
            // Refuse before taking a connection out of the pool: an
            // unsupported option costs nothing and disturbs nothing.
            opts.check(Family::Postgres)?;
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
    conn: sqlx::pool::PoolConnection<Postgres>,
}

impl RawConnection for RawConn {
    fn family(&self) -> Family {
        Family::Postgres
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
        // Detached: not returned to the pool. Dropping the raw connection
        // closes it, and the server discards the open transaction.
        let _ = self.conn.detach();
    }
}

/// A driver failure, with concurrency conflicts classified out of it.
///
/// PostgreSQL reports them as SQLSTATEs, so this is a table lookup rather
/// than message matching: `40001` serialization_failure, `40P01`
/// deadlock_detected, `55P03` lock_not_available (what `lock_timeout` and
/// `NOWAIT` raise). Everything else stays an opaque driver error.
fn driver_err(e: sqlx::Error) -> ExecError {
    if let sqlx::Error::Database(db) = &e {
        let kind = match db.code().as_deref() {
            Some("40001") => Some(TxConflict::Serialization),
            Some("40P01") => Some(TxConflict::Deadlock),
            Some("55P03") => Some(TxConflict::LockTimeout),
            _ => None,
        };
        if let Some(kind) = kind {
            let code = db.code().unwrap_or_default().into_owned();
            let message = db.message().to_owned();
            return TxConflictError::new(kind, code, message)
                .with_source(e)
                .into_exec_error();
        }
    }
    ExecError::driver(e)
}

async fn do_fetch<'e, E>(exec: E, sql: &str, args: Vec<Value>) -> Result<Vec<Row>, ExecError>
where
    E: sqlx::Executor<'e, Database = Postgres>,
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
    E: sqlx::Executor<'e, Database = Postgres>,
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
    // PostgreSQL has no last-insert-id; RETURNING is the honest story.
    Ok(ExecResult::new(done.rows_affected(), None))
}

/// An untyped SQL `NULL`: parameter OID `unknown`, so the server infers the
/// type from context. `Value::Null` carries no type, and a typed null (say,
/// `text`) would be refused where an `int` is expected.
#[derive(Debug)]
struct UnknownNull;

impl sqlx::Type<Postgres> for UnknownNull {
    fn type_info() -> PgTypeInfo {
        PgTypeInfo::with_name("unknown")
    }
}

impl sqlx::Encode<'_, Postgres> for UnknownNull {
    fn encode_by_ref(
        &self,
        _buf: &mut PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        Ok(sqlx::encode::IsNull::Yes)
    }
}

/// The total map `Value` → PostgreSQL parameter, per the "binds as" column of
/// `docs/type-mappings.md`: native parameter types throughout (this driver
/// has them all).
fn bind_args<'q>(
    sql: &'q str,
    args: Vec<Value>,
) -> Result<sqlx::query::Query<'q, Postgres, PgArguments>, ExecError> {
    let mut q = sqlx::query(sql);
    for v in args {
        q = match v {
            Value::Null => q.bind(UnknownNull),
            Value::Bool(x) => q.bind(x),
            // PostgreSQL has no 1-byte integer; widen. Unsigned widths widen
            // into the next signed size, and u64 must fit in i64 or is
            // refused loudly.
            Value::I8(x) => q.bind(i16::from(x)),
            Value::I16(x) => q.bind(x),
            Value::I32(x) => q.bind(x),
            Value::I64(x) => q.bind(x),
            Value::U8(x) => q.bind(i16::from(x)),
            Value::U16(x) => q.bind(i32::from(x)),
            Value::U32(x) => q.bind(i64::from(x)),
            Value::U64(x) => {
                q.bind(i64::try_from(x).map_err(|_| unsupported_value("u64 out of i64 range"))?)
            }
            Value::F32(x) => q.bind(x),
            Value::F64(x) => q.bind(x),
            Value::Text(x) => q.bind(x),
            Value::Bytes(x) => q.bind(x),
            Value::Array(items) => bind_array(q, items)?,
            #[cfg(feature = "chrono")]
            Value::Date(x) => q.bind(x),
            #[cfg(feature = "chrono")]
            Value::Time(x) => q.bind(x),
            #[cfg(feature = "chrono")]
            Value::DateTime(x) => q.bind(x),
            #[cfg(feature = "chrono")]
            Value::TimestampTz(x) => q.bind(x),
            #[cfg(feature = "uuid")]
            Value::Uuid(x) => q.bind(x),
            #[cfg(feature = "decimal")]
            Value::Decimal(x) => q.bind(x),
            #[cfg(feature = "json")]
            Value::Json(x) => q.bind(x),
            other => return Err(unsupported_value(other.type_name())),
        };
    }
    Ok(q)
}

fn unsupported_value(type_name: &'static str) -> ExecError {
    ExecError::UnsupportedValue {
        type_name,
        family: Family::Postgres,
    }
}

/// PostgreSQL arrays are typed, so the element variant picks the array type.
/// Elements must be homogeneous; `Null` elements ride along as SQL `NULL`s.
/// An empty array binds as `text[]` (there is nothing to infer from) — cast
/// in SQL if the column is another array type.
fn bind_array<'q>(
    q: sqlx::query::Query<'q, Postgres, PgArguments>,
    items: Vec<Value>,
) -> Result<sqlx::query::Query<'q, Postgres, PgArguments>, ExecError> {
    macro_rules! typed {
        ($variant:ident, $t:ty) => {{
            let mut out: Vec<Option<$t>> = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::$variant(x) => out.push(Some(x)),
                    Value::Null => out.push(None),
                    other => return Err(unsupported_value(other.type_name())),
                }
            }
            q.bind(out)
        }};
    }
    let first = items.iter().find(|v| !v.is_null());
    Ok(match first {
        None => {
            // All NULLs or empty: text[] is the only honest default.
            let nulls: Vec<Option<String>> = items.iter().map(|_| None).collect();
            q.bind(nulls)
        }
        Some(Value::Bool(_)) => typed!(Bool, bool),
        Some(Value::I16(_)) => typed!(I16, i16),
        Some(Value::I32(_)) => typed!(I32, i32),
        Some(Value::I64(_)) => typed!(I64, i64),
        Some(Value::F32(_)) => typed!(F32, f32),
        Some(Value::F64(_)) => typed!(F64, f64),
        Some(Value::Text(_)) => typed!(Text, String),
        #[cfg(feature = "uuid")]
        Some(Value::Uuid(_)) => typed!(Uuid, uuid::Uuid),
        Some(other) => return Err(unsupported_value(other.type_name())),
    })
}

/// Native row → keelson [`Row`], per the column-type column of the mappings
/// table. The header is built once per result set and shared.
fn decode_row(row: &PgRow, header: &mut Option<Arc<[Column]>>) -> Result<Row, ExecError> {
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

fn decode_value(row: &PgRow, i: usize) -> Result<Value, ExecError> {
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
        "BOOL" => Value::Bool(take!(bool)),
        "INT2" => Value::I16(take!(i16)),
        "INT4" => Value::I32(take!(i32)),
        "INT8" => Value::I64(take!(i64)),
        "FLOAT4" => Value::F32(take!(f32)),
        "FLOAT8" => Value::F64(take!(f64)),
        "TEXT" | "VARCHAR" | "BPCHAR" | "NAME" => Value::Text(take!(String)),
        "BYTEA" => Value::Bytes(take!(Vec<u8>)),
        #[cfg(feature = "chrono")]
        "DATE" => Value::Date(take!(chrono::NaiveDate)),
        #[cfg(feature = "chrono")]
        "TIME" => Value::Time(take!(chrono::NaiveTime)),
        #[cfg(feature = "chrono")]
        "TIMESTAMP" => Value::DateTime(take!(chrono::NaiveDateTime)),
        #[cfg(feature = "chrono")]
        "TIMESTAMPTZ" => Value::TimestampTz(take!(chrono::DateTime<chrono::Utc>)),
        #[cfg(not(feature = "chrono"))]
        "DATE" | "TIME" | "TIMESTAMP" | "TIMESTAMPTZ" => {
            return Err(crate::common::need_feature(name, ty, "chrono"));
        }
        #[cfg(feature = "uuid")]
        "UUID" => Value::Uuid(take!(uuid::Uuid)),
        #[cfg(not(feature = "uuid"))]
        "UUID" => return Err(crate::common::need_feature(name, ty, "uuid")),
        #[cfg(feature = "decimal")]
        "NUMERIC" => Value::Decimal(take!(rust_decimal::Decimal)),
        #[cfg(not(feature = "decimal"))]
        "NUMERIC" => return Err(crate::common::need_feature(name, ty, "decimal")),
        #[cfg(feature = "json")]
        "JSON" | "JSONB" => Value::Json(take!(serde_json::Value)),
        #[cfg(not(feature = "json"))]
        "JSON" | "JSONB" => return Err(crate::common::need_feature(name, ty, "json")),
        "BOOL[]" => array(take!(Vec<Option<bool>>), Value::Bool),
        "INT2[]" => array(take!(Vec<Option<i16>>), Value::I16),
        "INT4[]" => array(take!(Vec<Option<i32>>), Value::I32),
        "INT8[]" => array(take!(Vec<Option<i64>>), Value::I64),
        "FLOAT4[]" => array(take!(Vec<Option<f32>>), Value::F32),
        "FLOAT8[]" => array(take!(Vec<Option<f64>>), Value::F64),
        "TEXT[]" | "VARCHAR[]" => array(take!(Vec<Option<String>>), Value::Text),
        #[cfg(feature = "uuid")]
        "UUID[]" => array(take!(Vec<Option<uuid::Uuid>>), Value::Uuid),
        other => return Err(unhandled(name, other)),
    })
}

fn array<T>(items: Vec<Option<T>>, wrap: fn(T) -> Value) -> Value {
    Value::Array(
        items
            .into_iter()
            .map(|x| x.map_or(Value::Null, wrap))
            .collect(),
    )
}
