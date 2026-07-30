//! Combinatorial clause coverage for the PostgreSQL dialect (Linear DEV-181).
//!
//! Per-clause tests prove each clause renders correctly *alone*. This file
//! drives clause **presence** combinatorially, because the bugs worth finding
//! live in the interactions — `UNION` + `ORDER BY` + `LIMIT` needing the
//! branches parenthesised is a rule that exists only when all three are there.
//!
//! No expected strings. A hand-written expectation cannot exist for tens of
//! thousands of generated statements, and generating one from the builder would
//! only assert that the code equals itself. Every case asserts four invariants
//! instead:
//!
//! 1. **The grammar accepts it** — [`pg_query`], which *is* PostgreSQL's parser.
//! 2. **The parse tree contains exactly the clauses asked for, and no others.**
//!    `SelectStmt` (and friends) expose `with_clause`, `where_clause`,
//!    `group_clause`, `sort_clause`, `limit_count`, `locking_clause`, … — the
//!    mechanical replacement for a hand-written expected string.
//! 3. **Placeholder integrity** — the `$n` in the SQL are exactly `1..=args.len()`
//!    in emission order. The invariant that matters most: a numbering bug still
//!    yields valid SQL that every parser and engine accepts, and the wrong value
//!    silently binds to the wrong column. Per-clause tests cannot catch it
//!    because a clause tested alone always starts at `$1`.
//! 4. **Determinism** — building twice gives the same string and args, and a
//!    `clone` builds identically to its original.
//!
//! # The two tiers and their budgets
//!
//! The grammar judge costs tens of microseconds, so it takes the **full cross
//! product**; a real engine costs milliseconds, so the engine tier (behind the
//! `exhaustive` feature, which implies `live-docker`) takes **every
//! co-occurrence of up to three clauses plus a stratified random sample**, and
//! everything cheap enough to run whole (DML matrices, joins).
//!
//! The arithmetic, grammar tier (each count is asserted in its test):
//!
//! | matrix | cases |
//! | ------ | ----- |
//! | SELECT presence cross product (15 dimensions)   | 15 360 |
//! | SELECT value sweep over multi-valued dimensions | 36 828 |
//! | FROM-less SELECT cross product (10 dimensions)  |  1 024 |
//! | INSERT full product                             |    288 |
//! | UPDATE full product                             |     48 |
//! | DELETE full product                             |     24 |
//! | single joins (kind × condition × item × LATERAL)|    104 |
//! | join chains of 2–4 from-items                   |  2 379 |
//! | self-joins                                      |     13 |
//! | **total**                                       | **56 068** |
//!
//! # Where the semantic-compatibility rules come from
//!
//! The engine tier must skip combinations PostgreSQL *rejects by design*; each
//! rule in an `engine_ok` predicate cites the manual (PostgreSQL 17, sql-select
//! page unless said otherwise) and was confirmed against the live server while
//! this file was written. Everything not excluded runs.
//!
//! Every table name is from `tests/schema/psql.sql`, because the engine tier
//! resolves names and an invented table cannot be engine-checked at all.

use keelson_psql as psql;
use keelson_psql::{
    Chain, Expr, IntoExpr, Query, SelectQuery, Value, arg, cast, f, quote, raw, rollup, select,
    subquery, window,
};
use pg_query::protobuf::{
    self, JoinType, LimitOption, LockClauseStrength, LockWaitPolicy, OnConflictAction,
    OverridingKind, SetOperation, node::Node as N,
};

// ===========================================================================
// The invariants
// ===========================================================================

/// The `$n` placeholders of `sql`, in emission (textual) order.
fn placeholder_run(sql: &str) -> Vec<usize> {
    let bytes = sql.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' {
            let mut j = i + 1;
            let mut n = 0usize;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                n = n * 10 + usize::from(bytes[j] - b'0');
                j += 1;
            }
            if j > i + 1 {
                out.push(n);
                i = j;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Invariants 3 and 4, plus the build itself: build twice, build a clone, and
/// check the placeholders are exactly `1..=args.len()` in emission order.
///
/// `expected_args` is derived from the *configuration* — how many `arg(..)`
/// the chosen clause values carry — so a clause silently dropping its bound
/// value is caught even though the SQL would still be valid.
#[track_caller]
fn build_invariant<Q: Query + Clone>(q: &Q, expected_args: usize) -> (String, Vec<Value>) {
    let (sql, args) = q.build().expect("the generated query should build");
    let (sql2, args2) = q.build().expect("second build");
    assert_eq!(sql, sql2, "two builds of one query rendered differently");
    assert_eq!(args, args2, "two builds of one query bound differently");
    let (sql3, args3) = q.clone().build().expect("clone build");
    assert_eq!(sql, sql3, "a clone rendered differently from its original");
    assert_eq!(args, args3, "a clone bound differently from its original");

    assert_eq!(
        args.len(),
        expected_args,
        "the clauses asked for should bind exactly {expected_args} argument(s)\n  sql: {sql}"
    );
    let run = placeholder_run(&sql);
    let want: Vec<usize> = (1..=args.len()).collect();
    assert_eq!(
        run, want,
        "placeholders must be numbered 1..=len(args) in emission order\n  sql: {sql}"
    );
    (sql, args)
}

/// Invariant 1: the grammar accepts it — returning the tree for invariant 2.
#[track_caller]
fn parse_single(sql: &str) -> N {
    let parsed = pg_query::parse(sql).unwrap_or_else(|e| {
        panic!("libpg_query rejected the generated SQL\n  error: {e}\n  sql: {sql}")
    });
    let stmts = &parsed.protobuf.stmts;
    assert_eq!(stmts.len(), 1, "one statement expected: {sql}");
    stmts[0]
        .stmt
        .as_ref()
        .and_then(|s| s.node.as_ref())
        .expect("a parsed statement has a node")
        .clone()
}

#[track_caller]
fn parse_select(sql: &str) -> protobuf::SelectStmt {
    match parse_single(sql) {
        N::SelectStmt(s) => *s,
        other => panic!("expected a SelectStmt, got {other:?}\n  sql: {sql}"),
    }
}

/// A deterministic RNG (splitmix64) so the stratified sample is the same run
/// to run — a flake would otherwise be unreproducible.
#[cfg_attr(not(feature = "exhaustive"), allow(dead_code))]
struct Rng(u64);

#[cfg_attr(not(feature = "exhaustive"), allow(dead_code))]
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Iterate the full mixed-radix product of `radices`, calling `f` with each
/// value vector (an odometer, so no recursion and no allocation per case).
fn for_each_combo<const D: usize>(radices: [usize; D], mut f: impl FnMut(&[usize; D])) {
    let mut c = [0usize; D];
    loop {
        f(&c);
        let mut i = 0;
        loop {
            if i == D {
                return;
            }
            c[i] += 1;
            if c[i] < radices[i] {
                break;
            }
            c[i] = 0;
            i += 1;
        }
    }
}

// ===========================================================================
// SELECT — 15 dimensions
// ===========================================================================
//
// The optional clauses of PostgreSQL 17's SELECT, one dimension each. Value 0
// is always "absent"; value 1 is the canonical presence used by the boolean
// cross product; higher values are the clause's other shapes, driven by the
// value sweep and by the engine tier's single-dimension pass.

const S_WITH: usize = 0; //     [—, WITH c AS (…$…), WITH RECURSIVE c(id) AS (…$…)]
const S_DISTINCT: usize = 1; // [—, DISTINCT, DISTINCT ON ("users"."id")]
const S_JOIN: usize = 2; //     [—, INNER JOIN "posts" ON (…)]
const S_WHERE: usize = 3; //    [—, WHERE ("users"."age" >= $)]
const S_GROUP: usize = 4; //    [—, GROUP BY "users"."id", GROUP BY ROLLUP ("users"."id")]
const S_HAVING: usize = 5; //   [—, HAVING (count(*) > $)]
const S_WINDOW: usize = 6; //   [—, WINDOW "w" AS (…)]
const S_COMBINE: usize = 7; //  [—, UNION, UNION ALL, INTERSECT, EXCEPT, UNION+EXCEPT]
const S_COMB_ORDER: usize = 8; // [—, combined ORDER BY 1]
const S_COMB_LIMIT: usize = 9; // [—, combined LIMIT $ OFFSET $, combined FETCH NEXT $ ROWS ONLY]
const S_ORDER: usize = 10; //   [—, ORDER BY 1 DESC]
const S_LIMIT: usize = 11; //   [—, LIMIT $, LIMIT ALL]
const S_FETCH: usize = 12; //   [—, FETCH NEXT $ ROWS ONLY, FETCH NEXT $ ROWS WITH TIES]
const S_OFFSET: usize = 13; //  [—, OFFSET $]
const S_LOCKS: usize = 14; //   [—, FOR UPDATE, FOR NO KEY UPDATE OF "users" SKIP LOCKED,
//                                  FOR SHARE NOWAIT, FOR KEY SHARE]

#[cfg_attr(not(feature = "exhaustive"), allow(dead_code))]
const S_RADIX: [usize; 15] = [3, 3, 2, 2, 3, 2, 2, 6, 2, 3, 2, 3, 3, 2, 5];

type SelCfg = [usize; 15];

/// The CTE body: one column named `id`, carrying one bound argument so that a
/// `WITH` in front of anything shifts every later placeholder by one.
fn tag_ids() -> SelectQuery {
    psql::select((
        select::columns(quote(("tags", "id"))),
        select::from(quote("tags")),
        select::where_(quote(("tags", "id")).gt(arg(0i32))),
    ))
}

/// A set-operation operand whose arity and type line up with the main query's
/// projection: `count(*)` when the main query is grouped, `"tags"."id"` (plus
/// one bound argument) when it is not.
fn operand(grouped: bool) -> SelectQuery {
    if grouped {
        psql::select((
            select::columns(f("count", "*")),
            select::from(quote("tags")),
        ))
    } else {
        tag_ids()
    }
}

/// Whether the main query is an aggregate query, which decides the projection:
/// an ungrouped column in the select list of a grouped query is an analysis
/// error, so the projection is `count(*)` exactly when GROUP BY or HAVING is on.
fn sel_grouped(c: &SelCfg) -> bool {
    c[S_GROUP] > 0 || c[S_HAVING] > 0
}

fn build_select(c: &SelCfg) -> SelectQuery {
    let grouped = sel_grouped(c);
    let mut q = psql::select(());

    match c[S_WITH] {
        1 => q.apply(select::with("c", tag_ids())),
        2 => {
            q.apply(select::recursive(true));
            q.apply(select::with("c", tag_ids()).columns(["id"]));
        }
        _ => {}
    }
    match c[S_DISTINCT] {
        1 => q.apply(select::distinct()),
        2 => q.apply(select::distinct_on(quote(("users", "id")))),
        _ => {}
    }
    if grouped {
        q.apply(select::columns(f("count", "*")));
    } else {
        q.apply(select::columns(quote(("users", "id"))));
    }
    q.apply(select::from(quote("users")));
    if c[S_JOIN] == 1 {
        q.apply(
            select::inner_join(quote("posts"))
                .on_eq(quote(("posts", "user_id")), quote(("users", "id"))),
        );
    }
    if c[S_WHERE] == 1 {
        q.apply(select::where_(quote(("users", "age")).gte(arg(21i32))));
    }
    match c[S_GROUP] {
        1 => q.apply(select::group_by(quote(("users", "id")))),
        2 => q.apply(select::group_by(rollup(quote(("users", "id"))))),
        _ => {}
    }
    if c[S_HAVING] == 1 {
        q.apply(select::having(f("count", "*").into_expr().gt(arg(1i64))));
    }
    if c[S_WINDOW] == 1 {
        // The definition adapts to the aggregation context: in a grouped query
        // every expression in a window definition must be grouped or aggregated.
        if c[S_GROUP] > 0 {
            q.apply(select::window(
                "w",
                window::partition_by(quote(("users", "id"))),
            ));
        } else if c[S_HAVING] > 0 {
            q.apply(select::window("w", window::order_by(f("count", "*"))));
        } else {
            q.apply(select::window(
                "w",
                (
                    window::partition_by(quote(("users", "age"))),
                    window::order_by(quote(("users", "id"))),
                ),
            ));
        }
    }
    match c[S_COMBINE] {
        1 => q.apply(select::union(operand(grouped))),
        2 => q.apply(select::union_all(operand(grouped))),
        3 => q.apply(select::intersect(operand(grouped))),
        4 => q.apply(select::except(operand(grouped))),
        5 => {
            q.apply(select::union(operand(grouped)));
            q.apply(select::except(operand(grouped)));
        }
        _ => {}
    }
    if c[S_COMB_ORDER] == 1 {
        q.apply(select::order_by_combined(raw("1")));
    }
    match c[S_COMB_LIMIT] {
        1 => {
            q.apply(select::limit_combined(arg(7i64)));
            q.apply(select::offset_combined(arg(3i64)));
        }
        2 => q.apply(select::fetch_combined(arg(7i64))),
        _ => {}
    }
    if c[S_ORDER] == 1 {
        q.apply(select::order_by(raw("1")).desc());
    }
    match c[S_LIMIT] {
        1 => q.apply(select::limit(arg(10i64))),
        2 => q.apply(select::limit_all()),
        _ => {}
    }
    match c[S_FETCH] {
        1 => q.apply(select::fetch(arg(4i64))),
        2 => q.apply(select::fetch(arg(4i64)).with_ties()),
        _ => {}
    }
    if c[S_OFFSET] == 1 {
        q.apply(select::offset(arg(2i64)));
    }
    match c[S_LOCKS] {
        1 => q.apply(select::for_update()),
        2 => q.apply(select::for_no_key_update().of(["users"]).skip_locked()),
        3 => q.apply(select::for_share().no_wait()),
        4 => q.apply(select::for_key_share()),
        _ => {}
    }
    q
}

/// How many bound arguments the chosen values carry, derived from the
/// configuration — never from the built query.
fn sel_args(c: &SelCfg) -> usize {
    let grouped = sel_grouped(c);
    let operand_args = if grouped { 0 } else { 1 };
    (c[S_WITH] > 0) as usize
        + (c[S_WHERE] == 1) as usize
        + (c[S_HAVING] == 1) as usize
        + match c[S_COMBINE] {
            0 => 0,
            5 => 2 * operand_args,
            _ => operand_args,
        }
        + match c[S_COMB_LIMIT] {
            1 => 2,
            2 => 1,
            _ => 0,
        }
        + (c[S_LIMIT] == 1) as usize
        + (c[S_FETCH] > 0) as usize
        + (c[S_OFFSET] == 1) as usize
}

/// Combinations PostgreSQL's *grammar* has no sentence for.
fn sel_grammar_ok(c: &SelCfg) -> bool {
    // gram.y `select_limit`: LIMIT and FETCH are the same production's two
    // spellings — a statement gets one of them, never both.
    if c[S_LIMIT] > 0 && c[S_FETCH] > 0 {
        return false;
    }
    // A combined tail clause renders after the set operations; without any, it
    // collides with the direct tail clauses. See the pinned_wart tests.
    if (c[S_COMB_ORDER] > 0 || c[S_COMB_LIMIT] > 0) && c[S_COMBINE] == 0 {
        return false;
    }
    // gram.y's insertSelectOptions raises "WITH TIES cannot be specified
    // without ORDER BY clause" during *parse*, not analysis.
    if c[S_FETCH] == 2 && c[S_ORDER] == 0 {
        return false;
    }
    // Likewise "SKIP LOCKED and WITH TIES options cannot be used together" —
    // an interaction this sweep found the hard way.
    if c[S_FETCH] == 2 && c[S_LOCKS] == 2 {
        return false;
    }
    true
}

/// Combinations a real PostgreSQL rejects during analysis. Each rule cites the
/// PostgreSQL 17 manual and was confirmed against the live 17 server.
#[cfg_attr(not(feature = "exhaustive"), allow(dead_code))]
fn sel_engine_ok(c: &SelCfg) -> bool {
    // sql-select, The Locking Clause: "The locking clauses cannot be used in
    // contexts where returned rows cannot be clearly identified with individual
    // table rows" — DISTINCT, GROUP BY, HAVING (aggregation) and set operations
    // are all named. An unused named WINDOW is fine (confirmed live: the check
    // is on window *functions*, which the projection does not contain).
    if c[S_LOCKS] > 0
        && (c[S_DISTINCT] > 0 || c[S_GROUP] > 0 || c[S_HAVING] > 0 || c[S_COMBINE] > 0)
    {
        return false;
    }
    // sql-select, DISTINCT ON: the expressions are interpreted like ORDER BY
    // expressions over the select list; in a grouped query `"users"."id"` is
    // not grouped (the projection is count(*)) and fails analysis.
    if c[S_DISTINCT] == 2 && sel_grouped(c) {
        return false;
    }
    true
}

/// Invariant 2 for SELECT: the parse tree contains exactly the clauses asked
/// for, and no others. Field names are `pg_query` 6's `SelectStmt`.
#[track_caller]
fn verify_select(c: &SelCfg, sql: &str) {
    let top = parse_select(sql);

    // WITH always lands on the outermost node, even when set operations make
    // that node the set-op node rather than the query's own.
    assert_eq!(top.with_clause.is_some(), c[S_WITH] > 0, "with: {sql}");
    if let Some(w) = &top.with_clause {
        assert_eq!(w.recursive, c[S_WITH] == 2, "recursive: {sql}");
        assert_eq!(w.ctes.len(), 1, "one CTE: {sql}");
    }

    // Walk the set-operation spine down to the leading query. The operations
    // apply left to right, so the parse tree is left-deep: the outermost node
    // is the *last* operation.
    let expected_ops: &[(SetOperation, bool)] = match c[S_COMBINE] {
        0 => &[],
        1 => &[(SetOperation::SetopUnion, false)],
        2 => &[(SetOperation::SetopUnion, true)],
        3 => &[(SetOperation::SetopIntersect, false)],
        4 => &[(SetOperation::SetopExcept, false)],
        5 => &[
            (SetOperation::SetopUnion, false),
            (SetOperation::SetopExcept, false),
        ],
        _ => unreachable!(),
    };
    let mut spine: Vec<(SetOperation, bool)> = Vec::new();
    let mut cur: &protobuf::SelectStmt = &top;
    while cur.op() != SetOperation::SetopNone {
        spine.push((cur.op(), cur.all));
        let rarg = cur
            .rarg
            .as_deref()
            .expect("a set operation has a right arm");
        // Light check on the operand: it selects from "tags".
        assert!(
            matches!(
                rarg.from_clause.first().and_then(|n| n.node.as_ref()),
                Some(N::RangeVar(rv)) if rv.relname == "tags"
            ),
            "operand reads tags: {sql}"
        );
        if spine.len() > 1 {
            // Only the outermost node may carry the combination's tail clauses.
            assert!(
                cur.sort_clause.is_empty(),
                "inner set-op node sorted: {sql}"
            );
            assert!(
                cur.limit_count.is_none(),
                "inner set-op node limited: {sql}"
            );
        }
        cur = cur.larg.as_deref().expect("a set operation has a left arm");
    }
    spine.reverse();
    assert_eq!(spine, expected_ops, "set-operation spine: {sql}");
    let leading = cur;

    if c[S_COMBINE] > 0 {
        // The combination's own tail clauses sit on the outermost node …
        assert_eq!(
            !top.sort_clause.is_empty(),
            c[S_COMB_ORDER] == 1,
            "comb order: {sql}"
        );
        assert_eq!(
            top.limit_count.is_some(),
            c[S_COMB_LIMIT] > 0,
            "comb limit: {sql}"
        );
        assert_eq!(
            top.limit_offset.is_some(),
            c[S_COMB_LIMIT] == 1,
            "comb offset: {sql}"
        );
        // … and the leading query's WITH slot stays empty (it is on top).
        assert!(leading.with_clause.is_none(), "leading WITH: {sql}");
        assert!(top.locking_clause.is_empty(), "comb locks: {sql}");
    }

    // The leading query's own clauses, present exactly as configured.
    assert_eq!(
        !leading.distinct_clause.is_empty(),
        c[S_DISTINCT] > 0,
        "distinct: {sql}"
    );
    if c[S_DISTINCT] > 0 {
        // Plain DISTINCT parses as a list holding one empty node; DISTINCT ON
        // holds the expressions.
        let on = leading.distinct_clause[0].node.is_some();
        assert_eq!(on, c[S_DISTINCT] == 2, "distinct on: {sql}");
    }
    assert_eq!(leading.from_clause.len(), 1, "one from item: {sql}");
    match leading.from_clause[0].node.as_ref() {
        Some(N::JoinExpr(_)) => assert_eq!(c[S_JOIN], 1, "unexpected join: {sql}"),
        Some(N::RangeVar(rv)) => {
            assert_eq!(c[S_JOIN], 0, "missing join: {sql}");
            assert_eq!(rv.relname, "users", "from users: {sql}");
        }
        other => panic!("unexpected from item {other:?}\n  sql: {sql}"),
    }
    assert_eq!(
        leading.where_clause.is_some(),
        c[S_WHERE] == 1,
        "where: {sql}"
    );
    assert_eq!(
        !leading.group_clause.is_empty(),
        c[S_GROUP] > 0,
        "group by: {sql}"
    );
    if c[S_GROUP] == 2 {
        assert!(
            matches!(
                leading.group_clause[0].node.as_ref(),
                Some(N::GroupingSet(_))
            ),
            "rollup grouping set: {sql}"
        );
    }
    assert!(
        !leading.group_distinct,
        "GROUP BY DISTINCT not asked for: {sql}"
    );
    assert_eq!(
        leading.having_clause.is_some(),
        c[S_HAVING] == 1,
        "having: {sql}"
    );
    assert_eq!(leading.window_clause.len(), c[S_WINDOW], "window: {sql}");
    assert_eq!(
        !leading.sort_clause.is_empty(),
        c[S_ORDER] == 1,
        "order by: {sql}"
    );
    // LIMIT ALL parses as a NULL constant, so it is still a present limit_count.
    assert_eq!(
        leading.limit_count.is_some(),
        c[S_LIMIT] > 0 || c[S_FETCH] > 0,
        "limit/fetch: {sql}"
    );
    assert_eq!(
        leading.limit_offset.is_some(),
        c[S_OFFSET] == 1,
        "offset: {sql}"
    );
    // The parser marks the option `Count` whenever *any* of LIMIT, OFFSET or
    // FETCH is present — OFFSET alone included.
    let expected_option = if c[S_FETCH] == 2 {
        LimitOption::WithTies
    } else if c[S_LIMIT] > 0 || c[S_FETCH] == 1 || c[S_OFFSET] == 1 {
        LimitOption::Count
    } else {
        LimitOption::Default
    };
    assert_eq!(
        leading.limit_option(),
        expected_option,
        "limit option: {sql}"
    );
    assert_eq!(
        leading.locking_clause.len(),
        usize::from(c[S_LOCKS] > 0),
        "locks: {sql}"
    );
    if c[S_LOCKS] > 0 {
        let Some(N::LockingClause(lc)) = leading.locking_clause[0].node.as_ref() else {
            panic!("expected a locking clause: {sql}");
        };
        let (strength, wait, rels) = match c[S_LOCKS] {
            1 => (
                LockClauseStrength::LcsForupdate,
                LockWaitPolicy::LockWaitBlock,
                0,
            ),
            2 => (
                LockClauseStrength::LcsFornokeyupdate,
                LockWaitPolicy::LockWaitSkip,
                1,
            ),
            3 => (
                LockClauseStrength::LcsForshare,
                LockWaitPolicy::LockWaitError,
                0,
            ),
            4 => (
                LockClauseStrength::LcsForkeyshare,
                LockWaitPolicy::LockWaitBlock,
                0,
            ),
            _ => unreachable!(),
        };
        assert_eq!(lc.strength(), strength, "lock strength: {sql}");
        assert_eq!(lc.wait_policy(), wait, "lock wait: {sql}");
        assert_eq!(lc.locked_rels.len(), rels, "lock OF: {sql}");
    }
}

/// Build one configuration and run every always-on invariant.
#[track_caller]
fn check_select(c: &SelCfg) -> String {
    let (sql, _) = build_invariant(&build_select(c), sel_args(c));
    verify_select(c, &sql);
    sql
}

/// The boolean presence cross product: every clause on/off at its canonical
/// value. 2^15 = 32 768 raw combinations; minus LIMIT+FETCH co-presence
/// (× 3/4) and combined tails without a set operation (× 5/8) leaves 15 360.
#[test]
fn select_presence_cross_product() {
    let mut cases = 0usize;
    for mask in 0u32..(1 << 15) {
        let mut c: SelCfg = [0; 15];
        for (d, v) in c.iter_mut().enumerate() {
            *v = usize::from(mask >> d & 1 == 1);
        }
        if !sel_grammar_ok(&c) {
            continue;
        }
        cases += 1;
        check_select(&c);
    }
    assert_eq!(cases, 15_360);
}

/// The value sweep: the full product of every multi-valued dimension's values,
/// including the combined tails against every set-operation shape, plus ORDER
/// BY so that `FETCH … WITH TIES` (which the grammar ties to it) is present.
/// The other boolean dimensions stay off — their interactions are the presence
/// product's job. 3·3·3·6·2·3·2·3·3·5 = 87 480 raw; the LIMIT/FETCH/ORDER
/// rules keep 9 of 18 tail shapes, WITH TIES × SKIP LOCKED removes one
/// (tail, lock) pairing, and the combined tails keep 31 of 36 set-operation
/// shapes: 27 × 31 × (9·5 − 1) = 36 828.
#[test]
fn select_value_sweep() {
    let mut cases = 0usize;
    for_each_combo(
        [3, 3, 3, 6, 2, 3, 2, 3, 3, 5],
        |&[wi, di, gr, co, cor, col, or, li, fe, lo]| {
            let mut c: SelCfg = [0; 15];
            c[S_WITH] = wi;
            c[S_DISTINCT] = di;
            c[S_GROUP] = gr;
            c[S_COMBINE] = co;
            c[S_COMB_ORDER] = cor;
            c[S_COMB_LIMIT] = col;
            c[S_ORDER] = or;
            c[S_LIMIT] = li;
            c[S_FETCH] = fe;
            c[S_LOCKS] = lo;
            if !sel_grammar_ok(&c) {
                return;
            }
            cases += 1;
            check_select(&c);
        },
    );
    assert_eq!(cases, 36_828);
}

// ===========================================================================
// FROM-less SELECT — 10 dimensions
// ===========================================================================
//
// FROM itself is optional, and its absence changes what every other clause may
// reference: nothing. This matrix drives the clauses that survive with no
// table at all, projecting the constant `1`.

type MiniCfg = [usize; 10];

const M_WITH: usize = 0;
const M_DISTINCT: usize = 1;
const M_WHERE: usize = 2; //  WHERE (CAST($ AS int) = 1)
const M_GROUP: usize = 3; //  GROUP BY CAST($ AS int)
const M_HAVING: usize = 4;
const M_ORDER: usize = 5; //  ORDER BY 1
const M_LIMIT: usize = 6;
const M_OFFSET: usize = 7;
const M_COMBINE: usize = 8; // UNION ALL (SELECT 2)
const M_LOCKS: usize = 9; //  FOR UPDATE — legal without FROM (confirmed live)

fn build_mini(c: &MiniCfg) -> SelectQuery {
    let mut q = psql::select(select::columns(raw("1")));
    if c[M_WITH] == 1 {
        q.apply(select::with("c", tag_ids()));
    }
    if c[M_DISTINCT] == 1 {
        q.apply(select::distinct());
    }
    if c[M_WHERE] == 1 {
        q.apply(select::where_(cast(arg(1i32), "int").eq(raw("1"))));
    }
    if c[M_GROUP] == 1 {
        q.apply(select::group_by(cast(arg(2i32), "int")));
    }
    if c[M_HAVING] == 1 {
        q.apply(select::having(f("count", "*").into_expr().gt(arg(0i64))));
    }
    if c[M_COMBINE] == 1 {
        q.apply(select::union_all(psql::select(select::columns(raw("2")))));
    }
    if c[M_ORDER] == 1 {
        q.apply(select::order_by(raw("1")));
    }
    if c[M_LIMIT] == 1 {
        q.apply(select::limit(arg(10i64)));
    }
    if c[M_OFFSET] == 1 {
        q.apply(select::offset(arg(2i64)));
    }
    if c[M_LOCKS] == 1 {
        q.apply(select::for_update());
    }
    q
}

fn mini_args(c: &MiniCfg) -> usize {
    c[M_WITH] + c[M_WHERE] + c[M_GROUP] + c[M_HAVING] + c[M_LIMIT] + c[M_OFFSET]
}

#[cfg_attr(not(feature = "exhaustive"), allow(dead_code))]
fn mini_engine_ok(c: &MiniCfg) -> bool {
    // Same manual rule as `sel_engine_ok`: no locking over DISTINCT,
    // aggregation or set operations.
    !(c[M_LOCKS] == 1
        && (c[M_DISTINCT] == 1 || c[M_GROUP] == 1 || c[M_HAVING] == 1 || c[M_COMBINE] == 1))
}

#[track_caller]
fn check_mini(c: &MiniCfg) -> String {
    let (sql, _) = build_invariant(&build_mini(c), mini_args(c));
    let top = parse_select(&sql);
    let leading = if c[M_COMBINE] == 1 {
        assert_eq!(top.op(), SetOperation::SetopUnion, "union: {sql}");
        assert!(top.all, "union all: {sql}");
        top.larg.as_deref().expect("left arm").clone()
    } else {
        assert_eq!(top.op(), SetOperation::SetopNone, "no set op: {sql}");
        top.clone()
    };
    assert_eq!(top.with_clause.is_some(), c[M_WITH] == 1, "with: {sql}");
    assert!(leading.from_clause.is_empty(), "no FROM asked for: {sql}");
    assert_eq!(
        !leading.distinct_clause.is_empty(),
        c[M_DISTINCT] == 1,
        "distinct: {sql}"
    );
    assert_eq!(
        leading.where_clause.is_some(),
        c[M_WHERE] == 1,
        "where: {sql}"
    );
    assert_eq!(
        !leading.group_clause.is_empty(),
        c[M_GROUP] == 1,
        "group: {sql}"
    );
    assert_eq!(
        leading.having_clause.is_some(),
        c[M_HAVING] == 1,
        "having: {sql}"
    );
    assert_eq!(
        !leading.sort_clause.is_empty(),
        c[M_ORDER] == 1,
        "order: {sql}"
    );
    assert_eq!(
        leading.limit_count.is_some(),
        c[M_LIMIT] == 1,
        "limit: {sql}"
    );
    assert_eq!(
        leading.limit_offset.is_some(),
        c[M_OFFSET] == 1,
        "offset: {sql}"
    );
    assert_eq!(leading.locking_clause.len(), c[M_LOCKS], "locks: {sql}");
    sql
}

/// 2^10 = 1 024 — every combination is grammatical, so none is skipped.
#[test]
fn fromless_select_cross_product() {
    let mut cases = 0usize;
    for_each_combo([2; 10], |c| {
        cases += 1;
        check_mini(c);
    });
    assert_eq!(cases, 1_024);
}

// ===========================================================================
// INSERT — 6 dimensions, 288 cases
// ===========================================================================

type InsCfg = [usize; 6];

const I_WITH: usize = 0; //      [—, WITH c AS (…$…)]
const I_COLS: usize = 1; //      [—, ("id", "name")]
const I_SOURCE: usize = 2; //    [VALUES ×1, VALUES ×2, query] — a source is required
const I_OVERRIDING: usize = 3; // [—, SYSTEM, USER]
const I_CONFLICT: usize = 4; //  [—, DO NOTHING, (id) DO UPDATE … WHERE $, ON CONSTRAINT DO UPDATE]
const I_RETURNING: usize = 5; // [—, RETURNING *]

const I_RADIX: [usize; 6] = [2, 2, 3, 3, 4, 2];

fn build_insert(c: &InsCfg) -> psql::InsertQuery {
    let mut q = psql::insert(());
    if c[I_WITH] == 1 {
        q.apply(psql::insert::with("c", tag_ids()));
    }
    if c[I_COLS] == 1 {
        q.apply(psql::insert::into(quote("users")).columns(["id", "name"]));
    } else {
        // Without a column list the values fill the leading columns, so two
        // values still target (id, name).
        q.apply(psql::insert::into(quote("users")));
    }
    match c[I_OVERRIDING] {
        1 => q.apply(psql::insert::overriding_system()),
        2 => q.apply(psql::insert::overriding_user()),
        _ => {}
    }
    match c[I_SOURCE] {
        0 => q.apply(psql::insert::values((arg(1i32), arg("ada")))),
        1 => {
            q.apply(psql::insert::values((arg(1i32), arg("ada"))));
            q.apply(psql::insert::values((arg(2i32), arg("bob"))));
        }
        2 => {
            // The query's shape adapts to the column list: (id, name) when one
            // is given, the full row otherwise.
            let src = if c[I_COLS] == 1 {
                psql::select((
                    select::columns((quote(("tags", "id")), quote(("tags", "name")))),
                    select::from(quote("tags")),
                ))
            } else {
                psql::select(select::from(quote("users")))
            };
            q.apply(psql::insert::query(src));
        }
        _ => unreachable!(),
    }
    match c[I_CONFLICT] {
        1 => q.apply(psql::insert::on_conflict(()).do_nothing()),
        2 => q.apply(psql::insert::on_conflict(quote("id")).do_update((
            psql::insert::set_excluded(["name"]),
            // Qualified: an unqualified column that exists on the target table
            // is ambiguous against EXCLUDED inside DO UPDATE … WHERE.
            psql::insert::where_(quote(("users", "age")).gt(arg(0i32))),
        ))),
        3 => q.apply(
            psql::insert::on_conflict_on_constraint("users_pkey")
                .do_update(psql::insert::set_excluded(["name"])),
        ),
        _ => {}
    }
    if c[I_RETURNING] == 1 {
        q.apply(psql::insert::returning("*"));
    }
    q
}

fn ins_args(c: &InsCfg) -> usize {
    c[I_WITH]
        + match c[I_SOURCE] {
            0 => 2,
            1 => 4,
            _ => 0,
        }
        + usize::from(c[I_CONFLICT] == 2)
}

#[track_caller]
fn check_insert(c: &InsCfg) -> String {
    let (sql, _) = build_invariant(&build_insert(c), ins_args(c));
    let N::InsertStmt(stmt) = parse_single(&sql) else {
        panic!("expected an InsertStmt: {sql}");
    };
    assert_eq!(
        stmt.relation.as_ref().map(|r| r.relname.as_str()),
        Some("users"),
        "target: {sql}"
    );
    assert_eq!(
        stmt.cols.len(),
        if c[I_COLS] == 1 { 2 } else { 0 },
        "column list: {sql}"
    );
    assert_eq!(stmt.with_clause.is_some(), c[I_WITH] == 1, "with: {sql}");
    assert_eq!(
        !stmt.returning_list.is_empty(),
        c[I_RETURNING] == 1,
        "returning: {sql}"
    );
    let expected_override = match c[I_OVERRIDING] {
        1 => OverridingKind::OverridingSystemValue,
        2 => OverridingKind::OverridingUserValue,
        _ => OverridingKind::OverridingNotSet,
    };
    assert_eq!(stmt.r#override(), expected_override, "overriding: {sql}");

    let source = stmt
        .select_stmt
        .as_ref()
        .and_then(|n| n.node.as_ref())
        .expect("an INSERT has a source");
    let N::SelectStmt(source) = source else {
        panic!("the source is a SelectStmt: {sql}");
    };
    match c[I_SOURCE] {
        0 => assert_eq!(source.values_lists.len(), 1, "one VALUES row: {sql}"),
        1 => assert_eq!(source.values_lists.len(), 2, "two VALUES rows: {sql}"),
        2 => {
            assert!(
                source.values_lists.is_empty(),
                "no VALUES for a query source: {sql}"
            );
            assert!(
                !source.from_clause.is_empty(),
                "query source reads a table: {sql}"
            );
        }
        _ => unreachable!(),
    }

    match c[I_CONFLICT] {
        0 => assert!(stmt.on_conflict_clause.is_none(), "no ON CONFLICT: {sql}"),
        v => {
            let oc = stmt.on_conflict_clause.as_ref().expect("ON CONFLICT");
            let expected_action = if v == 1 {
                OnConflictAction::OnconflictNothing
            } else {
                OnConflictAction::OnconflictUpdate
            };
            assert_eq!(oc.action(), expected_action, "conflict action: {sql}");
            assert_eq!(!oc.target_list.is_empty(), v >= 2, "DO UPDATE SET: {sql}");
            assert_eq!(oc.where_clause.is_some(), v == 2, "DO UPDATE WHERE: {sql}");
            match v {
                1 => assert!(oc.infer.is_none(), "DO NOTHING targets any conflict: {sql}"),
                2 => {
                    let infer = oc.infer.as_ref().expect("inferred target");
                    assert!(!infer.index_elems.is_empty(), "column target: {sql}");
                    assert!(infer.conname.is_empty(), "no constraint name: {sql}");
                }
                3 => {
                    let infer = oc.infer.as_ref().expect("constraint target");
                    assert_eq!(infer.conname, "users_pkey", "constraint name: {sql}");
                    assert!(infer.index_elems.is_empty(), "no column target: {sql}");
                }
                _ => unreachable!(),
            }
        }
    }
    sql
}

/// 2·2·3·3·4·2 = 288, all grammatical, all engine-checkable — OVERRIDING on a
/// non-identity column is accepted at PREPARE (confirmed live), and the
/// schema's PK is named `users_pkey` by PostgreSQL's default rule.
#[test]
fn insert_cross_product() {
    let mut cases = 0usize;
    for_each_combo(I_RADIX, |c| {
        cases += 1;
        check_insert(c);
    });
    assert_eq!(cases, 288);
}

// ===========================================================================
// UPDATE — 6 dimensions, 48 cases
// ===========================================================================

type UpdCfg = [usize; 6];

const U_WITH: usize = 0;
const U_SETS: usize = 1; //     [one assignment, two]
const U_FROM: usize = 2; //     [—, FROM "users"]
const U_JOIN: usize = 3; //     [—, INNER JOIN "comments" ON (…)] — needs FROM
const U_WHERE: usize = 4;
const U_RETURNING: usize = 5;

const U_RADIX: [usize; 6] = [2, 2, 2, 2, 2, 2];

fn build_update(c: &UpdCfg) -> psql::UpdateQuery {
    let mut q = psql::update((
        psql::update::table(quote("posts")),
        psql::update::set_col("views").to(arg(1i32)),
    ));
    if c[U_WITH] == 1 {
        q.apply(psql::update::with("c", tag_ids()));
    }
    if c[U_SETS] == 1 {
        q.apply(psql::update::set_col("status").to(arg("done")));
    }
    if c[U_FROM] == 1 {
        q.apply(psql::update::from(quote("users")));
    }
    if c[U_JOIN] == 1 {
        q.apply(
            psql::update::inner_join(quote("comments"))
                .on_eq(quote(("comments", "user_id")), quote(("users", "id"))),
        );
    }
    if c[U_WHERE] == 1 {
        q.apply(psql::update::where_(
            quote(("posts", "views")).gt(arg(0i32)),
        ));
    }
    if c[U_RETURNING] == 1 {
        q.apply(psql::update::returning(quote(("posts", "id"))));
    }
    q
}

fn upd_args(c: &UpdCfg) -> usize {
    c[U_WITH] + 1 + c[U_SETS] + c[U_WHERE]
}

/// The joins hang off the from-item, deliberately — without a FROM there is
/// nothing for a join to attach to (and today it is silently dropped; see
/// `pinned_wart_update_join_without_from_is_silently_dropped`).
fn upd_grammar_ok(c: &UpdCfg) -> bool {
    !(c[U_JOIN] == 1 && c[U_FROM] == 0)
}

#[track_caller]
fn check_update(c: &UpdCfg) -> String {
    let (sql, _) = build_invariant(&build_update(c), upd_args(c));
    let N::UpdateStmt(stmt) = parse_single(&sql) else {
        panic!("expected an UpdateStmt: {sql}");
    };
    assert_eq!(
        stmt.relation.as_ref().map(|r| r.relname.as_str()),
        Some("posts"),
        "target: {sql}"
    );
    assert_eq!(stmt.target_list.len(), 1 + c[U_SETS], "assignments: {sql}");
    assert_eq!(stmt.with_clause.is_some(), c[U_WITH] == 1, "with: {sql}");
    assert_eq!(stmt.where_clause.is_some(), c[U_WHERE] == 1, "where: {sql}");
    assert_eq!(
        !stmt.returning_list.is_empty(),
        c[U_RETURNING] == 1,
        "returning: {sql}"
    );
    assert_eq!(stmt.from_clause.len(), c[U_FROM], "from: {sql}");
    if c[U_FROM] == 1 {
        let is_join = matches!(stmt.from_clause[0].node.as_ref(), Some(N::JoinExpr(_)));
        assert_eq!(is_join, c[U_JOIN] == 1, "join: {sql}");
    }
    sql
}

/// 2^6 = 64, minus a join with no FROM to attach to (16) = 48.
#[test]
fn update_cross_product() {
    let mut cases = 0usize;
    for_each_combo(U_RADIX, |c| {
        if !upd_grammar_ok(c) {
            return;
        }
        cases += 1;
        check_update(c);
    });
    assert_eq!(cases, 48);
}

// ===========================================================================
// DELETE — 5 dimensions, 24 cases
// ===========================================================================

type DelCfg = [usize; 5];

const D_WITH: usize = 0;
const D_USING: usize = 1; //    [—, USING "posts"]
const D_JOIN: usize = 2; //     [—, INNER JOIN "users" ON (…)] — needs USING
const D_WHERE: usize = 3;
const D_RETURNING: usize = 4;

const D_RADIX: [usize; 5] = [2, 2, 2, 2, 2];

fn build_delete(c: &DelCfg) -> psql::DeleteQuery {
    let mut q = psql::delete(psql::delete::from(quote("comments")));
    if c[D_WITH] == 1 {
        q.apply(psql::delete::with("c", tag_ids()));
    }
    if c[D_USING] == 1 {
        q.apply(psql::delete::using(quote("posts")));
    }
    if c[D_JOIN] == 1 {
        q.apply(
            psql::delete::inner_join(quote("users"))
                .on_eq(quote(("users", "id")), quote(("posts", "user_id"))),
        );
    }
    if c[D_WHERE] == 1 {
        q.apply(psql::delete::where_(
            quote(("comments", "id")).gt(arg(0i32)),
        ));
    }
    if c[D_RETURNING] == 1 {
        q.apply(psql::delete::returning(quote(("comments", "id"))));
    }
    q
}

fn del_args(c: &DelCfg) -> usize {
    c[D_WITH] + c[D_WHERE]
}

fn del_grammar_ok(c: &DelCfg) -> bool {
    !(c[D_JOIN] == 1 && c[D_USING] == 0)
}

#[track_caller]
fn check_delete(c: &DelCfg) -> String {
    let (sql, _) = build_invariant(&build_delete(c), del_args(c));
    let N::DeleteStmt(stmt) = parse_single(&sql) else {
        panic!("expected a DeleteStmt: {sql}");
    };
    assert_eq!(
        stmt.relation.as_ref().map(|r| r.relname.as_str()),
        Some("comments"),
        "target: {sql}"
    );
    assert_eq!(stmt.with_clause.is_some(), c[D_WITH] == 1, "with: {sql}");
    assert_eq!(stmt.where_clause.is_some(), c[D_WHERE] == 1, "where: {sql}");
    assert_eq!(
        !stmt.returning_list.is_empty(),
        c[D_RETURNING] == 1,
        "returning: {sql}"
    );
    assert_eq!(stmt.using_clause.len(), c[D_USING], "using: {sql}");
    if c[D_USING] == 1 {
        let is_join = matches!(stmt.using_clause[0].node.as_ref(), Some(N::JoinExpr(_)));
        assert_eq!(is_join, c[D_JOIN] == 1, "join: {sql}");
    }
    sql
}

/// 2^5 = 32, minus a join with no USING to attach to (8) = 24.
#[test]
fn delete_cross_product() {
    let mut cases = 0usize;
    for_each_combo(D_RADIX, |c| {
        if !del_grammar_ok(c) {
            return;
        }
        cases += 1;
        check_delete(c);
    });
    assert_eq!(cases, 24);
}

// ===========================================================================
// Joins, enumerated so that "every join" is a countable claim
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq)]
enum JKind {
    Inner,
    Left,
    Right,
    Full,
    Cross,
}

const QUALIFIED: [JKind; 4] = [JKind::Inner, JKind::Left, JKind::Right, JKind::Full];

#[derive(Debug, Clone, Copy, PartialEq)]
enum JCond {
    /// `ON (a = b)`.
    On,
    /// `USING ("id")` — every item exposes a column named `id` for this.
    Using,
    /// "nothing": a qualified join without ON/USING is only grammatical as
    /// `NATURAL`, so that is what "no condition" means for these kinds.
    /// `CROSS JOIN` is the other conditionless join and is its own kind.
    Natural,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum JItem {
    /// `"posts"` — a plain table.
    Table,
    /// A parenthesised sub-query exposing `id` (and one bound argument).
    Sub,
    /// `generate_series(…)` aliased to expose `id`.
    Func,
    /// `(VALUES (1), (2))` aliased to expose `id`.
    Values,
    /// A `WITH c AS (…)` name used as the join target.
    Cte,
}

#[derive(Debug, Clone, Copy)]
struct JoinCase {
    kind: JKind,
    cond: Option<JCond>, // None for Cross
    item: JItem,
    lateral: bool,
}

impl JoinCase {
    /// LATERAL is grammatical only in front of a sub-query or function-ish
    /// item — `JOIN LATERAL "posts"` is a syntax error (verified while writing
    /// this file), so a bare table or CTE name never gets the flag.
    fn lateral_allowed(self) -> bool {
        matches!(self.item, JItem::Sub | JItem::Func | JItem::Values)
    }

    /// Whether the LATERAL body may actually reference the left side: a
    /// LATERAL reference from the right side of a RIGHT/FULL join is an
    /// analysis error (PostgreSQL 17, sql-select LATERAL; confirmed live), so
    /// those get a self-contained body and keep only the keyword.
    fn lateral_referencing(self) -> bool {
        self.lateral && matches!(self.kind, JKind::Inner | JKind::Left | JKind::Cross)
    }
}

/// The sub-query item: one output column named `id`, one bound argument, and —
/// when the case may — a lateral reference to the left side.
fn sub_item(referencing: bool) -> Expr {
    if referencing {
        subquery(psql::select((
            select::columns(quote(("posts", "id"))),
            select::from(quote("posts")),
            select::where_(quote(("posts", "user_id")).eq(quote(("users", "id")))),
        )))
    } else {
        subquery(psql::select((
            select::columns(quote(("posts", "id"))),
            select::from(quote("posts")),
            select::where_(quote(("posts", "views")).gt(arg(0i32))),
        )))
    }
}

fn join_item_expr(case: JoinCase) -> Expr {
    match case.item {
        JItem::Table => quote("posts"),
        JItem::Sub => sub_item(case.lateral_referencing()),
        JItem::Func => {
            if case.lateral_referencing() {
                f("generate_series", (raw("1"), quote(("users", "id")))).into_expr()
            } else {
                f("generate_series", (raw("1"), raw("3"))).into_expr()
            }
        }
        JItem::Values => raw("(VALUES (1), (2))"),
        JItem::Cte => quote("c"),
    }
}

fn build_join_case(case: JoinCase) -> SelectQuery {
    let mut q = psql::select((
        select::from(quote("users")),
        select::where_(quote(("users", "age")).gt(arg(0i32))),
    ));
    if case.item == JItem::Cte {
        q.apply(select::with("c", tag_ids()));
    }
    let target = join_item_expr(case);
    // Every item is aliased "x" and exposes a column named "id", so ON, USING
    // and NATURAL all resolve against the same shape.
    let needs_cols = matches!(case.item, JItem::Func | JItem::Values);
    if case.kind == JKind::Cross {
        let mut ch = select::cross_join(target).as_("x");
        if needs_cols {
            ch = ch.columns(["id"]);
        }
        if case.lateral {
            ch = ch.lateral();
        }
        q.apply(ch);
    } else {
        let mut ch = match case.kind {
            JKind::Inner => select::inner_join(target),
            JKind::Left => select::left_join(target),
            JKind::Right => select::right_join(target),
            JKind::Full => select::full_join(target),
            JKind::Cross => unreachable!(),
        }
        .as_("x");
        if needs_cols {
            ch = ch.columns(["id"]);
        }
        if case.lateral {
            ch = ch.lateral();
        }
        match case.cond.expect("qualified joins carry a condition") {
            JCond::On => {
                let left = if case.item == JItem::Table {
                    quote(("x", "user_id"))
                } else {
                    quote(("x", "id"))
                };
                ch = ch.on_eq(left, quote(("users", "id")));
            }
            JCond::Using => ch = ch.using(["id"]),
            JCond::Natural => ch = ch.natural(),
        }
        q.apply(ch);
    }
    q
}

fn join_case_args(case: JoinCase) -> usize {
    // The base WHERE, the CTE body's argument, and the non-referencing
    // sub-query's argument.
    1 + usize::from(case.item == JItem::Cte)
        + usize::from(case.item == JItem::Sub && !case.lateral_referencing())
}

/// Invariant 2 for a single join: exactly one `JoinExpr` of the right type,
/// naturalness, condition, and right-arm node kind, with LATERAL where asked.
#[track_caller]
fn verify_join_case(case: JoinCase, sql: &str) {
    let stmt = parse_select(sql);
    assert_eq!(stmt.from_clause.len(), 1, "one from item: {sql}");
    let Some(N::JoinExpr(j)) = stmt.from_clause[0].node.as_ref() else {
        panic!("expected a JoinExpr: {sql}");
    };
    // CROSS JOIN parses as an inner join with no qualification at all.
    let expected_type = match case.kind {
        JKind::Inner | JKind::Cross => JoinType::JoinInner,
        JKind::Left => JoinType::JoinLeft,
        JKind::Right => JoinType::JoinRight,
        JKind::Full => JoinType::JoinFull,
    };
    assert_eq!(j.jointype(), expected_type, "join type: {sql}");
    assert_eq!(
        j.is_natural,
        case.cond == Some(JCond::Natural),
        "natural: {sql}"
    );
    assert_eq!(
        !j.using_clause.is_empty(),
        case.cond == Some(JCond::Using),
        "using: {sql}"
    );
    assert_eq!(j.quals.is_some(), case.cond == Some(JCond::On), "on: {sql}");
    assert!(
        matches!(
            j.larg.as_ref().and_then(|n| n.node.as_ref()),
            Some(N::RangeVar(rv)) if rv.relname == "users"
        ),
        "left arm is users: {sql}"
    );
    match (case.item, j.rarg.as_ref().and_then(|n| n.node.as_ref())) {
        (JItem::Table, Some(N::RangeVar(rv))) => assert_eq!(rv.relname, "posts", "table: {sql}"),
        (JItem::Cte, Some(N::RangeVar(rv))) => assert_eq!(rv.relname, "c", "cte: {sql}"),
        (JItem::Sub | JItem::Values, Some(N::RangeSubselect(rs))) => {
            assert_eq!(rs.lateral, case.lateral, "lateral: {sql}");
            let inner = rs.subquery.as_ref().and_then(|n| n.node.as_ref());
            let Some(N::SelectStmt(inner)) = inner else {
                panic!("sub-select body: {sql}");
            };
            assert_eq!(
                !inner.values_lists.is_empty(),
                case.item == JItem::Values,
                "values body: {sql}"
            );
        }
        (JItem::Func, Some(N::RangeFunction(rf))) => {
            assert_eq!(rf.lateral, case.lateral, "lateral: {sql}");
        }
        (item, other) => panic!("item {item:?} parsed as {other:?}\n  sql: {sql}"),
    }
}

fn all_single_join_cases() -> Vec<JoinCase> {
    let mut out = Vec::new();
    let items = [
        JItem::Table,
        JItem::Sub,
        JItem::Func,
        JItem::Values,
        JItem::Cte,
    ];
    for item in items {
        for lateral in [false, true] {
            let template = JoinCase {
                kind: JKind::Cross,
                cond: None,
                item,
                lateral,
            };
            if lateral && !template.lateral_allowed() {
                continue;
            }
            // The four qualified kinds × the three conditions …
            for kind in QUALIFIED {
                for cond in [JCond::On, JCond::Using, JCond::Natural] {
                    out.push(JoinCase {
                        kind,
                        cond: Some(cond),
                        item,
                        lateral,
                    });
                }
            }
            // … plus CROSS JOIN, which takes no condition.
            out.push(template);
        }
    }
    out
}

/// Each join kind × ON/USING/nothing × ± LATERAL × each from-item shape:
/// (4 kinds × 3 conditions + CROSS) × (5 items + 3 LATERAL-able items) = 104.
#[test]
fn single_joins() {
    let cases = all_single_join_cases();
    assert_eq!(cases.len(), 104);
    for case in cases {
        let (sql, _) = build_invariant(&build_join_case(case), join_case_args(case));
        verify_join_case(case, &sql);
    }
}

// --- chains ---------------------------------------------------------------

/// The 13 (kind, condition) shapes a single link can take.
fn all_links() -> Vec<(JKind, Option<JCond>)> {
    let mut out = Vec::new();
    for kind in QUALIFIED {
        for cond in [JCond::On, JCond::Using, JCond::Natural] {
            out.push((kind, Some(cond)));
        }
    }
    out.push((JKind::Cross, None));
    out
}

/// Chain targets and their ON conditions, in order. Every table shares an
/// `id` column with the growing join tree, so USING ("id") and NATURAL are
/// well-typed at every link.
const CHAIN: [(&str, (&str, &str)); 3] = [
    ("posts", ("user_id", "users")),
    ("comments", ("post_id", "posts")),
    ("tags", ("id", "comments")),
];

fn build_chain(links: &[(JKind, Option<JCond>)]) -> SelectQuery {
    let mut q = psql::select((
        select::from(quote("users")),
        select::where_(quote(("users", "age")).gt(arg(0i32))),
    ));
    for (i, &(kind, cond)) in links.iter().enumerate() {
        let (table, (col, other)) = CHAIN[i];
        if kind == JKind::Cross {
            q.apply(select::cross_join(quote(table)));
            continue;
        }
        let mut ch = match kind {
            JKind::Inner => select::inner_join(quote(table)),
            JKind::Left => select::left_join(quote(table)),
            JKind::Right => select::right_join(quote(table)),
            JKind::Full => select::full_join(quote(table)),
            JKind::Cross => unreachable!(),
        };
        match cond.expect("qualified link") {
            JCond::On => ch = ch.on_eq(quote((table, col)), quote((other, "id"))),
            JCond::Using => ch = ch.using(["id"]),
            JCond::Natural => ch = ch.natural(),
        }
        q.apply(ch);
    }
    q
}

/// Verify a chain's parse tree: a left-deep `JoinExpr` spine whose bottom is
/// `users` and whose i-th right arm is the i-th chain table, each link with
/// the kind and condition asked for.
#[track_caller]
fn verify_chain(links: &[(JKind, Option<JCond>)], sql: &str) {
    let stmt = parse_select(sql);
    assert_eq!(stmt.from_clause.len(), 1, "one from item: {sql}");
    let mut cur = stmt.from_clause[0].node.as_ref();
    // The outermost JoinExpr is the last link; walk down the left spine.
    for (i, &(kind, cond)) in links.iter().enumerate().rev() {
        let Some(N::JoinExpr(j)) = cur else {
            panic!("expected a JoinExpr for link {i}: {sql}");
        };
        let expected_type = match kind {
            JKind::Inner | JKind::Cross => JoinType::JoinInner,
            JKind::Left => JoinType::JoinLeft,
            JKind::Right => JoinType::JoinRight,
            JKind::Full => JoinType::JoinFull,
        };
        assert_eq!(j.jointype(), expected_type, "link {i} type: {sql}");
        assert_eq!(
            j.is_natural,
            cond == Some(JCond::Natural),
            "link {i} natural: {sql}"
        );
        assert_eq!(
            !j.using_clause.is_empty(),
            cond == Some(JCond::Using),
            "link {i} using: {sql}"
        );
        assert_eq!(
            j.quals.is_some(),
            cond == Some(JCond::On),
            "link {i} on: {sql}"
        );
        assert!(
            matches!(
                j.rarg.as_ref().and_then(|n| n.node.as_ref()),
                Some(N::RangeVar(rv)) if rv.relname == CHAIN[i].0
            ),
            "link {i} target: {sql}"
        );
        cur = j.larg.as_ref().and_then(|n| n.node.as_ref());
    }
    assert!(
        matches!(cur, Some(N::RangeVar(rv)) if rv.relname == "users"),
        "chain bottom is users: {sql}"
    );
}

/// Whether a real engine can resolve the chain: `USING`/`NATURAL` merge the
/// shared column, but an earlier `ON` or `CROSS` link leaves *two* columns
/// named `id` in the left tree, and the merge fails analysis with "common
/// column name … appears more than once in left table" (found live by this
/// matrix). So a merge link may only follow merge links.
#[cfg_attr(not(feature = "exhaustive"), allow(dead_code))]
fn chain_engine_ok(links: &[(JKind, Option<JCond>)]) -> bool {
    let mut non_merge_seen = false;
    for &(_, cond) in links {
        match cond {
            Some(JCond::Using | JCond::Natural) => {
                if non_merge_seen {
                    return false;
                }
            }
            _ => non_merge_seen = true,
        }
    }
    true
}

/// Chains of two to four from-items: every (kind, condition) shape at every
/// link, 13 + 13² + 13³ = 2 379 chains.
#[test]
fn join_chains() {
    let links = all_links();
    let mut cases = 0usize;
    for len in 1..=3usize {
        let mut idx = vec![0usize; len];
        loop {
            let chain: Vec<_> = idx.iter().map(|&i| links[i]).collect();
            cases += 1;
            let (sql, _) = build_invariant(&build_chain(&chain), 1);
            verify_chain(&chain, &sql);
            let mut d = 0;
            loop {
                if d == len {
                    break;
                }
                idx[d] += 1;
                if idx[d] < links.len() {
                    break;
                }
                idx[d] = 0;
                d += 1;
            }
            if d == len {
                break;
            }
        }
    }
    assert_eq!(cases, 13 + 13 * 13 + 13 * 13 * 13);
}

// --- self-joins -----------------------------------------------------------

fn build_self_join(kind: JKind, cond: Option<JCond>) -> SelectQuery {
    let mut q = psql::select((
        select::from(quote("users")).as_("a"),
        select::where_(quote(("a", "age")).gt(arg(0i32))),
    ));
    if kind == JKind::Cross {
        q.apply(select::cross_join(quote("users")).as_("b"));
        return q;
    }
    let mut ch = match kind {
        JKind::Inner => select::inner_join(quote("users")),
        JKind::Left => select::left_join(quote("users")),
        JKind::Right => select::right_join(quote("users")),
        JKind::Full => select::full_join(quote("users")),
        JKind::Cross => unreachable!(),
    }
    .as_("b");
    match cond.expect("qualified") {
        JCond::On => ch = ch.on_eq(quote(("b", "id")), quote(("a", "id"))),
        JCond::Using => ch = ch.using(["id"]),
        // NATURAL self-join: every column is common with itself, all
        // trivially type-compatible.
        JCond::Natural => ch = ch.natural(),
    }
    q.apply(ch);
    q
}

/// A table joined to itself under both aliases, in all 13 shapes.
#[test]
fn self_joins() {
    let mut cases = 0usize;
    for (kind, cond) in all_links() {
        cases += 1;
        let (sql, _) = build_invariant(&build_self_join(kind, cond), 1);
        let stmt = parse_select(&sql);
        let Some(N::JoinExpr(j)) = stmt.from_clause[0].node.as_ref() else {
            panic!("expected a JoinExpr: {sql}");
        };
        for (arm, alias) in [(&j.larg, "a"), (&j.rarg, "b")] {
            assert!(
                matches!(
                    arm.as_ref().and_then(|n| n.node.as_ref()),
                    Some(N::RangeVar(rv))
                        if rv.relname == "users"
                            && rv.alias.as_ref().is_some_and(|a| a.aliasname == alias)
                ),
                "self-join arm {alias}: {sql}"
            );
        }
    }
    assert_eq!(cases, 13);
}

// ===========================================================================
// Pinned warts — combinations where the builder emits unparseable SQL (or
// silently drops a clause) without recording an error
// ===========================================================================
//
// These were found by the matrices above while this file was written. They
// contradict the design rule that rendering failures are recorded on the
// writer and surfaced once by `build()`. Each test pins today's behaviour so
// the fix — when it lands — breaks the pin loudly and the matrices' skip
// predicates can be tightened.

/// A combined tail clause without any set operation renders after the direct
/// tail clauses — `LIMIT $1 ORDER BY 1` — which no PostgreSQL grammar accepts,
/// and `build()` reports no error.
#[test]
fn pinned_wart_combined_tail_without_set_op_renders_unparseable_sql() {
    let q = psql::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        select::limit(arg(1i64)),
        select::order_by_combined(raw("1")),
    ));
    let (sql, _) = q.build().expect("build() reports no error today");
    assert!(
        keelson_sqlcheck::check_psql(&sql).is_err(),
        "if this now parses the wart was fixed — drop the comb-tail restriction \
         from sel_grammar_ok and delete this pin: {sql}"
    );
}

/// `LIMIT` and `FETCH` are two spellings of one grammar production; setting
/// both renders `LIMIT $1 FETCH NEXT $2 ROWS ONLY`, which does not parse, and
/// `build()` reports no error.
#[test]
fn pinned_wart_limit_and_fetch_together_render_unparseable_sql() {
    let q = psql::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        select::limit(arg(1i64)),
        select::fetch(arg(2i64)),
    ));
    let (sql, _) = q.build().expect("build() reports no error today");
    assert!(
        keelson_sqlcheck::check_psql(&sql).is_err(),
        "if this now parses the wart was fixed — drop the LIMIT/FETCH \
         restriction from sel_grammar_ok and delete this pin: {sql}"
    );
}

/// An UPDATE's joins attach to the FROM item; with no FROM the join is
/// *silently dropped* — the built SQL is valid and simply misses a clause the
/// caller asked for, which neither a grammar nor an engine can notice.
#[test]
fn pinned_wart_update_join_without_from_is_silently_dropped() {
    let q = psql::update((
        psql::update::table(quote("posts")),
        psql::update::set_col("views").to(arg(1i32)),
        psql::update::inner_join(quote("users")).using(["id"]),
    ));
    let (sql, _) = q.build().expect("build() reports no error today");
    assert!(
        !sql.contains("JOIN"),
        "if the join now renders (or build() errors) the wart was fixed — \
         extend the UPDATE matrix to cover it and delete this pin: {sql}"
    );
}

/// `LATERAL` in front of a bare table name is a syntax error in PostgreSQL's
/// grammar; the builder writes it anyway and `build()` reports no error.
#[test]
fn pinned_wart_lateral_on_a_bare_table_renders_unparseable_sql() {
    let q = psql::select((
        select::from(quote("users")),
        select::inner_join(quote("posts")).lateral().on(raw("TRUE")),
    ));
    let (sql, _) = q.build().expect("build() reports no error today");
    assert!(
        keelson_sqlcheck::check_psql(&sql).is_err(),
        "if this now parses (or build() errors) the wart was fixed — allow \
         lateral tables in the join matrix and delete this pin: {sql}"
    );
}

// ===========================================================================
// The engine tier — behind `exhaustive` (which implies `live-docker`)
// ===========================================================================
//
// A real PostgreSQL 17 PREPAREs every statement: parse *and* analysis, with
// names resolved against the shared schema. Budgeted per the cost ratio —
// every co-occurrence of up to three SELECT clauses plus a stratified random
// sample, and the smaller matrices whole.

#[cfg(feature = "exhaustive")]
mod engine {
    use super::*;
    use std::collections::BTreeSet;

    #[track_caller]
    fn engine_check(sql: &str, what: &dyn std::fmt::Debug) {
        if let Err(e) = keelson_sqlcheck::live::check_psql(sql) {
            panic!(
                "real PostgreSQL rejected the generated SQL\n  case: {what:?}\n  error: {e}\n  sql: {sql}"
            );
        }
    }

    fn run_select(c: &SelCfg) {
        let sql = check_select(c);
        engine_check(&sql, c);
    }

    /// Every single clause value, every canonical pair, every canonical
    /// triple — three-wise because three-clause interactions are real (the
    /// UNION + ORDER BY + LIMIT parenthesisation exists only when all three
    /// are present).
    #[test]
    fn select_up_to_three_wise() {
        let mut seen: BTreeSet<SelCfg> = BTreeSet::new();
        // Singles, at every value the dimension has.
        for d in 0..15 {
            for v in 1..S_RADIX[d] {
                let mut c: SelCfg = [0; 15];
                c[d] = v;
                seen.insert(c);
            }
        }
        // Pairs and triples at canonical values.
        for i in 0..15 {
            for j in i + 1..15 {
                let mut c: SelCfg = [0; 15];
                c[i] = 1;
                c[j] = 1;
                seen.insert(c);
                for k in j + 1..15 {
                    let mut c = c;
                    c[k] = 1;
                    seen.insert(c);
                }
            }
        }
        let mut ran = 0usize;
        for c in &seen {
            if sel_grammar_ok(c) && sel_engine_ok(c) {
                run_select(c);
                ran += 1;
            }
        }
        eprintln!(
            "engine three-wise: {ran} of {} candidate configurations",
            seen.len()
        );
        // Most of the shrinkage is the locking clause's genuine
        // incompatibilities; below this the predicates are eating too much.
        assert!(ran > 300, "the three-wise pass shrank suspiciously: {ran}");
    }

    /// A stratified random sample of denser configurations: for every clause
    /// count from 4 to 10, forty random configurations with random values.
    /// Deterministic seed, so a failure reproduces.
    #[test]
    fn select_stratified_sample() {
        let mut rng = Rng(0xDEC181);
        let mut seen: BTreeSet<SelCfg> = BTreeSet::new();
        for popcount in 4..=10usize {
            let mut found = 0usize;
            let mut attempts = 0usize;
            while found < 40 && attempts < 4000 {
                attempts += 1;
                let mut c: SelCfg = [0; 15];
                let mut dims: Vec<usize> = (0..15).collect();
                for _ in 0..popcount {
                    let pick = rng.below(dims.len());
                    let d = dims.swap_remove(pick);
                    c[d] = 1 + rng.below(S_RADIX[d] - 1);
                }
                if sel_grammar_ok(&c) && sel_engine_ok(&c) && seen.insert(c) {
                    found += 1;
                }
            }
            assert_eq!(found, 40, "stratum {popcount} could not be filled");
        }
        for c in &seen {
            run_select(c);
        }
        eprintln!("engine stratified sample: {} configurations", seen.len());
    }

    /// The FROM-less matrix is small enough to run whole.
    #[test]
    fn fromless_select_whole() {
        let mut ran = 0usize;
        for_each_combo([2; 10], |c| {
            if !mini_engine_ok(c) {
                return;
            }
            let sql = check_mini(c);
            engine_check(&sql, c);
            ran += 1;
        });
        eprintln!("engine FROM-less: {ran} configurations");
        assert_eq!(ran, 1_024 - 15 * 32); // locks × any of the 4 conflicting dims
    }

    /// All 288 INSERTs — OVERRIDING included, since PostgreSQL accepts it at
    /// PREPARE even on a non-identity column.
    #[test]
    fn insert_whole() {
        for_each_combo(I_RADIX, |c| {
            let sql = check_insert(c);
            engine_check(&sql, c);
        });
    }

    #[test]
    fn update_whole() {
        for_each_combo(U_RADIX, |c| {
            if !upd_grammar_ok(c) {
                return;
            }
            let sql = check_update(c);
            engine_check(&sql, c);
        });
    }

    #[test]
    fn delete_whole() {
        for_each_combo(D_RADIX, |c| {
            if !del_grammar_ok(c) {
                return;
            }
            let sql = check_delete(c);
            engine_check(&sql, c);
        });
    }

    /// Every enumerated join, chain and self-join, PREPAREd for real — the
    /// tier that proves USING/NATURAL resolve and the LATERAL references are
    /// legal where claimed.
    #[test]
    fn joins_whole() {
        for case in all_single_join_cases() {
            let (sql, _) = build_invariant(&build_join_case(case), join_case_args(case));
            engine_check(&sql, &case);
        }
        let links = all_links();
        let mut chains_ran = 0usize;
        for len in 1..=3usize {
            let mut idx = vec![0usize; len];
            loop {
                let chain: Vec<_> = idx.iter().map(|&i| links[i]).collect();
                if chain_engine_ok(&chain) {
                    let (sql, _) = build_invariant(&build_chain(&chain), 1);
                    engine_check(&sql, &chain);
                    chains_ran += 1;
                }
                let mut d = 0;
                loop {
                    if d == len {
                        break;
                    }
                    idx[d] += 1;
                    if idx[d] < links.len() {
                        break;
                    }
                    idx[d] = 0;
                    d += 1;
                }
                if d == len {
                    break;
                }
            }
        }
        // Σ over merge-prefix lengths: len 1 → 13, len 2 → 129, len 3 → 1 157.
        assert_eq!(chains_ran, 1_299);
        for (kind, cond) in all_links() {
            let (sql, _) = build_invariant(&build_self_join(kind, cond), 1);
            engine_check(&sql, &(kind, cond));
        }
    }
}
