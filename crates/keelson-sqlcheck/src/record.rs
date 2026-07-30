//! Opt-in recording of every SQL string the judges accepted — Tier D's input.
//!
//! Tier D (`docs/testing-tiers.md`) answers "which grammar constructs did the
//! test suites actually exercise?" by measuring the SQL itself, not the Rust
//! that produced it: line coverage can say a function ran, but only the SQL
//! side can say the `USING` join form was never rendered. So the raw material
//! is every string that passed through the judges, and this module is the tap.
//!
//! Set [`ENV_VAR`] to a directory and every successful [`check`](crate::check)
//! (and therefore `assert_valid`, `assert_sql`, and the `testing` assertions,
//! which all funnel through it) appends one dialect-tagged line. With the
//! variable unset, [`record`] is a single `OnceLock` read — nothing is opened,
//! nothing is written.
//!
//! # Concurrency
//!
//! `cargo test` runs many test binaries at once, so the recording is
//! **one file per process**, named by pid and opened in append mode: no write
//! from one process can interleave inside a line of another, and a pid reused
//! by a later binary appends rather than truncates. Within a process a mutex
//! serialises the writes.
//!
//! # Format
//!
//! One record per line: `dialect<TAB>sql`, with `\`, tab, newline and carriage
//! return escaped so any SQL round-trips through line granularity. Only SQL
//! that a judge *accepted* is recorded — the corpus is evidence of what the
//! library renders, and the deliberately-malformed strings of negative tests
//! are not that.

use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::Dialect;

/// The environment variable that switches recording on: a directory path,
/// created if missing.
pub const ENV_VAR: &str = "KEELSON_SQLCHECK_RECORD";

static SINK: OnceLock<Option<Mutex<File>>> = OnceLock::new();

fn open_sink() -> Option<Mutex<File>> {
    let dir = std::env::var_os(ENV_VAR)?;
    let dir = PathBuf::from(dir);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("{ENV_VAR}: cannot create {}: {e}", dir.display());
        return None;
    }
    let path = dir.join(format!("record-{}.tsv", std::process::id()));
    match File::options().create(true).append(true).open(&path) {
        Ok(f) => Some(Mutex::new(f)),
        Err(e) => {
            eprintln!("{ENV_VAR}: cannot open {}: {e}", path.display());
            None
        }
    }
}

/// Append one judged SQL string to the recording, if recording is on.
///
/// Called by [`check`](crate::check) on success; call it directly only from a
/// test that judges SQL through a parser without going through `check` (the
/// psql combinatorial suite inspects the parse tree, so it parses for itself).
pub fn record(dialect: Dialect, sql: &str) {
    let Some(sink) = SINK.get_or_init(open_sink) else {
        return;
    };
    let mut line = String::with_capacity(sql.len() + 8);
    line.push_str(dialect.name());
    line.push('\t');
    escape_into(sql, &mut line);
    line.push('\n');
    // A poisoned lock means another test thread panicked mid-write; recording
    // is diagnostics, so keep going rather than propagate the panic.
    let mut file = match sink.lock() {
        Ok(f) => f,
        Err(poisoned) => poisoned.into_inner(),
    };
    let _ = file.write_all(line.as_bytes());
}

fn escape_into(sql: &str, out: &mut String) {
    for ch in sql.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
}

fn unescape(escaped: &str) -> String {
    let mut out = String::with_capacity(escaped.len());
    let mut chars = escaped.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            // A dangling or unknown escape is kept verbatim rather than lost:
            // the reader's job is to reconstruct, not to validate.
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Parse one recorded line back into its dialect and SQL.
///
/// `None` for a malformed line — no tab, or an unknown dialect name — which the
/// reader counts rather than silently drops.
pub fn parse_line(line: &str) -> Option<(Dialect, String)> {
    let (name, sql) = line.split_once('\t')?;
    Some((Dialect::from_name(name)?, unescape(sql)))
}

/// Every recording in `dir`, in file order. Returns `(records, malformed_lines)`.
///
/// # Errors
/// If the directory cannot be read at all. Unreadable single files are
/// reported the same way, because a partial corpus would understate coverage
/// and the gate's whole job is not to do that.
pub fn read_dir(dir: &Path) -> std::io::Result<(Vec<(Dialect, String)>, usize)> {
    let mut records = Vec::new();
    let mut malformed = 0;
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)?
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "tsv"))
        .collect();
    paths.sort();
    for path in paths {
        let text = std::fs::read_to_string(&path)?;
        for line in text.lines() {
            if line.is_empty() {
                continue;
            }
            match parse_line(line) {
                Some(rec) => records.push(rec),
                None => malformed += 1,
            }
        }
    }
    Ok((records, malformed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_round_trips_sql_with_every_escaped_character() {
        let sql = "SELECT 'a\tb\nc\r' AS \"x\\y\"";
        let mut line = String::from("psql\t");
        escape_into(sql, &mut line);
        assert!(
            !line.contains('\n'),
            "escaping must keep one record per line"
        );
        let (dialect, back) = parse_line(&line).expect("parses");
        assert_eq!(dialect, Dialect::Psql);
        assert_eq!(back, sql);
    }

    #[test]
    fn malformed_lines_are_none_not_garbage() {
        assert!(parse_line("no tab here").is_none());
        assert!(parse_line("oracle\tSELECT 1").is_none());
    }
}
