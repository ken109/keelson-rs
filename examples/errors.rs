//! **What failure looks like.** Every error keelson can hand you, provoked on
//! purpose and printed.
//!
//!     cargo run -p keelson-examples --example errors
//!
//! The rule the whole library is written to: *anything unsupported is an
//! explicit, described error* -- never a silent fallback, never a plausible
//! guess, never a clause quietly dropped. This example is that rule, made
//! visible. Nothing below is an unusual code path; it is the ordinary one,
//! with the failing case chosen deliberately.

use keelson::exec::{ExecError, Execute as _, Statement};
use keelson::prelude::*;
use keelson::sqlite::{self, arg, insert, quote, select};
use keelson::{FromRow, Value};
use keelson_examples::Sandbox;

#[derive(Debug, FromRow)]
struct User {
    id: i64,
    // Deliberately not an `Option`, while `users.email` is nullable: reading
    // a NULL into this is the decode error below.
    email: String,
}

#[tokio::main]
async fn main() -> Result<(), ExecError> {
    let sandbox = Sandbox::seeded().await?;
    let db = &sandbox.db;

    // ── 1. build failure: nothing reaches the database ──────────────────
    //
    // `build()` renders in one pass and cannot fail half way, so an
    // unrenderable construct records its error on the writer and `build()`
    // surfaces it once. `ExecError::Build` wraps it, and no statement was
    // sent.
    let broken = sqlite::select((
        select::from(quote("users")),
        select::where_(sqlite::template("age > ? AND name = ?", [])),
    ));
    let err = broken.fetch_all::<User>(db).await.unwrap_err();
    report(
        "a raw template whose arguments do not match its placeholders",
        &err,
    );
    assert!(matches!(err, ExecError::Build(_)));

    // ── 2. decode failure: the column is named ──────────────────────────
    //
    // "Kid" has no email. Reading that NULL into `String` fails, and the
    // error says which column -- the single most useful fact when a query
    // returns 40 columns.
    let err = sqlite::select((
        select::columns((quote("id"), quote("email"))),
        select::from(quote("users")),
        select::where_(quote("name").eq(arg("Kid"))),
    ))
    .fetch_one::<User>(db)
    .await
    .unwrap_err();
    report("a NULL read into a non-Option field", &err);
    assert!(matches!(&err, ExecError::Decode { column, .. } if column == "email"));

    // ── 3. a column that is not in the result set ───────────────────────
    //
    // The error lists the columns that *are* there, because a typo is the bug
    // nine times out of ten.
    let err = sqlite::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        select::limit(1),
    ))
    .fetch_one::<User>(db)
    .await
    .unwrap_err();
    report("a field with no matching column", &err);
    assert!(matches!(err, ExecError::MissingColumn { .. }));

    // ── 4. "one" means one ──────────────────────────────────────────────
    //
    // Zero rows is an error, and so is two. sqlx's `fetch_one` silently takes
    // the first row of many; here that is a data bug worth surfacing.
    let one_user = |name: &'static str| {
        sqlite::select((
            select::columns((quote("id"), quote("email"))),
            select::from(quote("users")),
            select::where_(quote("name").eq(arg(name))),
        ))
    };
    let err = one_user("Nobody").fetch_one::<User>(db).await.unwrap_err();
    report("fetch_one with no rows", &err);
    assert!(matches!(err, ExecError::RowNotFound));

    let err = sqlite::select((
        select::columns((quote("id"), quote("email"))),
        select::from(quote("users")),
    ))
    .fetch_optional::<User>(db)
    .await
    .unwrap_err();
    report("fetch_optional with several rows", &err);
    assert!(matches!(err, ExecError::TooManyRows));

    // ── 5. the database says no ─────────────────────────────────────────
    //
    // A constraint violation is the driver's error, passed through with the
    // engine's own message rather than reinterpreted.
    let err = sqlite::insert((
        insert::into(quote("tags")).columns(["id", "name"]),
        insert::values((arg(1), arg("rust"))), // already exists
    ))
    .execute(db)
    .await
    .unwrap_err();
    report("a UNIQUE constraint violation", &err);
    assert!(matches!(err, ExecError::Driver(_)));

    // ── 6. a value the engine cannot bind ───────────────────────────────
    //
    // SQLite's parameters are signed 64-bit, so a `u64` past `i64::MAX` has
    // no honest binding. It is refused at bind time, naming the type and the
    // backend -- not stringified and hoped for.
    let err = db
        .execute(Statement::new("SELECT ?1", vec![Value::U64(u64::MAX)]))
        .await
        .unwrap_err();
    report("a u64 too large for the engine's parameters", &err);
    assert!(matches!(err, ExecError::UnsupportedValue { .. }));

    // ── 7. errors compose with `?` ──────────────────────────────────────
    //
    // `ExecError` is a normal `std::error::Error`, so an application error
    // type only has to be `From<ExecError>` for `?` to work everywhere --
    // including inside `within`, whose bound is exactly that.
    #[derive(Debug)]
    enum AppError {
        Database(ExecError),
        NotFound(&'static str),
    }
    impl From<ExecError> for AppError {
        fn from(e: ExecError) -> Self {
            AppError::Database(e)
        }
    }
    impl std::fmt::Display for AppError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                AppError::Database(e) => write!(f, "database: {e}"),
                AppError::NotFound(what) => write!(f, "no such user: {what}"),
            }
        }
    }

    let lookup = async |name: &'static str| -> Result<(i64, String), AppError> {
        let user = one_user(name).fetch_optional::<User>(db).await?;
        user.map(|u| (u.id, u.email))
            .ok_or(AppError::NotFound(name))
    };
    println!("── application error type");
    match lookup("Ada").await {
        Ok((id, email)) => println!("  found {id} <{email}>"),
        Err(e) => println!("  {e}"),
    }
    match lookup("Nobody").await {
        Ok((id, email)) => println!("  found {id} <{email}>"),
        Err(e) => println!("  {e}"),
    }
    println!();

    println!("ok");
    Ok(())
}

fn report(what: &str, e: &ExecError) {
    println!("── {what}\n  {e}\n");
}
