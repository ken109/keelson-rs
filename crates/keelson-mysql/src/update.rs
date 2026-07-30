//! Mods for [`mysql::update`](crate::update()).
//!
//! [`table`] is the `table_references` being written to — and unlike PostgreSQL
//! there is no second list, because MySQL has no `UPDATE … FROM`. A join or a
//! [`table_also`] adds another table that the same statement may also assign to.
//!
//! ```
//! use keelson_mysql as mysql;
//! use keelson_mysql::{Chain, arg, quote, update};
//!
//! let q = mysql::update((
//!     update::table(quote("posts")).as_("p"),
//!     update::inner_join(quote("users")).as_("u").on_eq(quote(("u", "id")), quote(("p", "user_id"))),
//!     update::set_col(("p", "views")).to(quote(("p", "views")).plus(arg(1i32))),
//!     update::where_(quote(("u", "is_active")).eq(arg(true))),
//! ));
//! ```
//!
//! [`order_by`] and [`limit`] belong to the single-table form only; MySQL rejects
//! them as soon as more than one table is named.

pub use crate::shared::{
    cross_join, extra_from_item as table_also, ignore, inner_join, left_join, limit, low_priority,
    max_execution_time, optimizer_hint, order_by, qb_name, recursive, resource_group, right_join,
    set, set_col, set_var, straight_join, target_table as table, where_, with,
};
