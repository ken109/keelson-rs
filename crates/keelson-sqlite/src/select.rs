//! Mods for [`sqlite::select`](crate::select()).
//!
//! Everything a SQLite `SELECT` can carry, and nothing else. There is no
//! `returning` here, no `fetch`, no `for_update`, no `distinct_on`, no
//! `group_by_distinct` and no `_combined` variant of anything — SQLite's grammar
//! has none of them, and a mod for a construct a dialect lacks should not exist.
//!
//! ```
//! use keelson_sqlite as sqlite;
//! use keelson_sqlite::{Chain, arg, quote, select};
//!
//! let q = sqlite::select((
//!     select::columns((quote("id"), quote("name"))),
//!     select::from(quote("users")),
//!     select::where_(quote("age").gte(arg(21i32))),
//!     select::order_by(quote("name")).desc().nulls_last(),
//!     select::limit(10),
//!     select::offset(20),
//! ));
//! ```
//!
//! # `LIMIT` and `OFFSET` are one clause
//!
//! SQLite's production is `LIMIT expr [ ( OFFSET | , ) expr ]`, so [`offset`]
//! without [`limit`] is not a statement. Building one records
//! [`Error::Incomplete`](keelson_core::Error::Incomplete) rather than handing back
//! SQL the database will reject.
//!
//! # `VALUES` is a `SELECT`
//!
//! [`values`] and [`rows`] fill the other alternative of SQLite's `select-core`, so
//! `sqlite::select(select::rows([[1, 2], [3, 4]]))` is the statement
//! `VALUES (1, 2), (3, 4)`. Compounding a `VALUES` core onto a `SELECT` one — and
//! the reverse — is legal and is how a recursive CTE's seed row is usually written.

use keelson_core::{Mod, mod_fn};

use crate::statement::SelectQuery;

pub use crate::shared::{
    columns, cross_join, except, extra_from_item as from_also, from_item as from, full_join,
    group_by, having, inner_join, intersect, left_join, limit, offset, order_by, preload_columns,
    recursive, right_join, rows, union, union_all, values, where_, window, with,
};

/// `SELECT DISTINCT` — drop duplicate result rows.
///
/// `ALL` is the other alternative in the grammar and is the default; writing it adds
/// nothing, so it is not representable.
pub fn distinct() -> impl Mod<SelectQuery> {
    mod_fn(|q: &mut SelectQuery| q.distinct = true)
}
