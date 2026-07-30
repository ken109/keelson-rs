//! Mods for [`mysql::table`](crate::table()) — the `TABLE` statement
//! (MySQL 8.0.19+).
//!
//! [`name`] is the table, and the grammar takes a bare name — the chain's other
//! decorations have no `TABLE` sentence to appear in. The tail is `ORDER BY
//! column_name` and `LIMIT number [OFFSET number]`, and nothing else.
//!
//! ```
//! use keelson_mysql as mysql;
//! use keelson_mysql::{quote, table};
//!
//! let q = mysql::table((
//!     table::name(quote("users")),
//!     table::order_by(quote("id")),
//!     table::limit(10),
//! ));
//! ```

pub use crate::shared::{from_item as name, limit, offset, order_by};
