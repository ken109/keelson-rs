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

/// Table or view — keelson-models' `View`-only / `Table` split downstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableKind {
    /// A base table: gets the full `Table` surface when it has a primary key.
    Table,
    /// A view (or materialised view): `SELECT`-only, no primary key required.
    View,
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
    /// views, and keyless tables — both emit `View`-only models).
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
