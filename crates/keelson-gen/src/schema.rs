//! The introspected schema — the generator's intermediate representation.
//!
//! Every introspector (and every test that wants to skip the database)
//! produces this; the resolver and emitter consume only this. The IR is
//! deliberately plain data with `PartialEq`, so "live introspection equals
//! the hand-built IR" is a single `assert_eq!`.
//!
//! It is also `Serialize`/`Deserialize`, which is what makes a [`Snapshot`]
//! possible: the same IR, written to a file and committed, so a checkout with
//! no database can still generate and still answer `--check`.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::config::Dialect;
use crate::error::{GenError, Result};

/// A whole schema: every table and view the generator will consider, sorted
/// by name (the introspectors guarantee the order; determinism starts here).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForeignKey {
    /// Referencing columns, in key order.
    pub columns: Vec<String>,
    /// The referenced table.
    pub ref_table: String,
    /// Referenced columns, in the same order as [`ForeignKey::columns`].
    pub ref_columns: Vec<String>,
}

/// The bump-on-change format version written into every snapshot file.
///
/// The IR is the generator's own intermediate representation, and it moves
/// when the generator learns something new about a catalog. A snapshot written
/// by an older keelson-gen is not silently reinterpreted: it is refused, with
/// the two versions named, because a field that used to mean something else is
/// worse than no snapshot at all.
pub const SNAPSHOT_VERSION: u32 = 1;

/// An introspected [`Schema`], written to a file and committed.
///
/// The generator normally reads a live database, which is the right default —
/// the catalog is the truth and nothing can drift from it. But it makes the
/// database a build dependency of every checkout, and that is the wrong price
/// for the two jobs that need no truth of their own:
///
/// - **CI answering `--check`.** Verifying that committed generated code still
///   matches its schema should not require standing up PostgreSQL in the job
///   that does it.
/// - **Regenerating in a checkout without the database.** A contributor fixing
///   a typo in a hook should not need the production schema to build.
///
/// So: run the generator against the real database, commit the snapshot beside
/// the generated files, and let the machines that only need to *agree* read
/// that. The snapshot is a generated artefact like the `.rs` files are — it is
/// refreshed by the same run and reviewed in the same diff, which is what
/// keeps it honest. `--check` against a live database compares the snapshot
/// too, so a stale one is a reported drift rather than a silent wrong answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    /// What wrote this. Present so a reader who finds the file without
    /// context knows what it belongs to; not otherwise interpreted.
    pub generator: String,
    /// [`SNAPSHOT_VERSION`] at the time of writing.
    pub version: u32,
    /// The dialect the schema was introspected from. Checked on load: a
    /// PostgreSQL snapshot fed to a `dialect = "sqlite"` config is a
    /// configuration error, and one that would otherwise surface as a pile of
    /// unmapped types.
    pub dialect: Dialect,
    /// The IR itself, already canonicalised.
    pub schema: Schema,
}

impl Snapshot {
    /// Wrap a freshly introspected schema for writing.
    pub fn new(dialect: Dialect, schema: Schema) -> Snapshot {
        Snapshot {
            generator: format!("keelson-gen {}", env!("CARGO_PKG_VERSION")),
            version: SNAPSHOT_VERSION,
            dialect,
            schema,
        }
    }

    /// The exact bytes [`Snapshot::save`] writes.
    ///
    /// Separate from the writing so `--check` can compare without a temporary
    /// file, and pretty-printed with a trailing newline because this is a file
    /// people review in a pull request: a schema change should read as a diff
    /// of the columns that moved, not of one very long line.
    pub fn to_json(&self) -> Result<String> {
        let mut json = serde_json::to_string_pretty(self)
            .map_err(|e| GenError::Config(format!("internal: snapshot does not serialise: {e}")))?;
        json.push('\n');
        Ok(json)
    }

    /// Read a snapshot, refusing one this generator cannot interpret.
    pub fn load(path: impl AsRef<Path>) -> Result<Snapshot> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| {
            GenError::Config(format!(
                "{}: cannot read the schema snapshot: {e}",
                path.display()
            ))
        })?;
        // Read the version before the body: a snapshot from a future
        // keelson-gen may well fail to deserialise, and "unknown field
        // `foo`" is a worse message than "written by version 2".
        #[derive(Deserialize)]
        struct JustTheVersion {
            version: u32,
        }
        let probe: JustTheVersion = serde_json::from_str(&text).map_err(|e| {
            GenError::Config(format!(
                "{}: not a keelson-gen schema snapshot: {e}",
                path.display()
            ))
        })?;
        if probe.version != SNAPSHOT_VERSION {
            return Err(GenError::Config(format!(
                "{}: snapshot format version {}, but this keelson-gen speaks {SNAPSHOT_VERSION}; \
                 re-run the generator against the database to rewrite it",
                path.display(),
                probe.version,
            )));
        }
        serde_json::from_str(&text).map_err(|e| {
            GenError::Config(format!(
                "{}: cannot read the schema snapshot: {e}",
                path.display()
            ))
        })
    }

    /// Write the snapshot, creating the parent directory if needed.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, self.to_json()?)?;
        Ok(())
    }

    /// The schema, after checking it came from the dialect the config emits
    /// for.
    pub fn schema_for(self, dialect: Dialect, path: &Path) -> Result<Schema> {
        if self.dialect != dialect {
            return Err(GenError::Config(format!(
                "{}: snapshot was taken from `{}`, but the config says `dialect = \"{}\"`",
                path.display(),
                self.dialect,
                dialect,
            )));
        }
        Ok(self.schema)
    }
}
