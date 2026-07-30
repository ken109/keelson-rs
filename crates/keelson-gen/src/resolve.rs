//! Resolution: [`Schema`] + [`Config`] → the emit-ready model list.
//!
//! Everything configurable is decided here — filters, renames, relation
//! names, hook lists, resolved Rust types — so emission is a pure,
//! decision-free rendering pass. Determinism: tables sorted by name, columns
//! in catalog order, belongs-to in foreign-key order, has-many sorted by
//! (child table, relation name).
//!
//! # Views
//!
//! A view has no foreign keys and usually no primary key, so nothing in the
//! catalog says how it relates to anything, or what identifies one of its
//! rows. Two decisions follow, and both are enforced here rather than
//! guessed at (`docs/views.md` is the reader's version):
//!
//! - **Relations come from the configuration or not at all.** A
//!   `[[relationships]]` entry may name a view on either end; it must then
//!   declare its `cardinality`, since the referenced side is not a key.
//!   Every part of the declaration the catalog *can* check is checked
//!   ([`check_declared`]): both relations exist, both columns exist, and the
//!   two columns resolve to the same Rust type.
//! - **Identity is not needed to load a relation, and is not invented.** The
//!   generated loaders group by the declared join column, never by a row
//!   identity, so a keyless view can hold and be the target of relations. It
//!   simply gets *less*: no `Pk`, no `Setter`, no `INSERT`/`UPDATE`/
//!   `DELETE`, no keyed read-back on MySQL, no factory. A view earns those
//!   back only by declaring `[tables.<name>] key` — and only when the engine
//!   says writes reach it at all ([`check_declared_key`]).

use std::collections::BTreeSet;

use crate::config::{Cardinality, Config, Hook, ManualRelationship};
use crate::error::{GenError, Result};
use crate::schema::{Schema, TableDef, TableKind};
use crate::typemap;

/// One table or view, fully decided.
#[derive(Debug, Clone)]
pub(crate) struct Model {
    /// The database table name (also the module and file name).
    pub table: String,
    /// The marker struct name (`Users`).
    pub marker: String,
    /// The row struct name (`User`).
    pub row: String,
    /// What the catalog calls the relation, carried through unchanged.
    pub kind: TableKind,
    /// Whether the full `Table` surface (`Setter`, `Pk`, `INSERT`/`UPDATE`/
    /// `DELETE`) is generated. True for a base table with a usable primary
    /// key, and for a relation the engine says is writable whose key the
    /// configuration declares — nothing else. A keyless base table and a
    /// plain view are both `SELECT`-only: there is nothing sound to key
    /// mutations on.
    pub writable: bool,
    /// Columns, catalog order, filters applied.
    pub columns: Vec<ModelColumn>,
    /// Indices into `columns` forming the key (empty unless `writable`).
    pub pk: Vec<usize>,
    /// To-one relations (this table holds the foreign key).
    pub belongs_to: Vec<BelongsTo>,
    /// To-many back-references (some other table's foreign key points here).
    pub has_many: Vec<HasMany>,
    /// Hook methods delegated to the application's hooks module.
    pub hooks: Vec<Hook>,
}

impl Model {
    pub(crate) fn column(&self, db_name: &str) -> Option<(usize, &ModelColumn)> {
        self.columns
            .iter()
            .enumerate()
            .find(|(_, c)| c.db_name == db_name)
    }

    /// Whether this model carries a `Rel` field and the `preload`/`then_load`
    /// mods. Relations need no key of their own — the loaders group by the
    /// declared join column, never by an identity — so a `SELECT`-only model
    /// gets them as soon as some relation names it. A writable model always
    /// has the field, even when empty, because that is the shape the
    /// hand-written spec fixes.
    pub(crate) fn holds_relations(&self) -> bool {
        self.writable || !self.belongs_to.is_empty() || !self.has_many.is_empty()
    }

    /// The generated entry function's name: `table()` for the full surface,
    /// `view()` for a `SELECT`-only one.
    pub(crate) fn entry(&self) -> &'static str {
        if self.writable { "table" } else { "view" }
    }
}

/// One column, type resolved and rename applied.
#[derive(Debug, Clone)]
pub(crate) struct ModelColumn {
    /// The database column name (what SQL and `FromRow` use).
    pub db_name: String,
    /// The Rust field / column-fn name (the alias when configured).
    pub field: String,
    /// The resolved base Rust type (`i64`, `chrono::NaiveDateTime`, …);
    /// nullability wraps `Option` at the row struct only.
    pub rust_type: String,
    /// Whether the row field is `Option<_>`.
    pub nullable: bool,
    /// Whether the type came from configuration (emits `assert_bind`).
    pub overridden: bool,
    /// The declared database type (for the override's comment).
    pub db_type: String,
    /// The column's default expression, when the schema declares one.
    pub default: Option<String>,
    /// Auto-increment: the engine assigns this column.
    pub autoincrement: bool,
    /// The column is unique **on its own** — a single-column `UNIQUE` key, or
    /// a single-column primary key. A member of a composite key is not
    /// unique by itself and is not marked. Read by the factory emitter, which
    /// backs such a column with a sequence.
    pub unique: bool,
}

/// A to-one relation: this model's `fk_column` points at
/// `target.ref_column`.
#[derive(Debug, Clone)]
pub(crate) struct BelongsTo {
    /// The relation name (`user`): the `Rel` field and the mod fn.
    pub name: String,
    /// Index of the foreign-key column in this model's `columns`.
    pub fk_column: usize,
    /// The target table's database name.
    pub target: String,
    /// The referenced column's database name on the target.
    pub ref_column: String,
}

/// A back-reference: `child.child_fk_column` points at this model's
/// `parent_key_column`.
#[derive(Debug, Clone)]
pub(crate) struct HasMany {
    /// The relation name (`posts`): the `Rel` field and the mod fn.
    pub name: String,
    /// The child table's database name.
    pub child: String,
    /// The foreign-key column's database name on the child.
    pub child_fk_column: String,
    /// The referenced column's database name on this model.
    pub parent_key_column: String,
    /// One child row rather than many: the `Rel` field is an `Option` and the
    /// loader attaches to-one. Set by
    /// `cardinality = "one_to_one"` on a declared relationship; a foreign key
    /// always means many.
    pub to_one: bool,
}

/// Run the whole resolution.
pub(crate) fn resolve(schema: &Schema, config: &Config) -> Result<Vec<Model>> {
    let mut tables: Vec<&TableDef> = schema
        .tables
        .iter()
        .filter(|t| config.includes_table(&t.name))
        .collect();
    tables.sort_by(|a, b| a.name.cmp(&b.name));

    let mut models: Vec<Model> = Vec::with_capacity(tables.len());
    for t in &tables {
        models.push(resolve_table(t, config)?);
    }

    // Factories work from facts a view does not have: which columns the
    // engine assigns, and which are unique on their own. Emitting one for a
    // writable view would look right and collide on the second row, so it is
    // refused out loud instead.
    if config.output.factories
        && let Some(m) = models.iter().find(|m| m.writable && m.kind.is_view())
    {
        return Err(GenError::Unsupported(format!(
            "`{}` is a writable view, and `[output] factories = true` cannot cover it: \
             a view reports no auto-increment columns and no unique constraints, so the \
             generated template would have nothing to draw distinct values from. Drop \
             `[tables.{}] key`, or generate factories with `{}` in `except`.",
            m.table, m.table, m.table
        )));
    }

    // Relations: schema foreign keys first (in catalog order), then declared
    // ones from config, all validated against the filtered model set.
    for t in &tables {
        for fk in &t.foreign_keys {
            if fk.columns.len() != 1 {
                // Multi-column foreign keys are introspected faithfully but
                // relation emission covers single-column keys only (recorded
                // in the crate docs); the key is simply not a relation.
                continue;
            }
            add_relation(
                &mut models,
                config,
                &RelSpec {
                    child_table: &t.name,
                    child_column: &fk.columns[0],
                    parent_table: &fk.ref_table,
                    parent_column: fk.ref_columns.first().map_or("", String::as_str),
                    name: None,
                    no_back_reference: false,
                    to_one_back: false,
                    declared: None,
                },
            )?;
        }
    }
    for (i, r) in config.relationships.iter().enumerate() {
        let cardinality = check_declared(schema, config, i, r)?;
        add_relation(
            &mut models,
            config,
            &RelSpec {
                child_table: &r.table,
                child_column: &r.column,
                parent_table: &r.ref_table,
                parent_column: &r.ref_column,
                name: r.name.clone(),
                no_back_reference: r.no_back_reference,
                to_one_back: cardinality == Cardinality::OneToOne,
                declared: Some(declared_key(i, r)),
            },
        )?;
    }

    for m in &mut models {
        m.has_many
            .sort_by(|a, b| (&a.child, &a.name).cmp(&(&b.child, &b.name)));
    }
    for m in &models {
        check_rel_names(m)?;
    }
    Ok(models)
}

/// How a `[[relationships]]` entry is named in an error: the TOML key plus
/// the join it declares, so the message points at the line to fix.
fn declared_key(i: usize, r: &ManualRelationship) -> String {
    format!(
        "[[relationships]] #{} (`{}.{}` -> `{}.{}`)",
        i + 1,
        r.table,
        r.column,
        r.ref_table,
        r.ref_column
    )
}

/// Validate one `[[relationships]]` entry against the **introspected
/// schema** — before any filtering, so a typo is told apart from a table the
/// filters removed — and settle its cardinality.
///
/// Everything a declaration asserts that the catalog can check is checked
/// here: both relations exist, both columns exist, and the two columns
/// resolve to the same Rust type (a join between an `i64` and a `String`
/// would not compile, and saying so here beats saying it in generated code).
/// The one thing the catalog cannot check is how many rows sit on each end
/// when a view is involved — so that has to be declared.
fn check_declared(
    schema: &Schema,
    config: &Config,
    i: usize,
    r: &ManualRelationship,
) -> Result<Cardinality> {
    let key = declared_key(i, r);
    let child = relation_named(schema, &r.table, "table", &key)?;
    let parent = relation_named(schema, &r.ref_table, "ref_table", &key)?;
    let child_col = column_named(child, &r.column, "column", &key)?;
    let parent_col = column_named(parent, &r.ref_column, "ref_column", &key)?;

    let child_ty = typemap::resolve(config.dialect, &config.types, child, child_col)?;
    let parent_ty = typemap::resolve(config.dialect, &config.types, parent, parent_col)?;
    if child_ty.rust_type != parent_ty.rust_type {
        return Err(GenError::Config(format!(
            "{key}: the join columns are not comparable — `{}.{}` is `{}` (db type `{}`) but \
             `{}.{}` is `{}` (db type `{}`). Give them the same Rust type with [types.map] or \
             [[types.override]] if the comparison really is sound.",
            child.name,
            child_col.name,
            child_ty.rust_type,
            child_col.db_type,
            parent.name,
            parent_col.name,
            parent_ty.rust_type,
            parent_col.db_type,
        )));
    }

    match r.cardinality {
        Some(c) => Ok(c),
        None if child.kind.is_view() || parent.kind.is_view() => {
            let view = if child.kind.is_view() { child } else { parent };
            Err(GenError::Config(format!(
                "{key}: `cardinality` is required because `{}` is a {} — a view has no key \
                 and no constraint, so nothing in the catalog says how many rows sit on each \
                 end. Add `cardinality = \"many_to_one\"` or `cardinality = \"one_to_one\"`.",
                view.name,
                view.kind.noun()
            )))
        }
        // Between two base tables the referenced column's key constraint
        // answers it, and a foreign key always means many-to-one.
        None => Ok(Cardinality::ManyToOne),
    }
}

/// The table or view a configuration key names, or a message listing what
/// the schema does hold.
fn relation_named<'a>(
    schema: &'a Schema,
    name: &str,
    what: &str,
    key: &str,
) -> Result<&'a TableDef> {
    schema
        .tables
        .iter()
        .find(|t| t.name == name)
        .ok_or_else(|| {
            GenError::Config(format!(
                "{key}: `{what} = \"{name}\"` names no table or view in the introspected schema \
             (it holds: {})",
                join_names(schema.tables.iter().map(|t| t.name.as_str()))
            ))
        })
}

/// The column a configuration key names, or a message listing what the
/// relation does hold.
fn column_named<'a>(
    t: &'a TableDef,
    name: &str,
    what: &str,
    key: &str,
) -> Result<&'a crate::schema::ColumnDef> {
    t.column(name).ok_or_else(|| {
        GenError::Config(format!(
            "{key}: `{what} = \"{name}\"` names no column of {} `{}` (it has: {})",
            t.kind.noun(),
            t.name,
            join_names(t.columns.iter().map(|c| c.name.as_str()))
        ))
    })
}

fn join_names<'a>(names: impl Iterator<Item = &'a str>) -> String {
    names.collect::<Vec<_>>().join(", ")
}

fn resolve_table(t: &TableDef, config: &Config) -> Result<Model> {
    if t.name == "mod" {
        return Err(GenError::Unsupported(
            "a table named `mod` cannot become a module file".to_owned(),
        ));
    }
    let aliases = config.aliases.get(&t.name);
    let singular = aliases
        .and_then(|a| a.singular.clone())
        .unwrap_or_else(|| crate::names::singular(&t.name, &config.inflections));

    // Columns unique on their own: a single-column UNIQUE key, or a
    // single-column primary key.
    let single_unique: BTreeSet<&str> = t
        .unique_keys
        .iter()
        .chain(std::iter::once(&t.primary_key))
        .filter(|k| k.len() == 1)
        .map(|k| k[0].as_str())
        .collect();

    let mut columns: Vec<ModelColumn> = Vec::new();
    let mut kept: Vec<&str> = Vec::new();
    for c in &t.columns {
        if !config.includes_column(&t.name, &c.name) {
            continue;
        }
        kept.push(&c.name);
        let field = aliases
            .and_then(|a| a.columns.get(&c.name).cloned())
            .unwrap_or_else(|| c.name.clone());
        let resolved = typemap::resolve(config.dialect, &config.types, t, c)?;
        columns.push(ModelColumn {
            db_name: c.name.clone(),
            field,
            rust_type: resolved.rust_type,
            nullable: c.nullable,
            overridden: resolved.overridden,
            db_type: c.db_type.clone(),
            default: c.default.clone(),
            autoincrement: c.autoincrement,
            unique: single_unique.contains(c.name.as_str()),
        });
    }
    if columns.is_empty() {
        return Err(GenError::Config(format!(
            "table `{}` has no columns left after filters",
            t.name
        )));
    }
    check_field_names(&t.name, &columns)?;

    // The primary key survives only when every key column survived the
    // filters; a partial key is no key, and the model demotes to SELECT-only.
    let catalog_pk: Vec<usize> = t
        .primary_key
        .iter()
        .filter_map(|name| columns.iter().position(|c| c.db_name == *name))
        .collect();
    let catalog_key_intact = t.kind == TableKind::Table
        && !catalog_pk.is_empty()
        && catalog_pk.len() == t.primary_key.len();

    let declared_key = config
        .tables
        .get(&t.name)
        .map(|tc| tc.key.as_slice())
        .unwrap_or_default();
    let declared_pk = check_declared_key(t, &columns, declared_key)?;

    let (pk, writable) = match (catalog_key_intact, declared_pk) {
        // The configuration does not get to overrule the catalog: a relation
        // with a primary key is keyed by it, and `key` on such a table is
        // already refused above.
        (true, _) => (catalog_pk, true),
        (false, Some(pk)) => {
            // Declaring the key asserts the columns are never NULL; see
            // `check_declared_key`.
            for i in &pk {
                columns[*i].nullable = false;
            }
            (pk, true)
        }
        (false, None) => (vec![], false),
    };

    let hooks = config
        .tables
        .get(&t.name)
        .map(|tc| tc.hooks.clone())
        .unwrap_or_default();
    let mut hooks = hooks;
    hooks.sort();
    hooks.dedup();
    if !writable && let Some(h) = hooks.iter().find(|h| **h != Hook::AfterSelect) {
        return Err(GenError::Config(format!(
            "`{}` is SELECT-only ({}) but configures the `{h}` hook; only `after_select` \
             applies. A {} becomes writable by declaring `[tables.{}] key`, which needs the \
             engine to say writes reach it.",
            t.name,
            if t.kind.is_view() {
                "a view with no declared key"
            } else {
                "no primary key"
            },
            t.kind.noun(),
            t.name
        )));
    }

    Ok(Model {
        table: t.name.clone(),
        marker: crate::names::pascal(&t.name),
        row: crate::names::pascal(&singular),
        kind: t.kind,
        writable,
        columns,
        pk,
        belongs_to: Vec::new(),
        has_many: Vec::new(),
        hooks,
    })
}

/// Validate `[tables.<name>] key` and turn it into column indices.
///
/// The key is the answer to "what identifies a row of this relation", which
/// the catalog does not give for a view or a keyless table. Declaring it is
/// the *only* way a `SELECT`-only model gets `INSERT`/`UPDATE`/`DELETE`, so
/// every way the declaration could be wrong is refused here, by name:
///
/// - it is meaningless where the catalog already answers (a primary key);
/// - it is unsound where the engine refuses writes at all (a read-only view
///   — and the three engines decide that differently, see [`TableKind`]);
/// - it must name real, generated columns, once each.
///
/// Declaring a column as key **asserts it is never NULL**, and the generated
/// row field stops being an `Option` accordingly. That is not a liberty: a
/// key that can be NULL identifies nothing, PostgreSQL and SQLite report
/// *every* view column as nullable because a view carries no constraints, and
/// the SQLite introspector already applies the same reasoning to a base
/// table's primary key. The assertion is the caller's; a view that really
/// does yield NULL there fails to decode, by name, at the row.
fn check_declared_key(
    t: &TableDef,
    columns: &[ModelColumn],
    key: &[String],
) -> Result<Option<Vec<usize>>> {
    if key.is_empty() {
        return Ok(None);
    }
    let at = format!("[tables.{}] key", t.name);
    if !t.primary_key.is_empty() {
        return Err(GenError::Config(format!(
            "{at}: `{}` already has a primary key (`{}`) — the catalog's answer is not the \
             configuration's to overrule; remove the key, or the column filters that dropped \
             part of it.",
            t.name,
            t.primary_key.join("`, `")
        )));
    }
    if !t.kind.is_updatable() {
        return Err(GenError::Config(format!(
            "{at}: `{}` is a view this engine will not write through, so declaring its \
             identity cannot make it writable. {} Remove the key to generate a `SELECT`-only \
             model — relations do not need it.",
            t.name, NOT_UPDATABLE_HINT,
        )));
    }

    let mut seen = BTreeSet::new();
    let mut pk = Vec::with_capacity(key.len());
    for name in key {
        if !seen.insert(name) {
            return Err(GenError::Config(format!("{at}: `{name}` is listed twice")));
        }
        let Some(i) = columns.iter().position(|c| c.db_name == *name) else {
            return Err(GenError::Config(format!(
                "{at}: `{name}` is not a generated column of `{}` (it has: {})",
                t.name,
                join_names(columns.iter().map(|c| c.db_name.as_str()))
            )));
        };
        pk.push(i);
    }
    Ok(Some(pk))
}

/// Why an engine might refuse to write through a view — spelled out because
/// the three disagree and the answer comes from the catalog, not from us.
const NOT_UPDATABLE_HINT: &str = "PostgreSQL writes through a view only when it is \
     auto-updatable (one table, no aggregate/DISTINCT/set operation/GROUP BY, …) or carries \
     `INSTEAD OF` triggers; MySQL reports one `IS_UPDATABLE` flag it computes the same way and \
     has no `INSTEAD OF` triggers at all; SQLite writes through a view only when it carries \
     `INSTEAD OF` triggers for all three of INSERT, UPDATE and DELETE.";

fn check_field_names(table: &str, columns: &[ModelColumn]) -> Result<()> {
    let mut seen = BTreeSet::new();
    for c in columns {
        if c.field == "rel" {
            return Err(GenError::Config(format!(
                "column `{table}.{}` would be named `rel`, which the relations field owns; \
                 alias it in [aliases.{table}.columns]",
                c.db_name
            )));
        }
        if matches!(c.field.as_str(), "table" | "view" | "all_columns") {
            return Err(GenError::Config(format!(
                "column `{table}.{}` collides with the generated `{}()` fn; \
                 alias it in [aliases.{table}.columns]",
                c.db_name, c.field
            )));
        }
        if !seen.insert(&c.field) {
            return Err(GenError::Config(format!(
                "two columns of `{table}` are both named `{}` after aliasing",
                c.field
            )));
        }
    }
    Ok(())
}

/// One relation to add, however it was arrived at: a schema foreign key or a
/// `[[relationships]]` declaration.
struct RelSpec<'a> {
    /// The referencing side — the one that gets the to-one relation.
    child_table: &'a str,
    child_column: &'a str,
    /// The referenced side — the one that gets the back-reference.
    parent_table: &'a str,
    parent_column: &'a str,
    /// The declared relation name, when the configuration gave one.
    name: Option<String>,
    no_back_reference: bool,
    /// The back-reference holds one child row rather than many.
    to_one_back: bool,
    /// The `[[relationships]]` key this came from; `None` for a schema
    /// foreign key. Present means every mismatch is an error rather than a
    /// quiet skip: a foreign key can lose an end to the filters, but a
    /// declaration the user wrote by hand cannot.
    declared: Option<String>,
}

fn add_relation(models: &mut [Model], config: &Config, spec: &RelSpec<'_>) -> Result<()> {
    let RelSpec {
        child_table,
        child_column,
        parent_table,
        parent_column,
        ..
    } = *spec;
    // A declared relation that cannot be built is an error naming the config
    // key; a schema foreign key that lost an end to the filters is not — it
    // went with the table, which is what the filter asked for.
    let gone = |what: &str, why: &str| -> Result<()> {
        match &spec.declared {
            Some(key) => Err(GenError::Config(format!("{key}: {what} {why}"))),
            None => Ok(()),
        }
    };
    const FILTERED: &str = "is excluded by the `only`/`except` filters, so the relation has \
                            nothing to hang off";
    const COL_FILTERED: &str = "is excluded by its table's `only_columns`/`except_columns`, so \
                                the relation has no column to join on";

    let Some(child_idx) = models.iter().position(|m| m.table == child_table) else {
        return gone(&format!("`{child_table}`"), FILTERED);
    };
    let Some(parent_idx) = models.iter().position(|m| m.table == parent_table) else {
        return gone(&format!("`{parent_table}`"), FILTERED);
    };
    let Some((fk_column, _)) = models[child_idx].column(child_column) else {
        return gone(&format!("`{child_table}.{child_column}`"), COL_FILTERED);
    };
    if models[parent_idx].column(parent_column).is_none() {
        return gone(&format!("`{parent_table}.{parent_column}`"), COL_FILTERED);
    }

    // The belongs-to name: the declared name, else the fk column minus `_id`,
    // else the singularised parent table; then the child's relationship
    // aliases.
    let default_name =
        spec.name
            .clone()
            .unwrap_or_else(|| match child_column.strip_suffix("_id") {
                Some(stem) if !stem.is_empty() => stem.to_owned(),
                _ => crate::names::singular(parent_table, &config.inflections),
            });
    let name = config
        .aliases
        .get(child_table)
        .and_then(|a| a.relationships.get(&default_name).cloned())
        .unwrap_or(default_name);
    models[child_idx].belongs_to.push(BelongsTo {
        name: name.clone(),
        fk_column,
        target: parent_table.to_owned(),
        ref_column: parent_column.to_owned(),
    });

    if config.no_back_referencing || spec.no_back_reference {
        return Ok(());
    }
    // The back-reference name: the child's plural alias or its table name;
    // when one child table references the same parent twice, each
    // back-reference is disambiguated by its belongs-to name.
    let base = config
        .aliases
        .get(child_table)
        .and_then(|a| a.plural.clone())
        .unwrap_or_else(|| child_table.to_owned());
    let clashes = models[parent_idx]
        .has_many
        .iter()
        .any(|h| h.child == child_table);
    let default_back = if clashes {
        format!("{base}_via_{name}")
    } else {
        base.clone()
    };
    if clashes {
        // Retroactively disambiguate the earlier back-reference too.
        let child_names: std::collections::BTreeMap<String, String> = models[child_idx]
            .belongs_to
            .iter()
            .map(|b| {
                (
                    models[child_idx].columns[b.fk_column].db_name.clone(),
                    b.name.clone(),
                )
            })
            .collect();
        for h in &mut models[parent_idx].has_many {
            if h.child == child_table && h.name == base {
                let earlier = child_names
                    .get(&h.child_fk_column)
                    .cloned()
                    .unwrap_or_else(|| h.child_fk_column.clone());
                h.name = format!("{base}_via_{earlier}");
            }
        }
    }
    let back_name = config
        .aliases
        .get(parent_table)
        .and_then(|a| a.relationships.get(&default_back).cloned())
        .unwrap_or(default_back);
    models[parent_idx].has_many.push(HasMany {
        name: back_name,
        child: child_table.to_owned(),
        child_fk_column: child_column.to_owned(),
        parent_key_column: parent_column.to_owned(),
        to_one: spec.to_one_back,
    });
    Ok(())
}

fn check_rel_names(m: &Model) -> Result<()> {
    let mut seen = BTreeSet::new();
    for name in m
        .belongs_to
        .iter()
        .map(|b| &b.name)
        .chain(m.has_many.iter().map(|h| &h.name))
    {
        if !seen.insert(name.clone()) {
            return Err(GenError::Config(format!(
                "model `{}` has two relations named `{name}`; \
                 rename one in [aliases.{}.relationships]",
                m.table, m.table
            )));
        }
    }
    Ok(())
}
