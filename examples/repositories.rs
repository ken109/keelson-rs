//! **Repositories and usecases: who owns the transaction.** The layered
//! shape, where the same repository call has to work standalone *and* inside
//! a usecase's transaction.
//!
//!     cargo run -p keelson-examples --example repositories
//!
//! The answer keelson pushes you toward, in one line each:
//!
//! - **The executor is a parameter, never a field.** A repository that
//!   captured the pool cannot join a transaction it did not open — it would
//!   run on a different connection. This is the trap the last section
//!   demonstrates.
//! - **A repository takes `&dyn Executor`** and opens no scope of its own. It
//!   stays object-safe, so `Arc<dyn UserRepository>` works, and a single
//!   statement is already atomic in every engine.
//! - **The usecase owns the boundary**, with `within`. That is the layer that
//!   knows what "all or nothing" means for the business operation.
//! - **A multi-statement unit of work takes `&(impl Atomic + ?Sized)`** and is
//!   atomic wherever it is called: a transaction standalone, a savepoint
//!   inside the usecase. That is what `atomic` is for, and it is *not* what a
//!   one-statement CRUD method needs.
//!
//! The last two points pull against each other, so the file shows the fork
//! both ways: `UserRepository` keeps `Arc<dyn …>` and pays for it in boxed
//! futures and one named lifetime, while `UserStore` gives `dyn` up and is
//! plain `async fn` — no boxing, no lifetime, and a scope allowed in the
//! signature, because a trait with a generic method has no vtable to build.
//! `tests/compile_fail/repository_behind_dyn.rs` pins the error you get for
//! trying to have both.

use std::sync::Arc;

use keelson::exec::{ExecError, ExecFuture, Executor};
use keelson::models::set;
use keelson::prelude::*;
use keelson::sqlite::{arg, f, insert, quote, select};
use keelson::sqlx::sqlite::Pool;
use keelson_examples::Sandbox;
use keelson_examples::models::users;

/// What a repository method returns: `Result<T, ExecError>`, in the boxed
/// future keelson's own hook payloads use. `ExecFuture<'a, T>` is
/// `Pin<Box<dyn Future<Output = T> + Send + 'a>>` and nothing more.
type Fut<'a, T> = ExecFuture<'a, Result<T, ExecError>>;

// ── the port ────────────────────────────────────────────────────────────
//
// Note what is *not* here: a pool, a connection, a transaction. The executor
// arrives per call, which is the whole reason the same method can run inside
// a caller's transaction.
//
// The boxed future, and with it the `'a`, are the price of `Arc<dyn …>`:
// `async fn` in a trait returns an anonymous future type, which has no
// vtable. `'a` is one lifetime, and it is the *same* borrow throughout --
// `self` and the executor, for the duration of one call. If your application
// does not need trait objects, none of this is necessary: `UserStore` below
// is the same idea in plain `async fn`, naming no lifetime at all.

trait UserRepository: Send + Sync {
    fn find<'a>(&'a self, db: &'a dyn Executor, id: i64) -> Fut<'a, users::User>;

    fn set_email<'a>(&'a self, db: &'a dyn Executor, id: i64, email: &'a str) -> Fut<'a, ()>;
}

/// **The same port with `dyn` given up.** `async fn` in a trait has been
/// stable since Rust 1.75; what it is not is dyn-compatible. Trade the
/// `Arc<dyn …>` away and the ceremony goes with it — no boxing, no
/// `Pin<Box<…>>`, no lifetime to name, and the body of each method is
/// unchanged.
///
/// What you *gain* is the second method: a scope can appear in the signature,
/// because there is no vtable to build. The unit of work moves into the port
/// instead of living beside it, and it is still atomic wherever it is called.
///
/// What you give up, beyond `Arc<dyn …>`: every holder of this port is
/// generic in it (`struct RetireUser<S: UserStore>`), a heterogeneous
/// collection of repositories is not expressible, and a test double is a
/// second type parameter rather than a second `Arc`. One more caveat worth
/// knowing: `async fn` in a trait does not promise a `Send` future, so a
/// usecase that gets `tokio::spawn`ed needs `#[trait_variant::make(Send)]` or
/// a hand-written `-> impl Future<…> + Send`.
trait UserStore {
    async fn count_active(&self, db: &dyn Executor) -> Result<i64, ExecError>;

    /// The method `UserRepository` could not have.
    async fn deactivate(
        &self,
        db: &(impl Atomic + ?Sized),
        id: i64,
        reason: &str,
    ) -> Result<(), ExecError>;
}

// ── the adapter ─────────────────────────────────────────────────────────

struct SqlUserRepository;

impl UserRepository for SqlUserRepository {
    fn find<'a>(&'a self, db: &'a dyn Executor, id: i64) -> Fut<'a, users::User> {
        Box::pin(async move { users::table().query(users::id().eq(id)).one(db).await })
    }

    fn set_email<'a>(&'a self, db: &'a dyn Executor, id: i64, email: &'a str) -> Fut<'a, ()> {
        Box::pin(async move {
            let done = users::table()
                .update(
                    users::Setter {
                        email: set(email.to_owned()),
                        ..Default::default()
                    },
                    users::id().eq(id),
                )
                .exec(db)
                .await?;
            if done.rows_affected == 0 {
                return Err(ExecError::other(format!("no user {id}")));
            }
            Ok(())
        })
    }
}

impl UserStore for SqlUserRepository {
    async fn count_active(&self, db: &dyn Executor) -> Result<i64, ExecError> {
        keelson::sqlite::select((
            select::columns(f("count", quote("id"))),
            select::from(quote("users")),
            select::where_(quote("is_active").eq(arg(true))),
        ))
        .fetch_scalar(db)
        .await
    }

    async fn deactivate(
        &self,
        db: &(impl Atomic + ?Sized),
        id: i64,
        reason: &str,
    ) -> Result<(), ExecError> {
        // The same unit of work as below, now inside the port.
        deactivate_user(db, id, reason).await
    }
}

/// A usecase over the dyn-free port: generic in its repository rather than
/// holding an `Arc<dyn …>`. This is where the trade is actually paid — the
/// type parameter propagates to everything that constructs one.
struct RetireUser<S: UserStore> {
    users: S,
}

impl<S: UserStore> RetireUser<S> {
    async fn run(&self, db: &Pool, id: i64) -> Result<i64, ExecError> {
        db.within(async |tx| {
            // A savepoint, because the port took a scope and the usecase
            // handed it a transaction.
            self.users.deactivate(tx, id, "retired by request").await?;
            self.users.count_active(tx).await
        })
        .await
    }
}

// ── a unit of work that is more than one statement ──────────────────────
//
// This one *does* take a scope, because it has something to lose: two
// statements that must not half-apply. It is written once and is atomic
// wherever it is called -- a transaction when nothing is open, a savepoint
// when the usecase already opened one.
//
// It cannot be a method on `UserRepository` without giving up
// `Arc<dyn UserRepository>` (see the compile_fail case), which is the honest
// trade: object safety or a scope in the signature, not both.
async fn deactivate_user(
    db: &(impl Atomic + ?Sized),
    id: i64,
    reason: &str,
) -> Result<(), ExecError> {
    db.atomic(async |tx| {
        let done = users::table()
            .update(
                users::Setter {
                    is_active: set(false),
                    ..Default::default()
                },
                users::id().eq(id),
            )
            .exec(tx)
            .await?;
        if done.rows_affected == 0 {
            return Err(ExecError::other(format!("no user {id}")));
        }
        keelson::sqlite::insert((
            insert::into(quote("audit_logs")).columns(["entity", "entity_id", "note"]),
            insert::values((arg("users"), arg(id), arg(format!("deactivated: {reason}")))),
        ))
        .execute(tx)
        .await?;
        Ok(())
    })
    .await
}

// ── the usecase ─────────────────────────────────────────────────────────
//
// It holds ports, not connections, and it is where the transaction begins.

struct ChangeEmail {
    users: Arc<dyn UserRepository>,
}

impl ChangeEmail {
    /// One business operation, one transaction. `within` and not `atomic`:
    /// this layer is *asserting* that a transaction starts here, and the
    /// assertion is checked -- handing this a `&Transaction` would not
    /// compile.
    async fn run(&self, db: &Pool, id: i64, email: &str) -> Result<(), ExecError> {
        db.within(async |tx| {
            self.users.set_email(tx, id, email).await?;
            keelson::sqlite::insert((
                insert::into(quote("audit_logs")).columns(["entity", "entity_id", "note"]),
                insert::values((arg("users"), arg(id), arg(format!("email -> {email}")))),
            ))
            .execute(tx)
            .await?;
            Ok::<_, ExecError>(())
        })
        .await
    }
}

#[tokio::main]
async fn main() -> Result<(), ExecError> {
    let sandbox = Sandbox::seeded().await?;
    let db = &sandbox.db;
    let repo: Arc<dyn UserRepository> = Arc::new(SqlUserRepository);

    // ── 1. the repository, standalone ───────────────────────────────────
    //
    // Given the pool, it runs on its own. One statement, so it is already
    // all-or-nothing without anybody saying so.
    repo.set_email(db, 1, "ada@lovelace.example").await?;
    assert_eq!(
        repo.find(db, 1).await?.email.as_deref(),
        Some("ada@lovelace.example")
    );
    println!("── standalone\n  {:?}", repo.find(db, 1).await?.email);

    // ── 2. the same repository, inside a usecase's transaction ──────────
    //
    // Nothing about the repository changed. It was handed a `&Transaction`
    // instead of a pool, and `&dyn Executor` accepts both.
    let usecase = ChangeEmail {
        users: Arc::clone(&repo),
    };
    usecase.run(db, 1, "ada@analytical.example").await?;
    println!("── inside a usecase\n  {:?}", repo.find(db, 1).await?.email);
    assert_eq!(
        repo.find(db, 1).await?.email.as_deref(),
        Some("ada@analytical.example")
    );

    // And the usecase's boundary is real: a failure anywhere in it takes the
    // repository's write with it.
    let failed: Result<(), ExecError> = db
        .within(async |tx| {
            repo.set_email(tx, 1, "never@example.com").await?;
            Err(ExecError::other("a later step refused"))
        })
        .await;
    assert!(failed.is_err());
    assert_eq!(
        repo.find(db, 1).await?.email.as_deref(),
        Some("ada@analytical.example"),
        "the repository's write rolled back with the usecase"
    );
    println!("── a failed usecase\n  {}", failed.unwrap_err());

    // ── 3. the multi-statement unit of work, both ways ──────────────────
    //
    // Standalone: `atomic` is a transaction, and both statements land.
    deactivate_user(db, 3, "too young").await?;
    assert!(!repo.find(db, 3).await?.is_active);
    // Two audit rows so far: the usecase's, and this one. The failed
    // usecase's row is not among them.
    assert_eq!(audit_count(db).await?, 2);

    // Nested: `atomic` is a savepoint. Its failure costs its own two
    // statements and nothing else -- the usecase's transaction is still
    // open, still holds its own work, and decides for itself.
    db.within(async |tx| {
        repo.set_email(tx, 2, "grace@hopper.example").await?;

        let refused = deactivate_user(tx, 999, "no such user").await;
        assert!(refused.is_err());

        // Still usable, and the email above is still pending.
        deactivate_user(tx, 2, "retired").await
    })
    .await?;

    let grace = repo.find(db, 2).await?;
    println!(
        "── nested unit of work\n  {:?} active={}",
        grace.email, grace.is_active
    );
    assert_eq!(grace.email.as_deref(), Some("grace@hopper.example"));
    assert!(!grace.is_active);

    // ── 4. the same application with `dyn` given up ─────────────────────
    //
    // `UserStore` is the port written as plain `async fn`: no boxed future,
    // no `Pin<Box<…>>`, no lifetime named anywhere. In exchange it is not a
    // trait object, so `RetireUser` is generic in it — and *because* there is
    // no vtable, the port can take a scope, which is how the unit of work
    // moved inside it.
    let retire = RetireUser {
        users: SqlUserRepository,
    };
    let still_active = retire.run(db, 1).await?;
    println!("── dyn given up\n  {still_active} users still active");
    assert!(!repo.find(db, 1).await?.is_active);
    // Ada, Grace and Kid are all deactivated by now.
    assert_eq!(still_active, 0);

    // ── 5. the trap: a repository that captured the pool ────────────────
    //
    // This is the shape to avoid, and it fails quietly rather than loudly.
    // `PoolBound` holds a `Pool`, so every call checks out *its own*
    // connection -- which is not the connection the caller's transaction is
    // running on.
    struct PoolBound {
        pool: Pool,
    }

    impl PoolBound {
        async fn count_users(&self) -> Result<i64, ExecError> {
            keelson::sqlite::select((
                select::columns(f("count", quote("id"))),
                select::from(quote("users")),
            ))
            .fetch_scalar(&self.pool)
            .await
        }
    }

    let detached = PoolBound { pool: db.clone() };

    db.within(async |tx| {
        users::table()
            .insert(users::Setter {
                name: set("Edsger"),
                ..Default::default()
            })
            .one(tx)
            .await?;

        // Four users on this connection...
        let inside = count_users(tx).await?;
        // ...and three on the detached repository's own connection, because
        // this transaction has not committed. Nothing errors; the answer is
        // just wrong for the caller's purposes.
        let outside = detached.count_users().await?;
        println!("── a repository that captured the pool\n  inside {inside}, outside {outside}");
        assert_eq!((inside, outside), (4, 3));
        Ok::<_, ExecError>(())
    })
    .await?;

    // A *write* from the detached repository would not be quiet: it would sit
    // on the write lock this transaction holds until SQLite gave up
    // (`TxConflict::Busy`), which is the same bug wearing a different hat.

    println!("\nok");
    Ok(())
}

async fn audit_count(db: &dyn Executor) -> Result<i64, ExecError> {
    keelson::sqlite::select((
        select::columns(f("count", quote("id"))),
        select::from(quote("audit_logs")),
    ))
    .fetch_scalar(db)
    .await
}

async fn count_users(db: &dyn Executor) -> Result<i64, ExecError> {
    keelson::sqlite::select((
        select::columns(f("count", quote("id"))),
        select::from(quote("users")),
    ))
    .fetch_scalar(db)
    .await
}
