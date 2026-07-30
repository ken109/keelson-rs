//! Mods for [`mysql::insert`](crate::insert()).
//!
//! ```
//! use keelson_mysql as mysql;
//! use keelson_mysql::{arg, insert};
//!
//! let q = mysql::insert((
//!     insert::into("users").columns(["id", "name"]),
//!     insert::values((arg(1i32), arg("ada"))),
//!     insert::as_("new"),
//!     insert::on_duplicate_key_update(insert::set_row("new", ["name"])),
//! ));
//! ```
//!
//! # No `WITH`, and no `RETURNING`
//!
//! MySQL allows a CTE only immediately before the `SELECT` of an `INSERT … SELECT`
//! (*15.2.20*), so there is no `insert::with`: put the `WITH` on the sub-query
//! handed to [`query`]. And there is no `returning` in this crate at all.
//!
//! # The three row sources
//!
//! [`values`]/[`rows`], [`query`], and [`set`]/[`set_col`] are alternatives.
//! `SET` wins if it is combined with the others, and with none of them the
//! statement writes `VALUES ()` — MySQL's "take every default".
//!
//! # The two `SET` lists
//!
//! [`set`] and [`set_col`] resolve against the `INSERT … SET` row source when
//! applied to the statement, and against the assignment list when applied inside
//! [`on_duplicate_key_update`]. They are the same functions; the body of
//! `on_duplicate_key_update` is a bare [`Set`](keelson_core::clause::Set), so which
//! list is meant is decided by where the mod is written.
//!
//! [`set_values`] and [`set_row`] are the two ways to reach the incoming row from
//! inside an upsert — `VALUES(col)` and the 8.0.19 row alias respectively.

pub use crate::shared::{
    as_, delayed, from_item as into, high_priority, ignore, low_priority, max_execution_time,
    on_duplicate_key_update, optimizer_hint, qb_name, resource_group, rows, set, set_col, set_row,
    set_values, set_var, values, values_from_query as query,
};
