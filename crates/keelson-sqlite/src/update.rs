//! Mods for [`sqlite::update`](crate::update()).
//!
//! [`table`] is the table being written to; [`from`] is the extra from-item
//! `UPDATE … FROM` allows, and the joins attach to *that*, never to the target —
//! which is the whole reason the two are different mods.
//!
//! ```
//! use keelson_sqlite as sqlite;
//! use keelson_sqlite::{Chain, arg, quote, update};
//!
//! let q = sqlite::update((
//!     update::table(quote("posts")).as_("p"),
//!     update::set_col("views").to(quote(("p", "views")).plus(arg(1i32))),
//!     update::from(quote("users")).as_("u"),
//!     update::where_(quote(("u", "id")).eq(quote(("p", "user_id")))),
//!     update::returning(quote(("p", "id"))),
//! ));
//! ```
//!
//! There is no `limit`, no `offset` and no `order_by`: SQLite's parser accepts them
//! on an `UPDATE`, but only a build compiled with
//! `SQLITE_ENABLE_UPDATE_DELETE_LIMIT` does, and the ordinary one — including the
//! SQLite these tests link against — rejects them. There is no `where_current_of`
//! either, because SQLite has no cursors.

pub use crate::shared::{
    cross_join, extra_from_item as from_also, from_item as from, full_join, inner_join, left_join,
    or_abort, or_fail, or_ignore, or_replace, or_rollback, recursive, returning, right_join, set,
    set_col, target_table as table, where_, with,
};
