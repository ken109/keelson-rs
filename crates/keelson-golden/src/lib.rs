//! Replays the SQL fixtures extracted from Go's `bob` (see `tests/golden/README.md`).
//!
//! The fixtures record the SQL `bob` actually produced for each of its own test
//! cases. A dialect crate writes one test per case, builds the equivalent query,
//! and hands the result to [`assert_case`]. Comparison happens on [`clean`]ed
//! SQL, so formatting is free but tokens are pinned.

use std::sync::OnceLock;

use regex::Regex;
use serde::Deserialize;

const FIXTURE: &str = "tests/golden/bob-v0.42.0.jsonl";

/// A single bound argument as recorded from Go.
#[derive(Debug, Clone, Deserialize)]
pub struct Arg {
    /// The Go type of the value, e.g. `int` or `string`.
    pub go_type: String,
    /// The Go literal, as produced by `%#v`.
    pub repr: String,
    /// The value encoded as JSON, when it round-tripped.
    #[serde(default)]
    pub json: Option<serde_json::Value>,
}

/// One extracted test case.
#[derive(Debug, Clone, Deserialize)]
pub struct Case {
    /// `query` for a full statement, `expression` for a fragment.
    pub kind: String,
    /// `psql`, `mysql`, `sqlite`, or empty when dialect-agnostic.
    pub dialect: String,
    /// The `bob` test file the case came from.
    pub source: String,
    /// The case key in `bob`'s test table.
    pub name: String,
    #[serde(default)]
    pub doc: String,
    /// SQL as literally written in `bob`'s test. Informational only.
    pub expected_sql: String,
    /// What `bob` actually produced, byte for byte.
    pub generated_sql: String,
    /// [`clean`]ed `generated_sql`. This is the comparison target.
    pub clean_sql: String,
    /// `generated_sql` parsed and deparsed by the dialect's own parser.
    ///
    /// Lossy — it rewrites `"status"` to `status` and `LEAD` to `lead` — so it is
    /// a semantic sanity check only, never the assertion target.
    #[serde(default)]
    pub normalized_sql: String,
    /// Bound arguments, in placeholder order.
    pub args: Vec<Arg>,
    /// Set when `bob` returned an error instead of SQL.
    #[serde(default)]
    pub build_error: String,
    /// Expression cases only: the error `bob`'s test expected.
    #[serde(default)]
    pub expected_error: String,
}

impl Case {
    /// The expected arguments as JSON values, for comparison against a produced
    /// argument list serialised the same way.
    pub fn args_json(&self) -> Vec<serde_json::Value> {
        self.args
            .iter()
            .map(|a| a.json.clone().unwrap_or(serde_json::Value::Null))
            .collect()
    }

    /// The statement kind inferred from the source file, e.g. `select`.
    pub fn statement(&self) -> &str {
        let file = self.source.rsplit('/').next().unwrap_or(&self.source);
        file.strip_suffix("_test.go").unwrap_or(file)
    }
}

fn workspace_root() -> std::path::PathBuf {
    // This crate lives at <root>/crates/keelson-golden.
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("keelson-golden must live two levels below the workspace root")
        .to_path_buf()
}

/// Every extracted case, in fixture order.
pub fn all() -> &'static [Case] {
    static CASES: OnceLock<Vec<Case>> = OnceLock::new();
    CASES.get_or_init(|| {
        let path = workspace_root().join(FIXTURE);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));

        text.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                serde_json::from_str(l).unwrap_or_else(|e| panic!("malformed fixture line: {e}\n{l}"))
            })
            .collect()
    })
}

/// The cases for one dialect and statement kind, e.g. `("psql", "select")`.
pub fn cases(dialect: &str, statement: &str) -> Vec<&'static Case> {
    all()
        .iter()
        .filter(|c| c.dialect == dialect && c.statement() == statement)
        .collect()
}

/// Look up one case by dialect and name.
///
/// # Panics
/// If no such case exists — a typo in a test should fail loudly rather than
/// silently assert nothing.
pub fn case(dialect: &str, name: &str) -> &'static Case {
    all()
        .iter()
        .find(|c| c.dialect == dialect && c.name == name)
        .unwrap_or_else(|| {
            let near: Vec<_> = all()
                .iter()
                .filter(|c| c.dialect == dialect)
                .map(|c| c.name.as_str())
                .collect();
            panic!("no {dialect} case named {name:?}. available: {near:?}")
        })
}

/// Normalise SQL the way `bob`'s own test harness does before comparing.
///
/// Trim, collapse whitespace runs to a single space, then pad `(`, `)` and `|`
/// with spaces. Note that the padding step can introduce double spaces and they
/// are deliberately *not* re-collapsed, because `bob` does not re-collapse them
/// either — `NOW()` becomes `NOW (  ) `. Matching `bob` byte for byte after this
/// transform is the whole point.
pub fn clean(sql: &str) -> String {
    static SPACES: OnceLock<Regex> = OnceLock::new();
    static BRACKETS: OnceLock<Regex> = OnceLock::new();

    // Go's `\s` is ASCII-only; keep it that way so Unicode whitespace inside
    // string literals is left untouched.
    let spaces = SPACES.get_or_init(|| Regex::new(r"(?-u:\s)+").unwrap());
    // The character class in bob is `[\(|\)]`, which includes a literal `|`.
    // That is load-bearing: it pads the `||` concat operator too.
    let brackets = BRACKETS.get_or_init(|| Regex::new(r"(?-u:\s)*([(|)])(?-u:\s)*").unwrap());

    let trimmed = sql.trim();
    let collapsed = spaces.replace_all(trimmed, " ");
    brackets.replace_all(&collapsed, " $1 ").into_owned()
}

/// Assert that `produced` matches the recorded SQL for a case.
///
/// # Panics
/// With a diff-friendly message when the cleaned forms differ.
#[track_caller]
pub fn assert_sql(case: &Case, produced: &str) {
    let got = clean(produced);
    if got != case.clean_sql {
        panic!(
            "SQL mismatch for {} case {:?}\n  doc:      {}\n  expected: {}\n  actual:   {}\n\n  raw expected: {:?}\n  raw actual:   {:?}",
            case.dialect, case.name, case.doc, case.clean_sql, got, case.generated_sql, produced,
        );
    }
}

/// Assert that `produced` arguments match the recorded ones.
///
/// Arguments are compared as JSON so that a `Value` enum can be checked against
/// what Go bound without either side knowing about the other's types.
///
/// # Panics
/// When the argument lists differ in length or content.
#[track_caller]
pub fn assert_args(case: &Case, produced: &[serde_json::Value]) {
    let expected = case.args_json();
    if produced != expected.as_slice() {
        panic!(
            "args mismatch for {} case {:?}\n  expected: {:?}\n  actual:   {:?}",
            case.dialect, case.name, expected, produced,
        );
    }
}

/// Assert both the SQL and the arguments of a case.
///
/// # Panics
/// See [`assert_sql`] and [`assert_args`].
#[track_caller]
pub fn assert_case(dialect: &str, name: &str, produced_sql: &str, produced_args: &[serde_json::Value]) {
    let c = case(dialect, name);
    assert_sql(c, produced_sql);
    assert_args(c, produced_args);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_loads() {
        assert_eq!(all().len(), 80, "fixture case count changed");
    }

    #[test]
    fn clean_matches_recorded_clean_sql() {
        // The recorded clean_sql must be reproducible from generated_sql by our
        // port of bob's Clean. If this fails, the port of Clean is wrong and
        // every other golden assertion is meaningless.
        for c in all() {
            if c.generated_sql.is_empty() {
                continue;
            }
            assert_eq!(
                clean(&c.generated_sql),
                c.clean_sql,
                "clean() diverges from bob for {} case {:?}",
                c.dialect,
                c.name
            );
        }
    }

    #[test]
    fn clean_pads_brackets_and_pipes() {
        assert_eq!(clean("NOW()"), "NOW (  ) ");
        // Each `|` matches separately, and the padding of the first supplies the
        // leading space of the second, so `||` ends up with two spaces between.
        assert_eq!(clean("a  ||  b"), "a |  | b");
        assert_eq!(clean("  SELECT\n\t1  "), "SELECT 1");
    }

    #[test]
    fn statement_is_derived_from_source() {
        assert_eq!(case("psql", "simple select").statement(), "select");
    }
}
