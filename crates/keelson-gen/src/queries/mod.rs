//! Layer 4: hand-written SQL in, typed Rust out — and the same query usable
//! as a *mod*.
//!
//! This is the sqlc-shaped half of keelson-gen. Layer 2 generates a model per
//! table; this generates a module per **query file**: you write the SQL, and
//! the generator reads it against the introspected schema to give you a typed
//! parameter struct, a typed row struct, and an `async fn` against
//! `&dyn Executor`.
//!
//! What makes it keelson's rather than sqlc's is that a generated query has
//! **two faces**, both cut from one analysis:
//!
//! 1. **As a query** — a real `keelson_core::Query` over the file's own SQL,
//!    executed as written. It picks up the whole execution layer (`fetch_all`,
//!    tracing, transactions) and nests as a sub-select for free, because its
//!    placeholders are re-bound through the writer rather than copied.
//! 2. **As a mod** — `<name>_mod(params)` is an `impl Mod<SelectQuery>` that
//!    slices the *same* byte ranges and feeds each clause into the host
//!    statement's corresponding clause. Its `WHERE` is `AND`ed onto the host's;
//!    its `FROM` (joins included) is contributed when the host has none. **It
//!    does not nest as a sub-query**: one flat statement comes out, which is
//!    the whole point —
//!
//!    ```ignore
//!    users::table().query((
//!        queries::users::active_since_mod(cutoff),   // hand-written SQL, merged flat
//!        users::status().eq("published"),            // a typed model filter
//!        select::limit(20),                          // a Layer 1 mod
//!    ))
//!    ```
//!
//! Where the mod face cannot be honest — a set operation, a CTE, a
//! non-`SELECT` — the generator emits the query face and a recorded refusal
//! naming the reason. It never fakes flatness by nesting the query as a
//! sub-select.
//!
//! # The pipeline
//!
//! `spec` (annotations + statement spans) → the dialect analyser (`psql`,
//! `sqlite`) → [`ir::Analysis`] → `emit`. The analyser runs the dialect's own
//! parser for structure **and** a token scan for byte offsets in one pass, so
//! the row types and the clause slices are two readings of one analysis and
//! cannot drift.
//!
//! # The nullability decision table
//!
//! Getting `Option<T>` right is the whole game, so every rule is numbered,
//! carried on [`ir::OutputColumn::rule`], written into the generated file as a
//! doc comment, and tested one by one.
//!
//! | id | the shape | result |
//! |---|---|---|
//! | N1 | a column of a table reached by `FROM` or an inner join | the DDL's nullability |
//! | N2 | a column of a table on the nullable side of an outer join (`LEFT`'s right, `RIGHT`'s left, either side of `FULL`) | **nullable, even when the DDL says `NOT NULL`** |
//! | N3 | `WHERE col IS NOT NULL` | *no effect*: a filter narrows the rows, not the type |
//! | N4 | `COUNT(…)`, `COUNT(*)` | never NULL — an empty group counts zero |
//! | N5 | every other aggregate (`SUM`, `AVG`, `MIN`, `MAX`, `STRING_AGG`, `BOOL_AND`, …) | nullable — an empty group has no value |
//! | N7 | `COALESCE(a, b, …)` | nullable only when **every** argument is |
//! | N8 | a literal, `NOW()`, `CURRENT_DATE`, `CONCAT(…)` | never NULL (a bare `NULL` literal is nullable and untyped) |
//! | N9 | `CASE` | nullable when any arm is, or when there is no `ELSE` |
//! | N10 | an operator (`=`, `<`, `+`, `\|\|`, `AND`, …) | nullable when **any** operand is — SQL's three-valued logic |
//! | N11 | `IS NULL` / `IS NOT NULL` / `EXISTS` / `IN` | `bool`, never NULL |
//! | N12 | a scalar sub-query in the select list | nullable — zero rows yields NULL |
//! | N13 | `x::type` / `CAST(x AS t)` | the target type, the operand's nullability |
//! | N14 | a column of a set operation (`UNION`/`INTERSECT`/`EXCEPT`) | nullable when **any** arm's column is; a type disagreement is refused |
//! | N15 | a window function | `ROW_NUMBER`/`RANK`/`DENSE_RANK`/`NTILE` never NULL; the rest nullable |
//! | N16 | `-- nullable: <col> true\|false` | wins over everything above |
//! | A1 | `-- column: <col> <RustType>` | fixes the type (not the nullability) |
//!
//! And the parameter side, which the same machinery decides:
//!
//! | id | where the type came from |
//! |---|---|
//! | P1 | the column the placeholder is compared with (`WHERE user_id = $1`, `IN ($1, $2)`, `BETWEEN`, `LIKE`) |
//! | P2 | an explicit cast on the placeholder (`$1::uuid`) |
//! | P3 | the clause it sits in — `LIMIT`/`OFFSET` are `i64` |
//! | A2 | `-- param: $n [name] <RustType>` |
//!
//! A bound parameter is a *value*, so rule N10 does not treat it as a source
//! of NULL: `views > $1` over a `NOT NULL` column is a non-nullable boolean.
//! Declare `-- param: $1 Option<T>` if the call site really can pass NULL.
//!
//! Rule **N6 is deliberately absent** and recorded here so the gap is visible:
//! an aggregate's nullability under `GROUP BY` collapses into N4/N5 —
//! `GROUP BY` changes which rows exist, never whether a value can be NULL.
//!
//! When a type cannot be inferred the generator **refuses**, naming the
//! column and the annotation that would settle it. It never guesses `String`.
//!
//! # Dialects, honestly
//!
//! - **PostgreSQL** — complete: `SELECT` (set operations included) plus
//!   `INSERT`/`UPDATE`/`DELETE` typed from their `RETURNING` list. `pg_query`
//!   bundles the server's own parser, so the tree being read is PostgreSQL's.
//!   Pinned against a live PostgreSQL 17 under `--features live-docker`.
//! - **SQLite** — complete for the same shapes, through `sqlite3-parser`
//!   (`lemon-rs`: SQLite's own `parse.y`, ported), with `UPDATE`/`DELETE`
//!   typed from their `RETURNING`. The nullability rules are identical; the
//!   *types* differ where SQLite's do — every integer is `i64`, a comparison
//!   is an integer rather than a boolean, `sum` does not widen — and a
//!   declared type carries less, so more queries need a `-- column:`
//!   annotation. That is the schema's limit, recorded, not a weaker analyser.
//! - **MySQL** — [`GenError::Unsupported`], recorded: there is no trustworthy
//!   static parse tree for it in this workspace (`sqlparser` is a generic
//!   parser, not MySQL's), and the server will not describe a statement's
//!   result columns without executing it. An honest refusal beats an inferred
//!   type nobody can trust.
//!
//! # What the mod face refuses, and why
//!
//! A set operation, a `WITH` clause, a `FETCH`/`FOR UPDATE` tail, and any
//! non-`SELECT` have no clause a host `SELECT` could absorb without changing
//! meaning. Each keeps its query face and records the reason on
//! [`ir::Clauses::unsupported`], which the generated module repeats as a doc
//! comment. The generator never substitutes a sub-select for the flat merge.
//!
//! Two more rules of the merge, chosen and recorded rather than discovered:
//!
//! - the **select list is not contributed**, because the host statement owns
//!   its projection — that is what lets a typed model query and a mod sit in
//!   the same tuple;
//! - the **`FROM` is contributed only when the host has none**, so a model
//!   query already reading the same table keeps its own; the joins ride
//!   inside that one `FROM` item, which is what keeps the result flat.
//!
//! # Nested rows
//!
//! Output column names carry structure, bob's way: `author__name` is a to-one
//! nested field, `tags.name` is a to-many one, and `-- prefix:` switches the
//! separator. See [`nest`].

pub mod emit;
mod infer;
pub mod ir;
pub mod lex;
pub mod nest;
pub mod psql;
pub mod spec;
pub mod sqlite;

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::config::{Config, Dialect};
use crate::error::{GenError, Result};
use crate::schema::Schema;

pub use ir::{Analysis, Clauses, Nesting, OutputColumn, Param, Span};
pub use spec::{Cardinality, QueryFile, QuerySpec};

/// The `[queries]` section: where the `.sql` files are, and where the
/// generated modules go.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueriesConfig {
    /// The directory holding the `.sql` query files. Every `.sql` file
    /// directly inside it is generated from, in sorted order.
    pub dir: String,
    /// Where the generated modules land. Defaults to `<dir>` is deliberately
    /// *not* the rule — generated code and hand-written SQL stay apart.
    pub out: String,
    /// Path prefix the generated `include_str!` uses instead of the one
    /// computed from `out` → `dir`. Set it when the two are not relatable
    /// (different roots, or a build script writing outside the source tree).
    #[serde(default)]
    pub include_prefix: Option<String>,
}

/// Turn what the analysers found into the parameter list, in placeholder
/// order.
///
/// Shared by both dialects because the policy is the same one either way: an
/// explicit `-- param:` annotation wins, a type learnt from context comes
/// next, and a placeholder with neither is a **refusal** naming the annotation
/// that would settle it. Names are made unique by suffixing, so two
/// placeholders compared with the same column still produce two fields.
pub(crate) fn assemble_params(
    spec: &QuerySpec,
    placeholders: &[ir::Placeholder],
    found: &std::collections::BTreeMap<usize, (String, String, &'static str)>,
    spelling: char,
) -> Result<Vec<Param>> {
    let query = &spec.name;
    let mut numbers: Vec<usize> = placeholders.iter().map(|p| p.number).collect();
    numbers.sort_unstable();
    numbers.dedup();

    let mut used: Vec<String> = Vec::new();
    let mut params = Vec::with_capacity(numbers.len());
    for n in numbers {
        let annotated = spec.param_types.get(&n);
        let inferred = found.get(&n);
        let (rust_type, rule) = match (annotated, inferred) {
            (Some(t), _) => (t.clone(), "A2"),
            (None, Some((_, t, r))) => (t.clone(), *r),
            (None, None) => {
                return Err(GenError::Config(format!(
                    "query `{query}`: the type of `{spelling}{n}` cannot be inferred from its \
                     context; add `-- param: {spelling}{n} <RustType>`"
                )));
            }
        };
        let base = spec
            .param_names
            .get(&n)
            .cloned()
            .or_else(|| inferred.map(|(name, _, _)| name.clone()))
            .unwrap_or_else(|| format!("arg{n}"));
        let mut name = sanitise(&base);
        while used.contains(&name) {
            name.push('_');
        }
        used.push(name.clone());
        params.push(Param {
            number: n,
            name,
            rust_type,
            rule,
        });
    }
    Ok(params)
}

/// A SQL name made into a Rust field name.
fn sanitise(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    if out.is_empty() {
        out.push_str("arg");
    }
    out.to_lowercase()
}

/// Analyse one query file against the schema.
pub fn analyse(schema: &Schema, config: &Config, file: &QueryFile) -> Result<Vec<Analysis>> {
    file.queries
        .iter()
        .map(|spec| match config.dialect {
            Dialect::Psql => psql::analyse(schema, config, spec, &file.source),
            Dialect::Sqlite => sqlite::analyse(schema, config, spec, &file.source),
            Dialect::Mysql => Err(emit::mysql_refusal()),
        })
        .collect()
}

/// Every `.sql` file the configuration points at, in sorted order.
pub fn query_files(queries: &QueriesConfig) -> Result<Vec<QueryFile>> {
    let dir = Path::new(&queries.dir);
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| GenError::Config(format!("{}: {e}", dir.display())))?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "sql"))
        .collect();
    paths.sort();
    paths.iter().map(|p| spec::load(p)).collect()
}

/// Render every generated file as `(file name, contents)`, `mod.rs` first.
pub fn generate_from_schema(schema: &Schema, config: &Config) -> Result<Vec<(String, String)>> {
    let queries = config.queries.as_ref().ok_or_else(|| {
        GenError::Config(
            "no `[queries]` section in the config, so there is nothing to generate from".to_owned(),
        )
    })?;
    let dial = emit::Dial::new(config.dialect)?;
    let files = query_files(queries)?;

    let mut out = Vec::with_capacity(files.len() + 1);
    let mut modules: Vec<String> = files.iter().map(|f| f.module.clone()).collect();
    modules.sort();
    modules.dedup();
    out.push(("mod.rs".to_owned(), mod_rs(&modules)));

    for file in &files {
        let analyses = analyse(schema, config, file)?;
        let include = include_path(queries, &file.path)?;
        let tokens = emit::module(file, &analyses, &include, &dial)?;
        out.push((format!("{}.rs", file.module), render(tokens)?));
    }
    Ok(out)
}

/// Introspect, then render — without touching the filesystem.
pub fn generate(config: &Config) -> Result<Vec<(String, String)>> {
    let mut schema = crate::introspect::introspect(config)?;
    crate::introspect::canonicalise(&mut schema);
    generate_from_schema(&schema, config)
}

/// [`run`] without the writing: report how `[queries] out` differs from what
/// the `.sql` files would generate. What the CLI's `--check` calls.
///
/// The `.sql` files are the source of truth for these modules, so drift here
/// means either a query was edited without regenerating or the schema moved
/// under one. Both are the same fix — re-run the generator — and both are
/// invisible to `cargo build`, because the committed module still compiles.
pub fn check(config: &Config) -> Result<Vec<crate::Drift>> {
    let queries = config
        .queries
        .as_ref()
        .ok_or_else(|| GenError::Config("no `[queries]` section in the config".to_owned()))?;
    let (mut schema, mut drift) = crate::introspect_and_check(config)?;
    crate::introspect::canonicalise(&mut schema);
    let files = generate_from_schema(&schema, config)?;
    drift.extend(crate::check_files(Path::new(&queries.out), &files)?);
    Ok(drift)
}

/// Introspect, render, write into `[queries] out`. What the CLI's
/// `--queries` flag calls.
pub fn run(config: &Config) -> Result<Vec<PathBuf>> {
    let queries = config
        .queries
        .as_ref()
        .ok_or_else(|| GenError::Config("no `[queries]` section in the config".to_owned()))?;
    let (mut schema, snapshot) = crate::introspect_and_refresh(config)?;
    crate::introspect::canonicalise(&mut schema);
    let files = generate_from_schema(&schema, config)?;
    let mut written = crate::write_files(Path::new(&queries.out), &files)?;
    written.extend(snapshot);
    Ok(written)
}

const HEADER: &str = "// @generated by keelson-gen. DO NOT EDIT.\n\
                      // Regenerate from the .sql files instead; the SQL is the source of truth\n\
                      // and lives outside this directory.\n";

fn mod_rs(modules: &[String]) -> String {
    let mut out = String::from(HEADER);
    out.push_str("\n//! The generated queries, one module per .sql file.\n\n");
    for m in modules {
        let module = crate::names::ident(m);
        out.push_str(&format!("pub mod {module};\n"));
    }
    out
}

fn render(tokens: proc_macro2::TokenStream) -> Result<String> {
    let file: syn::File = syn::parse2(tokens)
        .map_err(|e| GenError::Config(format!("internal: generated tokens do not parse: {e}")))?;
    Ok(format!("{HEADER}\n{}", prettyplease::unparse(&file)))
}

/// The path a generated module's `include_str!` uses to reach its `.sql` file.
///
/// Computed from the configured strings rather than from canonicalised paths:
/// an absolute path would pin the output to one machine, and determinism is a
/// contract here.
fn include_path(queries: &QueriesConfig, sql: &Path) -> Result<String> {
    let name = sql
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| GenError::Config(format!("{}: unusable file name", sql.display())))?;
    if let Some(prefix) = &queries.include_prefix {
        return Ok(format!("{}{name}", with_slash(prefix)));
    }
    let rel = relative(Path::new(&queries.out), Path::new(&queries.dir)).ok_or_else(|| {
        GenError::Config(format!(
            "cannot express `{}` relative to `{}`; set `[queries] include_prefix`",
            queries.dir, queries.out
        ))
    })?;
    Ok(format!("{}{name}", with_slash(&rel)))
}

fn with_slash(p: &str) -> String {
    if p.is_empty() || p.ends_with('/') {
        p.to_owned()
    } else {
        format!("{p}/")
    }
}

/// A `../`-relative path from `from` to `to`, both read as written.
fn relative(from: &Path, to: &Path) -> Option<String> {
    use std::path::Component;
    let parts = |p: &Path| -> Option<Vec<String>> {
        let mut out = Vec::new();
        for c in p.components() {
            match c {
                Component::Normal(s) => out.push(s.to_str()?.to_owned()),
                Component::CurDir => {}
                Component::RootDir => out.push("/".to_owned()),
                Component::Prefix(_) | Component::ParentDir => return None,
            }
        }
        Some(out)
    };
    let (from, to) = (parts(from)?, parts(to)?);
    if from.first().map(String::as_str) == Some("/") || to.first().map(String::as_str) == Some("/")
    {
        // Mixing an absolute and a relative side cannot produce a portable
        // include path.
        if from.first() != to.first() {
            return None;
        }
    }
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut out: Vec<&str> = vec![".."; from.len() - common];
    out.extend(to[common..].iter().map(String::as_str));
    Some(out.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_include_path_walks_up_from_the_output_directory() {
        assert_eq!(
            relative(Path::new("src/queries"), Path::new("queries")).as_deref(),
            Some("../../queries")
        );
        assert_eq!(
            relative(Path::new("src/gen"), Path::new("src/sql")).as_deref(),
            Some("../sql")
        );
        assert_eq!(
            relative(Path::new("a"), Path::new("a/b")).as_deref(),
            Some("b")
        );
    }

    #[test]
    fn an_unrelatable_pair_is_a_config_error_naming_the_escape_hatch() {
        let q = QueriesConfig {
            dir: "/abs/queries".to_owned(),
            out: "src/queries".to_owned(),
            include_prefix: None,
        };
        let err = include_path(&q, Path::new("/abs/queries/users.sql")).unwrap_err();
        assert!(err.to_string().contains("include_prefix"), "{err}");
    }

    #[test]
    fn include_prefix_overrides_the_computation() {
        let q = QueriesConfig {
            dir: "/abs/queries".to_owned(),
            out: "src/queries".to_owned(),
            include_prefix: Some("../../sql".to_owned()),
        };
        assert_eq!(
            include_path(&q, Path::new("/abs/queries/users.sql")).unwrap(),
            "../../sql/users.sql"
        );
    }
}
