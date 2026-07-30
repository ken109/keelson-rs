//! Resolution: [`Schema`] + [`Config`] → the emit-ready model list.
//!
//! Everything configurable is decided here — filters, renames, relation
//! names, hook lists, resolved Rust types — so emission is a pure,
//! decision-free rendering pass. Determinism: tables sorted by name, columns
//! in catalog order, belongs-to in foreign-key order, has-many sorted by
//! (child table, relation name).

use std::collections::BTreeSet;

use crate::config::{Config, Hook};
use crate::error::{GenError, Result};
use crate::schema::{ForeignKey, Schema, TableDef, TableKind};
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
    /// Table (full surface) or view (`SELECT`-only). A keyless base table is
    /// demoted to a view model — there is nothing sound to key mutations on.
    pub kind: TableKind,
    /// Columns, catalog order, filters applied.
    pub columns: Vec<ModelColumn>,
    /// Indices into `columns` forming the primary key (empty for views).
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

/// A to-many back-reference: `child.child_fk_column` points at this model's
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

    // Relations: schema foreign keys first (in catalog order), then manual
    // ones from config, all validated against the filtered model set.
    for t in &tables {
        for fk in &t.foreign_keys {
            if fk.columns.len() != 1 {
                // Multi-column foreign keys are introspected faithfully but
                // relation emission covers single-column keys only (recorded
                // in the crate docs); the key is simply not a relation.
                continue;
            }
            add_relation(&mut models, config, &t.name, fk, None, false)?;
        }
    }
    for m in &config.relationships {
        let fk = ForeignKey {
            columns: vec![m.column.clone()],
            ref_table: m.ref_table.clone(),
            ref_columns: vec![m.ref_column.clone()],
        };
        add_relation(
            &mut models,
            config,
            &m.table,
            &fk,
            m.name.clone(),
            m.no_back_reference,
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

    let mut columns = Vec::new();
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
    // filters; a partial key is no key, and the model demotes to a view.
    let pk: Vec<usize> = t
        .primary_key
        .iter()
        .filter_map(|name| columns.iter().position(|c| c.db_name == *name))
        .collect();
    let kind = if t.kind == TableKind::Table && !pk.is_empty() && pk.len() == t.primary_key.len() {
        TableKind::Table
    } else {
        TableKind::View
    };
    let pk = if kind == TableKind::Table { pk } else { vec![] };

    let hooks = config
        .tables
        .get(&t.name)
        .map(|tc| tc.hooks.clone())
        .unwrap_or_default();
    let mut hooks = hooks;
    hooks.sort();
    hooks.dedup();
    if kind == TableKind::View
        && let Some(h) = hooks.iter().find(|h| **h != Hook::AfterSelect)
    {
        return Err(GenError::Config(format!(
            "table `{}` is SELECT-only (view or no primary key) but configures the `{h}` hook; \
             only `after_select` applies",
            t.name
        )));
    }

    Ok(Model {
        table: t.name.clone(),
        marker: crate::names::pascal(&t.name),
        row: crate::names::pascal(&singular),
        kind,
        columns,
        pk,
        belongs_to: Vec::new(),
        has_many: Vec::new(),
        hooks,
    })
}

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

fn add_relation(
    models: &mut [Model],
    config: &Config,
    child_table: &str,
    fk: &ForeignKey,
    manual_name: Option<String>,
    no_back_reference: bool,
) -> Result<()> {
    let manual = manual_name.is_some() || no_back_reference;
    let missing = |what: &str| {
        GenError::Config(format!(
            "relationship {child_table}.{} → {}.{}: {what}",
            fk.columns[0], fk.ref_table, fk.ref_columns[0]
        ))
    };

    let Some(child_idx) = models.iter().position(|m| m.table == child_table) else {
        // A schema FK whose child was filtered out is silently gone with the
        // table; a manual relationship naming an unknown child is a mistake.
        return if manual || config.relationships.iter().any(|r| r.table == child_table) {
            Err(missing("the table is not generated"))
        } else {
            Ok(())
        };
    };
    let Some(parent_idx) = models.iter().position(|m| m.table == fk.ref_table) else {
        // Filtered-out parent: the relation quietly disappears with it.
        return Ok(());
    };
    if models[child_idx].kind == TableKind::View || models[parent_idx].kind == TableKind::View {
        return if manual {
            Err(GenError::Unsupported(format!(
                "relationship {child_table}.{} → {}: relations on SELECT-only models \
                 are not supported yet",
                fk.columns[0], fk.ref_table
            )))
        } else {
            Ok(())
        };
    }

    let fk_col_name = &fk.columns[0];
    let ref_col_name = fk.ref_columns.first().cloned().unwrap_or_default();
    let Some((fk_column, _)) = models[child_idx].column(fk_col_name) else {
        return if manual {
            Err(missing("the referencing column is not generated"))
        } else {
            Ok(())
        };
    };
    if models[parent_idx].column(&ref_col_name).is_none() {
        return if manual {
            Err(missing("the referenced column is not generated"))
        } else {
            Ok(())
        };
    }

    // The belongs-to name: manual name, else the fk column minus `_id`, else
    // the singularised parent table; then the child's relationship aliases.
    let default_name = manual_name.unwrap_or_else(|| match fk_col_name.strip_suffix("_id") {
        Some(stem) if !stem.is_empty() => stem.to_owned(),
        _ => crate::names::singular(&fk.ref_table, &config.inflections),
    });
    let name = config
        .aliases
        .get(child_table)
        .and_then(|a| a.relationships.get(&default_name).cloned())
        .unwrap_or(default_name);
    models[child_idx].belongs_to.push(BelongsTo {
        name: name.clone(),
        fk_column,
        target: fk.ref_table.clone(),
        ref_column: ref_col_name.clone(),
    });

    if config.no_back_referencing || no_back_reference {
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
        .get(&fk.ref_table)
        .and_then(|a| a.relationships.get(&default_back).cloned())
        .unwrap_or(default_back);
    models[parent_idx].has_many.push(HasMany {
        name: back_name,
        child: child_table.to_owned(),
        child_fk_column: fk_col_name.clone(),
        parent_key_column: ref_col_name,
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
