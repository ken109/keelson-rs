//! Mods for [`sqlite::insert`](crate::insert()).
//!
//! ```
//! use keelson_sqlite as sqlite;
//! use keelson_sqlite::{arg, insert};
//!
//! let q = sqlite::insert((
//!     insert::into("users").columns(["id", "name"]),
//!     insert::values((arg(1i32), arg("ada"))),
//!     insert::on_conflict("id").do_update(insert::set_excluded(["name"])),
//!     insert::returning("*"),
//! ));
//! ```
//!
//! # The mods that are not for the `INSERT` itself
//!
//! [`set`], [`set_col`], [`set_excluded`] and [`where_`] apply to a
//! [`ConflictClause`](keelson_core::clause::ConflictClause), not to an
//! `InsertQuery` — an `INSERT` has no `SET` and no `WHERE`. They are here because
//! this is where they are used: inside
//! [`on_conflict(..).do_update(..)`](crate::shared::ConflictChain::do_update). An
//! `InsertQuery` does not implement the traits they need, so misplacing one is a
//! compile error rather than a surprise.
//!
//! The two `WHERE`s of an upsert are easy to conflate and behave nothing alike:
//! `on_conflict(..).where_(..)` is the *index* predicate, matched against a partial
//! unique index's own definition, while [`where_`] inside `do_update` filters which
//! conflicting rows are updated.
//!
//! # Several upserts
//!
//! [`on_conflict`] appends. SQLite 3.35 and later try each `ON CONFLICT` clause in
//! turn, and only the last may omit its conflict target — so
//! `on_conflict("id").do_update(..)` followed by `on_conflict(()).do_nothing()` is a
//! statement, and the reverse order is not.

pub use crate::shared::{
    into_table as into, on_conflict, or_abort, or_fail, or_ignore, or_replace, or_rollback,
    recursive, returning, rows, set, set_col, set_excluded, values, values_from_query as query,
    where_, with,
};
