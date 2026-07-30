//! Mods for [`mysql::values`](crate::values()) — the `VALUES` statement
//! (MySQL 8.0.19+).
//!
//! The rows come from [`row`] and [`rows`], the same mods an `INSERT`'s row
//! source uses; the statement itself spells each row `ROW(…)`, because that is
//! the grammar of the standalone form. The result's columns are named
//! `column_0`, `column_1`, …, which is what an [`order_by`] refers to.
//!
//! ```
//! use keelson_mysql as mysql;
//! use keelson_mysql::{arg, quote, values};
//!
//! let q = mysql::values((
//!     values::row((arg(1i32), arg("ada"))),
//!     values::row((arg(2i32), arg("bab"))),
//!     values::order_by(quote("column_0")).desc(),
//!     values::limit(1),
//! ));
//! ```

pub use crate::shared::{limit, order_by, rows, values as row};
