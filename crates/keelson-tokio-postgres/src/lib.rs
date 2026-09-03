//! A second Layer 2 backend, over [`tokio_postgres`] — and the proof that
//! there can be one.
//!
//! # Why this crate exists
//!
//! keelson-exec's pitch is that the execution layer is *driver-free*: the
//! traits, the transaction machinery, the verb layer and the row decoding are
//! written once, and a backend is the small adapter underneath. A claim like
//! that is unfalsifiable with one backend in the workspace. Every accidental
//! sqlx assumption — a lifetime shape only sqlx's `Executor` has, an error
//! classification only sqlx exposes, a pool concept baked into a trait — would
//! sit in keelson-exec unnoticed, and the first person to write a real second
//! backend would find them all at once.
//!
//! So this is the second implementor, and its whole job is to fail to compile
//! if the traits stop being general. It is `publish = false`: it has no
//! connection pool, no feature matrix and no compatibility promise, and
//! shipping it would turn all three into someone's expectation. If a
//! production tokio-postgres backend is ever wanted, it starts here and grows
//! the parts this one deliberately skipped.
//!
//! What it does implement is the whole surface, because a partial one proves
//! nothing: [`Executor`], [`StreamExecutor`], [`Begin`], [`BeginWith`] and
//! [`RawConnection`], plus the two halves of the type map — `Value` →
//! parameter and native row → `Value`.
//!
//! # What it deliberately skips
//!
//! **No pool.** [`Db`] holds one connection for statements outside a
//! transaction and opens a fresh one per transaction. That is a bad idea in
//! production and a good one here: pooling is the driver ecosystem's job
//! (`deadpool-postgres`, `bb8`), taking one on would be a dependency decision
//! this crate has no standing to make, and — the point — [`Begin`] never
//! mentions a pool, which is exactly the generality being demonstrated.
//!
//! **No `decimal`.** `rust_decimal`'s PostgreSQL integration is a feature *of
//! rust_decimal*, and Cargo unifies features across a workspace: enabling it
//! here would add tokio-postgres to the dependency graph of every crate in
//! this workspace that touches `rust_decimal`. `Value::Decimal` therefore does
//! not exist in this build (keelson-core's `decimal` feature is off), so there
//! is no arm to write and nothing silently guessed.
//!
//! # Parameter types are pinned, not inferred
//!
//! tokio-postgres sends `Parse` with no parameter types and lets the server
//! infer them, then encodes each argument with whatever the server decided.
//! That is wrong for a builder: `WHERE "age" >= $1` against an `int4` column
//! infers `int4`, and `i64::to_sql` refuses to encode itself as one — a
//! perfectly ordinary `Value::I64` would fail at the wire. So every statement
//! goes through [`tokio_postgres::Client::prepare_typed`] with the OIDs
//! `pg_type` derives from the values, and PostgreSQL applies its ordinary
//! assignment and comparison casts from there.
//!
//! `Value::Null` pins `unknown` (OID 705), which is what an untyped literal
//! is: the server resolves it from context rather than committing to `text`
//! and failing where an `int` was wanted.

#![warn(missing_docs)]
#![forbid(unsafe_code)]

use std::sync::Arc;

use keelson_core::Value;
use keelson_exec::{
    Begin, BeginWith, Column, ExecError, ExecFuture, ExecResult, Executor, Family, Header,
    RawConnection, Row, RowStream, Statement, StreamExecutor, Transaction, TxConflict,
    TxConflictError, TxOptions,
};
use tokio_postgres::types::{IsNull, ToSql, Type, to_sql_checked};
use tokio_postgres::{Client, Config, NoTls};

// ---------------------------------------------------------------------------
// The connection
// ---------------------------------------------------------------------------

/// One PostgreSQL connection, plus the configuration to open more.
///
/// The configuration is kept because a transaction needs a connection nobody
/// else is writing to, and this crate has no pool to take one from — see the
/// crate docs for why that absence is deliberate.
#[derive(Debug, Clone)]
pub struct Db {
    config: Config,
    client: Arc<Client>,
}

impl Db {
    /// Connect to `url` (`postgres://user:pass@host:port/db`).
    ///
    /// TLS is not configured: this is a test-support backend, and a TLS story
    /// is one of the things a published backend would have to have.
    pub async fn connect(url: &str) -> Result<Self, ExecError> {
        let config: Config = url.parse().map_err(ExecError::driver)?;
        let client = open(&config).await?.client;
        Ok(Db {
            config,
            client: Arc::new(client),
        })
    }

    /// The wrapped client — keelson is a layer, not a jail.
    pub fn client(&self) -> &Client {
        &self.client
    }
}

/// A connection and the task driving it.
///
/// tokio-postgres splits a connection in two: a [`Client`] that issues
/// statements and a `Connection` future that has to be polled for any of them
/// to make progress. Keeping the handle rather than detaching the task is what
/// lets [`RawConn::abandon`] actually close the socket.
#[derive(Debug)]
struct Conn {
    client: Client,
    driver: tokio::task::JoinHandle<()>,
}

async fn open(config: &Config) -> Result<Conn, ExecError> {
    let (client, connection) = config.connect(NoTls).await.map_err(ExecError::driver)?;
    // The connection future resolves when the client is dropped or the socket
    // fails; either way there is nobody left to report to, so the error is
    // deliberately dropped here rather than logged from a library.
    let driver = tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok(Conn { client, driver })
}

impl Executor for Db {
    fn family(&self) -> Family {
        Family::Postgres
    }

    fn fetch(&self, stmt: Statement) -> ExecFuture<'_, Result<Vec<Row>, ExecError>> {
        Box::pin(async move {
            let Statement { sql, args, .. } = stmt;
            do_fetch(&self.client, &sql, args).await
        })
    }

    fn execute(&self, stmt: Statement) -> ExecFuture<'_, Result<ExecResult, ExecError>> {
        Box::pin(async move {
            let Statement { sql, args, .. } = stmt;
            do_execute(&self.client, &sql, args).await
        })
    }
}

impl Begin for Db {
    fn begin(&self) -> ExecFuture<'_, Result<Transaction, ExecError>> {
        Box::pin(async move {
            let conn = open(&self.config).await?;
            Transaction::begin_on(Box::new(RawConn { conn })).await
        })
    }
}

impl BeginWith for Db {
    fn begin_with(&self, opts: TxOptions) -> ExecFuture<'_, Result<Transaction, ExecError>> {
        Box::pin(async move {
            // Refuse before opening a connection: an unsupported option costs
            // nothing and disturbs nothing.
            opts.check(Family::Postgres)?;
            let conn = open(&self.config).await?;
            Transaction::begin_on_with(Box::new(RawConn { conn }), opts).await
        })
    }
}

impl StreamExecutor for Db {
    fn fetch_stream(&self, stmt: Statement) -> ExecFuture<'_, Result<RowStream, ExecError>> {
        Box::pin(async move {
            let client = self.client.clone();
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<Row, ExecError>>(64);
            tokio::spawn(async move {
                use futures_util::StreamExt as _;
                let Statement { sql, args, .. } = stmt;
                let prepared = match prepare(&client, &sql, &args).await {
                    Ok(p) => p,
                    Err(e) => {
                        let _ = tx.send(Err(e)).await;
                        return;
                    }
                };
                let wrapped = wrap(&args);
                let params = as_params(&wrapped);
                let native = client.query_raw(&prepared, params.iter().copied()).await;
                let native = match native {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = tx.send(Err(driver_err(e))).await;
                        return;
                    }
                };
                let mut native = std::pin::pin!(native);
                let mut header: Option<Arc<Header>> = None;
                while let Some(next) = native.next().await {
                    let msg = match next {
                        Ok(row) => decode_row(&row, &mut header),
                        Err(e) => Err(driver_err(e)),
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

/// One connection, exclusively held by a [`Transaction`].
#[derive(Debug)]
struct RawConn {
    conn: Conn,
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
        Box::pin(async move { do_fetch(&self.conn.client, sql, args).await })
    }

    fn execute<'a>(
        &'a mut self,
        sql: &'a str,
        args: Vec<Value>,
    ) -> ExecFuture<'a, Result<ExecResult, ExecError>> {
        Box::pin(async move { do_execute(&self.conn.client, sql, args).await })
    }

    fn abandon(self: Box<Self>) {
        // Dropping the client closes the connection, and the server discards
        // the open transaction. The driver task then resolves on its own;
        // aborting it as well is what makes that immediate rather than
        // whenever the runtime next polls it.
        let Conn { client, driver } = self.conn;
        drop(client);
        driver.abort();
    }
}

// ---------------------------------------------------------------------------
// Running a statement
// ---------------------------------------------------------------------------

/// A driver failure, with concurrency conflicts classified out of it.
///
/// Conflicts are classified by [`TxConflict::from_postgres_sqlstate`], which
/// is keelson-exec's — not a table this crate keeps in step with the sqlx
/// backend's by hand. That both backends classify identically, from data the
/// *server* sent rather than anything driver-specific, is part of what this
/// crate checks: `TxConflict` is not an sqlx concept that leaked into
/// keelson-exec.
fn driver_err(e: tokio_postgres::Error) -> ExecError {
    let Some(state) = e.code() else {
        return ExecError::driver(e);
    };
    let kind = TxConflict::from_postgres_sqlstate(state.code());
    match kind {
        None => ExecError::driver(e),
        Some(kind) => {
            let code = state.code().to_owned();
            let message = e
                .as_db_error()
                .map(|db| db.message().to_owned())
                .unwrap_or_default();
            TxConflictError::new(kind, code, message)
                .with_source(e)
                .into_exec_error()
        }
    }
}

/// Prepare with the parameter OIDs pinned — see the crate docs.
async fn prepare(
    client: &Client,
    sql: &str,
    args: &[Value],
) -> Result<tokio_postgres::Statement, ExecError> {
    let types: Vec<Type> = args.iter().map(pg_type).collect::<Result<_, _>>()?;
    client.prepare_typed(sql, &types).await.map_err(driver_err)
}

/// The arguments as tokio-postgres wants them.
///
/// Two steps rather than one because both borrows have to outlive the call:
/// the wrappers borrow `args`, and the trait objects borrow the wrappers.
fn wrap(args: &[Value]) -> Vec<Param<'_>> {
    args.iter().map(Param).collect()
}

fn as_params<'a>(wrapped: &'a [Param<'a>]) -> Vec<&'a (dyn ToSql + Sync)> {
    wrapped.iter().map(|p| p as &(dyn ToSql + Sync)).collect()
}

async fn do_fetch(client: &Client, sql: &str, args: Vec<Value>) -> Result<Vec<Row>, ExecError> {
    let prepared = prepare(client, sql, &args).await?;
    let wrapped = wrap(&args);
    let params = as_params(&wrapped);
    let rows = client.query(&prepared, &params).await.map_err(driver_err)?;
    let mut header: Option<Arc<Header>> = None;
    rows.iter().map(|r| decode_row(r, &mut header)).collect()
}

async fn do_execute(client: &Client, sql: &str, args: Vec<Value>) -> Result<ExecResult, ExecError> {
    // Zero-argument statements go over the prepared path like every other one.
    // keelson-sqlx's MySQL backend has to special-case them — MySQL refuses
    // transaction control in the prepared-statement protocol — but PostgreSQL
    // accepts `BEGIN`/`SAVEPOINT …` through the extended protocol, and routing
    // them to the simple protocol instead would throw the row count away:
    // `DELETE FROM t` takes no arguments and its count is the whole answer.
    let prepared = prepare(client, sql, &args).await?;
    let wrapped = wrap(&args);
    let params = as_params(&wrapped);
    let affected = client
        .execute(&prepared, &params)
        .await
        .map_err(driver_err)?;
    // PostgreSQL has no last-insert-id; RETURNING is the honest story.
    Ok(ExecResult::new(affected, None))
}

// ---------------------------------------------------------------------------
// Value → parameter
// ---------------------------------------------------------------------------

/// The parameter OID to pin for a value, per the "binds as" column of
/// `docs/type-mappings.md`. The crate docs say why they are pinned at all.
///
/// PostgreSQL has no 1-byte integer, so the small widths widen; unsigned
/// widths widen into the next signed size, and `u64` pins `int8` and is
/// refused at encode time if it does not fit.
fn pg_type(v: &Value) -> Result<Type, ExecError> {
    Ok(match v {
        // `unknown`, not `text`: an untyped parameter the server resolves from
        // context, which is what a bare SQL `NULL` is.
        Value::Null => Type::UNKNOWN,
        Value::Bool(_) => Type::BOOL,
        Value::I8(_) | Value::I16(_) | Value::U8(_) => Type::INT2,
        Value::I32(_) | Value::U16(_) => Type::INT4,
        Value::I64(_) | Value::U32(_) | Value::U64(_) => Type::INT8,
        Value::F32(_) => Type::FLOAT4,
        Value::F64(_) => Type::FLOAT8,
        Value::Text(_) => Type::TEXT,
        Value::Bytes(_) => Type::BYTEA,
        Value::Date(_) => Type::DATE,
        Value::Time(_) => Type::TIME,
        Value::DateTime(_) => Type::TIMESTAMP,
        Value::TimestampTz(_) => Type::TIMESTAMPTZ,
        Value::Uuid(_) => Type::UUID,
        Value::Json(_) => Type::JSONB,
        Value::Array(items) => array_type(items)?,
        other => return Err(unsupported(other.type_name())),
    })
}

/// PostgreSQL arrays are typed, so the first non-null element picks the array
/// type. An all-null or empty array pins `text[]`: there is nothing to infer
/// from, and guessing a different one would be a guess.
fn array_type(items: &[Value]) -> Result<Type, ExecError> {
    let Some(first) = items.iter().find(|v| !v.is_null()) else {
        return Ok(Type::TEXT_ARRAY);
    };
    Ok(match first {
        Value::Bool(_) => Type::BOOL_ARRAY,
        Value::I16(_) => Type::INT2_ARRAY,
        Value::I32(_) => Type::INT4_ARRAY,
        Value::I64(_) => Type::INT8_ARRAY,
        Value::F32(_) => Type::FLOAT4_ARRAY,
        Value::F64(_) => Type::FLOAT8_ARRAY,
        Value::Text(_) => Type::TEXT_ARRAY,
        Value::Uuid(_) => Type::UUID_ARRAY,
        other => return Err(unsupported(other.type_name())),
    })
}

fn unsupported(type_name: &'static str) -> ExecError {
    ExecError::UnsupportedValue {
        type_name,
        family: Family::Postgres,
    }
}

/// A [`Value`] wearing tokio-postgres's [`ToSql`].
///
/// `accepts` answers `true` for every type because the wrapper is not a type —
/// it is a sum of them, and which arm applies is not known until `to_sql` sees
/// the value. The real check is not skipped: each arm delegates to the inner
/// type's `to_sql_checked`, which is what refuses an `i32` asked to encode
/// itself as `int8`. With the OIDs pinned by [`pg_type`] that refusal should
/// be unreachable, and it stays in place because "should be" is not "is".
#[derive(Debug)]
struct Param<'a>(&'a Value);

impl ToSql for Param<'_> {
    fn to_sql(
        &self,
        ty: &Type,
        out: &mut tokio_postgres::types::private::BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Sync + Send>> {
        match self.0 {
            Value::Null => Ok(IsNull::Yes),
            Value::Bool(x) => x.to_sql_checked(ty, out),
            Value::I8(x) => i16::from(*x).to_sql_checked(ty, out),
            Value::I16(x) => x.to_sql_checked(ty, out),
            Value::I32(x) => x.to_sql_checked(ty, out),
            Value::I64(x) => x.to_sql_checked(ty, out),
            Value::U8(x) => i16::from(*x).to_sql_checked(ty, out),
            Value::U16(x) => i32::from(*x).to_sql_checked(ty, out),
            Value::U32(x) => i64::from(*x).to_sql_checked(ty, out),
            Value::U64(x) => i64::try_from(*x)
                .map_err(|_| -> Box<dyn std::error::Error + Sync + Send> {
                    "u64 out of i64 range".into()
                })?
                .to_sql_checked(ty, out),
            Value::F32(x) => x.to_sql_checked(ty, out),
            Value::F64(x) => x.to_sql_checked(ty, out),
            Value::Text(x) => x.to_sql_checked(ty, out),
            Value::Bytes(x) => x.to_sql_checked(ty, out),
            Value::Date(x) => x.to_sql_checked(ty, out),
            Value::Time(x) => x.to_sql_checked(ty, out),
            Value::DateTime(x) => x.to_sql_checked(ty, out),
            Value::TimestampTz(x) => x.to_sql_checked(ty, out),
            Value::Uuid(x) => x.to_sql_checked(ty, out),
            Value::Json(x) => x.to_sql_checked(ty, out),
            Value::Array(items) => array_to_sql(items, ty, out),
            other => Err(format!("unsupported value type {}", other.type_name()).into()),
        }
    }

    fn accepts(_: &Type) -> bool {
        true
    }

    to_sql_checked!();
}

/// An array parameter. Elements must be homogeneous; `Null` elements ride
/// along as SQL `NULL`s, which is what `Vec<Option<T>>` encodes.
fn array_to_sql(
    items: &[Value],
    ty: &Type,
    out: &mut tokio_postgres::types::private::BytesMut,
) -> Result<IsNull, Box<dyn std::error::Error + Sync + Send>> {
    macro_rules! typed {
        ($variant:ident, $t:ty) => {{
            let mut xs: Vec<Option<$t>> = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::$variant(x) => xs.push(Some(x.clone())),
                    Value::Null => xs.push(None),
                    other => {
                        return Err(format!("mixed array: unexpected {}", other.type_name()).into());
                    }
                }
            }
            xs.to_sql_checked(ty, out)
        }};
    }
    match items.iter().find(|v| !v.is_null()) {
        None => {
            let nulls: Vec<Option<String>> = items.iter().map(|_| None).collect();
            nulls.to_sql_checked(ty, out)
        }
        Some(Value::Bool(_)) => typed!(Bool, bool),
        Some(Value::I16(_)) => typed!(I16, i16),
        Some(Value::I32(_)) => typed!(I32, i32),
        Some(Value::I64(_)) => typed!(I64, i64),
        Some(Value::F32(_)) => typed!(F32, f32),
        Some(Value::F64(_)) => typed!(F64, f64),
        Some(Value::Text(_)) => typed!(Text, String),
        Some(Value::Uuid(_)) => typed!(Uuid, uuid::Uuid),
        Some(other) => Err(format!("unsupported array element {}", other.type_name()).into()),
    }
}

// ---------------------------------------------------------------------------
// Row → Value
// ---------------------------------------------------------------------------

/// Native row → keelson [`Row`], per the column-type column of the mappings
/// table. The header is built once per result set and shared.
fn decode_row(
    row: &tokio_postgres::Row,
    header: &mut Option<Arc<Header>>,
) -> Result<Row, ExecError> {
    // Built from the first row of the result set and shared by the rest, so
    // the name lookup every `FromRow` does is prepared once rather than
    // re-scanned per row.
    let header = header
        .get_or_insert_with(|| {
            Arc::new(Header::new(
                row.columns()
                    .iter()
                    .map(|c| Column::new(c.name()))
                    .collect::<Vec<_>>(),
            ))
        })
        .clone();
    let mut values = Vec::with_capacity(row.columns().len());
    for i in 0..row.columns().len() {
        values.push(decode_value(row, i)?);
    }
    Ok(Row::with_header(header, values))
}

fn decode_value(row: &tokio_postgres::Row, i: usize) -> Result<Value, ExecError> {
    let col = &row.columns()[i];
    let name = col.name();
    let ty = col.type_().clone();

    macro_rules! take {
        ($t:ty) => {
            match row.try_get::<_, Option<$t>>(i) {
                Ok(None) => return Ok(Value::Null),
                Ok(Some(x)) => x,
                Err(e) => return Err(decode_err(name, e)),
            }
        };
    }

    Ok(match ty {
        Type::BOOL => Value::Bool(take!(bool)),
        Type::INT2 => Value::I16(take!(i16)),
        Type::INT4 => Value::I32(take!(i32)),
        Type::INT8 => Value::I64(take!(i64)),
        Type::FLOAT4 => Value::F32(take!(f32)),
        Type::FLOAT8 => Value::F64(take!(f64)),
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME => Value::Text(take!(String)),
        Type::BYTEA => Value::Bytes(take!(Vec<u8>)),
        Type::DATE => Value::Date(take!(chrono::NaiveDate)),
        Type::TIME => Value::Time(take!(chrono::NaiveTime)),
        Type::TIMESTAMP => Value::DateTime(take!(chrono::NaiveDateTime)),
        Type::TIMESTAMPTZ => Value::TimestampTz(take!(chrono::DateTime<chrono::Utc>)),
        Type::UUID => Value::Uuid(take!(uuid::Uuid)),
        Type::JSON | Type::JSONB => Value::Json(take!(serde_json::Value)),
        Type::BOOL_ARRAY => array(take!(Vec<Option<bool>>), Value::Bool),
        Type::INT2_ARRAY => array(take!(Vec<Option<i16>>), Value::I16),
        Type::INT4_ARRAY => array(take!(Vec<Option<i32>>), Value::I32),
        Type::INT8_ARRAY => array(take!(Vec<Option<i64>>), Value::I64),
        Type::FLOAT4_ARRAY => array(take!(Vec<Option<f32>>), Value::F32),
        Type::FLOAT8_ARRAY => array(take!(Vec<Option<f64>>), Value::F64),
        Type::TEXT_ARRAY | Type::VARCHAR_ARRAY => array(take!(Vec<Option<String>>), Value::Text),
        Type::UUID_ARRAY => array(take!(Vec<Option<uuid::Uuid>>), Value::Uuid),
        // `numeric` lands here on purpose: see the crate docs on `decimal`.
        other => return Err(unhandled(name, other.name())),
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

/// A driver refusal while reading a column, with the column named.
fn decode_err(column: &str, e: tokio_postgres::Error) -> ExecError {
    ExecError::Decode {
        column: column.to_owned(),
        source: keelson_core::Error::other(e.to_string()),
    }
}

/// A column type this backend has no mapping for. Loud, never guessed.
fn unhandled(column: &str, ty: &str) -> ExecError {
    ExecError::Decode {
        column: column.to_owned(),
        source: keelson_core::Error::other(format!("unsupported column type {ty}")),
    }
}
