//! **Transactions**: the closure form, savepoints, isolation levels, and the
//! refusals.
//!
//!     cargo run -p keelson-examples --example transactions
//!
//! Two properties worth knowing up front:
//!
//! - A `Transaction` carries **no lifetime parameter**. It owns its
//!   connection, and `commit`/`rollback` consume it -- so it can be stored,
//!   returned from a function and passed around like any other value.
//! - keelson-exec owns the transaction SQL (`BEGIN`, `SAVEPOINT`, `COMMIT`),
//!   not the backend. `TxOptions::plan` hands you the exact statements a
//!   `begin_with` will run, so "what does keelson actually send?" is
//!   answerable without a packet capture.

use keelson::exec::{
    Access, Begin as _, BeginExt as _, BeginWith as _, ExecError, Execute as _, Executor, Family,
    Isolation, SqliteBegin, TxConflict, TxOptions,
};
use keelson::prelude::*;
use keelson::sqlite::{self, arg, f, insert, quote, select};
use keelson_examples::Sandbox;

#[tokio::main]
async fn main() -> Result<(), ExecError> {
    let sandbox = Sandbox::seeded().await?;
    let db = &sandbox.db;

    // ── the recommended shape ───────────────────────────────────────────
    //
    // `within` commits on `Ok` and rolls back on `Err`. The closure gets
    // `&Transaction`, so it *cannot* commit or consume it -- neither a
    // forgotten commit nor a double commit is expressible here.
    let id: i64 = db
        .within(async |tx| {
            insert_tag(tx, "transactional").await?;
            sqlite::select((
                select::columns(f("max", quote("id"))),
                select::from(quote("tags")),
            ))
            .fetch_scalar(tx)
            .await
        })
        .await?;
    println!("── within\n  committed tag id {id}");
    assert_eq!(count_tags(db).await?, 4);

    // ── rollback ────────────────────────────────────────────────────────
    //
    // An `Err` out of the closure rolls the whole thing back. Note the error
    // type: `within` wants `E: From<ExecError>`, so an application error type
    // works as long as it can carry a database failure.
    let outcome: Result<(), ExecError> = db
        .within(async |tx| {
            insert_tag(tx, "doomed").await?;
            // The work so far is real *inside* the transaction...
            assert_eq!(count_tags(tx).await?, 5);
            Err(ExecError::other("business rule said no"))
        })
        .await;
    assert!(outcome.is_err());
    // ...and gone outside it.
    println!("\n── rollback\n  {}", outcome.unwrap_err());
    assert_eq!(count_tags(db).await?, 4);

    // ── explicit begin / commit ─────────────────────────────────────────
    //
    // When the transaction's extent is not a lexical block -- it is held in a
    // struct, or handed to another task -- take it by hand. `commit` and
    // `rollback` consume it, so using it afterwards does not compile.
    let tx = db.begin().await?;
    insert_tag(&tx, "explicit").await?;
    tx.commit().await?;
    assert_eq!(count_tags(db).await?, 5);

    // ── savepoints ──────────────────────────────────────────────────────
    //
    // A savepoint is a closure too: `Ok` releases it, `Err` rolls back to it,
    // and the outer transaction lives on. There is no handle to leak, and
    // nesting is unbounded.
    db.within(async |tx| {
        insert_tag(tx, "kept").await?;

        let attempted: Result<(), ExecError> = tx
            .savepoint(async |tx| {
                insert_tag(tx, "discarded").await?;
                Err(ExecError::other("inner step failed"))
            })
            .await;
        assert!(attempted.is_err());

        // The outer transaction is still usable, and still holds "kept".
        insert_tag(tx, "also kept").await?;
        Ok::<_, ExecError>(())
    })
    .await?;

    let names = tag_names(db).await?;
    println!("\n── savepoint\n  {names:?}");
    assert!(names.contains(&"kept".to_owned()));
    assert!(names.contains(&"also kept".to_owned()));
    assert!(!names.contains(&"discarded".to_owned()));

    // ── isolation and access mode ───────────────────────────────────────
    //
    // `TxOptions` asks; the engine's rules decide. SERIALIZABLE is SQLite's
    // only level, so this is accepted.
    db.begin_with(TxOptions::new().isolation(Isolation::Serializable))
        .await?
        .rollback()
        .await?;

    // SQLite's own begin modes, which no other engine has -- and which are
    // therefore only on `TxOptions` for SQLite.
    db.begin_with(TxOptions::new().sqlite_begin(SqliteBegin::Immediate))
        .await?
        .rollback()
        .await?;

    // ── the refusals ────────────────────────────────────────────────────
    //
    // A level is accepted only when the engine runs the transaction at *that*
    // level. Substituting a neighbouring level would satisfy the SQL standard
    // -- it permits running stricter than asked -- and would still be a lie
    // to the caller, who asked in order to get particular behaviour.
    println!("\n── refusals");
    for (label, opts) in [
        (
            "READ COMMITTED on SQLite",
            TxOptions::new().isolation(Isolation::ReadCommitted),
        ),
        (
            "READ ONLY on SQLite",
            TxOptions::new().access(Access::ReadOnly),
        ),
        (
            "a SQLite begin mode on PostgreSQL",
            TxOptions::new().sqlite_begin(SqliteBegin::Immediate),
        ),
    ] {
        let family = if label.ends_with("PostgreSQL") {
            Family::Postgres
        } else {
            Family::Sqlite
        };
        let err = opts.check(family).unwrap_err();
        println!("  {label}: {err}");
    }
    // PostgreSQL accepts READ UNCOMMITTED and then runs it as READ COMMITTED,
    // so keelson refuses it there too rather than pass on the server's fib.
    assert!(
        TxOptions::new()
            .isolation(Isolation::ReadUncommitted)
            .check(Family::Postgres)
            .is_err()
    );

    // ── what will actually be sent ──────────────────────────────────────
    //
    // `plan` answers without opening a transaction, so a configuration can be
    // pre-flighted -- or printed, as here.
    println!("\n── the statements keelson would send");
    let opts = TxOptions::new()
        .isolation(Isolation::Serializable)
        .access(Access::ReadOnly);
    for family in [Family::Postgres, Family::MySql] {
        println!("  {family:?}: {:?}", opts.plan(family)?);
    }
    println!(
        "  {:?}: {:?}",
        Family::Sqlite,
        TxOptions::new()
            .isolation(Isolation::Serializable)
            .plan(Family::Sqlite)?
    );

    // ── retrying a conflict ─────────────────────────────────────────────
    //
    // Serialisation failures and deadlocks are classified per engine, so a
    // retry loop can be written once. Nothing here conflicts -- this is the
    // shape, not a demonstration of contention.
    let mut attempts = 0;
    loop {
        attempts += 1;
        let outcome: Result<i64, ExecError> = db
            .within(async |tx| {
                sqlite::update((
                    sqlite::update::table(quote("posts")),
                    sqlite::update::set_col("views").to(quote("views").plus(arg(1))),
                    sqlite::update::where_(quote("id").eq(arg(1))),
                ))
                .execute(tx)
                .await?;
                count_tags(tx).await
            })
            .await;

        match outcome {
            Ok(_) => break,
            // `TxConflict::of` says whether an error is worth retrying, and
            // which kind it was.
            Err(e)
                if matches!(TxConflict::of(&e), Some(TxConflict::Serialization))
                    && attempts < 5 =>
            {
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    println!("\n── retry loop\n  succeeded after {attempts} attempt(s)");

    println!("\nok");
    Ok(())
}

async fn insert_tag(db: &dyn Executor, name: &str) -> Result<(), ExecError> {
    sqlite::insert((
        insert::into(quote("tags")).columns(["name"]),
        insert::values(arg(name.to_owned())),
    ))
    .execute(db)
    .await
    .map(|_| ())
}

async fn count_tags(db: &dyn Executor) -> Result<i64, ExecError> {
    sqlite::select((
        select::columns(f("count", quote("id"))),
        select::from(quote("tags")),
    ))
    .fetch_scalar(db)
    .await
}

async fn tag_names(db: &dyn Executor) -> Result<Vec<String>, ExecError> {
    sqlite::select((
        select::columns(quote("name")),
        select::from(quote("tags")),
        select::order_by(quote("id")),
    ))
    .fetch_scalars(db)
    .await
}
