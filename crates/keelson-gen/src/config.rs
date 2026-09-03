//! The TOML configuration — bob's gen config inventory, ported and adapted.
//!
//! What carried over and how it is spelled here:
//!
//! - **only / except** — `only` / `except` table lists, plus per-table
//!   `only_columns` / `except_columns` under `[tables.<name>]`.
//! - **shared_schema** — `schema` (PostgreSQL): which namespace to
//!   introspect; the default is `public`.
//! - **aliases** — `[aliases.<table>]` `singular` / `plural` rename the row
//!   struct and back-references; `[aliases.<table>.columns]` renames fields
//!   and column fns (the SQL name is untouched);
//!   `[aliases.<table>.relationships]` renames relation fields/mods.
//! - **inflections** — `[inflections]` maps irregular plurals to their
//!   singular (`people = "person"`).
//! - **relationships** — `[[relationships]]` declares a foreign key the
//!   schema does not (FK-less schemas, views). `cardinality` is optional
//!   between two base tables and **required** when either end is a view; see
//!   [`Cardinality`] and `docs/views.md`.
//! - **no_back_referencing** — global flag, plus `no_back_reference` on a
//!   manual relationship.
//! - **key** — not in bob. `[tables.<name>] key = [...]` declares the
//!   identity of a relation the catalog gives none (a view, a keyless
//!   table), which is what turns a `SELECT`-only model into a writable one.
//!   Only accepted when the engine says writes reach the relation.
//! - **replacements / types** — `[types.map]` re-maps a database type
//!   everywhere; `[[types.override]]` re-maps columns matched by
//!   name/db_type/nullable/default/autoincrement/comment, optionally scoped
//!   to tables. Every override emits an `assert_bind` line in the generated
//!   file, so a non-binding replacement is a compile error (see the crate
//!   docs).
//! - **Go struct-tag options → serde-attribute options** — `[output]
//!   serde = true` derives `serde::Serialize`/`Deserialize` on row and `Rel`
//!   structs.
//! - **hooks** — not in bob (bob's hooks are runtime opt-in); here
//!   `[tables.<name>] hooks = [...]` opts a table into delegating hook
//!   overrides, aimed at the module named by `[hooks] module`.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{GenError, Result};

/// Which dialect to introspect and emit for.
///
/// `Serialize` as well as `Deserialize` because a schema snapshot records the
/// dialect it was taken from (see [`crate::schema::Snapshot`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Dialect {
    /// PostgreSQL: introspect `pg_catalog`, emit against `keelson_psql`.
    Psql,
    /// SQLite: introspect `sqlite_master`/pragmas, emit against
    /// `keelson_sqlite`.
    Sqlite,
    /// MySQL: introspect `information_schema`, emit against `keelson_mysql`
    /// — with the no-`RETURNING` mutation surface (see the crate docs).
    Mysql,
}

impl Dialect {
    /// The name this dialect is spelled with in `keelson.toml` and in a
    /// snapshot file.
    pub fn as_str(self) -> &'static str {
        match self {
            Dialect::Psql => "psql",
            Dialect::Sqlite => "sqlite",
            Dialect::Mysql => "mysql",
        }
    }
}

impl fmt::Display for Dialect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The seven hook methods a table can opt into delegating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Hook {
    /// keelson-models' `Table::before_insert`.
    BeforeInsert,
    /// keelson-models' `Table::after_insert`.
    AfterInsert,
    /// keelson-models' `Table::before_update`.
    BeforeUpdate,
    /// keelson-models' `Table::after_update`.
    AfterUpdate,
    /// keelson-models' `Table::before_delete`.
    BeforeDelete,
    /// keelson-models' `Table::after_delete`.
    AfterDelete,
    /// keelson-models' `View::after_select`.
    AfterSelect,
}

impl Hook {
    /// The method (and hand-written hook fn) name.
    pub fn method(self) -> &'static str {
        match self {
            Hook::BeforeInsert => "before_insert",
            Hook::AfterInsert => "after_insert",
            Hook::BeforeUpdate => "before_update",
            Hook::AfterUpdate => "after_update",
            Hook::BeforeDelete => "before_delete",
            Hook::AfterDelete => "after_delete",
            Hook::AfterSelect => "after_select",
        }
    }
}

impl fmt::Display for Hook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.method())
    }
}

/// The whole configuration file.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The dialect to introspect and emit for.
    pub dialect: Dialect,
    /// The connection string (`sqlite://path` or a plain path;
    /// `postgres://…`). Schema provenance is the user's migration flow's
    /// business — the generator reads whatever the connection sees.
    #[serde(default)]
    pub url: Option<String>,
    /// Where to keep a committed [`Snapshot`](crate::schema::Snapshot) of the
    /// introspected schema.
    ///
    /// This is the other answer to "what schema am I generating from": with a
    /// `url` the generator reads the database and, if this is set, refreshes
    /// the file; with no `url` it reads the file. That is what lets a CI job
    /// run `--check`, and a contributor run the generator, without a database
    /// in the loop. Unlike `url`, this belongs in the committed config — a
    /// connection string is an environment's business, but where the snapshot
    /// lives is the repository's.
    #[serde(default)]
    pub snapshot: Option<String>,
    /// The directory the generated files land in.
    #[serde(default)]
    pub out: Option<String>,
    /// PostgreSQL: the namespace to introspect (bob's shared_schema).
    #[serde(default = "default_schema")]
    pub schema: String,
    /// Emit no has-many back-references anywhere.
    #[serde(default)]
    pub no_back_referencing: bool,
    /// When non-empty, only these tables are generated.
    #[serde(default)]
    pub only: Vec<String>,
    /// These tables are skipped.
    #[serde(default)]
    pub except: Vec<String>,
    /// Output options.
    #[serde(default)]
    pub output: Output,
    /// Where the application's hand-written hook functions live.
    #[serde(default)]
    pub hooks: Hooks,
    /// Irregular plural → singular (`people = "person"`).
    #[serde(default)]
    pub inflections: BTreeMap<String, String>,
    /// Per-table options, keyed by table name.
    #[serde(default)]
    pub tables: BTreeMap<String, TableConfig>,
    /// Renames, keyed by table name.
    #[serde(default)]
    pub aliases: BTreeMap<String, TableAliases>,
    /// Manual relationships for keys the schema does not declare.
    #[serde(default)]
    pub relationships: Vec<ManualRelationship>,
    /// The user-overridable type map.
    #[serde(default)]
    pub types: Types,
    /// Hand-written SQL → typed code (Layer 4). Absent means no query files
    /// are generated from; see [`crate::queries`].
    #[serde(default)]
    pub queries: Option<crate::queries::QueriesConfig>,
}

fn default_schema() -> String {
    "public".to_owned()
}

/// Output options.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Output {
    /// Derive `serde::Serialize`/`Deserialize` on row and `Rel` structs (the
    /// serde-attribute port of bob's Go struct-tag options).
    #[serde(default)]
    pub serde: bool,
    /// Emit `factories.rs` beside the models: one keelson-factory template
    /// module per writable table, as `keelson-factory/tests/spec_*.rs`
    /// specifies. Off by default — a production crate has no reason to carry
    /// test-data machinery it never calls.
    #[serde(default)]
    pub factories: bool,
}

/// Where hand-written hooks live.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hooks {
    /// The module path generated hook overrides delegate to; the application
    /// writes `<module>::<table>::<hook>` functions there, outside the
    /// generated tree.
    #[serde(default = "default_hooks_module")]
    pub module: String,
}

impl Default for Hooks {
    fn default() -> Self {
        Hooks {
            module: default_hooks_module(),
        }
    }
}

fn default_hooks_module() -> String {
    "crate::hooks".to_owned()
}

/// Per-table options.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TableConfig {
    /// When non-empty, only these columns are generated.
    #[serde(default)]
    pub only_columns: Vec<String>,
    /// These columns are skipped.
    #[serde(default)]
    pub except_columns: Vec<String>,
    /// The hook methods this table delegates to the hooks module.
    #[serde(default)]
    pub hooks: Vec<Hook>,
    /// The identity of a relation the catalog gives none: a view, or a base
    /// table declared without a primary key. Declaring it is what turns the
    /// `SELECT`-only model into a writable one — and it is accepted **only**
    /// when the engine says writes reach the relation (see
    /// [`TableKind::UpdatableView`](crate::schema::TableKind::UpdatableView)).
    /// Declaring it on a relation that already has a primary key is an
    /// error: the catalog's answer is not the configuration's to overrule.
    #[serde(default)]
    pub key: Vec<String>,
}

/// Renames for one table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TableAliases {
    /// Row-struct base name (default: singularised table name).
    #[serde(default)]
    pub singular: Option<String>,
    /// Back-reference field name on other models (default: the table name).
    #[serde(default)]
    pub plural: Option<String>,
    /// Column → field/fn rename. The SQL name is untouched.
    #[serde(default)]
    pub columns: BTreeMap<String, String>,
    /// Relation → field/mod rename, keyed by the default relation name.
    #[serde(default)]
    pub relationships: BTreeMap<String, String>,
}

/// How many rows sit on each end of a declared relation.
///
/// A foreign key answers this for itself: the referenced side is a key, so
/// it is the "one", and the referencing side is the "many". A view answers
/// nothing — it has no key and no constraint — so a relation that touches
/// one must say which it is. The declaration is an assertion the generator
/// takes on trust and cannot check; what it buys is the shape of the
/// back-reference.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cardinality {
    /// Many referencing rows per referenced row: the referencing side gets a
    /// to-one relation, the referenced side a `Vec` back-reference. This is
    /// what a foreign key means, and the default.
    #[default]
    ManyToOne,
    /// One row on each side: the referencing side gets a to-one relation and
    /// the referenced side an `Option` back-reference rather than a `Vec`.
    OneToOne,
}

impl fmt::Display for Cardinality {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Cardinality::ManyToOne => "many_to_one",
            Cardinality::OneToOne => "one_to_one",
        })
    }
}

/// A foreign key the schema does not declare (bob's manual relationships,
/// which double as its manual constraints for FK-less joins) — and the only
/// way to relate a view to anything, since a view has no foreign keys and
/// usually no key at all.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManualRelationship {
    /// The referencing (child) table or view.
    pub table: String,
    /// The referencing column.
    pub column: String,
    /// The referenced (parent) table or view.
    pub ref_table: String,
    /// The referenced column.
    pub ref_column: String,
    /// The relation name on the child (default: column minus `_id`).
    #[serde(default)]
    pub name: Option<String>,
    /// Emit no has-many back-reference on the parent for this key.
    #[serde(default)]
    pub no_back_reference: bool,
    /// How many rows sit on each end. Optional between two base tables,
    /// where the referenced column's key constraint answers it; **required**
    /// when either end is a view, because nothing in the catalog does.
    #[serde(default)]
    pub cardinality: Option<Cardinality>,
}

/// The user-overridable type map.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Types {
    /// Database type → Rust type path, applied everywhere after per-column
    /// overrides. Keys are matched case-insensitively, precision stripped
    /// (`numeric(10,2)` matches `numeric`).
    #[serde(default)]
    pub map: BTreeMap<String, String>,
    /// Per-column overrides, first match wins.
    #[serde(default, rename = "override")]
    pub overrides: Vec<TypeOverride>,
}

/// One per-column type override.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypeOverride {
    /// Table scope; empty means every table.
    #[serde(default)]
    pub tables: Vec<String>,
    /// What the column must look like to match.
    #[serde(rename = "match", default)]
    pub matcher: Matcher,
    /// The Rust type to emit (a path, e.g. `chrono::NaiveDateTime` or
    /// `crate::types::UserId`). The generated file asserts it binds.
    pub rust_type: String,
}

/// The column matcher: every present field must match.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Matcher {
    /// Column name, exact.
    #[serde(default)]
    pub name: Option<String>,
    /// Declared database type, case-insensitive, precision stripped.
    #[serde(default)]
    pub db_type: Option<String>,
    /// Nullability.
    #[serde(default)]
    pub nullable: Option<bool>,
    /// Default expression text, exact (`"CURRENT_TIMESTAMP"`).
    #[serde(default)]
    pub default: Option<String>,
    /// Auto-increment.
    #[serde(default)]
    pub autoincrement: Option<bool>,
    /// Column comment, exact (PostgreSQL).
    #[serde(default)]
    pub comment: Option<String>,
}

impl Config {
    /// Parse a configuration from TOML text.
    pub fn from_toml(text: &str) -> Result<Config> {
        toml::from_str(text).map_err(|e| GenError::Config(e.to_string()))
    }

    /// Read and parse a configuration file.
    pub fn load(path: impl AsRef<Path>) -> Result<Config> {
        let text = std::fs::read_to_string(path.as_ref())?;
        Config::from_toml(&text)
    }

    /// Whether `table` survives the `only`/`except` filters.
    pub fn includes_table(&self, table: &str) -> bool {
        if self.except.iter().any(|t| t == table) {
            return false;
        }
        self.only.is_empty() || self.only.iter().any(|t| t == table)
    }

    /// Whether `table.column` survives the per-table column filters.
    pub fn includes_column(&self, table: &str, column: &str) -> bool {
        let Some(tc) = self.tables.get(table) else {
            return true;
        };
        if tc.except_columns.iter().any(|c| c == column) {
            return false;
        }
        tc.only_columns.is_empty() || tc.only_columns.iter().any(|c| c == column)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minimal_config_parses_with_defaults() {
        let c = Config::from_toml("dialect = \"sqlite\"").unwrap();
        assert_eq!(c.dialect, Dialect::Sqlite);
        assert_eq!(c.schema, "public");
        assert_eq!(c.hooks.module, "crate::hooks");
        assert!(!c.no_back_referencing);
        assert!(c.includes_table("anything"));
    }

    #[test]
    fn the_full_inventory_parses() {
        let c = Config::from_toml(
            r#"
            dialect = "psql"
            url = "postgres://localhost/app"
            out = "src/models"
            schema = "app"
            no_back_referencing = true
            only = ["users", "posts"]
            except = ["schema_migrations"]

            [output]
            serde = true

            [hooks]
            module = "crate::model_hooks"

            [inflections]
            people = "person"

            [tables.users]
            except_columns = ["password_digest"]
            hooks = ["before_insert", "after_select"]

            [aliases.users]
            singular = "member"
            plural = "membership"
            [aliases.users.columns]
            created_at = "created"
            [aliases.users.relationships]
            posts = "articles"

            [[relationships]]
            table = "posts"
            column = "author_name"
            ref_table = "users"
            ref_column = "name"
            name = "author"
            no_back_reference = true

            [types.map]
            citext = "String"

            [[types.override]]
            tables = ["users"]
            rust_type = "crate::types::UserId"
            [types.override.match]
            name = "id"
            db_type = "integer"
            nullable = false
            "#,
        )
        .unwrap();
        assert_eq!(c.dialect, Dialect::Psql);
        assert!(c.includes_table("users"));
        assert!(!c.includes_table("schema_migrations"));
        assert!(!c.includes_table("tags"), "only wins");
        assert!(!c.includes_column("users", "password_digest"));
        assert_eq!(
            c.tables["users"].hooks,
            vec![Hook::BeforeInsert, Hook::AfterSelect]
        );
        assert_eq!(c.aliases["users"].columns["created_at"], "created");
        assert_eq!(c.relationships[0].name.as_deref(), Some("author"));
        assert_eq!(c.types.overrides[0].matcher.name.as_deref(), Some("id"));
    }

    #[test]
    fn unknown_keys_are_config_errors_not_silent_noise() {
        let err = Config::from_toml("dialect = \"sqlite\"\ntypo_key = 1").unwrap_err();
        assert!(matches!(err, GenError::Config(_)), "{err}");
    }
}
