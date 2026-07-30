//! Mods for [`psql::delete`](crate::delete()).
//!
//! [`from`] is the table rows are removed from; [`using`] is `DELETE … USING`,
//! which is a `FROM` list under another keyword, and the joins attach to that.
//!
//! ```
//! use keelson_psql as psql;
//! use keelson_psql::{Chain, arg, delete, quote};
//!
//! let q = psql::delete((
//!     delete::from(quote("comments")).as_("c"),
//!     delete::using(quote("posts")).as_("p"),
//!     delete::where_(quote(("c", "post_id")).eq(quote(("p", "id")))),
//!     delete::where_(quote(("p", "status")).eq(arg("draft"))),
//!     delete::returning(quote(("c", "id"))),
//! ));
//! ```

pub use crate::shared::{
    cross_join, extra_from_item as using_also, from_functions as using_function,
    from_item as using, full_join, inner_join, left_join, recursive, returning, right_join,
    target_table as from, where_, where_current_of, with,
};
