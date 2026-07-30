//! Transaction semantics against real engines: commit persists,
//! drop-without-commit rolls back, explicit rollback rolls back, savepoints
//! nest, and the closure form owns its outcome. One generic suite, run
//! against SQLite always and PostgreSQL/MySQL behind `live-docker` — the
//! suite itself only sees `&dyn Begin`, which is the point.

use std::sync::atomic::{AtomicI64, Ordering};

use keelson_core::Value;
use keelson_exec::{Begin, BeginExt as _, ExecError, Executor, Family, Statement};

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

fn ph(family: Family) -> &'static str {
    match family {
        Family::Postgres => "$1",
        Family::MySql => "?",
        Family::Sqlite => "?1",
        _ => unreachable!(),
    }
}

async fn insert(db: &dyn Executor, k: i64) -> Result<(), ExecError> {
    db.execute(Statement::new(
        format!("INSERT INTO keelson_tx (k) VALUES ({})", ph(db.family())),
        vec![Value::I64(k)],
    ))
    .await
    .map(|_| ())
}

async fn present(db: &dyn Executor, k: i64) -> bool {
    let rows = db
        .fetch(Statement::new(
            format!("SELECT k FROM keelson_tx WHERE k = {}", ph(db.family())),
            vec![Value::I64(k)],
        ))
        .await
        .unwrap();
    !rows.is_empty()
}

/// The whole suite, written against the traits alone.
async fn tx_suite(db: &dyn Begin) {
    // Commit persists.
    let k_commit = next_key();
    let tx = db.begin().await.unwrap();
    insert(&tx, k_commit).await.unwrap();
    // Inside the transaction the row is visible...
    assert!(present(&tx, k_commit).await);
    // ...outside, not yet (the pool reads on other connections).
    assert!(!present(db, k_commit).await);
    tx.commit().await.unwrap();
    assert!(present(db, k_commit).await, "commit must persist");

    // Explicit rollback discards.
    let k_rollback = next_key();
    let tx = db.begin().await.unwrap();
    insert(&tx, k_rollback).await.unwrap();
    tx.rollback().await.unwrap();
    assert!(!present(db, k_rollback).await, "rollback must discard");

    // Drop without commit rolls back (the connection is abandoned; the
    // server discards the transaction).
    let k_drop = next_key();
    let tx = db.begin().await.unwrap();
    insert(&tx, k_drop).await.unwrap();
    drop(tx);
    assert!(!present(db, k_drop).await, "drop must not commit");

    // Savepoints: the inner failure rolls back to the savepoint, the outer
    // transaction lives on and commits; nesting nests.
    let k_outer = next_key();
    let k_lost = next_key();
    let k_deep = next_key();
    let tx = db.begin().await.unwrap();
    insert(&tx, k_outer).await.unwrap();
    let err = tx
        .savepoint(async |sp| {
            insert(sp, k_lost).await?;
            assert!(present(sp, k_lost).await);
            Err::<(), _>(ExecError::other("abort the savepoint"))
        })
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "abort the savepoint");
    tx.savepoint(async |sp| sp.savepoint(async |sp2| insert(sp2, k_deep).await).await)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(present(db, k_outer).await, "outer work must survive");
    assert!(!present(db, k_lost).await, "rolled-back savepoint must not");
    assert!(
        present(db, k_deep).await,
        "nested savepoint work must survive"
    );

    // The closure form: Ok commits...
    let k_within = next_key();
    db.within(async |tx| insert(tx, k_within).await)
        .await
        .unwrap();
    assert!(present(db, k_within).await);

    // ...Err rolls back, and the caller's error comes through.
    let k_failed = next_key();
    let err = db
        .within(async |tx| {
            insert(tx, k_failed).await?;
            Err::<(), _>(ExecError::other("boom"))
        })
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "boom");
    assert!(!present(db, k_failed).await);

    // A function generic over "anywhere I can run queries" accepts pool and
    // transaction alike — the &dyn Executor currency.
    async fn anywhere(db: &dyn Executor, k: i64) -> bool {
        present(db, k).await
    }
    let tx = db.begin().await.unwrap();
    assert!(anywhere(&tx, k_within).await);
    tx.rollback().await.unwrap();
    assert!(anywhere(db, k_within).await);
}

const DDL: &str = "CREATE TABLE IF NOT EXISTS keelson_tx (k BIGINT PRIMARY KEY)";

#[tokio::test]
async fn sqlite_transaction_semantics() {
    let path = std::env::temp_dir().join(format!(
        "keelson-sqlx-tx-{}-{}.db",
        std::process::id(),
        next_key()
    ));
    let pool = keelson_sqlx::sqlite::Pool::connect(&format!("sqlite://{}", path.display()))
        .await
        .unwrap();
    pool.execute(Statement::new(DDL, vec![])).await.unwrap();
    tx_suite(&pool).await;
}

#[cfg(feature = "live-docker")]
mod live_engines {
    use super::*;

    #[tokio::test]
    async fn psql_transaction_semantics() {
        let url = tokio::task::spawn_blocking(|| keelson_sqlcheck::live::psql_url().to_owned())
            .await
            .unwrap();
        let pool = keelson_sqlx::psql::Pool::connect(&url).await.unwrap();
        pool.execute(Statement::new(DDL, vec![])).await.unwrap();
        tx_suite(&pool).await;
    }

    #[tokio::test]
    async fn mysql_transaction_semantics() {
        let url = tokio::task::spawn_blocking(|| keelson_sqlcheck::live::mysql_url().to_owned())
            .await
            .unwrap();
        let pool = keelson_sqlx::mysql::Pool::connect(&url).await.unwrap();
        pool.execute(Statement::new(DDL, vec![])).await.unwrap();
        tx_suite(&pool).await;
    }
}
