//! The introspected schema — the generator's intermediate representation.
//!
//! Every introspector (and every test that wants to skip the database)
//! produces this; the resolver and emitter consume only this. The IR is
//! deliberately plain data with `PartialEq`, so "live introspection equals
//! the hand-built IR" is a single `assert_eq!`.

/// A whole schema: every table and view the generator will consider, sorted
/// by name (the introspectors guarantee the order; determinism starts here).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schema {
    /// Tables and views, sorted by [`TableDef::name`].
    pub tables: Vec<TableDef>,
}

/// What the catalog says a relation *is*. Three answers, because writability
/// and viewness are separate facts: a base table is always writable, a view
/// is writable only when the engine says so, and the engines disagree about
/// when that is (see [`TableKind::UpdatableView`]).
///
/// This is the catalog's answer, not the generator's decision. Whether a
/// model ends up with the `Table` surface is resolved from this *plus* the
/// configuration, in `resolve`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableKind {
    /// A base table: gets the full `Table` surface when it has a primary key.
    Table,
    /// A view (or materialised view) the engine will not write through:
    /// `SELECT`-only, unconditionally.
    View,
    /// A view the engine reports as writable — PostgreSQL's auto-updatable
    /// views and any view with the right `INSTEAD OF` triggers, MySQL's
    /// `IS_UPDATABLE = 'YES'`, SQLite's views carrying all three `INSTEAD OF`
    /// triggers. Still keyless: a view has no primary key, so the write
    /// surface needs a `[tables.<name>] key` before it can be generated.
    UpdatableView,
}

impl TableKind {
    /// Whether the catalog calls this a view (updatable or not).
    pub fn is_view(self) -> bool {
        !matches!(self, TableKind::Table)
    }

    /// Whether the engine will accept `INSERT`/`UPDATE`/`DELETE` against it.
    /// A base table always will; a view only when the catalog says so.
    pub fn is_updatable(self) -> bool {
        matches!(self, TableKind::Table | TableKind::UpdatableView)
    }

    /// The word to use for this relation in an error message.
    pub(crate) fn noun(self) -> &'static str {
        match self {
            TableKind::Table => "table",
            TableKind::View => "view",
            TableKind::UpdatableView => "updatable view",
        }
    }
}

/// One table or view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableDef {
    /// The unqualified name as the catalog spells it.
    pub name: String,
    /// Table or view.
    pub kind: TableKind,
    /// Columns in catalog (ordinal) order — the order the generated model
    /// lists them, matching the hand-written spec.
    pub columns: Vec<ColumnDef>,
    /// Primary-key column names in key order; empty when there is none (all
    /// views, and keyless tables — both emit `View`-only models unless the
    /// configuration declares a key, see `[tables.<name>] key`).
    pub primary_key: Vec<String>,
    /// Foreign keys in a stable catalog order.
    pub foreign_keys: Vec<ForeignKey>,
    /// Declared `UNIQUE` constraints, each a column list in key order,
    /// sorted by column list; the primary key is **not** repeated here.
    ///
    /// Read by the factory emitter, which backs a unique column with a
    /// [`Sequence`](https://docs.rs/keelson-factory) value so
    /// `create_many(&db, 100)` cannot collide. Only *declared constraints*
    /// are introspected — a bare `CREATE UNIQUE INDEX` is an index, not a
    /// constraint, and is deliberately not read (the generator would have no
    /// honest way to tell a partial or expression index from a plain one).
    pub unique_keys: Vec<Vec<String>>,
}

impl TableDef {
    /// The column named `name`, if any.
    pub fn column(&self, name: &str) -> Option<&ColumnDef> {
        self.columns.iter().find(|c| c.name == name)
    }
}

/// One column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnDef {
    /// The name as the catalog spells it.
    pub name: String,
    /// The declared database type, as the dialect's catalog reports it
    /// (PostgreSQL: `format_type` output like `timestamp with time zone`;
    /// SQLite: the declared type text). The type map normalises it.
    pub db_type: String,
    /// Whether NULL is allowed.
    pub nullable: bool,
    /// The default expression's text, when there is one.
    pub default: Option<String>,
    /// Auto-increment (PostgreSQL identity/serial; SQLite rowid alias).
    pub autoincrement: bool,
    /// The column comment, where the dialect has them (PostgreSQL).
    pub comment: Option<String>,
}

/// One foreign key. Multi-column keys are carried faithfully but relation
/// emission covers single-column keys only (recorded in the crate docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignKey {
    /// Referencing columns, in key order.
    pub columns: Vec<String>,
    /// The referenced table.
    pub ref_table: String,
    /// Referenced columns, in the same order as [`ForeignKey::columns`].
    pub ref_columns: Vec<String>,
}
