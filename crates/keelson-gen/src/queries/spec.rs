//! The query file: sqlc-style annotation comments, and the byte span of the
//! SQL each one introduces.
//!
//! A query file is ordinary SQL. Everything the generator needs beyond the SQL
//! itself rides in `--` comments immediately above a statement, so the file
//! stays runnable in `psql`/`sqlite3` unchanged:
//!
//! ```sql
//! -- name: users_with_author :many
//! -- Every post with the author it belongs to.
//! -- param: $1 min_views i32
//! -- column: comment_count i64
//! SELECT p.id, p.title, u.id AS author__id, u.name AS author__name
//! FROM posts p LEFT JOIN users u ON u.id = p.user_id
//! WHERE p.views >= $1;
//! ```
//!
//! | annotation | meaning |
//! |---|---|
//! | `-- name: <ident> :one\|:optional\|:many\|:exec` | opens a query; the identifier is the generated fn name |
//! | `-- param: <$n> [name] <RustType>` | name and/or type for a placeholder inference could not settle |
//! | `-- column: <output name> <RustType>` | the Rust type of one output column (inner type; nullability is separate) |
//! | `-- nullable: <output name> true\|false` | overrides the inferred nullability |
//! | `-- prefix: <text>` | the nested-row prefix separator for this query (bob's `--prefix:videos.`) |
//!
//! Any other `--` line above the statement becomes a doc comment on the
//! generated items. A `-- name:` line ends the previous query and opens the
//! next, so one file holds as many queries as you like.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::{GenError, Result};

/// How many rows the caller gets back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cardinality {
    /// Exactly one row: `RowNotFound`/`TooManyRows` otherwise.
    One,
    /// Zero or one row.
    Optional,
    /// Every row.
    Many,
    /// No rows at all — run it for the side effect.
    Exec,
}

impl Cardinality {
    fn parse(s: &str) -> Option<Cardinality> {
        Some(match s {
            ":one" => Cardinality::One,
            ":optional" | ":opt" => Cardinality::Optional,
            ":many" => Cardinality::Many,
            ":exec" => Cardinality::Exec,
            _ => return None,
        })
    }

    /// Whether the query decodes rows into the generated row struct.
    pub fn returns_rows(self) -> bool {
        !matches!(self, Cardinality::Exec)
    }
}

/// One annotated statement in a query file.
#[derive(Debug, Clone)]
pub struct QuerySpec {
    /// The generated fn name, from `-- name:`.
    pub name: String,
    /// How many rows come back.
    pub cardinality: Cardinality,
    /// Free-form `--` lines above the statement, as doc comment text.
    pub doc: Vec<String>,
    /// Byte offset of the first character of the SQL, in the file's text.
    pub sql_start: usize,
    /// Byte offset one past the last character of the SQL (a trailing `;` and
    /// any trailing whitespace are excluded, so the span is exactly the
    /// statement).
    pub sql_end: usize,
    /// `-- param: $n <name> …` names, keyed by placeholder number.
    pub param_names: BTreeMap<usize, String>,
    /// `-- param: $n … <RustType>` types, keyed by placeholder number.
    pub param_types: BTreeMap<usize, String>,
    /// `-- column: <name> <RustType>` overrides, keyed by output column name.
    pub column_types: BTreeMap<String, String>,
    /// `-- nullable: <name> <bool>` overrides, keyed by output column name.
    pub column_nullable: BTreeMap<String, bool>,
    /// `-- prefix:` — the nested-row separator this query uses instead of the
    /// defaults (`__` for to-one, `.` for to-many).
    pub prefix: Option<String>,
}

impl QuerySpec {
    /// The SQL text this spec covers.
    pub fn sql<'a>(&self, source: &'a str) -> &'a str {
        &source[self.sql_start..self.sql_end]
    }
}

/// One `.sql` file: its module name, its text, and every query in it.
#[derive(Debug, Clone)]
pub struct QueryFile {
    /// The module name — the file stem.
    pub module: String,
    /// Where the file was read from.
    pub path: PathBuf,
    /// The whole file, verbatim. Generated code `include_str!`s it, and every
    /// span in every [`QuerySpec`] indexes it.
    pub source: String,
    /// The queries, in file order.
    pub queries: Vec<QuerySpec>,
}

/// Read and parse one query file.
pub fn load(path: &Path) -> Result<QueryFile> {
    let source = std::fs::read_to_string(path)?;
    let module = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| GenError::Config(format!("{}: unusable file name", path.display())))?
        .to_owned();
    let queries =
        parse(&source).map_err(|e| GenError::Config(format!("{}: {e}", path.display())))?;
    if queries.is_empty() {
        return Err(GenError::Config(format!(
            "{}: no `-- name:` annotation, so there is nothing to generate",
            path.display()
        )));
    }
    Ok(QueryFile {
        module,
        path: path.to_path_buf(),
        source,
        queries,
    })
}

/// Split a file's text into annotated queries.
pub fn parse(source: &str) -> std::result::Result<Vec<QuerySpec>, String> {
    let mut out: Vec<QuerySpec> = Vec::new();
    let mut current: Option<QuerySpec> = None;
    // Where the SQL of the query being accumulated starts, and where the last
    // non-blank SQL byte was seen.
    let mut sql_start: Option<usize> = None;
    let mut sql_end = 0usize;

    for (offset, line) in lines_with_offsets(source) {
        let trimmed = line.trim();
        let is_comment = trimmed.starts_with("--");
        let body = trimmed.strip_prefix("--").map(str::trim).unwrap_or("");

        if is_comment && body.starts_with("name:") {
            finish(source, &mut current, &mut out, &mut sql_start, sql_end)?;
            current = Some(parse_name(body["name:".len()..].trim())?);
            continue;
        }

        let Some(spec) = current.as_mut() else {
            // Leading text before the first `-- name:` is preamble; a
            // non-comment there is a file-level statement the generator does
            // not own, and saying so beats generating from half a file.
            if !trimmed.is_empty() && !is_comment {
                return Err(format!(
                    "statement text before the first `-- name:` annotation: {trimmed:?}"
                ));
            }
            continue;
        };

        if is_comment && sql_start.is_none() {
            annotate(spec, body)?;
            continue;
        }
        if trimmed.is_empty() && sql_start.is_none() {
            continue;
        }
        // SQL (a `--` comment inside the statement body stays part of it).
        if sql_start.is_none() {
            sql_start = Some(offset + leading_ws(line));
        }
        if !trimmed.is_empty() {
            sql_end = offset + line.trim_end().len();
        }
    }
    finish(source, &mut current, &mut out, &mut sql_start, sql_end)?;
    Ok(out)
}

fn finish(
    source: &str,
    current: &mut Option<QuerySpec>,
    out: &mut Vec<QuerySpec>,
    sql_start: &mut Option<usize>,
    sql_end: usize,
) -> std::result::Result<(), String> {
    let Some(mut spec) = current.take() else {
        return Ok(());
    };
    let start = sql_start
        .take()
        .ok_or_else(|| format!("`-- name: {}` is followed by no SQL", spec.name))?;
    if sql_end <= start {
        return Err(format!("`-- name: {}` is followed by no SQL", spec.name));
    }
    // Trim trailing `;`/whitespace: the span is the statement, which is what an
    // executor is handed and what every clause slice is measured against. Both
    // are ASCII, so byte-wise trimming stays on a character boundary.
    let mut end = sql_end;
    while end > start {
        let b = source.as_bytes()[end - 1];
        if b == b';' || b.is_ascii_whitespace() {
            end -= 1;
        } else {
            break;
        }
    }
    if end <= start {
        return Err(format!("`-- name: {}` is followed by no SQL", spec.name));
    }
    spec.sql_start = start;
    spec.sql_end = end;
    out.push(spec);
    Ok(())
}

fn leading_ws(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

fn lines_with_offsets(source: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut offset = 0usize;
    source.split_inclusive('\n').map(move |line| {
        let at = offset;
        offset += line.len();
        (at, line.trim_end_matches('\n').trim_end_matches('\r'))
    })
}

fn parse_name(rest: &str) -> std::result::Result<QuerySpec, String> {
    let mut parts = rest.split_whitespace();
    let name = parts
        .next()
        .ok_or_else(|| "`-- name:` needs a query name".to_owned())?;
    if !is_ident(name) {
        return Err(format!(
            "`-- name: {name}` is not a usable Rust identifier (snake_case letters, digits and `_`)"
        ));
    }
    let card = match parts.next() {
        Some(c) => Cardinality::parse(c).ok_or_else(|| {
            format!("`-- name: {name} {c}`: expected one of :one, :optional, :many, :exec")
        })?,
        None => {
            return Err(format!(
                "`-- name: {name}` needs a result kind (:one, :optional, :many or :exec)"
            ));
        }
    };
    if let Some(extra) = parts.next() {
        return Err(format!("`-- name: {name}`: unexpected trailing {extra:?}"));
    }
    Ok(QuerySpec {
        name: name.to_owned(),
        cardinality: card,
        doc: Vec::new(),
        sql_start: 0,
        sql_end: 0,
        param_names: BTreeMap::new(),
        param_types: BTreeMap::new(),
        column_types: BTreeMap::new(),
        column_nullable: BTreeMap::new(),
        prefix: None,
    })
}

fn annotate(spec: &mut QuerySpec, body: &str) -> std::result::Result<(), String> {
    if let Some(rest) = body.strip_prefix("param:") {
        let parts: Vec<&str> = rest.split_whitespace().collect();
        let (place, name, ty) = match parts.as_slice() {
            [p, t] => (*p, None, *t),
            [p, n, t] => (*p, Some(*n), *t),
            _ => {
                return Err(format!(
                    "`-- param:{rest}`: expected `$n [name] <RustType>`"
                ));
            }
        };
        let n = placeholder_number(place)
            .ok_or_else(|| format!("`-- param:`: {place:?} is not a placeholder like `$1`/`?1`"))?;
        if let Some(name) = name {
            if !is_ident(name) {
                return Err(format!("`-- param:`: {name:?} is not a Rust identifier"));
            }
            spec.param_names.insert(n, name.to_owned());
        }
        spec.param_types.insert(n, ty.to_owned());
        return Ok(());
    }
    if let Some(rest) = body.strip_prefix("column:") {
        let parts: Vec<&str> = rest.split_whitespace().collect();
        let [name, ty] = parts.as_slice() else {
            return Err(format!(
                "`-- column:{rest}`: expected `<output name> <RustType>`"
            ));
        };
        spec.column_types
            .insert((*name).to_owned(), (*ty).to_owned());
        return Ok(());
    }
    if let Some(rest) = body.strip_prefix("nullable:") {
        let parts: Vec<&str> = rest.split_whitespace().collect();
        let [name, flag] = parts.as_slice() else {
            return Err(format!(
                "`-- nullable:{rest}`: expected `<output name> true|false`"
            ));
        };
        let value = match *flag {
            "true" => true,
            "false" => false,
            other => {
                return Err(format!(
                    "`-- nullable:`: expected true or false, got {other:?}"
                ));
            }
        };
        spec.column_nullable.insert((*name).to_owned(), value);
        return Ok(());
    }
    if let Some(rest) = body.strip_prefix("prefix:") {
        let p = rest.trim();
        if p.is_empty() {
            return Err("`-- prefix:` needs a separator, e.g. `videos.`".to_owned());
        }
        spec.prefix = Some(p.to_owned());
        return Ok(());
    }
    if !body.is_empty() {
        spec.doc.push(body.to_owned());
    }
    Ok(())
}

/// `$12` / `?12` / `12` → 12.
pub(crate) fn placeholder_number(s: &str) -> Option<usize> {
    let digits = s
        .strip_prefix('$')
        .or_else(|| s.strip_prefix('?'))
        .unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with(|c: char| c.is_ascii_digit())
        && s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "\
-- A file-level note before anything is named is preamble.

-- name: get_user :one
-- Fetch one user.
-- param: $1 user_id i32
-- column: total i64
-- nullable: total false
-- prefix: author.
SELECT id, name FROM users WHERE id = $1;

-- name: list_users :many
SELECT id FROM users
";

    #[test]
    fn a_file_splits_into_one_query_per_name_annotation() {
        let qs = parse(FILE).unwrap();
        assert_eq!(qs.len(), 2);
        assert_eq!(qs[0].name, "get_user");
        assert_eq!(qs[0].cardinality, Cardinality::One);
        assert_eq!(qs[1].name, "list_users");
        assert_eq!(qs[1].cardinality, Cardinality::Many);
    }

    #[test]
    fn the_span_is_the_statement_without_its_trailing_semicolon() {
        let qs = parse(FILE).unwrap();
        assert_eq!(qs[0].sql(FILE), "SELECT id, name FROM users WHERE id = $1");
        assert_eq!(qs[1].sql(FILE), "SELECT id FROM users");
    }

    #[test]
    fn every_annotation_lands_where_it_belongs() {
        let q = &parse(FILE).unwrap()[0];
        assert_eq!(q.param_names[&1], "user_id");
        assert_eq!(q.param_types[&1], "i32");
        assert_eq!(q.column_types["total"], "i64");
        assert!(!q.column_nullable["total"]);
        assert_eq!(q.prefix.as_deref(), Some("author."));
        assert_eq!(q.doc, vec!["Fetch one user.".to_owned()]);
    }

    #[test]
    fn a_comment_inside_the_statement_stays_part_of_it() {
        let src = "-- name: q :many\nSELECT id\n-- a note about the FROM\nFROM users";
        let q = &parse(src).unwrap()[0];
        assert!(q.sql(src).contains("-- a note about the FROM"));
    }

    #[test]
    fn a_missing_or_unknown_result_kind_is_an_error() {
        assert!(parse("-- name: q\nSELECT 1").is_err());
        assert!(parse("-- name: q :lots\nSELECT 1").is_err());
        assert!(parse("-- name: 9q :one\nSELECT 1").is_err());
    }

    #[test]
    fn a_name_with_no_sql_after_it_is_an_error() {
        let err = parse("-- name: q :one\n-- just a comment\n").unwrap_err();
        assert!(err.contains("followed by no SQL"), "{err}");
    }

    #[test]
    fn a_statement_before_the_first_name_is_an_error() {
        let err = parse("SELECT 1;\n-- name: q :one\nSELECT 1").unwrap_err();
        assert!(err.contains("before the first"), "{err}");
    }

    #[test]
    fn a_malformed_annotation_names_the_form_it_wanted() {
        let err = parse("-- name: q :one\n-- param: $1\nSELECT 1").unwrap_err();
        assert!(err.contains("$n [name] <RustType>"), "{err}");
        let err = parse("-- name: q :one\n-- nullable: c maybe\nSELECT 1").unwrap_err();
        assert!(err.contains("true or false"), "{err}");
    }

    #[test]
    fn placeholder_numbers_are_read_in_either_dialects_spelling() {
        assert_eq!(placeholder_number("$12"), Some(12));
        assert_eq!(placeholder_number("?3"), Some(3));
        assert_eq!(placeholder_number("$x"), None);
        assert_eq!(placeholder_number("?"), None);
    }
}
