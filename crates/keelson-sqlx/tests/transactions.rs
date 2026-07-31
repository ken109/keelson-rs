//! Transaction semantics against real engines: commit persists,
//! drop-without-commit rolls back, explicit rollback rolls back, savepoints
//! nest, and the closure form owns its outcome. One generic suite, run
//! against SQLite always and PostgreSQL/MySQL behind `live-docker` — the
//! suite itself only sees `&dyn Begin`, which is the point.
//!
//! Isolation levels are the other half of this file, and they are
//! deliberately **not** one generic suite: this is where the three engines
//! stop agreeing, so each gets its own test saying what it actually does.
//! Every one of them is a behavioural proof — two concurrent transactions and
//! an anomaly that either happens or does not — rather than an assertion
//! about the SQL text, which lives in keelson-exec's unit tests.

use std::sync::atomic::{AtomicI64, Ordering};

use keelson_core::Value;
use keelson_exec::{
    Atomic, Begin, BeginExt as _, BeginWith as _, ExecError, ExecResult, Executor, Family,
    Isolation, SqliteBegin, Statement, TxConflict, TxOptions,
};

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

    // `atomic`: one helper, written once, atomic wherever it is called. At
    // the top it *is* the transaction; inside one it is a savepoint, so its
    // failure costs its own block and nothing the caller did.
    async fn unit(db: &(impl Atomic + ?Sized), k: i64, fail: bool) -> Result<(), ExecError> {
        db.atomic(async |tx| {
            insert(tx, k).await?;
            if fail {
                return Err(ExecError::other("the unit of work refused"));
            }
            Ok(())
        })
        .await
    }

    let k_top = next_key();
    unit(db, k_top, false).await.unwrap();
    assert!(present(db, k_top).await, "at the top, Ok commits");

    let k_top_lost = next_key();
    assert!(unit(db, k_top_lost, true).await.is_err());
    assert!(
        !present(db, k_top_lost).await,
        "at the top, the block is the transaction"
    );

    let k_caller = next_key();
    let k_nested = next_key();
    let k_nested_lost = next_key();
    let tx = db.begin().await.unwrap();
    insert(&tx, k_caller).await.unwrap();
    unit(&tx, k_nested, false).await.unwrap();
    assert!(unit(&tx, k_nested_lost, true).await.is_err());
    // The caller's transaction survived a failed unit of work and decides for
    // itself: here it commits.
    tx.commit().await.unwrap();
    assert!(present(db, k_caller).await);
    assert!(present(db, k_nested).await);
    assert!(
        !present(db, k_nested_lost).await,
        "a nested failure costs only its own block"
    );

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

/// The table the isolation tests read and write: one row, one counter.
const DDL_ISO: &str =
    "CREATE TABLE IF NOT EXISTS keelson_tx_iso (k BIGINT PRIMARY KEY, v BIGINT NOT NULL)";

fn ph_n(family: Family, n: usize) -> String {
    match family {
        Family::Postgres => format!("${n}"),
        Family::MySql => "?".to_owned(),
        Family::Sqlite => format!("?{n}"),
        _ => unreachable!(),
    }
}

async fn seed(db: &dyn Executor, k: i64, v: i64) {
    let f = db.family();
    db.execute(Statement::new(
        format!(
            "INSERT INTO keelson_tx_iso (k, v) VALUES ({}, {})",
            ph_n(f, 1),
            ph_n(f, 2)
        ),
        vec![Value::I64(k), Value::I64(v)],
    ))
    .await
    .unwrap();
}

async fn set_v(db: &dyn Executor, k: i64, v: i64) -> Result<ExecResult, ExecError> {
    let f = db.family();
    db.execute(Statement::new(
        format!(
            "UPDATE keelson_tx_iso SET v = {} WHERE k = {}",
            ph_n(f, 1),
            ph_n(f, 2)
        ),
        vec![Value::I64(v), Value::I64(k)],
    ))
    .await
}

async fn get_v(db: &dyn Executor, k: i64) -> i64 {
    let f = db.family();
    let rows = db
        .fetch(Statement::new(
            format!("SELECT v FROM keelson_tx_iso WHERE k = {}", ph_n(f, 1)),
            vec![Value::I64(k)],
        ))
        .await
        .unwrap();
    rows[0].get_at::<i64>(0).unwrap()
}

#[tokio::test]
async fn sqlite_transaction_semantics() {
    let pool = sqlite_pool().await;
    pool.execute(Statement::new(DDL, vec![])).await.unwrap();
    tx_suite(&pool).await;
}

/// A pool on its own fresh file, in WAL mode.
///
/// WAL is a property of the database file, not of a connection, so setting it
/// once here applies to every connection the pool opens — which is what makes
/// "one writer alongside readers" observable below.
async fn sqlite_pool() -> keelson_sqlx::sqlite::Pool {
    let path = std::env::temp_dir().join(format!(
        "keelson-sqlx-tx-{}-{}.db",
        std::process::id(),
        next_key()
    ));
    let pool = keelson_sqlx::sqlite::Pool::connect(&format!("sqlite://{}", path.display()))
        .await
        .unwrap();
    pool.execute(Statement::new("PRAGMA journal_mode = WAL", vec![]))
        .await
        .unwrap();
    pool.execute(Statement::new(DDL_ISO, vec![])).await.unwrap();
    pool
}

/// Give every connection in the pool a short busy timeout.
///
/// sqlx sets a five-second one on each connection it opens, which is a long
/// time to spend proving that a lock conflict is a lock conflict. Holding
/// several transactions at once forces the pool to open several connections,
/// so the pragma reaches all of them rather than whichever was idle; they are
/// then released, and the yield lets sqlx's release task put them back before
/// the next checkout. Purely a speed measure — with the default timeout these
/// tests still pass, five seconds later.
async fn prime_short_busy_timeout(pool: &keelson_sqlx::sqlite::Pool) {
    let mut held = Vec::new();
    for _ in 0..4 {
        let tx = pool.begin().await.unwrap();
        tx.execute(Statement::new("PRAGMA busy_timeout = 50", vec![]))
            .await
            .unwrap();
        held.push(tx);
    }
    for tx in held {
        tx.commit().await.unwrap();
    }
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

// ---- SQLite: no standard levels, and the begin modes it does have --------

#[tokio::test]
async fn sqlite_refuses_the_levels_it_would_have_to_fake() {
    let pool = sqlite_pool().await;

    for level in [
        Isolation::ReadUncommitted,
        Isolation::ReadCommitted,
        Isolation::RepeatableRead,
    ] {
        let err = pool.begin_with(level.into()).await.unwrap_err().to_string();
        assert!(err.contains(level.as_sql()), "{err}");
        assert!(err.contains("one isolation level"), "{err}");
    }

    // Serializable is accepted, because it is exactly what SQLite runs.
    pool.begin_with(Isolation::Serializable.into())
        .await
        .unwrap()
        .rollback()
        .await
        .unwrap();

    // And there is no per-transaction read-only mode to give.
    let err = pool
        .begin_with(TxOptions::new().read_only())
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("query_only"), "{err}");
}

#[tokio::test]
async fn sqlite_cannot_show_a_non_repeatable_read() {
    // The behavioural half of the refusal above: a plain SQLite transaction
    // already gives repeatable reads, so READ COMMITTED is not something the
    // engine is declining to offer for want of syntax — it cannot offer it.
    let pool = sqlite_pool().await;
    let k = next_key();
    seed(&pool, k, 1).await;

    let tx = pool
        .begin_with(Isolation::Serializable.into())
        .await
        .unwrap();
    assert_eq!(get_v(&tx, k).await, 1);

    // A concurrent writer on another connection commits (WAL lets it).
    set_v(&pool, k, 2).await.unwrap();
    assert_eq!(get_v(&pool, k).await, 2, "the write really did commit");

    assert_eq!(
        get_v(&tx, k).await,
        1,
        "SQLite's reader keeps its snapshot; there is no READ COMMITTED here to ask for"
    );
    tx.commit().await.unwrap();
    assert_eq!(get_v(&pool, k).await, 2);
}

#[tokio::test]
async fn sqlite_begin_modes_change_when_the_write_lock_is_taken() {
    let pool = sqlite_pool().await;
    let k = next_key();
    seed(&pool, k, 1).await;

    // DEFERRED (the default) takes no lock at BEGIN, so two of them coexist.
    let a = pool.begin_with(SqliteBegin::Deferred.into()).await.unwrap();
    let b = pool.begin_with(SqliteBegin::Deferred.into()).await.unwrap();
    assert_eq!(get_v(&a, k).await, 1);
    assert_eq!(get_v(&b, k).await, 1);
    a.rollback().await.unwrap();
    b.rollback().await.unwrap();

    // IMMEDIATE takes the write lock at BEGIN, so the second one loses right
    // there — and loses as a *matchable* conflict, not a string.
    let a = pool
        .begin_with(SqliteBegin::Immediate.into())
        .await
        .unwrap();
    prime_short_busy_timeout(&pool).await;
    let err = pool
        .begin_with(SqliteBegin::Immediate.into())
        .await
        .unwrap_err();
    assert_eq!(TxConflict::of(&err), Some(TxConflict::Busy), "{err}");
    a.rollback().await.unwrap();

    // With the lock released, the same begin succeeds.
    pool.begin_with(SqliteBegin::Immediate.into())
        .await
        .unwrap()
        .rollback()
        .await
        .unwrap();
}

#[cfg(feature = "live-docker")]
mod live_engines {
    use super::*;

    /// Create the two tables, tolerating the race between the tests in this
    /// file: each needs its own pool (sqlx pools are runtime-bound) but they
    /// share one server, and `CREATE TABLE IF NOT EXISTS` is *not* race-free
    /// on PostgreSQL — concurrent creators collide on `pg_type`. Losing that
    /// race means the table exists by the time the error arrives, so one
    /// retry settles it; a genuinely broken DDL still fails loudly.
    async fn ensure_ddl(db: &dyn Executor) {
        for ddl in [DDL, DDL_ISO] {
            if db.execute(Statement::new(ddl, vec![])).await.is_err() {
                db.execute(Statement::new(ddl, vec![])).await.unwrap();
            }
        }
    }

    async fn psql_pool() -> keelson_sqlx::psql::Pool {
        let url = tokio::task::spawn_blocking(|| keelson_sqlcheck::live::psql_url().to_owned())
            .await
            .unwrap();
        let pool = keelson_sqlx::psql::Pool::connect(&url).await.unwrap();
        ensure_ddl(&pool).await;
        pool
    }

    async fn mysql_pool() -> keelson_sqlx::mysql::Pool {
        let url = tokio::task::spawn_blocking(|| keelson_sqlcheck::live::mysql_url().to_owned())
            .await
            .unwrap();
        let pool = keelson_sqlx::mysql::Pool::connect(&url).await.unwrap();
        ensure_ddl(&pool).await;
        pool
    }

    #[tokio::test]
    async fn psql_transaction_semantics() {
        tx_suite(&psql_pool().await).await;
    }

    #[tokio::test]
    async fn mysql_transaction_semantics() {
        tx_suite(&mysql_pool().await).await;
    }

    async fn level(db: &dyn Executor) -> String {
        let sql = match db.family() {
            Family::Postgres => "SHOW transaction_isolation",
            Family::MySql => "SELECT @@transaction_isolation",
            _ => unreachable!(),
        };
        db.fetch(Statement::new(sql, vec![]))
            .await
            .unwrap()
            .swap_remove(0)
            .take_at::<String>(0)
            .unwrap()
    }

    // ---- PostgreSQL ----------------------------------------------------

    #[tokio::test]
    async fn psql_level_lands_on_the_transactions_own_connection() {
        let pool = psql_pool().await;
        let k = next_key();
        seed(&pool, k, 1).await;

        // `a` asks for REPEATABLE READ; `b` is opened from the same pool
        // while `a` is still open, so it is a *different* connection, and it
        // must still be on the server default.
        let a = pool
            .begin_with(Isolation::RepeatableRead.into())
            .await
            .unwrap();
        let b = pool.begin().await.unwrap();
        assert_eq!(level(&a).await, "repeatable read");
        assert_eq!(level(&b).await, "read committed");
        assert_eq!(level(&pool).await, "read committed");

        // Behaviourally, not just by name: both take their snapshot, a third
        // connection commits, and only the default-level one sees it.
        assert_eq!(get_v(&a, k).await, 1);
        assert_eq!(get_v(&b, k).await, 1);
        set_v(&pool, k, 2).await.unwrap();
        assert_eq!(get_v(&a, k).await, 1, "REPEATABLE READ keeps its snapshot");
        assert_eq!(get_v(&b, k).await, 2, "READ COMMITTED does not");
        a.commit().await.unwrap();
        b.commit().await.unwrap();

        // And the connection `a` used goes back to the pool unchanged.
        for _ in 0..8 {
            assert_eq!(level(&pool).await, "read committed");
        }
    }

    #[tokio::test]
    async fn psql_serialization_failures_are_matchable() {
        let pool = psql_pool().await;
        let k = next_key();
        seed(&pool, k, 1).await;

        let a = pool
            .begin_with(Isolation::RepeatableRead.into())
            .await
            .unwrap();
        let b = pool
            .begin_with(Isolation::RepeatableRead.into())
            .await
            .unwrap();
        // Both snapshots predate either write.
        assert_eq!(get_v(&a, k).await, 1);
        assert_eq!(get_v(&b, k).await, 1);

        set_v(&a, k, 10).await.unwrap();
        a.commit().await.unwrap();

        // `b` now tries to write a row that changed under it. PostgreSQL
        // refuses with SQLSTATE 40001 rather than losing the update.
        let err = set_v(&b, k, 20).await.unwrap_err();
        assert_eq!(
            TxConflict::of(&err),
            Some(TxConflict::Serialization),
            "{err}"
        );
        b.rollback().await.unwrap();
        assert_eq!(get_v(&pool, k).await, 10);
    }

    #[tokio::test]
    async fn psql_read_only_transactions_refuse_writes() {
        let pool = psql_pool().await;
        let k = next_key();
        seed(&pool, k, 1).await;

        let tx = pool.begin_with(TxOptions::new().read_only()).await.unwrap();
        assert_eq!(get_v(&tx, k).await, 1);
        let err = set_v(&tx, k, 2).await.unwrap_err();
        assert!(err.to_string().contains("read-only"), "{err}");
        tx.rollback().await.unwrap();
        assert_eq!(get_v(&pool, k).await, 1);
    }

    #[tokio::test]
    async fn psql_refuses_what_it_would_only_pretend_to_honour() {
        let pool = psql_pool().await;
        let err = pool
            .begin_with(Isolation::ReadUncommitted.into())
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("READ UNCOMMITTED"), "{err}");
        let err = pool
            .begin_with(SqliteBegin::Immediate.into())
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("SqliteBegin"), "{err}");
    }

    // ---- MySQL ---------------------------------------------------------

    #[tokio::test]
    async fn mysql_level_lands_on_the_transactions_own_connection() {
        // The one that could really go wrong: MySQL cannot put the level on
        // `START TRANSACTION`, so keelson issues `SET TRANSACTION ISOLATION
        // LEVEL` first. Unqualified, it scopes to the next transaction on
        // that connection — which this test is what proves.
        let pool = mysql_pool().await;
        let k = next_key();
        seed(&pool, k, 1).await;

        let a = pool
            .begin_with(Isolation::ReadCommitted.into())
            .await
            .unwrap();
        let b = pool.begin().await.unwrap();
        assert_eq!(get_v(&a, k).await, 1);
        assert_eq!(get_v(&b, k).await, 1);
        set_v(&pool, k, 2).await.unwrap();
        assert_eq!(get_v(&a, k).await, 2, "READ COMMITTED sees the commit");
        assert_eq!(
            get_v(&b, k).await,
            1,
            "another pooled connection must not have inherited it"
        );
        a.commit().await.unwrap();
        b.commit().await.unwrap();

        // The connection `a` used is back in the pool. Every transaction
        // opened on it afterwards must be REPEATABLE READ again — a
        // `SET SESSION` would fail every round here.
        for round in 0..6 {
            let c = pool.begin().await.unwrap();
            let before = get_v(&c, k).await;
            set_v(&pool, k, before + 1).await.unwrap();
            assert_eq!(
                get_v(&c, k).await,
                before,
                "round {round}: the pooled connection kept a level it was lent"
            );
            c.commit().await.unwrap();
        }
        assert_eq!(level(&pool).await, "REPEATABLE-READ");
    }

    #[tokio::test]
    async fn mysql_read_committed_and_repeatable_read_differ() {
        let pool = mysql_pool().await;
        let k = next_key();
        seed(&pool, k, 1).await;

        let tx = pool
            .begin_with(Isolation::ReadCommitted.into())
            .await
            .unwrap();
        assert_eq!(get_v(&tx, k).await, 1);
        set_v(&pool, k, 2).await.unwrap();
        assert_eq!(get_v(&tx, k).await, 2, "a non-repeatable read, on purpose");
        tx.commit().await.unwrap();

        let tx = pool
            .begin_with(Isolation::RepeatableRead.into())
            .await
            .unwrap();
        assert_eq!(get_v(&tx, k).await, 2);
        set_v(&pool, k, 3).await.unwrap();
        assert_eq!(get_v(&tx, k).await, 2, "and now not");
        tx.commit().await.unwrap();
    }

    #[tokio::test]
    async fn mysql_deadlocks_are_matchable() {
        // InnoDB has no 40001 to raise for a write-write conflict — it takes
        // row locks and reports the unresolvable case as a deadlock. That is
        // MySQL's serialization failure, and it classifies as one.
        let pool = mysql_pool().await;
        let (k1, k2) = (next_key(), next_key());
        seed(&pool, k1, 1).await;
        seed(&pool, k2, 1).await;

        let a = pool
            .begin_with(Isolation::RepeatableRead.into())
            .await
            .unwrap();
        let b = pool
            .begin_with(Isolation::RepeatableRead.into())
            .await
            .unwrap();
        set_v(&a, k1, 2).await.unwrap();
        set_v(&b, k2, 2).await.unwrap();

        // Each now waits on the row the other holds.
        let (ra, rb) = tokio::join!(set_v(&a, k2, 3), set_v(&b, k1, 3));
        let err = match (ra, rb) {
            (Err(e), Ok(_)) | (Ok(_), Err(e)) => e,
            (Ok(_), Ok(_)) => panic!("one of the two had to lose"),
            (Err(a), Err(b)) => panic!("both lost: {a} / {b}"),
        };
        assert_eq!(TxConflict::of(&err), Some(TxConflict::Deadlock), "{err}");

        let _ = a.rollback().await;
        let _ = b.rollback().await;
    }

    #[tokio::test]
    async fn mysql_read_only_transactions_refuse_writes() {
        let pool = mysql_pool().await;
        let k = next_key();
        seed(&pool, k, 1).await;

        let tx = pool
            .begin_with(
                TxOptions::new()
                    .isolation(Isolation::Serializable)
                    .read_only(),
            )
            .await
            .unwrap();
        assert_eq!(get_v(&tx, k).await, 1);
        let err = set_v(&tx, k, 2).await.unwrap_err();
        // 1792, SQLSTATE 25006 — the same class PostgreSQL raises.
        assert!(err.to_string().contains("READ ONLY"), "{err}");
        tx.rollback().await.unwrap();
        assert_eq!(get_v(&pool, k).await, 1);
    }

    #[tokio::test]
    async fn mysql_really_has_read_uncommitted() {
        // The level PostgreSQL only pretends to have. Proving MySQL's is real
        // is what makes refusing PostgreSQL's a considered decision rather
        // than a blanket one.
        let pool = mysql_pool().await;
        let k = next_key();
        seed(&pool, k, 1).await;

        let writer = pool.begin().await.unwrap();
        set_v(&writer, k, 99).await.unwrap(); // deliberately not committed

        let dirty = pool
            .begin_with(Isolation::ReadUncommitted.into())
            .await
            .unwrap();
        assert_eq!(
            get_v(&dirty, k).await,
            99,
            "a dirty read, which is the point"
        );

        let clean = pool
            .begin_with(Isolation::ReadCommitted.into())
            .await
            .unwrap();
        assert_eq!(get_v(&clean, k).await, 1, "and one level up, no dirty read");

        dirty.rollback().await.unwrap();
        clean.rollback().await.unwrap();
        writer.rollback().await.unwrap();
        assert_eq!(get_v(&pool, k).await, 1);
    }

    #[tokio::test]
    async fn mysql_refuses_sqlite_begin_modes() {
        let pool = mysql_pool().await;
        let err = pool
            .begin_with(SqliteBegin::Deferred.into())
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("SqliteBegin"), "{err}");
    }
}
