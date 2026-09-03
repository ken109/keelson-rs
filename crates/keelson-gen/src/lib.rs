//! The code generator: a live schema in, readable model `.rs` files out.
//!
//! keelson-gen is an **independent CLI emitting `.rs` files** (the bob/sqlc
//! stance), not a proc macro: generated code is meant to be read, diffed and
//! stepped through. The pipeline is introspect → resolve → emit:
//! [`introspect::introspect`] turns a connection string into a plain
//! [`schema::Schema`] IR, `resolve` applies the whole [`config::Config`]
//! (filters, renames, relations, hooks, the type map) so emission is
//! decision-free, and `emit` renders TokenStreams built with `quote`,
//! formatted with `prettyplease`. What it emits is exactly what the
//! hand-written spec models in `keelson-models/tests/spec_psql.rs` /
//! `spec_sqlite.rs` / `spec_mysql.rs` fix — those files are the
//! authoritative specification, and this crate's tests run the generated code
//! through the same assertions. The factory half is specified the same way,
//! by `keelson-factory/tests/spec_*.rs`.
//!
//! The main entry point is [`run`]; [`generate`] returns the files without
//! writing them, and [`generate_from_schema`] skips introspection for
//! callers (and tests) that already hold an IR.
//!
//! The generator has a second, independent output: [`queries`] turns
//! **hand-written `.sql` files** into typed modules (the sqlc-shaped half),
//! keyed off the `[queries]` section of the same config and reading the same
//! [`schema::Schema`]. Its docs carry the nullability decision table and the
//! two-faces design; nothing in the model pipeline depends on it.
//!
//! # Where this sits
//!
//! Layer 4 of keelson, and the only crate here you run rather than link: it is
//! a CLI (`cargo install keelson-gen`) that reads a live schema and writes `.rs`
//! files against [keelson-models](https://docs.rs/keelson-models) and
//! [keelson-factory](https://docs.rs/keelson-factory), in the dialect of
//! [keelson-psql](https://docs.rs/keelson-psql), [keelson-mysql](https://docs.rs/keelson-mysql) or
//! [keelson-sqlite](https://docs.rs/keelson-sqlite). It emits no DDL and tracks no migration
//! history — point it at the database your migration tool produced. The whole
//! map is the [keelson](https://docs.rs/keelson) facade crate.
//!
//! # The decisions, recorded
//!
//! **Introspection: direct catalog queries, not sea-schema.** sea-schema
//! (SeaORM's introspection crate) was evaluated against querying
//! `pg_catalog` / `sqlite_master` directly, per dialect:
//!
//! - *Weight:* sea-schema brings sea-query plus an async runtime coupling
//!   (its discovery API is async over sqlx), where this crate otherwise
//!   needs only the sync `rusqlite` and `postgres` clients **already in the
//!   workspace** for keelson-sqlcheck's live judges. A dependency that heavy
//!   must earn its keep, and here it cannot:
//! - *Lossy middle layer:* sea-schema normalises types into its own
//!   `ColumnType` enum, which would have to be translated back into type
//!   *names* to feed `docs/type-mappings.md`'s table and the config's
//!   `db_type` matchers. Querying `format_type(...)` / the declared SQLite
//!   type text hands the type map its keys verbatim.
//! - *Determinism:* owning the catalog queries means owning every `ORDER
//!   BY`; byte-identical output is a contract, not a hope.
//!
//! So: SQLite via `sqlite_master` + `pragma_table_info` /
//! `pragma_foreign_key_list` (rusqlite), PostgreSQL via `pg_catalog` +
//! `format_type` (postgres), MySQL via `information_schema` + `COLUMN_TYPE`
//! (mysql) — the same pattern, and the same reason for taking each type
//! spelling verbatim (`COLUMN_TYPE`, not `DATA_TYPE`, because only the
//! former distinguishes `tinyint(1)` from `tinyint`).
//!
//! **Schema provenance is the user's migration flow.** keelson-gen takes a
//! connection string and reads what is there; it neither parses migration
//! files nor tracks schema history. Point it at the database your
//! migrations produced.
//!
//! **Determinism.** Same schema + same config ⇒ byte-identical files:
//! tables sorted by name, columns in catalog order, foreign keys sorted by
//! column list, one bundled formatter (prettyplease — the user's rustfmt
//! version never touches the output), a fixed header with no timestamps.
//! Pinned by a generate-twice test.
//!
//! **Generated files are never hand-edited (the sqlc stance), and hooks
//! live outside them.** bob regenerates wholesale and so does keelson-gen:
//! every emitted file starts with `@generated … DO NOT EDIT`. The spec
//! models show application-written hooks *inside* the `Table` impl, which a
//! wholesale regenerator would clobber — the recorded resolution is
//! **config-declared hook delegation**: `[tables.users] hooks =
//! ["before_insert", …]` makes the generator emit an override of exactly
//! those trait methods, each a one-line delegation to
//! `<hooks.module>::users::before_insert(…)` — a module the application
//! writes by hand, outside the generated directory. Unlisted hooks stay
//! trait defaults (nothing is emitted, per the models crate's design), a
//! listed-but-unwritten hook is a compile error naming the missing path,
//! and regeneration can never eat application code because application code
//! never lives in a generated file.
//!
//! **Overrides must bind, at one named line.** Every column whose type came
//! from `[types.map]` or `[[types.override]]` emits
//! `const _: () = keelson_exec::assert_bind::<T>();` under a doc comment
//! naming the column — a replacement type that cannot bind fails to compile
//! on that line, not in an inference swamp (the contract
//! `keelson_exec::Bind` was built for).
//!
//! **Dialects.** PostgreSQL and SQLite are identical in shape (both have
//! `RETURNING` and `DEFAULT VALUES`); the machinery differences live
//! entirely in which crate the statements come from. MySQL is deliberately
//! *not* a copy of that path, because it has no `RETURNING` anywhere: its
//! `Table` body writes a plain `INSERT` (an all-unset setter being MySQL's
//! `VALUES ()`), and the model hands out its **marker** from `table()`
//! rather than `ModelTable`, with inherent verbs that can be honoured —
//! `insert(…).one()` inserts and then re-`SELECT`s by key (the setter's own
//! primary key, else `last_insert_id`), `update`/`delete` offer `exec` and
//! no `all`. The read-back is two statements and says so, in the generated
//! docs and in `keelson-models/tests/spec_mysql.rs`, which is the
//! specification this emits.
//!
//! **Every to-one relation field is `Option<Box<Row>>`.** A generated `Rel`
//! holds the target's whole row, so two models that reference each other
//! to-one hold each other by value — which is a recursive type of infinite
//! size, a compile error in the emitted code that no user of this generator
//! could work around. Two base tables with mutual single-column foreign keys
//! are enough to produce it. Boxing is therefore unconditional rather than
//! applied only to the edges that close a cycle: a field's type must not be
//! a function of the whole schema graph, or adding an unrelated foreign key
//! would change an existing struct and break code at a distance. To-many
//! fields stay `Vec<Row>`, which carries its own indirection. The argument
//! is recorded in full in `emit/model.rs`.
//!
//! **Factories are opt-in output, not a second generator.** `[output]
//! factories = true` adds one `factories.rs` — a keelson-factory template
//! module per writable table, exactly as `keelson-factory/tests/spec_*.rs`
//! specifies, writing through the *model's* insert path so hooks fire. It is
//! off by default: a production crate has no reason to carry test-data
//! machinery it never calls. The per-column default rule (unique columns
//! take sequences, defaulted columns are omitted, the rest are faked) is
//! recorded in `emit/factory.rs`.
//!
//! **Views are configured, not inferred** (`docs/views.md`). A view has no
//! foreign keys and usually no primary key, so the catalog cannot say how it
//! relates to anything or what identifies a row of it. Neither is guessed:
//! a relation touching a view is a `[[relationships]]` declaration carrying
//! an explicit `cardinality`, validated against the introspected schema so a
//! typo is a generation-time error naming the TOML key; and identity is
//! simply not required for reads, because the loaders group by the declared
//! join column rather than by a row identity. A keyless view therefore
//! *holds* and *is the target of* relations while getting less than a table
//! — no `Pk`, no `Setter`, no `INSERT`/`UPDATE`/`DELETE`, no keyed read-back
//! on MySQL, no factory. It earns the write surface only by declaring
//! `[tables.<name>] key`, and only when the engine says writes reach it,
//! which the three engines decide differently (PostgreSQL's
//! `pg_relation_is_updatable`, MySQL's `IS_UPDATABLE`, SQLite's `INSTEAD OF`
//! triggers).
//!
//! **Recorded limitations.** Multi-column foreign keys are introspected but
//! emit no relation (composite keys still work as `Pk` tuples); a base table
//! whose primary key falls to the column filters demotes to a view model;
//! `[output] factories = true` cannot cover a writable view and says so.

#![warn(missing_docs)]
#![forbid(unsafe_code)]

pub mod config;
mod emit;
mod error;
pub mod introspect;
mod names;
pub mod queries;
mod resolve;
mod rust_types;
pub mod schema;
mod typemap;

use std::path::{Path, PathBuf};

pub use config::Config;
pub use error::{GenError, Result};
pub use typemap::ResolvedType;

/// Introspect the configured database — or read the configured schema
/// snapshot — and render every generated file as `(file name, contents)`,
/// `mod.rs` first, without touching the filesystem.
pub fn generate(config: &Config) -> Result<Vec<(String, String)>> {
    let schema = introspect::introspect(config)?;
    generate_from_schema(&schema, config)
}

/// Introspect once, refreshing the configured schema snapshot when the answer
/// came from a live catalog.
///
/// Every entry point that *writes* goes through here, so the snapshot is a
/// by-product of an ordinary generator run rather than a second command
/// somebody has to remember. A run that already read the snapshot rewrites
/// nothing: there is no new truth to record.
pub(crate) fn introspect_and_refresh(config: &Config) -> Result<(schema::Schema, Option<PathBuf>)> {
    let (schema, origin) = introspect::introspect_from(config)?;
    let Some(path) = config.snapshot.as_deref() else {
        return Ok((schema, None));
    };
    if origin != introspect::Origin::Database {
        return Ok((schema, None));
    }
    let mut canonical = schema.clone();
    introspect::canonicalise(&mut canonical);
    schema::Snapshot::new(config.dialect, canonical).save(path)?;
    Ok((schema, Some(PathBuf::from(path))))
}

/// Introspect once, reporting how the configured schema snapshot differs from
/// the live catalog.
///
/// The mirror of [`introspect_and_refresh`], and the reason `--check` against
/// a real database is worth running at all: it is the only thing that can
/// catch a snapshot somebody forgot to commit. Against the snapshot itself
/// there is nothing to compare, and nothing is reported.
pub(crate) fn introspect_and_check(config: &Config) -> Result<(schema::Schema, Vec<Drift>)> {
    let (schema, origin) = introspect::introspect_from(config)?;
    let Some(path) = config.snapshot.as_deref() else {
        return Ok((schema, Vec::new()));
    };
    if origin != introspect::Origin::Database {
        return Ok((schema, Vec::new()));
    }
    let path = PathBuf::from(path);
    let mut canonical = schema.clone();
    introspect::canonicalise(&mut canonical);
    let rendered = schema::Snapshot::new(config.dialect, canonical).to_json()?;
    let drift = match std::fs::read_to_string(&path) {
        Ok(on_disk) if on_disk == rendered => Vec::new(),
        Ok(_) => vec![Drift::Stale(path)],
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![Drift::Missing(path)],
        Err(e) => return Err(GenError::Io(e)),
    };
    Ok((schema, drift))
}

/// Render from an already-held [`schema::Schema`] — the seam tests and
/// build scripts use to skip the database.
pub fn generate_from_schema(
    schema: &schema::Schema,
    config: &Config,
) -> Result<Vec<(String, String)>> {
    // Refuse unsupported dialects before anything else, so the error names
    // the real gap rather than the first unmapped column type.
    emit::Dial::new(config.dialect)?;
    let mut schema = schema.clone();
    introspect::canonicalise(&mut schema);
    let models = resolve::resolve(&schema, config)?;
    emit::render(&models, config)
}

/// Write rendered files into `out_dir`, creating it if needed. Returns the
/// written paths. Stale files from earlier runs are removed only if they
/// carry the `@generated` header, so a hand-written file dropped into the
/// directory by mistake is never deleted silently.
pub fn write_files(out_dir: &Path, files: &[(String, String)]) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(out_dir)?;
    // Remove generated leftovers whose table has disappeared.
    for entry in std::fs::read_dir(out_dir)? {
        let path = entry?.path();
        let is_ours = path.extension().is_some_and(|e| e == "rs")
            && std::fs::read_to_string(&path)
                .is_ok_and(|s| s.starts_with("// @generated by keelson-gen"));
        let still_wanted = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| files.iter().any(|(f, _)| f == n));
        if is_ours && !still_wanted {
            std::fs::remove_file(&path)?;
        }
    }
    let mut written = Vec::with_capacity(files.len());
    for (name, contents) in files {
        let path = out_dir.join(name);
        std::fs::write(&path, contents)?;
        written.push(path);
    }
    Ok(written)
}

/// One generated file that does not match what is on disk.
///
/// The three kinds are the three ways a committed generated tree can be wrong,
/// and they are kept apart because the fix differs: a `Missing` file was never
/// committed, a `Stale` one was committed before the schema moved, and an
/// `Orphaned` one belongs to a table that no longer exists — the file
/// [`write_files`] would have deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Drift {
    /// The generator writes this file and the output directory does not have
    /// it.
    Missing(PathBuf),
    /// The file is there and its contents are not what the generator renders.
    Stale(PathBuf),
    /// A file carrying the `@generated` header that the generator no longer
    /// writes.
    Orphaned(PathBuf),
}

impl Drift {
    /// The path the drift is about.
    pub fn path(&self) -> &Path {
        match self {
            Drift::Missing(p) | Drift::Stale(p) | Drift::Orphaned(p) => p,
        }
    }
}

impl std::fmt::Display for Drift {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Drift::Missing(p) => write!(f, "{}: not generated yet", p.display()),
            Drift::Stale(p) => write!(f, "{}: out of date", p.display()),
            Drift::Orphaned(p) => write!(f, "{}: no longer generated", p.display()),
        }
    }
}

/// [`write_files`] without the writing: compare rendered files against
/// `out_dir` and report what differs.
///
/// This is the half of the *migrate → regenerate → compile* loop that a CI job
/// needs. Committed generated code is only safe if something notices when it
/// stops matching the schema it came from, and noticing must not require
/// write access to the checkout — so nothing here touches the filesystem
/// except to read.
///
/// An empty result means the tree on disk is byte-for-byte what a run of the
/// generator would produce. The `@generated` rule is the same one
/// [`write_files`] deletes by, so a hand-written file dropped into the
/// directory is not reported as orphaned any more than it would be removed.
pub fn check_files(out_dir: &Path, files: &[(String, String)]) -> Result<Vec<Drift>> {
    let mut drift = Vec::new();

    for (name, rendered) in files {
        let path = out_dir.join(name);
        match std::fs::read_to_string(&path) {
            Ok(on_disk) if &on_disk == rendered => {}
            Ok(_) => drift.push(Drift::Stale(path)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => drift.push(Drift::Missing(path)),
            Err(e) => return Err(GenError::Io(e)),
        }
    }

    // A missing output directory is not an error here: every file it should
    // hold is already reported as `Missing`, and reporting the directory as
    // well would say the same thing twice.
    match std::fs::read_dir(out_dir) {
        Ok(entries) => {
            for entry in entries {
                let path = entry?.path();
                let is_ours = path.extension().is_some_and(|e| e == "rs")
                    && std::fs::read_to_string(&path)
                        .is_ok_and(|s| s.starts_with("// @generated by keelson-gen"));
                let still_wanted = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| files.iter().any(|(f, _)| f == n));
                if is_ours && !still_wanted {
                    drift.push(Drift::Orphaned(path));
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(GenError::Io(e)),
    }

    drift.sort_by(|a, b| a.path().cmp(b.path()));
    Ok(drift)
}

/// [`run`] without the writing: introspect, render, and report how the
/// configured output directory differs. What the CLI's `--check` calls.
pub fn check(config: &Config) -> Result<Vec<Drift>> {
    let out = config.out.clone().ok_or_else(|| {
        GenError::Config(
            "no `out` directory in the config (and none given on the command line)".to_owned(),
        )
    })?;
    let (schema, mut drift) = introspect_and_check(config)?;
    let files = generate_from_schema(&schema, config)?;
    drift.extend(check_files(Path::new(&out), &files)?);
    Ok(drift)
}

/// The documented main entry: introspect, render, write to the configured
/// output directory. This is what the `keelson-gen` binary calls.
pub fn run(config: &Config) -> Result<Vec<PathBuf>> {
    let out = config.out.clone().ok_or_else(|| {
        GenError::Config(
            "no `out` directory in the config (and none given on the command line)".to_owned(),
        )
    })?;
    let (schema, snapshot) = introspect_and_refresh(config)?;
    let files = generate_from_schema(&schema, config)?;
    let mut written = write_files(Path::new(&out), &files)?;
    written.extend(snapshot);
    Ok(written)
}
