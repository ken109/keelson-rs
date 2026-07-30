//! The promise `docs/type-mappings.md` makes, proved against a real engine.
//!
//! A column's generated Rust type can be replaced with your own; what a
//! replacement must satisfy is `keelson_exec::Bind` (`ToValue + FromValue +
//! Send + 'static`), asserted in generated code by
//! `const _: () = keelson_exec::assert_bind::<T>();`. `#[derive(Bind)]` is how
//! a newtype satisfies it, and this test walks a derived newtype the whole
//! way: built by a dialect crate, bound as a statement argument, stored by
//! SQLite, selected back, and mapped by a derived `FromRow` — the same path an
//! application takes.
//!
//! Real SQLite, in-process, so plain `cargo test` runs it. The engine is not
//! the interesting variable here: `Bind` is defined against `Value`, never
//! against a driver, so a type that binds does so on every backend or on none.

use keelson_core::{Bind, FromRow, Value};
use keelson_exec::{Execute as _, Executor as _, Statement};
use keelson_sqlite::{Chain as _, Query as _, arg, insert, quote, select};
use keelson_sqlx::sqlite::Pool;
use uuid::Uuid;

/// The three overrides. Each is what a `[[types.override]]` in a keelson-gen
/// configuration would point a column at, and each satisfies the bound the
/// generator asserts — the `assert_bind` lines below are the line keelson-gen
/// emits per override, verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Bind)]
struct AccountId(i64);

#[derive(Debug, Clone, PartialEq, Bind)]
struct Email(String);

#[derive(Debug, Clone, Copy, PartialEq, Bind)]
struct Token(Uuid);

const _: () = keelson_exec::assert_bind::<AccountId>();
const _: () = keelson_exec::assert_bind::<Email>();
const _: () = keelson_exec::assert_bind::<Token>();

/// The row those columns come back as: overridden types in the fields, one
/// renamed column, and a flattened nested struct.
#[derive(Debug, PartialEq, FromRow)]
struct Account {
    id: AccountId,
    #[keelson(rename = "email_address")]
    email: Email,
    token: Option<Token>,
    #[keelson(flatten)]
    audit: Audit,
}

#[derive(Debug, PartialEq, FromRow)]
struct Audit {
    created_by: AccountId,
}

const DDL: &str = "CREATE TABLE accounts (
    id INTEGER PRIMARY KEY,
    email_address TEXT NOT NULL,
    token TEXT,
    created_by INTEGER NOT NULL)";

async fn pool() -> Pool {
    let path = std::env::temp_dir().join(format!(
        "keelson-macros-e2e-{}-{:?}.db",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_file(&path);
    let pool = Pool::connect(&format!("sqlite://{}", path.display()))
        .await
        .expect("opening the SQLite database");
    pool.execute(Statement::new(DDL, vec![])).await.unwrap();
    pool
}

#[tokio::test]
async fn a_derived_newtype_binds_and_reads_back_through_a_real_engine() {
    let db = pool().await;
    let token = Token(Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap());

    // Bound as arguments, by value, with no conversion at the call site: this
    // is what "usable anywhere the inner type is" means.
    let q = keelson_sqlite::insert((
        insert::into("accounts").columns(["id", "email_address", "token", "created_by"]),
        insert::values((
            arg(AccountId(1)),
            arg(Email("ada@example.com".to_owned())),
            arg(token),
            arg(AccountId(9)),
        )),
    ));
    assert_eq!(q.execute(&db).await.unwrap().rows_affected, 1);

    // A NULL for the nullable overridden column, through `Option<Token>`.
    let q = keelson_sqlite::insert((
        insert::into("accounts").columns(["id", "email_address", "token", "created_by"]),
        insert::values((
            arg(AccountId(2)),
            arg(Email("kay@example.com".to_owned())),
            arg(None::<Token>),
            arg(AccountId(9)),
        )),
    ));
    q.execute(&db).await.unwrap();

    // The engine really did store the newtype's inner form: a UUID column on
    // SQLite is the hyphenated text `docs/type-mappings.md` pins.
    let stored: String = keelson_sqlite::select((
        select::columns(quote("token")),
        select::from(quote("accounts")),
        select::where_(quote("id").eq(arg(AccountId(1)))),
    ))
    .fetch_scalar(&db)
    .await
    .unwrap();
    assert_eq!(stored, "550e8400-e29b-41d4-a716-446655440000");

    // And back out, mapped by the derive, into the overridden types.
    let accounts: Vec<Account> = keelson_sqlite::select((
        select::columns((
            quote("id"),
            quote("email_address"),
            quote("token"),
            quote("created_by"),
        )),
        select::from(quote("accounts")),
        select::order_by(quote("id")),
    ))
    .fetch_all(&db)
    .await
    .unwrap();

    assert_eq!(
        accounts,
        vec![
            Account {
                id: AccountId(1),
                email: Email("ada@example.com".to_owned()),
                token: Some(token),
                audit: Audit {
                    created_by: AccountId(9)
                },
            },
            Account {
                id: AccountId(2),
                email: Email("kay@example.com".to_owned()),
                token: None,
                audit: Audit {
                    created_by: AccountId(9)
                },
            },
        ]
    );
}

/// A newtype is a scalar too: `fetch_scalar` reads a single column into one.
#[tokio::test]
async fn a_derived_newtype_reads_as_a_scalar() {
    let db = pool().await;
    keelson_sqlite::insert((
        insert::into("accounts").columns(["id", "email_address", "created_by"]),
        insert::values((arg(AccountId(3)), arg(Email("g@h".to_owned())), arg(7i64))),
    ))
    .execute(&db)
    .await
    .unwrap();

    let id: AccountId = keelson_sqlite::select((
        select::columns(quote("id")),
        select::from(quote("accounts")),
        select::where_(quote("email_address").eq(arg(Email("g@h".to_owned())))),
    ))
    .fetch_scalar(&db)
    .await
    .unwrap();
    assert_eq!(id, AccountId(3));
}

/// The decode failure a wrong override produces at runtime still names the
/// column — the derive adds no layer that could swallow it.
#[tokio::test]
async fn a_decode_failure_through_the_engine_names_the_column() {
    let db = pool().await;
    keelson_sqlite::insert((
        insert::into("accounts").columns(["id", "email_address", "token", "created_by"]),
        insert::values((
            arg(AccountId(4)),
            arg(Email("i@j".to_owned())),
            arg("not-a-uuid"),
            arg(7i64),
        )),
    ))
    .execute(&db)
    .await
    .unwrap();

    let e = keelson_sqlite::select((
        select::columns((
            quote("id"),
            quote("email_address"),
            quote("token"),
            quote("created_by"),
        )),
        select::from(quote("accounts")),
        select::where_(quote("id").eq(arg(AccountId(4)))),
    ))
    .fetch_one::<Account>(&db)
    .await
    .unwrap_err();
    assert!(
        e.to_string().starts_with("column \"token\": "),
        "expected the column to be named, got: {e}"
    );
}

/// What `Row`-level access sees underneath: the argument list a built query
/// carries holds the newtype's inner `Value`, not a wrapper. This is why one
/// `Bind` impl is enough for every backend.
#[test]
fn the_bound_argument_is_the_inner_value() {
    // Only the argument list is asserted: what the SQL text must look like is
    // keelson-sqlcheck's question, not this crate's, and pasting builder
    // output as an expectation is exactly what the house rules forbid.
    let (_sql, args) = keelson_sqlite::insert((
        insert::into("accounts").columns(["id", "email_address"]),
        insert::values((arg(AccountId(5)), arg(Email("k@l".to_owned())))),
    ))
    .build()
    .unwrap();
    assert_eq!(args, vec![Value::I64(5), Value::Text("k@l".into())]);
}
