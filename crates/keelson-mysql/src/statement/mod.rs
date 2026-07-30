//! The five statement types, each shaped by MySQL's own grammar.
//!
//! Every one composes the shared clause structs as named fields, in the order
//! *13.2 Data Manipulation Statements* lists them, and implements the `Has*`
//! traits for the clauses it actually has. *Not* implementing one is how "this
//! statement has no such clause" is said: `select::having(..)` will not compile
//! against an `UpdateQuery`.
//!
//! # What MySQL does not have, and therefore is not here
//!
//! * **No `RETURNING`** on any of them. There is no
//!   [`Returning`](keelson_core::clause::Returning) field anywhere in this crate
//!   and no `returning` mod to apply to one.
//! * **No `FETCH`**, so no [`HasFetch`](keelson_core::clause::HasFetch).
//! * **No `WITH` on `INSERT` or `REPLACE`.** MySQL permits a `WITH` clause "at the
//!   beginning of `SELECT`, `UPDATE`, and `DELETE` statements" and, for
//!   `INSERT … SELECT`, only *immediately preceding the `SELECT`*
//!   (*15.2.20 WITH*). So `WITH c AS (…) INSERT …` is not MySQL; the CTE goes
//!   inside the sub-query handed to [`insert::query`](crate::insert::query).
//! * **No `OFFSET` on `UPDATE` or `DELETE`** — their `LIMIT` takes a row count and
//!   nothing else.
//!
//! # Which table a mod means
//!
//! | statement | [`HasTableRef`](keelson_core::clause::HasTableRef) | [`HasTargetTable`] | [`HasExtraTables`] | [`HasDeleteTables`] |
//! |---|---|---|---|---|
//! | `SELECT` | first `FROM` item | — | further `FROM` items | — |
//! | `INSERT`/`REPLACE` | `INTO` target | — | — | — |
//! | `UPDATE` | — | the updated `table_references` | further ones | — |
//! | `DELETE` | first `USING` item | — | further `USING` items | the `FROM` list |
//!
//! `UPDATE` is the row that differs from PostgreSQL: MySQL has no `UPDATE … FROM`,
//! so there is only one table list and it *is* the target — joins and all. That is
//! why `UpdateQuery` implements [`HasTargetTable`] and not `HasTableRef`, and why
//! `update::inner_join` lands on the updated table rather than on a separate
//! from-item.

mod delete;
mod insert;
mod replace;
mod select;
mod update;

pub use delete::DeleteQuery;
pub use insert::InsertQuery;
pub use replace::ReplaceQuery;
pub use select::SelectQuery;
pub use update::UpdateQuery;

use std::borrow::Cow;

use keelson_core::clause::TableRef;

/// A statement whose table list is the thing being modified: the
/// `table_references` an `UPDATE` writes to.
///
/// `SELECT`, `INSERT` and `REPLACE` have one table each and use
/// [`HasTableRef`](keelson_core::clause::HasTableRef) for it, which is why
/// `update::table(..)` cannot be applied to them.
pub trait HasTargetTable {
    /// The target table to modify.
    fn target_table_mut(&mut self) -> &mut TableRef;
}

/// A statement whose table list may hold more than one entry.
///
/// MySQL's `table_references` is comma-separated, and a comma there means the same
/// thing as `CROSS JOIN`. The first entry lives in `HasTableRef` or
/// [`HasTargetTable`]; the rest are appended here.
pub trait HasExtraTables {
    /// The additional table references to modify.
    fn extra_tables_mut(&mut self) -> &mut Vec<TableRef>;
}

/// A `DELETE`'s `FROM` list — the tables rows are actually removed from.
///
/// Separate from every other table trait because `DELETE` is the one statement
/// where the tables being modified and the tables being *read* are two different
/// lists:
///
/// ```text
/// DELETE FROM t1, t2 USING t1 INNER JOIN t2 ON … WHERE …
/// ```
///
/// The partition list comes with it, because `DELETE` is also the one statement
/// that writes `PARTITION` *after* the alias (*15.2.2*):
/// `DELETE FROM tbl [[AS] alias] [PARTITION (…)]`. Everywhere else it precedes
/// the alias, which is where [`TableRef`] puts it — so the chain's partitions are
/// moved out of the table reference and into this slot.
pub trait HasDeleteTables {
    /// The tables to delete from.
    fn delete_tables_mut(&mut self) -> &mut Vec<TableRef>;

    /// The partitions to restrict the delete to.
    fn delete_partitions_mut(&mut self) -> &mut Vec<Cow<'static, str>>;
}

/// Write a keyword and its comma-separated table list, skipping absent entries.
///
/// An entry with no table renders nothing, so it must not contribute a comma
/// either; and if the leading item is absent the whole clause goes, because
/// `FROM , \`x\`` is not a repair of anything.
///
/// Two things must not go with it: joins and the extra items. Joins hang off
/// the leading item, so with no item they have nowhere to attach; extra items
/// are second and later entries of a list the leading item opens, so with no
/// item there is no list to be in. Dropping either one the caller asked for
/// would build *valid* SQL that silently means something else, which no
/// grammar or engine can catch after the fact. That is recorded as
/// [`Error::Incomplete`](keelson_core::Error::Incomplete) with `missing`
/// naming the absent item. (`UPDATE` reaches neither guard: its absent target
/// is already an `Incomplete` before this writer runs, so its `table_also`
/// entries always have their leading `table_references` entry.)
fn write_table_list(
    w: &mut keelson_core::SqlWriter<'_>,
    keyword: &str,
    first: &TableRef,
    rest: &[TableRef],
    missing: &'static str,
) {
    if first.is_empty() {
        if !first.joins.is_empty() || rest.iter().any(|t| !t.is_empty()) {
            w.record_error(keelson_core::Error::Incomplete(missing));
        }
        return;
    }
    let items = std::iter::once(first)
        .chain(rest.iter())
        .filter(|t| !t.is_empty());
    w.write_iter(items, keyword, ", ", "");
}

/// Write a statement's optimizer hints and modifiers, each followed by a space.
///
/// *10.9.2* puts the hint comment immediately after the statement's first
/// keyword, before the modifiers: `SELECT /*+ … */ DISTINCT …`.
fn write_hints_and_modifiers(
    w: &mut keelson_core::SqlWriter<'_>,
    hints: &crate::extras::Hints,
    modifiers: &crate::extras::Modifiers,
) {
    w.write_if(!hints.is_empty(), "", hints, " ");
    w.write_if(!modifiers.is_empty(), "", modifiers, " ");
}
