//! The execution layer's traits, owned by no driver.
//!
//! Layer 1 builds a statement — `Query::build()` hands back `(String,
//! Vec<Value>)`, synchronously, with no driver in the loop. This crate is
//! everything between that pair and a mapped Rust value coming back out of a
//! real database, expressed as traits a backend crate implements:
//!
//! - [`Executor`] — object-safe, three methods, `&self`. A pool, a connection
//!   and a [`Transaction`] all implement it, so `&dyn Executor` is the type
//!   application code, generated models and hooks pass around.
//! - [`Execute`] — the ergonomic verbs, blanket-implemented on every
//!   [`Query`](keelson_core::Query): `q.fetch_all(&db)`, `q.fetch_one(&db)`,
//!   `q.execute(&db)`. Tracing (feature `tracing`) lives here, in the one
//!   funnel every backend flows through.
//! - [`Transaction`] and [`Begin`] — an owned, lifetime-free transaction that
//!   consumes itself on commit/rollback; savepoints are closures.
//!   [`BeginWith`] adds isolation levels and access modes ([`TxOptions`]),
//!   refusing per engine anything that engine would only appear to honour.
//!   [`Atomic`] is the one a *reusable* unit of work takes: a transaction at
//!   the top, a savepoint inside one, and the same call site either way.
//! - [`Row`] and [`FromRow`] — rows decoded once, at the driver seam, into
//!   [`Value`](keelson_core::Value)s; every decode error names its column.
//! - [`RawConnection`] — the seam a backend implements per driver; this crate
//!   owns the transaction SQL (`BEGIN`/`COMMIT`/`SAVEPOINT …`) so its
//!   semantics cannot drift between backends.
//!
//! The full design, with every rejected alternative, is `docs/execution.md`.
//! The type-by-type binding contract backends implement against is
//! `docs/type-mappings.md`.
//!
//! No public type here carries a lifetime parameter (the house rule); the only
//! lifetimes are the transient `'_` on futures borrowed from `&self` for one
//! call. Nothing here names a driver: Layer 2's generated models depend on
//! this crate and pick up a backend only in the application's own `Cargo.toml`.
//!
//! # Where this sits
//!
//! Layer 2 of keelson, and the half of it that names no driver. Below:
//! [keelson-core](https://docs.rs/keelson-core) and the dialect crates, which build the
//! `(String, Vec<Value>)` these traits carry. Beside: [keelson-sqlx](https://docs.rs/keelson-sqlx),
//! the backend that implements them over sqlx's PostgreSQL, MySQL and SQLite
//! drivers. Above: [keelson-models](https://docs.rs/keelson-models), whose generated models
//! execute through `&dyn Executor` and therefore through whatever backend the
//! application picked. The whole map is the [keelson](https://docs.rs/keelson) facade crate.
#![warn(missing_docs)]

mod bind;
mod error;
mod execute;
mod executor;
mod row;
mod transaction;

pub use bind::{Bind, assert_bind};
pub use error::ExecError;
pub use execute::Execute;
pub use executor::{
    ExecFuture, ExecResult, Executor, Family, RowStream, Statement, StreamExecutor,
};
pub use row::{Column, FromRow, Row};
pub use transaction::{
    Access, Atomic, Begin, BeginExt, BeginWith, BeginWithExt, ExecHook, ExecLoader, Isolation,
    RawConnection, SqliteBegin, Transaction, TxConflict, TxConflictError, TxOptions,
};

// For `bind_newtype!` expansion only.
#[doc(hidden)]
pub use keelson_core as __core;
