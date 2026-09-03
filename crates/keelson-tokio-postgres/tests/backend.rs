//! The second backend, run against a real PostgreSQL.
//!
//! The compile is already half the proof — if keelson-exec's traits had an
//! sqlx assumption in them, `src/lib.rs` would not build. This is the other
//! half: that the implementation *behaves* the way keelson-exec's contract
//! says, over a driver that shares nothing with sqlx but the wire protocol.
//!
//! The suites below are written against `&dyn Executor` and `&dyn Begin` on
//! purpose. Not one of them names this crate, which is the property being
//! demonstrated: application code that talks Layer 2 traits does not know
//! which driver is underneath, and these tests are that application code.
//!
//! ```sh
//! cargo test -p keelson-tokio-postgres --features live-docker
//! ```
#![cfg(feature = "live-docker")]

use std::sync::atomic::{AtomicI64, Ordering};

use keelson_core::Value;
use keelson_exec::{
    Begin, BeginExt as _, BeginWith as _, ExecError, Executor, Isolation, Statement,
    StreamExecutor, TxOptions,
};
use keelson_sqlcheck::conformance;
use keelson_tokio_postgres::Db;

const DDL: &str = "\
CREATE TABLE IF NOT EXISTS keelson_tp (
    k    bigint PRIMARY KEY,
    b    boolean,
    i    integer,
    big  bigint,
    t    text,
    ts   timestamptz,
    u    uuid,
    j    jsonb,
    arr  bigint[]
)";

/// Process-unique row keys, so a shared server and parallel tests never
/// collide.
fn next_key() -> i64 {
    static NEXT: AtomicI64 = AtomicI64::new(0);
    static BASE: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    let base = *BASE.get_or_init(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        (nanos as i64 & 0x7fff_ffff_ffff) << 16
    });
    base + NEXT.fetch_add(1, Ordering::Relaxed)
}

async fn db() -> Db {
    let url = tokio::task::spawn_blocking(|| keelson_sqlcheck::live::psql_url().to_owned())
        .await
        .unwrap();
    let db = Db::connect(&url).await.unwrap();
    // `CREATE TABLE IF NOT EXISTS` is not race-free on PostgreSQL — concurrent
    // creators collide on `pg_type` — and losing the race means the table is
    // there by the time the error arrives. One retry settles it; genuinely
    // broken DDL still fails loudly.
    if db.execute(Statement::new(DDL, vec![])).await.is_err() {
        db.execute(Statement::new(DDL, vec![])).await.unwrap();
    }
    db
}

async fn insert(db: &dyn Executor, k: i64) -> Result<(), ExecError> {
    db.execute(Statement::new(
        "INSERT INTO keelson_tp (k) VALUES ($1)",
        vec![Value::I64(k)],
    ))
    .await
    .map(|_| ())
}

async fn present(db: &dyn Executor, k: i64) -> bool {
    !db.fetch(Statement::new(
        "SELECT k FROM keelson_tp WHERE k = $1",
        vec![Value::I64(k)],
    ))
    .await
    .unwrap()
    .is_empty()
}

/// Bind a value into `col`, read it back, and return what came out.
async fn round_trip(db: &dyn Executor, col: &str, v: Value) -> Value {
    let k = next_key();
    db.execute(Statement::new(
        format!("INSERT INTO keelson_tp (k, {col}) VALUES ($1, $2)"),
        vec![Value::I64(k), v],
    ))
    .await
    .unwrap();
    let rows = db
        .fetch(Statement::new(
            format!("SELECT {col} FROM keelson_tp WHERE k = $1"),
            vec![Value::I64(k)],
        ))
        .await
        .unwrap();
    rows[0].value(col).unwrap().clone()
}

/// The floor every backend runs: every mapped type out and back, with the
/// edges of each. Shared with keelson-sqlx rather than written again here —
/// a second backend that quietly tested less than the first would prove less
/// than it looks like it proves.
#[tokio::test]
async fn the_shared_conformance_suite_passes() {
    let db = db().await;
    let ddl = conformance::ddl(db.family());
    // Same `CREATE TABLE IF NOT EXISTS` race as the table above.
    if db
        .execute(Statement::new(ddl.clone(), vec![]))
        .await
        .is_err()
    {
        db.execute(Statement::new(ddl, vec![])).await.unwrap();
    }
    // This crate carries no `Decimal` — `rust_decimal`'s PostgreSQL support
    // is a feature of `rust_decimal` itself, and enabling it would put
    // tokio-postgres in the graph of every crate here that touches it. See
    // the crate docs. Everything else in the suite is mandatory.
    conformance::every_mapped_type_round_trips_except(&db, &[conformance::Mapped::Decimal]).await;
}

#[tokio::test]
async fn a_value_survives_the_round_trip_through_a_second_driver() {
    let db = db().await;
    let ts = chrono::DateTime::<chrono::Utc>::from_timestamp(1_700_000_000, 0).unwrap();
    let uuid = uuid::Uuid::from_u128(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);

    assert_eq!(
        round_trip(&db, "b", Value::Bool(true)).await,
        Value::Bool(true)
    );
    assert_eq!(round_trip(&db, "i", Value::I32(42)).await, Value::I32(42));
    assert_eq!(
        round_trip(&db, "t", Value::Text("hello".into())).await,
        Value::Text("hello".into())
    );
    assert_eq!(
        round_trip(&db, "ts", Value::TimestampTz(ts)).await,
        Value::TimestampTz(ts)
    );
    assert_eq!(
        round_trip(&db, "u", Value::Uuid(uuid)).await,
        Value::Uuid(uuid)
    );
    assert_eq!(
        round_trip(&db, "j", Value::Json(serde_json::json!({"a": 1}))).await,
        Value::Json(serde_json::json!({"a": 1}))
    );
    assert_eq!(
        round_trip(
            &db,
            "arr",
            Value::Array(vec![Value::I64(1), Value::Null, Value::I64(3)])
        )
        .await,
        Value::Array(vec![Value::I64(1), Value::Null, Value::I64(3)])
    );
    assert_eq!(round_trip(&db, "i", Value::Null).await, Value::Null);
}

#[tokio::test]
async fn a_narrow_integer_binds_into_a_wider_column() {
    // The reason parameter OIDs are pinned rather than inferred: the server
    // would infer `int4` for this column and refuse an `i64` encoding itself
    // as one. With the OID pinned, PostgreSQL's own assignment cast applies.
    let db = db().await;
    // `i` is `integer`; every one of these pins a different parameter OID.
    assert_eq!(round_trip(&db, "i", Value::I64(7)).await, Value::I32(7));
    assert_eq!(round_trip(&db, "i", Value::I16(7)).await, Value::I32(7));
    assert_eq!(round_trip(&db, "i", Value::I8(7)).await, Value::I32(7));
    assert_eq!(round_trip(&db, "i", Value::U32(7)).await, Value::I32(7));
    // And the other direction: `big` is `bigint`, bound from a narrower value.
    assert_eq!(round_trip(&db, "big", Value::I32(7)).await, Value::I64(7));
}

#[tokio::test]
async fn the_transaction_contract_holds_over_a_driver_that_is_not_sqlx() {
    let db = db().await;

    // Commit persists.
    let k = next_key();
    let tx = db.begin().await.unwrap();
    insert(&tx, k).await.unwrap();
    assert!(present(&tx, k).await, "visible inside its own transaction");
    tx.commit().await.unwrap();
    assert!(present(&db, k).await, "and after the commit");

    // Explicit rollback does not.
    let k = next_key();
    let tx = db.begin().await.unwrap();
    insert(&tx, k).await.unwrap();
    tx.rollback().await.unwrap();
    assert!(!present(&db, k).await);

    // Dropping without either abandons the connection, and the server rolls
    // back. This is the case that needs `RawConnection::abandon` to actually
    // close the socket rather than hand it back.
    let k = next_key();
    {
        let tx = db.begin().await.unwrap();
        insert(&tx, k).await.unwrap();
    }
    assert!(!present(&db, k).await);

    // Savepoints nest.
    let k_outer = next_key();
    let k_inner = next_key();
    let tx = db.begin().await.unwrap();
    insert(&tx, k_outer).await.unwrap();
    let inner: Result<(), ExecError> = tx
        .savepoint(async |sp| {
            insert(sp, k_inner).await?;
            Err(ExecError::other("roll the savepoint back"))
        })
        .await;
    assert!(inner.is_err());
    tx.commit().await.unwrap();
    assert!(present(&db, k_outer).await, "the outer write survived");
    assert!(!present(&db, k_inner).await, "the inner one did not");
}

#[tokio::test]
async fn the_closure_form_owns_its_outcome() {
    let db = db().await;
    let k = next_key();
    let out: Result<i64, ExecError> = db
        .within(async |tx| {
            insert(tx, k).await?;
            Ok(k)
        })
        .await;
    assert_eq!(out.unwrap(), k);
    assert!(present(&db, k).await);

    let k = next_key();
    let out: Result<(), ExecError> = db
        .within(async |tx| {
            insert(tx, k).await?;
            Err(ExecError::other("no"))
        })
        .await;
    assert!(out.is_err());
    assert!(
        !present(&db, k).await,
        "an error rolls the whole thing back"
    );
}

#[tokio::test]
async fn an_isolation_level_reaches_the_transactions_own_connection() {
    let db = db().await;
    let tx = db
        .begin_with(TxOptions::default().isolation(Isolation::Serializable))
        .await
        .unwrap();
    let rows = tx
        .fetch(Statement::new("SHOW transaction_isolation", vec![]))
        .await
        .unwrap();
    assert_eq!(
        rows[0].value("transaction_isolation").unwrap(),
        &Value::Text("serializable".into())
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn a_read_only_transaction_refuses_a_write() {
    let db = db().await;
    let tx = db
        .begin_with(TxOptions::default().read_only())
        .await
        .unwrap();
    assert!(insert(&tx, next_key()).await.is_err());
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn an_argument_less_statement_still_reports_its_row_count() {
    // The reason every statement goes through the prepared path here, unlike
    // keelson-sqlx's MySQL backend: `DELETE FROM …` takes no arguments and its
    // count is the whole answer. Sending it over the simple query protocol
    // because it happens to have no parameters would throw that away.
    let db = db().await;
    let k = next_key();
    insert(&db, k).await.unwrap();
    let done = db
        .execute(Statement::new(
            format!("DELETE FROM keelson_tp WHERE k = {k}"),
            vec![],
        ))
        .await
        .unwrap();
    assert_eq!(done.rows_affected, 1);
}

#[tokio::test]
async fn rows_arrive_one_at_a_time() {
    let db = db().await;
    let ks: Vec<i64> = (0..5).map(|_| next_key()).collect();
    for k in &ks {
        insert(&db, *k).await.unwrap();
    }

    let mut stream = db
        .fetch_stream(Statement::new(
            "SELECT k FROM keelson_tp WHERE k = ANY($1) ORDER BY k",
            vec![Value::Array(ks.iter().copied().map(Value::I64).collect())],
        ))
        .await
        .unwrap();

    let mut got = Vec::new();
    while let Some(row) = stream.next().await {
        got.push(row.unwrap().value("k").unwrap().clone());
    }
    assert_eq!(got, ks.into_iter().map(Value::I64).collect::<Vec<_>>());
}
