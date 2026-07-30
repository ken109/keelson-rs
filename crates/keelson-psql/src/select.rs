//! Mods for [`psql::select`](crate::select()).
//!
//! Everything a PostgreSQL `SELECT` can carry, and nothing else: there is no
//! `returning` here, because the grammar has none.
//!
//! ```
//! use keelson_psql as psql;
//! use keelson_psql::{Chain, arg, quote, select};
//!
//! let q = psql::select((
//!     select::columns((quote("id"), quote("name"))),
//!     select::from(quote("users")),
//!     select::where_(quote("age").gte(arg(21i32))),
//!     select::order_by(quote("name")).desc().nulls_last(),
//!     select::limit(10),
//! ));
//! ```

use keelson_core::expr::IntoExprList;
use keelson_core::{Mod, mod_fn};

use crate::extras::Distinct;
use crate::statement::SelectQuery;

pub use crate::shared::{
    columns, cross_join, except, except_all, extra_from_item as from_also, fetch, fetch_combined,
    for_key_share, for_no_key_update, for_share, for_update, from_functions as from_function,
    from_item as from, full_join, group_by, group_by_distinct, having, inner_join, intersect,
    intersect_all, left_join, limit, limit_all, limit_combined, offset, offset_combined, order_by,
    order_by_combined, preload_columns, recursive, right_join, union, union_all, where_, window,
    with,
};

/// `SELECT DISTINCT` — drop duplicate result rows.
pub fn distinct() -> impl Mod<SelectQuery> {
    mod_fn(|q: &mut SelectQuery| q.distinct = Some(Distinct::default()))
}

/// `SELECT DISTINCT ON (a, b)` — keep the first row of each group of rows agreeing
/// on these expressions.
///
/// PostgreSQL-only, and it requires the `ORDER BY` to begin with the same
/// expressions, which is the statement's business rather than this mod's.
pub fn distinct_on(on: impl IntoExprList) -> impl Mod<SelectQuery> {
    let on = on.into_expr_list();
    mod_fn(move |q: &mut SelectQuery| q.distinct = Some(Distinct { on }))
}
