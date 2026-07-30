//! Tier D — grammar-construct coverage measured from the SQL side.
//!
//! Tiers A–C generate and judge SQL; this module answers the question none of
//! them can: **across everything they judged, which grammar constructs did the
//! library actually exercise?** Line coverage cannot answer it — a line can run
//! without the construct appearing in the output — so the measurement replays
//! the recorded SQL itself (see [`crate::record`]) against a checked-in
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

use serde_json::Value as Json;

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
                include_str!("../coverage/psql.manifest"),
                include_str!("../coverage/psql.exclusions"),
            ),
            (
                Dialect::Mysql,
                include_str!("../coverage/mysql.manifest"),
                include_str!("../coverage/mysql.exclusions"),
            ),
            (
                Dialect::Sqlite,
                include_str!("../coverage/sqlite.manifest"),
                include_str!("../coverage/sqlite.exclusions"),
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

// ===========================================================================
// The psql tree walker
// ===========================================================================

/// What one psql statement's parse tree yielded.
#[derive(Debug, Default)]
pub struct PsqlObservation {
    /// Construct ids found.
    pub found: BTreeSet<&'static str>,
    /// Every node kind seen, for the manifest-rot report.
    pub kinds: BTreeSet<String>,
    /// Operator spellings no detector maps, e.g. `AExpr:%`.
    pub unknown_ops: BTreeSet<String>,
}

/// Walk one psql statement's parse tree.
///
/// # Errors
/// If pg_query rejects the SQL — which for a recording of *accepted* SQL means
/// the recording is stale or corrupt, and the gate reports it rather than
/// understating coverage.
pub fn psql_constructs(sql: &str) -> Result<PsqlObservation, String> {
    let parsed = pg_query::parse(sql).map_err(|e| e.to_string())?;
    let tree = serde_json::to_value(&parsed.protobuf)
        .map_err(|e| format!("parse tree did not serialise: {e}"))?;
    let mut obs = PsqlObservation::default();
    visit(&tree, None, sql, &mut obs);
    Ok(obs)
}

/// A serialised protobuf object keyed by a CamelCase name is a grammar node;
/// everything lowercase is a field.
fn is_node_key(key: &str) -> bool {
    key.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

fn visit(value: &Json, parent: Option<&str>, sql: &str, obs: &mut PsqlObservation) {
    match value {
        Json::Object(map) => {
            for (key, val) in map {
                if is_node_key(key) && val.is_object() {
                    obs.kinds.insert(key.clone());
                    let node = val.as_object().expect("just checked is_object");
                    detect(key, node, parent, sql, obs);
                    visit(val, Some(key), sql, obs);
                } else {
                    visit(val, parent, sql, obs);
                }
            }
        }
        Json::Array(items) => {
            for item in items {
                visit(item, parent, sql, obs);
            }
        }
        _ => {}
    }
}

// --- field accessors over serde_json objects -------------------------------

type Obj = serde_json::Map<String, Json>;

fn int(node: &Obj, key: &str) -> i64 {
    node.get(key).and_then(Json::as_i64).unwrap_or(0)
}

fn boolean(node: &Obj, key: &str) -> bool {
    node.get(key).and_then(Json::as_bool).unwrap_or(false)
}

fn text<'a>(node: &'a Obj, key: &str) -> &'a str {
    node.get(key).and_then(Json::as_str).unwrap_or("")
}

fn list<'a>(node: &'a Obj, key: &str) -> &'a [Json] {
    node.get(key).and_then(Json::as_array).map_or(&[], |v| v)
}

fn present(node: &Obj, key: &str) -> bool {
    node.get(key).is_some_and(|v| !v.is_null())
}

fn object<'a>(node: &'a Obj, key: &str) -> Option<&'a Obj> {
    node.get(key).and_then(Json::as_object)
}

/// A list element is a `Node` message — `{"node": {"String": {...}}}` — so the
/// variant map sits one `node` layer down. Tolerates the direct shape too.
fn node_variants(element: &Json) -> Option<&Obj> {
    let obj = element.as_object()?;
    match obj.get("node") {
        Some(inner) => inner.as_object(),
        None => Some(obj),
    }
}

fn svals(nodes: &[Json]) -> impl DoubleEndedIterator<Item = &str> {
    nodes
        .iter()
        .filter_map(node_variants)
        .filter_map(|n| n.get("String"))
        .filter_map(Json::as_object)
        .filter_map(|s| s.get("sval"))
        .filter_map(Json::as_str)
}

/// The first `String` node's `sval` in a name list — how pg_query spells
/// operator names and function names.
fn first_sval(nodes: &[Json]) -> &str {
    svals(nodes).next().unwrap_or("")
}

fn last_sval(nodes: &[Json]) -> &str {
    svals(nodes).next_back().unwrap_or("")
}

// --- frame_options bits, from PostgreSQL's parsenodes.h (stable since 12) --

const FRAME_NONDEFAULT: i64 = 0x00001;
const FRAME_RANGE: i64 = 0x00002;
const FRAME_ROWS: i64 = 0x00004;
const FRAME_GROUPS: i64 = 0x00008;
const FRAME_BETWEEN: i64 = 0x00010;
const FRAME_START_UNBOUNDED_PRECEDING: i64 = 0x00020;
const FRAME_END_UNBOUNDED_PRECEDING: i64 = 0x00040;
const FRAME_START_UNBOUNDED_FOLLOWING: i64 = 0x00080;
const FRAME_END_UNBOUNDED_FOLLOWING: i64 = 0x00100;
const FRAME_START_CURRENT_ROW: i64 = 0x00200;
const FRAME_END_CURRENT_ROW: i64 = 0x00400;
const FRAME_START_OFFSET_PRECEDING: i64 = 0x00800;
const FRAME_END_OFFSET_PRECEDING: i64 = 0x01000;
const FRAME_START_OFFSET_FOLLOWING: i64 = 0x02000;
const FRAME_END_OFFSET_FOLLOWING: i64 = 0x04000;
const FRAME_EXCLUDE_CURRENT_ROW: i64 = 0x08000;
const FRAME_EXCLUDE_GROUP: i64 = 0x10000;
const FRAME_EXCLUDE_TIES: i64 = 0x20000;

/// Every id the tree walker can emit. A manifest entry without a `sig =` must
/// name one of these; the list is asserted against the manifest at load time.
pub const PSQL_DETECTABLE: &[&str] = &[
    // statements
    "stmt.select",
    "stmt.insert",
    "stmt.update",
    "stmt.delete",
    // select core
    "select.distinct",
    "select.distinct_on",
    "clause.where",
    "clause.having",
    "clause.group_by",
    "group.distinct",
    "group.rollup",
    "group.cube",
    "group.sets",
    "clause.order_by",
    "order.asc",
    "order.desc",
    "order.using",
    "order.nulls_first",
    "order.nulls_last",
    "expr.collate",
    "clause.limit",
    "clause.limit_all",
    "clause.offset",
    "clause.fetch",
    "fetch.with_ties",
    "clause.window",
    "clause.returning",
    // locking
    "lock.for_update",
    "lock.for_no_key_update",
    "lock.for_share",
    "lock.for_key_share",
    "lock.of",
    "lock.nowait",
    "lock.skip_locked",
    // set operations
    "combine.union",
    "combine.union_all",
    "combine.intersect",
    "combine.intersect_all",
    "combine.except",
    "combine.except_all",
    "combine.order_by",
    "combine.limit",
    "combine.offset",
    "combine.fetch",
    // WITH
    "cte.with",
    "cte.recursive",
    "cte.columns",
    "cte.materialized",
    "cte.not_materialized",
    "cte.search_breadth",
    "cte.search_depth",
    "cte.cycle",
    "cte.cycle_value",
    // FROM items
    "from.table",
    "from.alias",
    "from.column_aliases",
    "from.only",
    "from.lateral",
    "from.subquery",
    "from.function",
    "from.rows_from",
    "from.with_ordinality",
    "from.tablesample",
    "from.tablesample_repeatable",
    "from.comma_list",
    "function.column_defs",
    // joins
    "join.inner",
    "join.left",
    "join.right",
    "join.full",
    "join.cross",
    "join.natural",
    "join.on",
    "join.using",
    // INSERT
    "insert.columns",
    "insert.values",
    "insert.multi_row",
    "insert.query",
    "insert.overriding_system",
    "insert.overriding_user",
    "conflict.do_nothing",
    "conflict.do_update",
    "conflict.target_columns",
    "conflict.on_constraint",
    "conflict.target_where",
    "conflict.update_where",
    "conflict.excluded",
    // UPDATE / DELETE
    "update.set",
    "update.set_row",
    "update.from",
    "delete.using",
    "where.current_of",
    // windows and frames
    "window.named",
    "window.based_on",
    "window.partition_by",
    "window.order_by",
    "func.over_def",
    "func.over_name",
    "frame.rows",
    "frame.range",
    "frame.groups",
    "frame.between",
    "frame.from_unbounded_preceding",
    "frame.from_preceding",
    "frame.from_current_row",
    "frame.from_following",
    "frame.from_unbounded_following",
    "frame.to_unbounded_preceding",
    "frame.to_preceding",
    "frame.to_current_row",
    "frame.to_following",
    "frame.to_unbounded_following",
    "frame.exclude_current_row",
    "frame.exclude_group",
    "frame.exclude_ties",
    // functions
    "expr.func",
    "func.distinct",
    "func.order_by",
    "func.within_group",
    "func.filter",
    "func.star_arg",
    // expressions
    "expr.arg",
    "expr.string_literal",
    "expr.number_literal",
    "expr.ident",
    "expr.star",
    "expr.alias",
    "expr.case",
    "case.else",
    "expr.cast",
    "expr.and",
    "expr.or",
    "expr.not",
    "expr.row",
    "expr.scalar_subquery",
    "op.at_time_zone",
    // operators
    "op.eq",
    "op.ne",
    "op.lt",
    "op.lte",
    "op.gt",
    "op.gte",
    "op.plus",
    "op.minus",
    "op.concat",
    "op.like",
    "op.not_like",
    "op.ilike",
    "op.not_ilike",
    "op.similar_to",
    "op.not_similar_to",
    "op.between",
    "op.not_between",
    "op.between_symmetric",
    "op.not_between_symmetric",
    "op.in",
    "op.not_in",
    "op.is_distinct_from",
    "op.is_not_distinct_from",
    "op.matches",
    "op.imatches",
    "op.not_matches",
    "op.not_imatches",
    "op.contains",
    "op.contained_by",
    "op.overlaps",
    "op.text_search",
    "op.json_get",
    "op.json_get_text",
    "op.json_get_path",
    "op.json_get_path_text",
    "op.json_has_key",
    "op.json_has_any_key",
    "op.json_has_all_keys",
    "op.eq_any",
    "op.ne_all",
    "op.any",
    "op.all",
    "op.is_null",
    "op.is_not_null",
    "op.is_true",
    "op.is_not_true",
    "op.is_false",
    "op.is_not_false",
    "op.is_unknown",
    "op.is_not_unknown",
];

/// Node kinds the walker understands (or knows to be structural), for the
/// observed-but-undeclared report.
const PSQL_ACCOUNTED_KINDS: &[&str] = &[
    // detected
    "SelectStmt",
    "InsertStmt",
    "UpdateStmt",
    "DeleteStmt",
    "JoinExpr",
    "RangeVar",
    "RangeSubselect",
    "RangeFunction",
    "RangeTableSample",
    "CommonTableExpr",
    "SortBy",
    "CollateClause",
    "LockingClause",
    "GroupingSet",
    "WindowDef",
    "FuncCall",
    "AExpr",
    "NullTest",
    "BooleanTest",
    "BoolExpr",
    "CaseExpr",
    "CaseWhen",
    "SubLink",
    "RowExpr",
    "ParamRef",
    "AConst",
    "ColumnRef",
    "TypeCast",
    "CurrentOfExpr",
    "MultiAssignRef",
    "ResTarget",
    "ColumnDef",
    // structural serialisation artefacts, not grammar constructs
    "List",
    "String",
    "Integer",
    "Float",
    "Boolean",
    "BitString",
    "AStar",
    "TypeName",
    "IndexElem",
    "Alias",
    "WithClause",
    // AConst's value oneof variants
    "Sval",
    "Ival",
    "Fval",
    "Boolval",
    "Bsval",
    // Parse forms that arise from raw() fragments or from PostgreSQL
    // special-casing a function name keelson renders as an ordinary call —
    // not separate keelson APIs, so their appearance is not manifest rot.
    "AArrayExpr",
    "CoalesceExpr",
    "MinMaxExpr",
    "SetToDefault",
    "SqlvalueFunction",
];

#[allow(clippy::too_many_lines)] // one match arm per grammar node, and the length *is* the inventory
fn detect(kind: &str, node: &Obj, parent: Option<&str>, sql: &str, obs: &mut PsqlObservation) {
    use pg_query::protobuf as pb;
    let found = &mut obs.found;
    match kind {
        "SelectStmt" => {
            // Values lists are a select-core alternative; a SELECT that is one
            // is not a rendered SELECT statement.
            let values = list(node, "values_lists");
            if values.is_empty() {
                found.insert("stmt.select");
            }
            let distinct = list(node, "distinct_clause");
            if !distinct.is_empty() {
                let plain = distinct
                    .iter()
                    .all(|n| n.as_object().is_none_or(|o| !present(o, "node")));
                found.insert(if plain {
                    "select.distinct"
                } else {
                    "select.distinct_on"
                });
            }
            if present(node, "where_clause") {
                found.insert("clause.where");
            }
            if present(node, "having_clause") {
                found.insert("clause.having");
            }
            if !list(node, "group_clause").is_empty() {
                found.insert("clause.group_by");
            }
            if boolean(node, "group_distinct") {
                found.insert("group.distinct");
            }
            if !list(node, "window_clause").is_empty() {
                found.insert("clause.window");
            }
            if list(node, "from_clause").len() > 1 {
                found.insert("from.comma_list");
            }
            match parent {
                Some("InsertStmt") if !values.is_empty() => {
                    found.insert("insert.values");
                    if values.len() > 1 {
                        found.insert("insert.multi_row");
                    }
                }
                Some("InsertStmt") => {
                    found.insert("insert.query");
                }
                _ => {}
            }

            let op = int(node, "op");
            let combined = op != pb::SetOperation::SetopNone as i64
                && op != pb::SetOperation::Undefined as i64;
            let all = boolean(node, "all");
            if op == pb::SetOperation::SetopUnion as i64 {
                found.insert(if all {
                    "combine.union_all"
                } else {
                    "combine.union"
                });
            } else if op == pb::SetOperation::SetopIntersect as i64 {
                found.insert(if all {
                    "combine.intersect_all"
                } else {
                    "combine.intersect"
                });
            } else if op == pb::SetOperation::SetopExcept as i64 {
                found.insert(if all {
                    "combine.except_all"
                } else {
                    "combine.except"
                });
            }

            let sort = !list(node, "sort_clause").is_empty();
            let has_limit = present(node, "limit_count");
            let has_offset = present(node, "limit_offset");
            if sort {
                found.insert(if combined {
                    "combine.order_by"
                } else {
                    "clause.order_by"
                });
            }
            if has_offset {
                found.insert(if combined {
                    "combine.offset"
                } else {
                    "clause.offset"
                });
            }
            if has_limit {
                // The tree cannot tell `LIMIT n` from `FETCH NEXT n ROWS ONLY`
                // — PostgreSQL normalises them to one shape — so the keyword in
                // the SQL is what decides. Token-assisted, and said so.
                let fetch = sql.contains("FETCH");
                let limit_all = object(node, "limit_count")
                    .and_then(|n| object(n, "node"))
                    .and_then(|n| object(n, "AConst"))
                    .is_some_and(|c| boolean(c, "isnull"));
                let id = if limit_all {
                    "clause.limit_all"
                } else if combined {
                    if fetch {
                        "combine.fetch"
                    } else {
                        "combine.limit"
                    }
                } else if fetch {
                    "clause.fetch"
                } else {
                    "clause.limit"
                };
                found.insert(id);
                if int(node, "limit_option") == pb::LimitOption::WithTies as i64 {
                    found.insert("fetch.with_ties");
                }
            }
        }
        "InsertStmt" => {
            found.insert("stmt.insert");
            if !list(node, "cols").is_empty() {
                found.insert("insert.columns");
            }
            if !list(node, "returning_list").is_empty() {
                found.insert("clause.returning");
            }
            let overriding = int(node, "override");
            if overriding == pb::OverridingKind::OverridingSystemValue as i64 {
                found.insert("insert.overriding_system");
            } else if overriding == pb::OverridingKind::OverridingUserValue as i64 {
                found.insert("insert.overriding_user");
            }
            if let Some(conflict) = object(node, "on_conflict_clause") {
                let action = int(conflict, "action");
                if action == pb::OnConflictAction::OnconflictNothing as i64 {
                    found.insert("conflict.do_nothing");
                } else if action == pb::OnConflictAction::OnconflictUpdate as i64 {
                    found.insert("conflict.do_update");
                }
                if present(conflict, "where_clause") {
                    found.insert("conflict.update_where");
                }
                if let Some(infer) = object(conflict, "infer") {
                    if !list(infer, "index_elems").is_empty() {
                        found.insert("conflict.target_columns");
                    }
                    if !text(infer, "conname").is_empty() {
                        found.insert("conflict.on_constraint");
                    }
                    if present(infer, "where_clause") {
                        found.insert("conflict.target_where");
                    }
                }
            }
        }
        "UpdateStmt" => {
            found.insert("stmt.update");
            found.insert("update.set");
            if present(node, "where_clause") {
                found.insert("clause.where");
            }
            if !list(node, "from_clause").is_empty() {
                found.insert("update.from");
            }
            if list(node, "from_clause").len() > 1 {
                found.insert("from.comma_list");
            }
            if !list(node, "returning_list").is_empty() {
                found.insert("clause.returning");
            }
        }
        "DeleteStmt" => {
            found.insert("stmt.delete");
            if present(node, "where_clause") {
                found.insert("clause.where");
            }
            if !list(node, "using_clause").is_empty() {
                found.insert("delete.using");
            }
            if list(node, "using_clause").len() > 1 {
                found.insert("from.comma_list");
            }
            if !list(node, "returning_list").is_empty() {
                found.insert("clause.returning");
            }
        }
        "RangeVar" => {
            found.insert("from.table");
            if !boolean(node, "inh") {
                found.insert("from.only");
            }
            note_alias(node, found);
        }
        "RangeSubselect" => {
            found.insert("from.subquery");
            if boolean(node, "lateral") {
                found.insert("from.lateral");
            }
            note_alias(node, found);
        }
        "RangeFunction" => {
            found.insert("from.function");
            if boolean(node, "lateral") {
                found.insert("from.lateral");
            }
            if boolean(node, "ordinality") {
                found.insert("from.with_ordinality");
            }
            if boolean(node, "is_rowsfrom") && list(node, "functions").len() > 1 {
                found.insert("from.rows_from");
            }
            if !list(node, "coldeflist").is_empty() {
                found.insert("function.column_defs");
            }
            note_alias(node, found);
        }
        "RangeTableSample" => {
            found.insert("from.tablesample");
            if present(node, "repeatable") {
                found.insert("from.tablesample_repeatable");
            }
        }
        "JoinExpr" => {
            let jt = int(node, "jointype");
            let natural = boolean(node, "is_natural");
            let on = present(node, "quals");
            let using = !list(node, "using_clause").is_empty();
            if jt == pb::JoinType::JoinInner as i64 {
                if !natural && !on && !using {
                    found.insert("join.cross");
                } else {
                    found.insert("join.inner");
                }
            } else if jt == pb::JoinType::JoinLeft as i64 {
                found.insert("join.left");
            } else if jt == pb::JoinType::JoinRight as i64 {
                found.insert("join.right");
            } else if jt == pb::JoinType::JoinFull as i64 {
                found.insert("join.full");
            }
            if natural {
                found.insert("join.natural");
            }
            if on {
                found.insert("join.on");
            }
            if using {
                found.insert("join.using");
            }
        }
        "CommonTableExpr" => {
            found.insert("cte.with");
            if !list(node, "aliascolnames").is_empty() {
                found.insert("cte.columns");
            }
            let materialized = int(node, "ctematerialized");
            if materialized == pb::CteMaterialize::Always as i64 {
                found.insert("cte.materialized");
            } else if materialized == pb::CteMaterialize::Never as i64 {
                found.insert("cte.not_materialized");
            }
            if let Some(search) = object(node, "search_clause") {
                found.insert(if boolean(search, "search_breadth_first") {
                    "cte.search_breadth"
                } else {
                    "cte.search_depth"
                });
            }
            if let Some(cycle) = object(node, "cycle_clause") {
                found.insert("cte.cycle");
                if present(cycle, "cycle_mark_value") {
                    found.insert("cte.cycle_value");
                }
            }
        }
        "WithClause" => {
            // Reached only if some tree ever serialises it as a node; the
            // recursive flag is normally read by the stmt detectors below.
            if boolean(node, "recursive") {
                found.insert("cte.recursive");
            }
        }
        "SortBy" => {
            let dir = int(node, "sortby_dir");
            if dir == pb::SortByDir::SortbyAsc as i64 {
                found.insert("order.asc");
            } else if dir == pb::SortByDir::SortbyDesc as i64 {
                found.insert("order.desc");
            } else if dir == pb::SortByDir::SortbyUsing as i64 {
                found.insert("order.using");
            }
            let nulls = int(node, "sortby_nulls");
            if nulls == pb::SortByNulls::SortbyNullsFirst as i64 {
                found.insert("order.nulls_first");
            } else if nulls == pb::SortByNulls::SortbyNullsLast as i64 {
                found.insert("order.nulls_last");
            }
        }
        "CollateClause" => {
            found.insert("expr.collate");
        }
        "LockingClause" => {
            let strength = int(node, "strength");
            if strength == pb::LockClauseStrength::LcsForupdate as i64 {
                found.insert("lock.for_update");
            } else if strength == pb::LockClauseStrength::LcsFornokeyupdate as i64 {
                found.insert("lock.for_no_key_update");
            } else if strength == pb::LockClauseStrength::LcsForshare as i64 {
                found.insert("lock.for_share");
            } else if strength == pb::LockClauseStrength::LcsForkeyshare as i64 {
                found.insert("lock.for_key_share");
            }
            if !list(node, "locked_rels").is_empty() {
                found.insert("lock.of");
            }
            let wait = int(node, "wait_policy");
            if wait == pb::LockWaitPolicy::LockWaitSkip as i64 {
                found.insert("lock.skip_locked");
            } else if wait == pb::LockWaitPolicy::LockWaitError as i64 {
                found.insert("lock.nowait");
            }
        }
        "GroupingSet" => {
            let gk = int(node, "kind");
            if gk == pb::GroupingSetKind::GroupingSetRollup as i64 {
                found.insert("group.rollup");
            } else if gk == pb::GroupingSetKind::GroupingSetCube as i64 {
                found.insert("group.cube");
            } else if gk == pb::GroupingSetKind::GroupingSetSets as i64 {
                found.insert("group.sets");
            }
        }
        "WindowDef" => {
            detect_window_def(node, parent, found);
        }
        "FuncCall" => {
            found.insert("expr.func");
            if boolean(node, "agg_distinct") {
                found.insert("func.distinct");
            }
            if !list(node, "agg_order").is_empty() {
                found.insert("func.order_by");
            }
            if boolean(node, "agg_within_group") {
                found.insert("func.within_group");
            }
            if present(node, "agg_filter") {
                found.insert("func.filter");
            }
            if boolean(node, "agg_star") {
                found.insert("func.star_arg");
            }
            // `a AT TIME ZONE z` parses as a call to pg_catalog.timezone.
            if last_sval(list(node, "funcname")) == "timezone" && sql.contains("AT TIME ZONE") {
                found.insert("op.at_time_zone");
            }
            if let Some(over) = object(node, "over") {
                if text(over, "name").is_empty() {
                    found.insert("func.over_def");
                } else {
                    found.insert("func.over_name");
                }
                detect_window_def(over, Some("FuncCall"), found);
            }
        }
        "AExpr" => {
            detect_a_expr(node, obs);
        }
        "NullTest" => {
            let t = int(node, "nulltesttype");
            if t == pb::NullTestType::IsNull as i64 {
                found.insert("op.is_null");
            } else if t == pb::NullTestType::IsNotNull as i64 {
                found.insert("op.is_not_null");
            }
        }
        "BooleanTest" => {
            let t = int(node, "booltesttype");
            let id = match t {
                x if x == pb::BoolTestType::IsTrue as i64 => "op.is_true",
                x if x == pb::BoolTestType::IsNotTrue as i64 => "op.is_not_true",
                x if x == pb::BoolTestType::IsFalse as i64 => "op.is_false",
                x if x == pb::BoolTestType::IsNotFalse as i64 => "op.is_not_false",
                x if x == pb::BoolTestType::IsUnknown as i64 => "op.is_unknown",
                x if x == pb::BoolTestType::IsNotUnknown as i64 => "op.is_not_unknown",
                _ => return,
            };
            found.insert(id);
        }
        "BoolExpr" => {
            let t = int(node, "boolop");
            if t == pb::BoolExprType::AndExpr as i64 {
                found.insert("expr.and");
            } else if t == pb::BoolExprType::OrExpr as i64 {
                found.insert("expr.or");
            } else if t == pb::BoolExprType::NotExpr as i64 {
                found.insert("expr.not");
            }
        }
        "CaseExpr" => {
            found.insert("expr.case");
            if present(node, "defresult") {
                found.insert("case.else");
            }
        }
        "SubLink" => {
            let t = int(node, "sub_link_type");
            let oper = first_sval(list(node, "oper_name"));
            if t == pb::SubLinkType::AnySublink as i64 {
                // `x IN (SELECT …)` parses as ANY with no operator name.
                match oper {
                    "" => found.insert("op.in"),
                    "=" => found.insert("op.eq_any"),
                    _ => found.insert("op.any"),
                };
            } else if t == pb::SubLinkType::AllSublink as i64 {
                match oper {
                    "<>" => found.insert("op.ne_all"),
                    _ => found.insert("op.all"),
                };
            } else if t == pb::SubLinkType::ExprSublink as i64 {
                found.insert("expr.scalar_subquery");
            }
        }
        "RowExpr" => {
            found.insert("expr.row");
        }
        "ParamRef" => {
            found.insert("expr.arg");
        }
        "AConst" => {
            // The constant's value is a protobuf oneof, serialised as a
            // CamelCase variant under `val`: {"val": {"Sval": {…}}}.
            if let Some(val) = object(node, "val") {
                if val.contains_key("Sval") {
                    found.insert("expr.string_literal");
                }
                if val.contains_key("Ival") || val.contains_key("Fval") {
                    found.insert("expr.number_literal");
                }
            }
        }
        "ColumnRef" => {
            let fields = list(node, "fields");
            if first_sval(fields) == "excluded" {
                found.insert("conflict.excluded");
            } else if fields
                .iter()
                .filter_map(node_variants)
                .any(|f| f.contains_key("String"))
            {
                found.insert("expr.ident");
            }
            if fields
                .iter()
                .filter_map(node_variants)
                .any(|f| f.contains_key("AStar"))
            {
                found.insert("expr.star");
            }
        }
        "TypeCast" => {
            found.insert("expr.cast");
        }
        "CurrentOfExpr" => {
            found.insert("where.current_of");
        }
        "MultiAssignRef" => {
            found.insert("update.set_row");
        }
        "ResTarget" if parent == Some("SelectStmt") && !text(node, "name").is_empty() => {
            found.insert("expr.alias");
        }
        _ => {}
    }

    // The `WITH [RECURSIVE]` flag lives on a plain struct field of the four
    // statements, so it is read here rather than through a node key.
    if matches!(
        kind,
        "SelectStmt" | "InsertStmt" | "UpdateStmt" | "DeleteStmt"
    ) && let Some(with) = object(node, "with_clause")
        && boolean(with, "recursive")
    {
        obs.found.insert("cte.recursive");
    }
}

fn note_alias(node: &Obj, found: &mut BTreeSet<&'static str>) {
    if let Some(alias) = object(node, "alias") {
        found.insert("from.alias");
        if !list(alias, "colnames").is_empty() {
            found.insert("from.column_aliases");
        }
    }
}

fn detect_window_def(node: &Obj, parent: Option<&str>, found: &mut BTreeSet<&'static str>) {
    // In the statement's WINDOW clause the definition is named; under OVER it
    // is anonymous. `refname` is the `based_on` reference either way.
    if !text(node, "name").is_empty() && parent != Some("FuncCall") {
        found.insert("window.named");
    }
    if !text(node, "refname").is_empty() {
        found.insert("window.based_on");
    }
    if !list(node, "partition_clause").is_empty() {
        found.insert("window.partition_by");
    }
    if !list(node, "order_clause").is_empty() {
        found.insert("window.order_by");
    }
    let bits = int(node, "frame_options");
    if bits & FRAME_NONDEFAULT == 0 {
        return;
    }
    for (bit, id) in [
        (FRAME_ROWS, "frame.rows"),
        (FRAME_RANGE, "frame.range"),
        (FRAME_GROUPS, "frame.groups"),
        (FRAME_BETWEEN, "frame.between"),
        (
            FRAME_START_UNBOUNDED_PRECEDING,
            "frame.from_unbounded_preceding",
        ),
        (FRAME_START_OFFSET_PRECEDING, "frame.from_preceding"),
        (FRAME_START_CURRENT_ROW, "frame.from_current_row"),
        (FRAME_START_OFFSET_FOLLOWING, "frame.from_following"),
        (
            FRAME_START_UNBOUNDED_FOLLOWING,
            "frame.from_unbounded_following",
        ),
        (
            FRAME_END_UNBOUNDED_PRECEDING,
            "frame.to_unbounded_preceding",
        ),
        (FRAME_END_OFFSET_PRECEDING, "frame.to_preceding"),
        (FRAME_END_CURRENT_ROW, "frame.to_current_row"),
        (FRAME_END_OFFSET_FOLLOWING, "frame.to_following"),
        (
            FRAME_END_UNBOUNDED_FOLLOWING,
            "frame.to_unbounded_following",
        ),
        (FRAME_EXCLUDE_CURRENT_ROW, "frame.exclude_current_row"),
        (FRAME_EXCLUDE_GROUP, "frame.exclude_group"),
        (FRAME_EXCLUDE_TIES, "frame.exclude_ties"),
    ] {
        if bits & bit != 0 {
            found.insert(id);
        }
    }
}

fn detect_a_expr(node: &Obj, obs: &mut PsqlObservation) {
    use pg_query::protobuf::AExprKind as K;
    let kind = int(node, "kind");
    let op = first_sval(list(node, "name"));
    let found = &mut obs.found;
    let id = if kind == K::AexprOp as i64 {
        match op {
            "=" => "op.eq",
            "<>" | "!=" => "op.ne",
            "<" => "op.lt",
            "<=" => "op.lte",
            ">" => "op.gt",
            ">=" => "op.gte",
            "+" => "op.plus",
            "-" => "op.minus",
            "||" => "op.concat",
            "@>" => "op.contains",
            "<@" => "op.contained_by",
            "&&" => "op.overlaps",
            "@@" => "op.text_search",
            "->" => "op.json_get",
            "->>" => "op.json_get_text",
            "#>" => "op.json_get_path",
            "#>>" => "op.json_get_path_text",
            "?" => "op.json_has_key",
            "?|" => "op.json_has_any_key",
            "?&" => "op.json_has_all_keys",
            "~" => "op.matches",
            "~*" => "op.imatches",
            "!~" => "op.not_matches",
            "!~*" => "op.not_imatches",
            other => {
                obs.unknown_ops.insert(format!("AExpr:{other}"));
                return;
            }
        }
    } else if kind == K::AexprOpAny as i64 {
        match op {
            "=" => "op.eq_any",
            _ => "op.any",
        }
    } else if kind == K::AexprOpAll as i64 {
        match op {
            "<>" => "op.ne_all",
            _ => "op.all",
        }
    } else if kind == K::AexprIn as i64 {
        match op {
            "<>" => "op.not_in",
            _ => "op.in",
        }
    } else if kind == K::AexprLike as i64 {
        match op {
            "!~~" => "op.not_like",
            _ => "op.like",
        }
    } else if kind == K::AexprIlike as i64 {
        match op {
            "!~~*" => "op.not_ilike",
            _ => "op.ilike",
        }
    } else if kind == K::AexprSimilar as i64 {
        match op {
            "!~" => "op.not_similar_to",
            _ => "op.similar_to",
        }
    } else if kind == K::AexprBetween as i64 {
        "op.between"
    } else if kind == K::AexprNotBetween as i64 {
        "op.not_between"
    } else if kind == K::AexprBetweenSym as i64 {
        "op.between_symmetric"
    } else if kind == K::AexprNotBetweenSym as i64 {
        "op.not_between_symmetric"
    } else if kind == K::AexprDistinct as i64 {
        "op.is_distinct_from"
    } else if kind == K::AexprNotDistinct as i64 {
        "op.is_not_distinct_from"
    } else {
        obs.unknown_ops.insert(format!("AExprKind:{kind}"));
        return;
    };
    found.insert(id);
}

// ===========================================================================
// Token-level keyword scan (mysql / sqlite manifest-rot report)
// ===========================================================================

/// Keywords whose appearance in recorded SQL, unclaimed by any manifest
/// signature, suggests a construct the manifest forgot. The everyday spine of
/// a statement is deliberately absent — `SELECT` or `AND` unclaimed would mean
/// the manifest is empty, and the gate says that more directly.
const TOKEN_DIALECT_KEYWORDS: &[&str] = &[
    "STRAIGHT_JOIN",
    "NATURAL",
    "LATERAL",
    "PARTITION",
    "ROLLUP",
    "DISTINCTROW",
    "IGNORE",
    "QUICK",
    "DELAYED",
    "LOW_PRIORITY",
    "HIGH_PRIORITY",
    "SQL_SMALL_RESULT",
    "SQL_BIG_RESULT",
    "SQL_BUFFER_RESULT",
    "SQL_NO_CACHE",
    "SQL_CALC_FOUND_ROWS",
    "NOWAIT",
    "LOCKED",
    "COLLATE",
    "REGEXP",
    "RLIKE",
    "GLOB",
    "MATCH",
    "AGAINST",
    "XOR",
    "DIV",
    "MOD",
    "BINARY",
    "ESCAPE",
    "SEPARATOR",
    "INDEXED",
    "REPLACE",
    "CONFLICT",
    "RETURNING",
    "EXCLUDED",
    "ABORT",
    "FAIL",
    "ROLLBACK",
    "RECURSIVE",
    "MATERIALIZED",
    "WINDOW",
    "OVER",
    "FILTER",
    "UNBOUNDED",
    "PRECEDING",
    "FOLLOWING",
    "TIES",
    "GROUPS",
    "NULLS",
    "INTERSECT",
    "EXCEPT",
    "UNION",
    "OFFSET",
    "LIMIT",
    "HAVING",
    "USING",
    "CROSS",
    "OUTER",
    "SOUNDS",
    "MEMBER",
];

fn scan_keywords(sql: &str, seen: &mut BTreeSet<String>) {
    let mut word = String::new();
    for ch in sql.chars().chain(std::iter::once(' ')) {
        if ch.is_ascii_uppercase() || ch == '_' {
            word.push(ch);
        } else {
            if word.len() >= 2 && TOKEN_DIALECT_KEYWORDS.contains(&word.as_str()) {
                seen.insert(word.clone());
            }
            word.clear();
        }
    }
}

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
