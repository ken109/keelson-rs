//! Mods for [`mysql::select`](crate::select()).
//!
//! Everything a MySQL `SELECT` can carry, and nothing else. There is no
//! `returning` — MySQL has none — no `fetch`, no `full_join`, no `distinct_on`, and
//! no `nulls_first`/`nulls_last` on a sort key.
//!
//! ```
//! use keelson_mysql as mysql;
//! use keelson_mysql::{Chain, arg, quote, select};
//!
//! let q = mysql::select((
//!     select::columns((quote("id"), quote("name"))),
//!     select::from(quote("users")),
//!     select::where_(quote("age").gte(arg(21i32))),
//!     select::order_by(quote("name")).desc(),
//!     select::limit(10),
//! ));
//! ```
//!
//! # The two ways to say `STRAIGHT_JOIN`
//!
//! [`straight`] is the statement modifier — join every table in the order written.
//! [`straight_join`] is the join operator, applying to one pair. MySQL spells them
//! with the same keyword in two different productions, and they are not
//! interchangeable.

use keelson_core::{Mod, mod_fn};

use crate::statement::SelectQuery;

pub use crate::shared::{
    columns, cross_join, distinct, distinct_row, except, except_all, extra_from_item as from_also,
    for_share, for_update, from_item as from, group_by, having, high_priority, inner_join,
    intersect, intersect_all, left_join, limit, limit_combined, max_execution_time, offset,
    offset_combined, optimizer_hint, order_by, order_by_combined, preload_columns, qb_name,
    recursive, resource_group, right_join, set_var, sql_big_result, sql_buffer_result,
    sql_calc_found_rows, sql_no_cache, sql_small_result, straight, straight_join, union, union_all,
    where_, window, with, with_rollup,
};

/// `LOCK IN SHARE MODE` — the pre-8.0 spelling of [`for_share`].
///
/// A production of its own rather than a lock strength: MySQL's grammar is
///
/// ```text
/// [FOR {UPDATE | SHARE} [OF tbl_name [, ...]] [NOWAIT | SKIP LOCKED]
///  | LOCK IN SHARE MODE]
/// ```
///
/// so this form takes neither an `OF` list nor a wait option, and it is an
/// *alternative* to the `FOR …` clause rather than an addition to it. Combining the
/// two is a caller error the server refuses.
pub fn lock_in_share_mode() -> impl Mod<SelectQuery> {
    mod_fn(|q: &mut SelectQuery| q.lock_in_share_mode = true)
}
