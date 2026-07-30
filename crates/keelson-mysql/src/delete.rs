//! Mods for [`mysql::delete`](crate::delete()).
//!
//! [`from`] names a table rows are removed from — several calls give the
//! multiple-table `DELETE FROM t1, t2`. [`using`] is the `table_references` the
//! statement reads, and the joins attach to *that*.
//!
//! ```
//! use keelson_mysql as mysql;
//! use keelson_mysql::{Chain, arg, delete, quote};
//!
//! let q = mysql::delete((
//!     delete::from(quote("comments")),
//!     delete::using(quote("comments")),
//!     delete::inner_join(quote("posts")).on_eq(quote(("posts", "id")), quote(("comments", "post_id"))),
//!     delete::where_(quote(("posts", "status")).eq(arg("draft"))),
//! ));
//! ```
//!
//! # `PARTITION` on the target
//!
//! `delete::from("t").partition(["p0"])` renders
//! `DELETE FROM \`t\` PARTITION (\`p0\`)`. `DELETE` is the one statement where MySQL
//! writes the partition list *after* the alias and once for the whole statement, so
//! the chain's list is lifted out of the table reference and onto the query — see
//! [`HasDeleteTables`](crate::HasDeleteTables).
//!
//! [`order_by`] and [`limit`] belong to the single-table form only.

pub use crate::shared::{
    cross_join, delete_table as from, extra_from_item as using_also, from_item as using, ignore,
    inner_join, left_join, limit, low_priority, max_execution_time, optimizer_hint, order_by,
    qb_name, quick, recursive, resource_group, right_join, set_var, straight_join, where_, with,
};
