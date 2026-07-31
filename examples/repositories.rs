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
//! - **A port is plain `async fn`.** No boxed futures, no lifetime to name,
//!   no `async_trait`. The cost is that it is not a trait object, which most
//!   applications are not using anyway; the appendix at the bottom shows what
//!   changes if you do need one.
//! - **The usecase owns the boundary**, with `within`. That is the layer that
//!   knows what "all or nothing" means for the business operation.
//! - **A multi-statement unit of work takes `&(impl Atomic + ?Sized)`** and is
//!   atomic wherever it is called: a transaction standalone, a savepoint
//!   inside the usecase. A one-statement CRUD method needs no such thing —
//!   one statement is already all-or-nothing in every engine.
//!
//! # A note on `Send`
//!
//! Awaiting an `async fn` from a trait is `Send` when the compiler can see
//! the concrete type — which covers a usecase generic in its repository, an
//! axum handler registered at a concrete instantiation, and `tokio::spawn` of
//! a concrete usecase. What cannot be done today is *promising* it in the
//! abstract: a port method declared `-> impl Future<…> + Send` may not call
//! `atomic`, because the future `AsyncFnOnce` returns cannot be bounded
//! `Send` on stable Rust. Keep the spawning at the composition root, where
//! the types are concrete, and the question does not arise.

use std::sync::Arc;

use keelson::exec::{ExecError, ExecFuture, Executor};
use keelson::models::set;
use keelson::prelude::*;
use keelson::sqlite::{arg, f, insert, quote, select};
use keelson::sqlx::sqlite::Pool;
use keelson_examples::Sandbox;
use keelson_examples::models::users;

// ── the port ────────────────────────────────────────────────────────────
//
// Note what is *not* here: a pool, a connection, a transaction. The executor
// arrives per call, which is the whole reason the same method can run inside
// a caller's transaction.
//
// Note also what the signatures do not have: a lifetime, a `Box`, a
// `Pin<Box<dyn Future<…>>>`, an attribute macro. `async fn` in a trait has
// been stable since Rust 1.75, and this is all it takes.

trait UserStore {
    async fn find(&self, db: &dyn Executor, id: i64) -> Result<users::User, ExecError>;

    async fn set_email(&self, db: &dyn Executor, id: i64, email: &str) -> Result<(), ExecError>;

    async fn count_active(&self, db: &dyn Executor) -> Result<i64, ExecError>;

    /// A unit of work rather than a statement: it writes an audit row *and*
    /// deactivates, and it must not half-apply.
    ///
    /// Taking `&(impl Atomic + ?Sized)` instead of `&dyn Executor` is what
    /// makes it atomic wherever it is called. That is a generic parameter, so
    /// this is also the method a trait-object port could not have — see the
    /// appendix.
    async fn deactivate(
        &self,
        db: &(impl Atomic + ?Sized),
        id: i64,
        reason: &str,
    ) -> Result<(), ExecError>;
}

// ── the adapter ─────────────────────────────────────────────────────────

struct SqlUserRepository;

impl UserStore for SqlUserRepository {
    async fn find(&self, db: &dyn Executor, id: i64) -> Result<users::User, ExecError> {
        users::table().query(users::id().eq(id)).one(db).await
    }

    async fn set_email(&self, db: &dyn Executor, id: i64, email: &str) -> Result<(), ExecError> {
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
    }

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
        db.atomic(async |tx| {
            // Written first on purpose: when the update below finds nothing,
            // this row has to go back too, and the example asserts that it
            // does -- at whichever level the block turned out to be.
            keelson::sqlite::insert((
                insert::into(quote("audit_logs")).columns(["entity", "entity_id", "note"]),
                insert::values((arg("users"), arg(id), arg(format!("deactivated: {reason}")))),
            ))
            .execute(tx)
            .await?;

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
            Ok(())
        })
        .await
    }
}

// ── the usecase ─────────────────────────────────────────────────────────
//
// It holds ports, not connections, and it is where the transaction begins.
// Generic in its repository: that is the price of the port being plain
// `async fn`, and it is paid here and at the composition root, nowhere else.

struct ChangeEmail<S: UserStore> {
    users: S,
}

impl<S: UserStore> ChangeEmail<S> {
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
    let store = SqlUserRepository;

    // ── 1. the repository, standalone ───────────────────────────────────
    //
    // Given the pool, it runs on its own. One statement, so it is already
    // all-or-nothing without anybody saying so.
    store.set_email(db, 1, "ada@lovelace.example").await?;
    assert_eq!(
        store.find(db, 1).await?.email.as_deref(),
        Some("ada@lovelace.example")
    );
    println!("── standalone\n  {:?}", store.find(db, 1).await?.email);

    // ── 2. the same repository, inside a usecase's transaction ──────────
    //
    // Nothing about the repository changed. It was handed a `&Transaction`
    // instead of a pool, and `&dyn Executor` accepts both.
    let usecase = ChangeEmail {
        users: SqlUserRepository,
    };
    usecase.run(db, 1, "ada@analytical.example").await?;
    println!(
        "── inside a usecase\n  {:?}",
        store.find(db, 1).await?.email
    );
    assert_eq!(
        store.find(db, 1).await?.email.as_deref(),
        Some("ada@analytical.example")
    );

    // And the usecase's boundary is real: a failure anywhere in it takes the
    // repository's write with it.
    let failed: Result<(), ExecError> = db
        .within(async |tx| {
            store.set_email(tx, 1, "never@example.com").await?;
            Err(ExecError::other("a later step refused"))
        })
        .await;
    assert!(failed.is_err());
    assert_eq!(
        store.find(db, 1).await?.email.as_deref(),
        Some("ada@analytical.example"),
        "the repository's write rolled back with the usecase"
    );
    println!("── a failed usecase\n  {}", failed.unwrap_err());

    // ── 3. the unit of work, both ways ──────────────────────────────────
    //
    // Standalone: `atomic` is a transaction, and both statements land.
    store.deactivate(db, 3, "too young").await?;
    assert!(!store.find(db, 3).await?.is_active);
    // Two audit rows: the usecase's, and this one. The failed usecase's row
    // is not among them.
    assert_eq!(audit_count(db).await?, 2);

    // Nested: `atomic` is a savepoint. Its failure costs its own two
    // statements and nothing else -- the usecase's transaction is still
    // open, still holds its own work, and decides for itself.
    db.within(async |tx| {
        store.set_email(tx, 2, "grace@hopper.example").await?;

        let refused = store.deactivate(tx, 999, "no such user").await;
        assert!(refused.is_err());

        // Still usable, and the email above is still pending.
        store.deactivate(tx, 2, "retired").await
    })
    .await?;

    let grace = store.find(db, 2).await?;
    println!(
        "── nested unit of work\n  {:?} active={} ({} still active)",
        grace.email,
        grace.is_active,
        store.count_active(db).await?
    );
    assert_eq!(grace.email.as_deref(), Some("grace@hopper.example"));
    assert!(!grace.is_active);
    // Three: the two above, plus the successful `deactivate`. The refused
    // one wrote its audit row and then took it back with the savepoint.
    assert_eq!(audit_count(db).await?, 3);

    // ── 4. the trap: a repository that captured the pool ────────────────
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

    // ── appendix: if you do need a trait object ─────────────────────────
    //
    // Everything above is generic in its repository. If yours has to be
    // swapped at run time -- a plugin, a feature flag choosing between two
    // implementations, a heterogeneous collection of them -- the port becomes
    // a trait object, and this is the whole of what changes.
    let swappable: Arc<dyn UserPort> = Arc::new(BoxedUserRepository);

    db.within(async |tx| swappable.set_email(tx, 1, "ada@byron.example").await)
        .await?;
    println!(
        "── through a trait object\n  {:?}",
        store.find(db, 1).await?.email
    );
    assert_eq!(
        store.find(db, 1).await?.email.as_deref(),
        Some("ada@byron.example")
    );

    println!("\nok");
    Ok(())
}

/// The port from the top of this file, made into a trait object.
///
/// Three things changed, and they are all the same thing: `async fn` in a
/// trait returns an anonymous future type, which has no vtable, so the future
/// is boxed by hand (`ExecFuture<'a, T>` is keelson's alias for
/// `Pin<Box<dyn Future<Output = T> + Send + 'a>>`, the one its hook payloads
/// use), and boxing it means naming the borrow it holds. That `'a` is the
/// only lifetime in this file, and it is one borrow: `self` and the executor,
/// for the duration of one call.
///
/// What cannot come along at all is `deactivate`. A method taking
/// `&(impl Atomic + ?Sized)` is generic, and a generic method has no vtable
/// either — boxing the future does not help. A unit of work that needs a
/// scope lives outside a `dyn` port, as a free function taking the scope.
/// `tests/compile_fail/repository_behind_dyn.rs` pins the error you get for
/// trying anyway.
trait UserPort: Send + Sync {
    fn set_email<'a>(
        &'a self,
        db: &'a dyn Executor,
        id: i64,
        email: &'a str,
    ) -> ExecFuture<'a, Result<(), ExecError>>;
}

/// The adapter for it. A type of its own rather than a second impl on
/// `SqlUserRepository`, because two traits with a `set_email` on one type
/// would make every call on the concrete type ambiguous — which is a small
/// preview of what having both shapes at once costs.
struct BoxedUserRepository;

impl UserPort for BoxedUserRepository {
    fn set_email<'a>(
        &'a self,
        db: &'a dyn Executor,
        id: i64,
        email: &'a str,
    ) -> ExecFuture<'a, Result<(), ExecError>> {
        // The body is unchanged; it is only wrapped.
        Box::pin(SqlUserRepository.set_email(db, id, email))
    }
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
