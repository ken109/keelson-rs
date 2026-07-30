//! Mods for [`sqlite::delete`](crate::delete()).
//!
//! The shortest of the four modules, because the statement is:
//!
//! ```text
//! [WITH …] DELETE FROM qualified-table-name [WHERE expr] [RETURNING …]
//! ```
//!
//! ```
//! use keelson_sqlite as sqlite;
//! use keelson_sqlite::{Chain, arg, delete, quote, select, subquery};
//!
//! let q = sqlite::delete((
//!     delete::from(quote("comments")),
//!     delete::where_(quote("post_id").in_(subquery(sqlite::select((
//!         select::columns(quote("id")),
//!         select::from(quote("posts")),
//!         select::where_(quote("status").eq(arg("draft"))),
//!     ))))),
//!     delete::returning(quote("id")),
//! ));
//! ```
//!
//! SQLite has no `USING`, so there is no `using`, no `using_also` and no join mod:
//! a delete driven by another table is written with a sub-query in the `WHERE`, as
//! above. It has no `conflict-clause` either, so none of the `or_*` mods appear —
//! only `INSERT` and `UPDATE` can violate a constraint. And no `limit`/`order_by`,
//! for the reason [`crate::update`] gives.

pub use crate::shared::{recursive, returning, target_table as from, where_, with};
