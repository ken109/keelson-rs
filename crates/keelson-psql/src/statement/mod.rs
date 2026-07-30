//! The four statement types, each shaped by PostgreSQL's own grammar.
//!
//! Every one composes the shared clause structs as named fields, in the order the
//! reference manual lists them, and implements the `Has*` traits for the clauses it
//! actually has. *Not* implementing one is how "this statement has no such clause"
//! is said: `select::having(..)` will not compile against an `UpdateQuery`, because
//! `UpdateQuery` does not implement
//! [`HasHaving`](keelson_core::clause::HasHaving).
//!
//! # Which table a mod means
//!
//! Two statements have two tables, and the two `Has*` traits below are how they are
//! told apart:
//!
//! | statement | [`HasTableRef`](keelson_core::clause::HasTableRef) | [`HasTargetTable`] | [`HasExtraTables`] |
//! |---|---|---|---|
//! | `SELECT` | `FROM` item | — | further `FROM` items |
//! | `INSERT` | `INTO` target | — | — |
//! | `UPDATE` | `FROM` item | the updated table | further `FROM` items |
//! | `DELETE` | `USING` item | the deleted-from table | further `USING` items |
//!
//! So `HasTableRef` always means "the from-item", which is what makes one
//! `select::from` / `update::from` / `delete::using` chain type serve all three, and
//! what puts joins in the right place — `HasJoins` reaches the from-item's joins,
//! never the target's.

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
/// `SELECT` and `INSERT` have only one table each and use
/// [`HasTableRef`](keelson_core::clause::HasTableRef) for it, so they do not
/// implement this — which is precisely why `update::table(..)` cannot be applied to
/// them.
pub trait HasTargetTable {
    /// The target table to modify.
    fn target_table_mut(&mut self) -> &mut TableRef;
}

/// A statement whose from-item list may hold more than one entry.
///
/// PostgreSQL's `FROM from_item [, ...]` and `USING from_item [, ...]` are
/// comma-separated lists, and a comma there means the same thing as `CROSS JOIN`.
/// The first entry lives in [`HasTableRef`](keelson_core::clause::HasTableRef); the
/// rest are appended here.
pub trait HasExtraTables {
    /// The additional from-items to modify.
    fn extra_tables_mut(&mut self) -> &mut Vec<TableRef>;
}

/// Write `FROM`/`USING` and its comma-separated list, skipping absent entries.
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
/// naming the absent item (`FROM` or `USING`, per statement).
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
