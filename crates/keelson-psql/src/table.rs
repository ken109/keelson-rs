//! Mods for [`psql::table`](crate::table()) — the `TABLE name` command.
//!
//! [`name`] is the table, and takes the same chain a `FROM` item does, of which
//! only `.only()` is grammatical here — `TABLE [ ONLY ] table_name [ * ]` takes
//! a bare name, no alias. The rest is the tail the manual allows with `TABLE`:
//! `ORDER BY`, `LIMIT`/`OFFSET`/`FETCH`, the locking clauses, `WITH`, and the
//! set operations. `WHERE`, `GROUP BY` and the rest of a `SELECT` do not exist
//! here, and do not compile.
//!
//! ```
//! use keelson_psql as psql;
//! use keelson_psql::{quote, table};
//!
//! let q = psql::table((
//!     table::name(quote("users")),
//!     table::order_by(quote("id")),
//!     table::limit(10),
//! ));
//! ```

pub use crate::shared::{
    except, except_all, fetch, fetch_combined, for_key_share, for_no_key_update, for_share,
    for_update, from_item as name, intersect, intersect_all, limit, limit_all, limit_combined,
    offset, offset_combined, order_by, order_by_combined, recursive, union, union_all, with,
};
