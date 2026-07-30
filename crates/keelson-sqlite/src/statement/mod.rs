//! The four statement types, each shaped by SQLite's own syntax diagrams.
//!
//! Every one composes the shared clause structs as named fields, in the order
//! <https://www.sqlite.org/lang.html> lists them, and implements the `Has*` traits
//! for the clauses it actually has. *Not* implementing one is how "this statement
//! has no such clause" is said: `select::having(..)` will not compile against an
//! `UpdateQuery`.
//!
//! # Which table a mod means
//!
//! | statement | [`HasTableRef`](keelson_core::clause::HasTableRef) | [`HasTargetTable`] | [`HasExtraTables`] |
//! |---|---|---|---|
//! | `SELECT` | `FROM` item | — | further `FROM` items |
//! | `INSERT` | `INTO` target | — | — |
//! | `UPDATE` | `FROM` item | the updated table | further `FROM` items |
//! | `DELETE` | — | the deleted-from table | — |
//!
//! A `DELETE` has one table and nothing else: SQLite has no `USING`, so
//! `HasTableRef` and `HasExtraTables` are deliberately unimplemented for it and
//! there is no `delete::using` to import.
//!
//! # What SQLite does not have, and so is not here
//!
//! - **No locking clause.** SQLite locks whole database files, so there is no
//!   `FOR UPDATE`, no `SKIP LOCKED`, and no [`Locks`](keelson_core::clause::Locks).
//! - **No `FETCH … ROWS ONLY`.** `LIMIT` is the only spelling, and `OFFSET` is part
//!   of the `LIMIT` production rather than a clause of its own.
//! - **No `ORDER BY`/`LIMIT` on `UPDATE` or `DELETE`.** SQLite's parser accepts
//!   them, but only a build configured with `SQLITE_ENABLE_UPDATE_DELETE_LIMIT`
//!   does, and the default — including the one linked into these tests — rejects
//!   them outright. A mod that produced SQL an ordinary SQLite refuses would be a
//!   trap, so none exists.
//! - **No `RETURNING` on `SELECT`**, and no `WHERE CURRENT OF`: SQLite has no
//!   cursors.

mod delete;
mod insert;
mod select;
mod update;

pub use delete::DeleteQuery;
pub use insert::InsertQuery;
pub use select::SelectQuery;
pub use update::UpdateQuery;

use keelson_core::clause::TableRef;

/// A statement whose *target* table is separate from its from-item: the table an
/// `UPDATE` writes to, or the one a `DELETE` removes from.
///
/// In SQLite both are a `qualified-table-name`
/// (<https://www.sqlite.org/syntax/qualified-table-name.html>), which is what makes
/// `INDEXED BY` available there as well as on a `FROM` item.
pub trait HasTargetTable {
    /// The target table to modify.
    fn target_table_mut(&mut self) -> &mut TableRef;
}

/// A statement whose from-item list may hold more than one entry.
///
/// SQLite's `FROM` takes either a comma-separated `table-or-subquery` list or a
/// `join-clause`, and `,` is itself one of the `join-operator`s — so the two are
/// the same thing and mixing them is legal. The first entry lives in
/// [`HasTableRef`](keelson_core::clause::HasTableRef); the rest are appended here.
pub trait HasExtraTables {
    /// The additional from-items to modify.
    fn extra_tables_mut(&mut self) -> &mut Vec<TableRef>;
}

/// Write `FROM` and its comma-separated list, skipping absent entries.
///
/// An entry with no table renders nothing, so it must not contribute a comma
/// either; and if the leading item is absent the whole clause goes, because
/// `FROM , "x"` is not a repair of anything.
///
/// One thing must not go with it: joins. They hang off the leading item, so
/// with no item they have nowhere to attach — and dropping a join the caller
/// asked for would build *valid* SQL that silently means something else, which
/// no grammar or engine can catch after the fact. That is recorded as
/// [`Error::Incomplete`](keelson_core::Error::Incomplete) with `missing`
/// naming the absent item.
fn write_from_list(
    w: &mut keelson_core::SqlWriter<'_>,
    keyword: &str,
    first: &TableRef,
    rest: &[TableRef],
    missing: &'static str,
) {
    if first.is_empty() {
        if !first.joins.is_empty() {
            w.record_error(keelson_core::Error::Incomplete(missing));
        }
        return;
    }
    let items = std::iter::once(first)
        .chain(rest.iter())
        .filter(|t| !t.is_empty());
    w.write_iter(items, keyword, ", ", "");
}
