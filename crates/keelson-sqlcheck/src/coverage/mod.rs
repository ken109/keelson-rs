//! Tier D — grammar-construct coverage measured from the SQL side.
//!
//! Tiers A–C generate and judge SQL; this module answers the question none of
//! them can: **across everything they judged, which grammar constructs did the
//! library actually exercise?** Line coverage cannot answer it — a line can run
//! without the construct appearing in the output — so the measurement replays
//! the recorded SQL itself (see [`crate::record()`]) against a checked-in
//! *manifest* of every construct each dialect claims to render, and the gate
//! fails when a declared construct never appeared.
//!
//! # How each dialect is measured
//!
//! **psql** is parsed with [`pg_query`] — PostgreSQL's own parser — and the
//! parse tree is walked: node kinds and their discriminating fields
//! (`JoinExpr.jointype`, `SortBy.sortby_nulls`, `LockingClause.strength`, …)
//! are mapped to construct ids. A handful of spellings the parse tree
//! normalises away (`FETCH FIRST` vs `LIMIT`, `::` vs `CAST`, `EXCLUDE NO
//! OTHERS`) fall back to token signatures, marked `sig =` in the manifest.
//!
//! **mysql and sqlite have no pg_query equivalent**, so their tier is
//! *token-level*: every manifest entry carries one or more `sig =` patterns
//! (literal substrings, `{*}` matching one operand-shaped token — see
//! [`sig_matches`]) matched against the recorded SQL. Token matching cannot
//! see structure — a `LIMIT` inside a sub-query counts the same as one
//! outside — and that limit is accepted and stated here rather than papered
//! over.
//!
//! # The manifest is the deliverable
//!
//! `coverage/<dialect>.manifest` lists every construct with the keelson API
//! that produces it; `coverage/<dialect>.exclusions` lists, each with a
//! self-contained reason, every construct deliberately *not* in the manifest.
//! The gate also reports (without failing) constructs it observed that no
//! manifest entry accounts for, so the manifest cannot silently rot.

use std::collections::{BTreeMap, BTreeSet};

use crate::Dialect;

// ===========================================================================
// Manifest and exclusion files
// ===========================================================================

/// One declared construct: the grammar production keelson claims to render.
#[derive(Debug, Clone)]
pub struct ManifestEntry {
    /// Stable construct id, e.g. `join.left`.
    pub id: String,
    /// The keelson API that produces it, for the human reading a gate failure.
    pub api: String,
    /// Token signatures. Required for mysql/sqlite; for psql only where the
    /// parse tree normalises the spelling away.
    pub sigs: Vec<String>,
}

/// One construct deliberately left out of the manifest, with its audit trail.
#[derive(Debug, Clone)]
pub struct ExclusionEntry {
    /// The construct id that would otherwise be declared.
    pub id: String,
    /// Why it is excluded — self-contained, readable without any tracker.
    pub reason: String,
}

/// A dialect's manifest plus its exclusion list.
#[derive(Debug, Clone, Default)]
pub struct DialectPlan {
    /// The declared constructs.
    pub manifest: Vec<ManifestEntry>,
    /// The reasoned exclusions.
    pub exclusions: Vec<ExclusionEntry>,
}

/// The three dialects' plans.
#[derive(Debug, Clone)]
pub struct Config {
    /// Per-dialect manifest and exclusions.
    pub plans: BTreeMap<&'static str, DialectPlan>,
}

impl Config {
    /// The checked-in manifests, compiled into the binary so the gate needs no
    /// paths at run time.
    ///
    /// # Errors
    /// If a file fails validation — which means the checked-in file is broken
    /// and the gate must not pretend to have measured anything.
    pub fn embedded() -> Result<Config, String> {
        let mut plans = BTreeMap::new();
        for (dialect, manifest, exclusions) in [
            (
                Dialect::Psql,
                include_str!("../../coverage/psql.manifest"),
                include_str!("../../coverage/psql.exclusions"),
            ),
            (
                Dialect::Mysql,
                include_str!("../../coverage/mysql.manifest"),
                include_str!("../../coverage/mysql.exclusions"),
            ),
            (
                Dialect::Sqlite,
                include_str!("../../coverage/sqlite.manifest"),
                include_str!("../../coverage/sqlite.exclusions"),
            ),
        ] {
            let plan = DialectPlan {
                manifest: parse_manifest(manifest)
                    .map_err(|e| format!("{}.manifest: {e}", dialect.name()))?,
                exclusions: parse_exclusions(exclusions)
                    .map_err(|e| format!("{}.exclusions: {e}", dialect.name()))?,
            };
            validate_plan(dialect, &plan)?;
            plans.insert(dialect.name(), plan);
        }
        Ok(Config { plans })
    }

    fn plan(&self, dialect: Dialect) -> &DialectPlan {
        &self.plans[dialect.name()]
    }
}

/// Parse a manifest file: `[id]` sections with `api =` and repeatable `sig =`.
///
/// # Errors
/// On an unknown key, a duplicate id, an entry without `api`, or a key outside
/// any section — the manifest is load-bearing and half-parsed is worse than
/// refused.
pub fn parse_manifest(text: &str) -> Result<Vec<ManifestEntry>, String> {
    let mut entries: Vec<ManifestEntry> = Vec::new();
    let mut seen = BTreeSet::new();
    for (ln, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(id) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            if !seen.insert(id.to_string()) {
                return Err(format!("line {}: duplicate id [{id}]", ln + 1));
            }
            entries.push(ManifestEntry {
                id: id.to_string(),
                api: String::new(),
                sigs: Vec::new(),
            });
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("line {}: expected `key = value`: {line}", ln + 1));
        };
        let Some(entry) = entries.last_mut() else {
            return Err(format!("line {}: key before any [id] section", ln + 1));
        };
        match key.trim() {
            "api" => entry.api = value.trim().to_string(),
            // A signature's spaces are load-bearing (` WHERE` is not `WHERE`),
            // so only the single separator space after `=` is consumed:
            // `sig =  WHERE` declares ` WHERE`.
            "sig" => entry.sigs.push(sig_value(value)),
            other => return Err(format!("line {}: unknown key `{other}`", ln + 1)),
        }
    }
    for entry in &entries {
        if entry.api.is_empty() {
            return Err(format!("[{}] has no `api =` line", entry.id));
        }
    }
    Ok(entries)
}

/// A signature value: everything after `= `, spaces preserved. Trailing spaces
/// do not survive the line trim, so signatures must not rely on them.
fn sig_value(after_equals: &str) -> String {
    after_equals
        .strip_prefix(' ')
        .unwrap_or(after_equals)
        .to_string()
}

/// Parse an exclusion file: `[id]` sections with a required `reason =`.
///
/// # Errors
/// As [`parse_manifest`] — an unreasoned exclusion is not auditable, which is
/// the only thing an exclusion list is for.
pub fn parse_exclusions(text: &str) -> Result<Vec<ExclusionEntry>, String> {
    let mut entries: Vec<ExclusionEntry> = Vec::new();
    let mut seen = BTreeSet::new();
    for (ln, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(id) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            if !seen.insert(id.to_string()) {
                return Err(format!("line {}: duplicate id [{id}]", ln + 1));
            }
            entries.push(ExclusionEntry {
                id: id.to_string(),
                reason: String::new(),
            });
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("line {}: expected `key = value`: {line}", ln + 1));
        };
        let Some(entry) = entries.last_mut() else {
            return Err(format!("line {}: key before any [id] section", ln + 1));
        };
        match key.trim() {
            "reason" => entry.reason = value.trim().to_string(),
            other => return Err(format!("line {}: unknown key `{other}`", ln + 1)),
        }
    }
    for entry in &entries {
        if entry.reason.is_empty() {
            return Err(format!("[{}] has no `reason =` line", entry.id));
        }
    }
    Ok(entries)
}

fn validate_plan(dialect: Dialect, plan: &DialectPlan) -> Result<(), String> {
    let ids: BTreeSet<&str> = plan.manifest.iter().map(|e| e.id.as_str()).collect();
    for excl in &plan.exclusions {
        if ids.contains(excl.id.as_str()) {
            return Err(format!(
                "{}: [{}] is both declared and excluded — pick one",
                dialect.name(),
                excl.id
            ));
        }
    }
    match dialect {
        // A psql entry with no signature must be one the tree walker can emit,
        // or nothing could ever mark it exercised and the gate would fail
        // forever for a reason that is the manifest's, not the tests'.
        Dialect::Psql => {
            let detectable: BTreeSet<&str> = PSQL_DETECTABLE.iter().copied().collect();
            for entry in &plan.manifest {
                if entry.sigs.is_empty() && !detectable.contains(entry.id.as_str()) {
                    return Err(format!(
                        "psql: [{}] has no `sig =` and no tree detector emits it",
                        entry.id
                    ));
                }
            }
        }
        // The token dialects have nothing but signatures.
        Dialect::Mysql | Dialect::Sqlite => {
            for entry in &plan.manifest {
                if entry.sigs.is_empty() {
                    return Err(format!(
                        "{}: [{}] needs at least one `sig =` — this dialect is token-level",
                        dialect.name(),
                        entry.id
                    ));
                }
            }
        }
    }
    Ok(())
}

// ===========================================================================
// Signature matching
// ===========================================================================

/// Match a token signature against SQL.
///
/// A signature is a literal substring, except that `{*}` matches exactly one
/// operand-shaped token: a run of non-whitespace characters that is *not* a
/// bare uppercase keyword. That is enough for `OFFSET {*} ROWS` to match
/// `OFFSET 5 ROWS` without matching across `OFFSET 5 FETCH … ROWS`, and for
/// ` {*} PRECEDING` to match `$1 PRECEDING` without matching
/// `UNBOUNDED PRECEDING`. A literal `*` (multiplication, `SELECT *`) is just a
/// character.
pub fn sig_matches(sql: &str, sig: &str) -> bool {
    const WILDCARD: &str = "{*}";
    if !sig.contains(WILDCARD) {
        return sql.contains(sig);
    }
    let segments: Vec<&str> = sig.split(WILDCARD).collect();
    let first = segments[0];
    let mut search_from = 0;
    'starts: while search_from <= sql.len() {
        let Some(found) = sql[search_from..].find(first) else {
            return false;
        };
        let start = search_from + found;
        // A later attempt restarts one char further on; `first` may be empty
        // (a signature beginning with the wildcard), so advance by at least 1.
        search_from = start + first.len().max(1);
        let mut pos = start + first.len();
        for seg in &segments[1..] {
            if seg.is_empty() {
                // A trailing wildcard constrains nothing beyond "some token
                // follows", which the segment before it already anchors.
                continue;
            }
            let Some(next) = sql[pos..].find(seg) else {
                continue 'starts;
            };
            if !wildcard_token_ok(&sql[pos..pos + next]) {
                continue 'starts;
            }
            pos = pos + next + seg.len();
        }
        return true;
    }
    false
}

/// What `{*}` may consume: no whitespace (one token), and not a bare keyword —
/// operands are numbers, placeholders, quoted names or parenthesised
/// expressions, none of which is a run of capital letters.
fn wildcard_token_ok(gap: &str) -> bool {
    if gap.chars().any(char::is_whitespace) {
        return false;
    }
    gap.is_empty() || !gap.chars().all(|c| c.is_ascii_uppercase())
}

mod psql_ast;

use psql_ast::{PSQL_ACCOUNTED_KINDS, scan_keywords};
pub use psql_ast::{PSQL_DETECTABLE, PsqlObservation, psql_constructs};

// ===========================================================================
// Analysis
// ===========================================================================

/// One dialect's measured coverage.
#[derive(Debug)]
pub struct Outcome {
    /// Which dialect.
    pub dialect: Dialect,
    /// Unique judged statements replayed.
    pub unique_statements: usize,
    /// Declared constructs (manifest size).
    pub declared: usize,
    /// Reasoned exclusions (not counted in `declared`).
    pub excluded: usize,
    /// Declared construct ids that appeared.
    pub exercised: BTreeSet<String>,
    /// Declared-but-never-observed constructs — the gate's failure list.
    pub unexercised: Vec<ManifestEntry>,
    /// Observed-but-undeclared constructs — reported, not fatal.
    pub undeclared: Vec<String>,
    /// Recorded psql SQL the parser now rejects (should be none).
    pub parse_failures: Vec<String>,
}

/// The whole gate result.
#[derive(Debug)]
pub struct Report {
    /// Per-dialect outcomes, in `Dialect` order.
    pub outcomes: Vec<Outcome>,
    /// Recording lines that did not parse (should be zero).
    pub malformed_lines: usize,
}

impl Report {
    /// Whether the gate passes: every declared construct exercised, nothing
    /// unparseable, nothing malformed.
    pub fn passed(&self) -> bool {
        self.malformed_lines == 0
            && self
                .outcomes
                .iter()
                .all(|o| o.unexercised.is_empty() && o.parse_failures.is_empty())
    }

    /// The gate's human-readable output.
    pub fn render(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        for o in &self.outcomes {
            let _ = writeln!(
                out,
                "== {}: {} / {} declared constructs exercised, {} excluded, \
                 {} unique statements",
                o.dialect.name(),
                o.exercised.len(),
                o.declared,
                o.excluded,
                o.unique_statements,
            );
            if !o.unexercised.is_empty() {
                let _ = writeln!(
                    out,
                    "   UNEXERCISED — add a test that renders it, or a reasoned exclusion:"
                );
                for entry in &o.unexercised {
                    let _ = writeln!(out, "     - {}  ({})", entry.id, entry.api);
                }
            }
            if !o.parse_failures.is_empty() {
                let _ = writeln!(
                    out,
                    "   RECORDED SQL NO LONGER PARSES ({}):",
                    o.parse_failures.len()
                );
                for sql in o.parse_failures.iter().take(5) {
                    let _ = writeln!(out, "     - {sql}");
                }
            }
            if !o.undeclared.is_empty() {
                let _ = writeln!(
                    out,
                    "   observed but undeclared (manifest rot check, not fatal):"
                );
                for item in &o.undeclared {
                    let _ = writeln!(out, "     - {item}");
                }
            }
        }
        if self.malformed_lines > 0 {
            let _ = writeln!(out, "!! {} malformed recording lines", self.malformed_lines);
        }
        let _ = writeln!(
            out,
            "{}",
            if self.passed() {
                "coverage gate: PASS"
            } else {
                "coverage gate: FAIL"
            }
        );
        out
    }
}

/// Measure `records` against `config`.
///
/// `malformed_lines` is passed through from the reader so the report can fail
/// on a corrupt recording instead of silently measuring less.
pub fn analyze(records: &[(Dialect, String)], config: &Config, malformed_lines: usize) -> Report {
    let mut by_dialect: BTreeMap<&'static str, BTreeSet<&str>> = BTreeMap::new();
    for (dialect, sql) in records {
        by_dialect
            .entry(dialect.name())
            .or_default()
            .insert(sql.as_str());
    }

    let outcomes = [Dialect::Psql, Dialect::Mysql, Dialect::Sqlite]
        .into_iter()
        .map(|dialect| {
            let corpus = by_dialect.remove(dialect.name()).unwrap_or_default();
            match dialect {
                Dialect::Psql => analyze_psql(&corpus, config.plan(dialect)),
                Dialect::Mysql | Dialect::Sqlite => {
                    analyze_tokens(dialect, &corpus, config.plan(dialect))
                }
            }
        })
        .collect();

    Report {
        outcomes,
        malformed_lines,
    }
}

fn analyze_psql(corpus: &BTreeSet<&str>, plan: &DialectPlan) -> Outcome {
    let mut found: BTreeSet<&'static str> = BTreeSet::new();
    let mut kinds: BTreeSet<String> = BTreeSet::new();
    let mut unknown_ops: BTreeSet<String> = BTreeSet::new();
    let mut parse_failures = Vec::new();
    let mut sig_hit: BTreeSet<&str> = BTreeSet::new();

    // Signature entries are few (spellings the tree normalises away), so scan
    // them per statement alongside the parse.
    let sig_entries: Vec<&ManifestEntry> = plan
        .manifest
        .iter()
        .filter(|e| !e.sigs.is_empty())
        .collect();

    for sql in corpus {
        match psql_constructs(sql) {
            Ok(obs) => {
                found.extend(obs.found);
                kinds.extend(obs.kinds);
                unknown_ops.extend(obs.unknown_ops);
            }
            Err(e) => parse_failures.push(format!("{e}: {sql}")),
        }
        for entry in &sig_entries {
            if !sig_hit.contains(entry.id.as_str())
                && entry.sigs.iter().any(|sig| sig_matches(sql, sig))
            {
                sig_hit.insert(entry.id.as_str());
            }
        }
    }

    let excluded_ids: BTreeSet<&str> = plan.exclusions.iter().map(|e| e.id.as_str()).collect();
    let mut exercised = BTreeSet::new();
    let mut unexercised = Vec::new();
    for entry in &plan.manifest {
        if found.contains(entry.id.as_str()) || sig_hit.contains(entry.id.as_str()) {
            exercised.insert(entry.id.clone());
        } else {
            unexercised.push(entry.clone());
        }
    }

    // Rot report: node kinds the walker does not understand, operator
    // spellings it does not map, and detector ids that are not declared.
    let declared_ids: BTreeSet<&str> = plan.manifest.iter().map(|e| e.id.as_str()).collect();
    let mut undeclared: Vec<String> = kinds
        .iter()
        .filter(|k| !PSQL_ACCOUNTED_KINDS.contains(&k.as_str()))
        .map(|k| format!("node kind {k}"))
        .collect();
    undeclared.extend(unknown_ops.iter().map(|op| format!("operator {op}")));
    undeclared.extend(
        found
            .iter()
            .filter(|id| !declared_ids.contains(**id) && !excluded_ids.contains(**id))
            .map(|id| format!("construct {id}")),
    );

    Outcome {
        dialect: Dialect::Psql,
        unique_statements: corpus.len(),
        declared: plan.manifest.len(),
        excluded: plan.exclusions.len(),
        exercised,
        unexercised,
        undeclared,
        parse_failures,
    }
}

fn analyze_tokens(dialect: Dialect, corpus: &BTreeSet<&str>, plan: &DialectPlan) -> Outcome {
    let mut exercised = BTreeSet::new();
    let mut unexercised = Vec::new();
    let mut keywords_seen: BTreeSet<String> = BTreeSet::new();
    for sql in corpus {
        scan_keywords(sql, &mut keywords_seen);
    }
    for entry in &plan.manifest {
        let hit = corpus
            .iter()
            .any(|sql| entry.sigs.iter().any(|sig| sig_matches(sql, sig)));
        if hit {
            exercised.insert(entry.id.clone());
        } else {
            unexercised.push(entry.clone());
        }
    }

    // Rot report: a keyword of interest present in the corpus but claimed by
    // no signature means a construct is being rendered that the manifest does
    // not know about.
    let claimed: BTreeSet<String> = plan
        .manifest
        .iter()
        .flat_map(|e| e.sigs.iter())
        .flat_map(|sig| {
            sig.split(|c: char| !(c.is_ascii_uppercase() || c == '_'))
                .filter(|w| w.len() >= 2)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect();
    let undeclared: Vec<String> = keywords_seen
        .iter()
        .filter(|k| !claimed.contains(*k))
        .map(|k| format!("keyword {k}"))
        .collect();

    Outcome {
        dialect,
        unique_statements: corpus.len(),
        declared: plan.manifest.len(),
        excluded: plan.exclusions.len(),
        exercised,
        unexercised,
        undeclared,
        parse_failures: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sig_wildcard_matches_one_operand_token_only() {
        assert!(sig_matches("OFFSET 5 ROWS", "OFFSET {*} ROWS"));
        assert!(sig_matches("LIMIT 3 OFFSET 500 ROWS", "OFFSET {*} ROWS"));
        assert!(!sig_matches(
            "OFFSET 5 FETCH NEXT 3 ROWS ONLY",
            "OFFSET {*} ROWS"
        ));
        // The wildcard is an operand, never a keyword: `$1 PRECEDING` is an
        // offset bound, `UNBOUNDED PRECEDING` is a different construct.
        assert!(sig_matches("ROWS $1 PRECEDING", " {*} PRECEDING"));
        assert!(sig_matches(
            "ROWS BETWEEN ?1 PRECEDING AND",
            " {*} PRECEDING"
        ));
        assert!(!sig_matches("ROWS UNBOUNDED PRECEDING", " {*} PRECEDING"));
        // A literal `*` is only a character.
        assert!(sig_matches("(`age` * 2)", "` *"));
        assert!(!sig_matches("(`age` -> 2)", "` *"));
        assert!(sig_matches("plain contains", "contains"));
        assert!(!sig_matches("plain", "absent"));
    }

    #[test]
    fn manifest_parsing_round_trips_and_refuses_rot() {
        let entries =
            parse_manifest("# c\n[join.left]\napi = select::left_join\nsig = LEFT JOIN\nsig = X\n")
                .expect("parses");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "join.left");
        assert_eq!(entries[0].sigs, vec!["LEFT JOIN", "X"]);

        assert!(parse_manifest("[a]\napi = x\n[a]\napi = y\n").is_err());
        assert!(parse_manifest("[a]\n").is_err(), "api is required");
        assert!(parse_manifest("api = orphan\n").is_err());
        assert!(parse_exclusions("[a]\n").is_err(), "reason is required");
    }

    #[test]
    fn the_embedded_config_is_valid() {
        // Manifest ids resolve to detectors, token entries carry signatures,
        // and nothing is both declared and excluded. This is the test that
        // keeps the checked-in files honest as they are edited.
        Config::embedded().expect("the checked-in manifests must validate");
    }

    #[test]
    fn the_walker_sees_the_constructs_in_a_statement() {
        let obs = psql_constructs(
            r#"SELECT DISTINCT ON ("a") "a", count(*) AS "n" FROM only_t
               LEFT JOIN u USING ("id")
               WHERE "x" IS NOT NULL GROUP BY ROLLUP ("a") HAVING count(*) > $1
               ORDER BY "a" DESC NULLS LAST LIMIT 10 OFFSET 2
               FOR UPDATE OF u SKIP LOCKED"#,
        )
        .expect("parses");
        for id in [
            "stmt.select",
            "select.distinct_on",
            "join.left",
            "join.using",
            "clause.where",
            "op.is_not_null",
            "clause.group_by",
            "group.rollup",
            "clause.having",
            "order.desc",
            "order.nulls_last",
            "clause.limit",
            "clause.offset",
            "lock.for_update",
            "lock.of",
            "lock.skip_locked",
            "func.star_arg",
            "expr.alias",
            "expr.arg",
        ] {
            assert!(obs.found.contains(id), "missing {id}: {:?}", obs.found);
        }
    }
}
