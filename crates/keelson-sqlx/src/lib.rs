//! The sqlx backend for keelson.
//!
//! One crate, three drivers behind features — `psql`, `mysql`, `sqlite`
//! — each exposing a `Pool` (and the transaction machinery via
//! [`keelson_exec::Begin`]) that implements [`keelson_exec::Executor`]. An
//! application constructs a pool here, in `main`, and everything above it —
//! generated models, hooks, plain query code — talks `keelson_exec` traits
//! and never names sqlx.
//!
//! Per-database drivers, not `sqlx::Any`: `Any` erases exactly what
//! `docs/type-mappings.md` requires kept (native `uuid`/temporal/decimal
//! parameter binds on the engines that have them).
//!
//! Each driver module owns two functions that make the type-mappings table
//! executable: `bind_value` (a total map `Value` → driver parameter, per the
//! "binds as" column) and `decode_value` (native row → `Value`, per the
//! column-type column). The round-trip suites in `tests/` are those two
//! functions' tests.
//!
//! # Where this sits
//!
//! The backend half of Layer 2: it implements
//! [keelson-exec](https://docs.rs/keelson-exec)'s traits and is the only crate in keelson
//! that links a database driver. Application code names it once, in `main`,
//! to build a pool; everything above talks `keelson_exec` traits. The
//! statements it runs come from a Layer 1 dialect ([keelson-psql](https://docs.rs/keelson-psql),
//! [keelson-mysql](https://docs.rs/keelson-mysql), [keelson-sqlite](https://docs.rs/keelson-sqlite)) or
//! from a generated model. The whole map is the [keelson](https://docs.rs/keelson) facade
//! crate.
#![warn(missing_docs)]
#![forbid(unsafe_code)]

#[cfg(feature = "mysql")]
pub mod mysql;
#[cfg(feature = "psql")]
pub mod psql;
#[cfg(feature = "sqlite")]
pub mod sqlite;

#[cfg(any(feature = "psql", feature = "mysql", feature = "sqlite"))]
mod common {
    use keelson_exec::ExecError;

    /// A driver refusal while reading a column, with the column named.
    pub(crate) fn decode_err(column: &str, e: sqlx::Error) -> ExecError {
        ExecError::Decode {
            column: column.to_owned(),
            source: keelson_core::Error::other(e.to_string()),
        }
    }

    /// A column type whose decode needs a cargo feature that is off. Unused
    /// (and allowed dead) when every type feature is on, since the arms that
    /// call it compile out.
    #[allow(dead_code)]
    pub(crate) fn need_feature(column: &str, ty: &str, feature: &str) -> ExecError {
        ExecError::Decode {
            column: column.to_owned(),
            source: keelson_core::Error::other(format!(
                "column type {ty} needs the keelson-sqlx \"{feature}\" feature"
            )),
        }
    }

    /// A column type this backend has no mapping for. Loud, never guessed.
    pub(crate) fn unhandled(column: &str, ty: &str) -> ExecError {
        ExecError::Decode {
            column: column.to_owned(),
            source: keelson_core::Error::other(format!("unsupported column type {ty}")),
        }
    }
}
