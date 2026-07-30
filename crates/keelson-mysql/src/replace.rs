//! Mods for [`mysql::replace`](crate::replace()).
//!
//! ```
//! use keelson_mysql as mysql;
//! use keelson_mysql::{arg, replace};
//!
//! let q = mysql::replace((
//!     replace::into("users").columns(["id", "name"]),
//!     replace::values((arg(1i32), arg("ada"))),
//! ));
//! ```
//!
//! A deliberately short list. `REPLACE` deletes the conflicting row and inserts the
//! new one, so `IGNORE`, `HIGH_PRIORITY`, the row alias and
//! `ON DUPLICATE KEY UPDATE` are all absent from its production — and therefore
//! from this module. `LOW_PRIORITY` and `DELAYED` are the only modifiers it takes.

pub use crate::shared::{
    delayed, from_item as into, low_priority, max_execution_time, optimizer_hint, qb_name,
    resource_group, rows, set, set_col, set_var, values, values_from_query as query,
};
