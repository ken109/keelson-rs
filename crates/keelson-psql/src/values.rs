//! Mods for [`psql::values`](crate::values()) — the standalone `VALUES`
//! statement.
//!
//! The rows come from [`row`] and [`rows`], which are the same mods an
//! `INSERT`'s row source uses (`insert::values`/`insert::rows`); the name
//! differs only because `values::values` would say nothing. The result's
//! columns are named `column1`, `column2`, … by PostgreSQL, which is what an
//! [`order_by`] here refers to — or `ORDER BY 1`.
//!
//! ```
//! use keelson_psql as psql;
//! use keelson_psql::{arg, raw, values};
//!
//! let q = psql::values((
//!     values::row((arg(1i32), arg("ada"))),
//!     values::row((arg(2i32), arg("bab"))),
//!     values::order_by(raw("1")).desc(),
//!     values::limit(1),
//! ));
//! ```
//!
//! There is no `for_update` here: PostgreSQL rejects a locking clause on
//! `VALUES`, so the query type does not implement `HasLocks` and the mod does
//! not resolve.

pub use crate::shared::{
    except, except_all, fetch, fetch_combined, intersect, intersect_all, limit, limit_all,
    limit_combined, offset, offset_combined, order_by, order_by_combined, recursive, rows, union,
    union_all, values as row, with,
};
