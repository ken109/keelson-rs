//! Mods for [`psql::update`](crate::update()).
//!
//! [`table`] is the table being written to; [`from`] is the extra from-item
//! `UPDATE … FROM` allows, and the joins attach to *that*, never to the target —
//! which is the whole reason the two are different mods.
//!
//! ```
//! use keelson_psql as psql;
//! use keelson_psql::{Chain, arg, quote, update};
//!
//! let q = psql::update((
//!     update::table(quote("posts")).as_("p"),
//!     update::set_col("views").to(quote(("p", "views")).plus(arg(1i32))),
//!     update::from(quote("users")).as_("u"),
//!     update::where_(quote(("u", "id")).eq(quote(("p", "user_id")))),
//!     update::returning(quote(("p", "id"))),
//! ));
//! ```

pub use crate::shared::{
    cross_join, extra_from_item as from_also, from_functions as from_function, from_item as from,
    full_join, inner_join, left_join, recursive, returning, right_join, set, set_col,
    target_table as table, where_, where_current_of, with,
};
