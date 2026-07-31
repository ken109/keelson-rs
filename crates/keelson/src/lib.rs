#![doc = include_str!("../README.md")]
#![warn(missing_docs)]

// ---------------------------------------------------------------------------
// The crates, re-exported under their short names.
//
// A facade re-exports; it does not wrap. `keelson::psql` *is* `keelson_psql`,
// so every path in the individual crates' documentation is reachable from
// here, nothing is hidden behind a second name, and a project that outgrows
// the facade can switch to the crates directly by deleting one `use` line
// prefix.
// ---------------------------------------------------------------------------

/// Core primitives — `Value`, `Expression`, `Mod`, `Query`, `SqlWriter`,
/// `Dialect`. Always available; every other layer is written in its terms.
pub use keelson_core as core;

/// The PostgreSQL dialect (feature `psql`).
#[cfg(feature = "psql")]
pub use keelson_psql as psql;

/// The MySQL dialect (feature `mysql`).
#[cfg(feature = "mysql")]
pub use keelson_mysql as mysql;

/// The SQLite dialect (feature `sqlite`).
#[cfg(feature = "sqlite")]
pub use keelson_sqlite as sqlite;

/// The execution traits — `Executor`, `Execute`, `Transaction`, `Row`
/// (feature `exec`). Driver-free: a backend implements them.
#[cfg(feature = "exec")]
pub use keelson_exec as exec;

/// The sqlx backend (features `sqlx-psql`, `sqlx-mysql`, `sqlx-sqlite`).
///
/// This is keelson's `keelson-sqlx`, not the `sqlx` crate itself; the pools
/// live in `sqlx::psql`, `sqlx::mysql` and `sqlx::sqlite`, each behind its own
/// feature, and each wraps the corresponding sqlx pool.
#[cfg(any(feature = "sqlx-psql", feature = "sqlx-mysql", feature = "sqlx-sqlite"))]
pub use keelson_sqlx as sqlx;

/// The typed model layer the generator emits against (feature `models`).
#[cfg(feature = "models")]
pub use keelson_models as models;

/// The test-data factory runtime the generator emits against
/// (feature `factory`).
#[cfg(feature = "factory")]
pub use keelson_factory as factory;

// keelson-gen is deliberately absent. It is a CLI you install
// (`cargo install keelson-gen`) and run before the build, not a library an
// application links: it opens a database connection, walks a catalog and writes
// files. Re-exporting it here would put three database drivers and a code
// formatter into the dependency tree of every program that wanted a query
// builder. What it *emits* depends on `models` and `factory`, which are here.

// ---------------------------------------------------------------------------
// The handful of names that are neither dialect- nor layer-specific. Everything
// else a caller needs is dialect-shaped (`arg`, `quote`, `select`) and comes
// from the dialect module, which re-exports this vocabulary in turn.
// ---------------------------------------------------------------------------

pub use keelson_core::{
    Error, Expression, Mod, Query, QueryType, RawQuery, Result, ToValue, Value,
};

/// `#[derive(Bind)]` — a newtype over a bindable type becomes bindable
/// (feature `macros`).
///
/// This is the bound `keelson-gen` asserts for every `[[types.override]]`
/// column, at a named line, so a type override that could not bind is a
/// compile error rather than a runtime one.
#[cfg(feature = "macros")]
pub use keelson_core::Bind;

/// `#[derive(FromRow)]` — map a result row onto a struct by field name
/// (feature `macros`).
///
/// `#[keelson(rename = "…")]` when the column disagrees, `#[keelson(flatten)]`
/// for a nested struct. The trait it implements is
/// `exec::FromRow` (feature `exec`); the two names live in different
/// namespaces, so importing both is fine and is what generated code does.
#[cfg(feature = "macros")]
pub use keelson_core::FromRow;

/// The traits whose methods you would otherwise import one by one.
///
/// Deliberately small: it carries **traits that are used through method
/// syntax** and nothing else, because a prelude that also exported functions
/// would collide with the dialect modules — `select`, `insert`, `arg` and
/// `quote` differ per dialect and belong to it. Import those from the dialect:
///
/// ```
/// use keelson::prelude::*;
/// use keelson::sqlite::{select, arg, quote};
///
/// let q = keelson::sqlite::select((
///     select::from(quote("crew")),
///     select::where_(quote("age").gte(arg(21))),
/// ));
/// let (sql, args) = q.build()?;
/// assert_eq!(sql, r#"SELECT * FROM "crew" WHERE ("age" >= ?1)"#);
/// assert_eq!(args, vec![keelson::Value::I32(21)]);
/// # Ok::<_, keelson::Error>(())
/// ```
pub mod prelude {
    pub use keelson_core::expr::{Chain, IntoExpr};
    pub use keelson_core::{Mod, Query, QueryExtensions};

    #[cfg(feature = "exec")]
    pub use keelson_exec::{Begin, BeginExt, BeginWith, BeginWithExt, Execute, Executor, FromRow};

    #[cfg(feature = "models")]
    pub use keelson_models::{Table, View};
}
