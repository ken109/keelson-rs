//! Property-based tests of recursive nesting.
//!
//! Combinatorial coverage handles clause *presence*; it cannot reach the
//! recursive part of the grammar, where expressions and sub-queries nest without
//! bound. This file generates random query trees instead: binary operators,
//! function calls, `CASE`, `CAST`, scalar/`EXISTS`/`IN` sub-queries, derived
//! tables and raw fragments (`raw` and `template`), composed recursively under a
//! depth bound.
//!
//! # Why there is a typed AST between proptest and the builder
//!
//! Two of the judges resolve names and types. The engine tier `PREPARE`s each
//! statement against a real PostgreSQL (under `live-docker`), and analysis there
//! fails on `'x' + 1` just as surely as on a syntax error — so an untyped
//! generator would drown real findings in type noise. The AST here is typed
//! (int / text / bool), every table and column comes from `tests/schema/`, and
//! every bound argument that could otherwise be untypeable is wrapped in
//! `CAST(… AS …)` (PostgreSQL cannot type an operator whose every operand is a
//! placeholder). Bare `$n` still appears where context types it: against a
//! column, and in `LIMIT`/`OFFSET`.
//!
//! The AST is also what makes shrinking work: proptest shrinks the tree, and the
//! tree, not the SQL, is what a failing case prints.
//!
//! # The invariants (there is no expected string)
//!
//! 1. **The grammar accepts it** — libpg_query, always; and **the engine
//!    accepts it** when one is compiled in (`live-docker`).
//! 2. **The parse tree contains exactly the clauses asked for, and no
//!    others** — `pg_query`'s `SelectStmt` fields, checked per generated clause.
//! 3. **Placeholder integrity** — scanning the SQL left to right yields
//!    placeholders numbered exactly `1..=args.len()` in order, and the bound
//!    values arrive in exactly the order the leaves render. This is the target:
//!    a numbering bug still parses and still prepares — the wrong value simply
//!    binds to the wrong column. Every per-clause test starts at `$1`; only
//!    depth can catch it.
//! 4. **Determinism** — building twice, and building a clone, both reproduce
//!    the same SQL and the same arguments.
//!
//! # The promotion convention
//!
//! Any failing case the generator finds gets **promoted into a hand-written
//! regression test** in `mod promoted` at the bottom of this file, with its
//! expected string derived from the grammar (cite the production), never from
//! the builder. The proptest shrinker gives the minimal tree; the promoted test
//! pins it forever, so the property does not have to re-find it by luck.

use keelson_psql as psql;
use keelson_psql::{
    Chain, Expr, IntoExpr, Query, RawArg, case_, cast, f, not, quote, raw, s, select, subquery,
    template,
};
use keelson_sqlcheck::property::{
    BoolExpr, CmpOp, ColExpr, CountExpr, Ctx, FromAst, IntExpr, QueryAst, Scope, TEXT_LITERALS,
    TextExpr, from_strat, levels, scope_of, table,
};
use keelson_sqlcheck::{Dialect, live};
use proptest::prelude::*;
use proptest::test_runner::FileFailurePersistence;

// ===========================================================================
// The shared schema, as data the generator can draw from
// ===========================================================================

// ===========================================================================
// The typed expression AST the strategies generate
// ===========================================================================

// ===========================================================================
// AST → builder conversion, recording the expected argument order
// ===========================================================================

fn int_expr(a: &IntExpr, sc: &Scope, ctx: &mut Ctx) -> Expr {
    match a {
        IntExpr::Col(i) => sc.int_col(*i),
        IntExpr::Lit(n) => raw(n.to_string()),
        IntExpr::Arg => cast(ctx.int_arg(), "int"),
        IntExpr::Add(l, r) => int_expr(l, sc, ctx).plus(int_expr(r, sc, ctx)),
        IntExpr::Sub(l, r) => int_expr(l, sc, ctx).minus(int_expr(r, sc, ctx)),
        IntExpr::Abs(e) => f("abs", int_expr(e, sc, ctx)).into_expr(),
        IntExpr::Coalesce(l, r) => {
            f("coalesce", (int_expr(l, sc, ctx), int_expr(r, sc, ctx))).into_expr()
        }
        IntExpr::Length(t) => f("length", text_expr(t, sc, ctx)).into_expr(),
        IntExpr::Case(c, t, e) => {
            let c = bool_expr(c, sc, ctx);
            let t = int_expr(t, sc, ctx);
            case_().when(c, t).else_(int_expr(e, sc, ctx))
        }
        IntExpr::Template(inner) => {
            let v = ctx.int_raw_arg();
            let e = int_expr(inner, sc, ctx);
            template("(COALESCE(?, 0) + ?)", [v, RawArg::expr(e)])
        }
        IntExpr::CountSub(t, w) => {
            let t = table(*t);
            let sub_sc = Scope::of_table(t);
            let mut q = psql::select((
                select::columns(f("count", "*")),
                select::from(quote(t.name)),
            ));
            if let Some(w) = w {
                q.apply(select::where_(bool_expr(w, &sub_sc, ctx)));
            }
            subquery(q)
        }
    }
}

fn text_expr(a: &TextExpr, sc: &Scope, ctx: &mut Ctx) -> Expr {
    match a {
        TextExpr::Col(i) => sc
            .text_col(*i)
            .unwrap_or_else(|| s(TEXT_LITERALS[*i as usize % TEXT_LITERALS.len()])),
        TextExpr::Lit(i) => s(TEXT_LITERALS[*i as usize % TEXT_LITERALS.len()]),
        TextExpr::Arg => cast(ctx.text_arg(), "text"),
        TextExpr::Concat(l, r) => text_expr(l, sc, ctx).concat(text_expr(r, sc, ctx)),
        TextExpr::Lower(e) => f("lower", text_expr(e, sc, ctx)).into_expr(),
        TextExpr::Upper(e) => f("upper", text_expr(e, sc, ctx)).into_expr(),
        TextExpr::Coalesce(l, r) => {
            f("coalesce", (text_expr(l, sc, ctx), text_expr(r, sc, ctx))).into_expr()
        }
        TextExpr::Case(c, t, e) => {
            let c = bool_expr(c, sc, ctx);
            let t = text_expr(t, sc, ctx);
            case_().when(c, t).else_(text_expr(e, sc, ctx))
        }
    }
}

fn bool_expr(a: &BoolExpr, sc: &Scope, ctx: &mut Ctx) -> Expr {
    match a {
        BoolExpr::Leaf(i) => sc
            .bool_col(*i)
            .unwrap_or_else(|| raw(if i % 2 == 0 { "TRUE" } else { "FALSE" })),
        BoolExpr::Cmp(op, l, r) => {
            let le = int_expr(l, sc, ctx);
            let re = int_expr(r, sc, ctx);
            match op {
                CmpOp::Eq => le.eq(re),
                CmpOp::Ne => le.ne(re),
                CmpOp::Lt => le.lt(re),
                CmpOp::Lte => le.lte(re),
                CmpOp::Gt => le.gt(re),
                CmpOp::Gte => le.gte(re),
            }
        }
        BoolExpr::TextEq(l, r) => text_expr(l, sc, ctx).eq(text_expr(r, sc, ctx)),
        BoolExpr::Like(l, r) => text_expr(l, sc, ctx).like(text_expr(r, sc, ctx)),
        BoolExpr::ColCmpArg(i) => sc.int_col(*i).gt(ctx.int_arg()),
        BoolExpr::InList(i, items) => {
            let lhs = sc.int_col(*i);
            let items: Vec<Expr> = items.iter().map(|e| int_expr(e, sc, ctx)).collect();
            lhs.in_(items)
        }
        BoolExpr::InSub {
            col,
            tab,
            inner_col,
            filter,
        } => {
            let lhs = sc.int_col(*col);
            let t = table(*tab);
            let sub_sc = Scope::of_table(t);
            let mut q = psql::select((
                select::columns(sub_sc.int_col(*inner_col)),
                select::from(quote(t.name)),
            ));
            if let Some(w) = filter {
                q.apply(select::where_(bool_expr(w, &sub_sc, ctx)));
            }
            lhs.in_(psql::query(q))
        }
        BoolExpr::Exists(tab, w) => {
            let t = table(*tab);
            let sub_sc = Scope::of_table(t);
            let q = psql::select((
                select::columns(raw("1")),
                select::from(quote(t.name)),
                select::where_(bool_expr(w, &sub_sc, ctx)),
            ));
            Expr::prefix("EXISTS", subquery(q))
        }
        BoolExpr::And(l, r) => bool_expr(l, sc, ctx).and(bool_expr(r, sc, ctx)),
        BoolExpr::Or(l, r) => bool_expr(l, sc, ctx).or(bool_expr(r, sc, ctx)),
        BoolExpr::Not(e) => not(bool_expr(e, sc, ctx)),
        BoolExpr::IsNull(e) => int_expr(e, sc, ctx).is_null(),
        BoolExpr::Between(e, a, b) => {
            let e = int_expr(e, sc, ctx);
            let a = int_expr(a, sc, ctx);
            e.between(a, int_expr(b, sc, ctx))
        }
    }
}

fn count_expr(a: &CountExpr, ctx: &mut Ctx) -> Expr {
    match a {
        CountExpr::Lit(n) => raw(n.to_string()),
        // Bare on purpose: `LIMIT $n` / `OFFSET $n` are typed by the clause.
        CountExpr::Arg => ctx.int_arg(),
    }
}

fn apply_from(q: &mut psql::SelectQuery, from: &FromAst, depth: usize, ctx: &mut Ctx) {
    match from {
        FromAst::Table(i) => q.apply(select::from(quote(table(*i).name))),
        FromAst::Derived(inner, filter) => {
            // `SELECT * FROM inner [WHERE …]` — the default select list is `*`,
            // so the derived table passes the base table's columns through and
            // the outer scope's typing stays exact.
            let inner_scope = scope_of(inner, depth + 1);
            let mut sub = psql::SelectQuery::default();
            apply_from(&mut sub, inner, depth + 1, ctx);
            if let Some(w) = filter {
                sub.apply(select::where_(bool_expr(w, &inner_scope, ctx)));
            }
            q.apply(select::from(subquery(sub)).as_(format!("d{depth}")));
        }
    }
}

/// Build the `SelectQuery`, converting clause contents in render order so that
/// `ctx.expected` ends up in placeholder order.
fn build_select(ast: &QueryAst, ctx: &mut Ctx) -> psql::SelectQuery {
    let scope = scope_of(&ast.from, 0);
    let mut q = psql::SelectQuery::default();

    for c in &ast.cols {
        let e = match c {
            ColExpr::I(i) => int_expr(i, &scope, ctx),
            ColExpr::T(t) => text_expr(t, &scope, ctx),
        };
        q.apply(select::columns(e));
    }

    apply_from(&mut q, &ast.from, 0, ctx);

    if let Some(w) = &ast.where_ {
        q.apply(select::where_(bool_expr(w, &scope, ctx)));
    }
    if let Some((e, desc)) = &ast.order {
        // `0 + e` rather than `e`: a bare integer literal in ORDER BY is a
        // *positional* reference (PostgreSQL 17, SELECT, sort_expression), and
        // parentheses do not shield it — the analyser sees the same A_Const.
        // Folding it into an operator expression keeps it an expression.
        let e = raw("0").plus(int_expr(e, &scope, ctx));
        let chain = select::order_by(e);
        q.apply(if *desc { chain.desc() } else { chain.asc() });
    }
    if let Some(l) = &ast.limit {
        q.apply(select::limit(count_expr(l, ctx)));
    }
    if let Some(o) = &ast.offset {
        q.apply(select::offset(count_expr(o, ctx)));
    }
    q
}

// ===========================================================================
// Strategies — built bottom-up per depth so the three mutually recursive
// types share sub-strategies instead of exploding combinatorially
// ===========================================================================

fn query_strat(depth: usize) -> BoxedStrategy<QueryAst> {
    let lv = levels(depth);
    let (i, t, b) = (
        lv.int[depth].clone(),
        lv.text[depth].clone(),
        lv.bool_[depth].clone(),
    );
    let col = prop_oneof![
        2 => i.clone().prop_map(ColExpr::I),
        1 => t.prop_map(ColExpr::T),
    ];
    let count = prop_oneof![(0u8..50).prop_map(CountExpr::Lit), Just(CountExpr::Arg),];
    (
        from_strat(b.clone()),
        proptest::collection::vec(col, 1..=3),
        proptest::option::of(b),
        proptest::option::of((i, any::<bool>())),
        proptest::option::of(count.clone()),
        proptest::option::of(count),
    )
        .prop_map(|(from, cols, where_, order, limit, offset)| QueryAst {
            from,
            cols,
            where_,
            order,
            limit,
            offset,
        })
        .boxed()
}

// ===========================================================================
// The invariants
// ===========================================================================

/// Every `$n` in the SQL, in order of appearance. Nothing else in the
/// generated statements contains a `$` — literals are drawn from a fixed
/// dollar-free set — so a plain scan is exact.
fn placeholder_sequence(sql: &str) -> Vec<usize> {
    let bytes = sql.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            if end > start {
                out.push(sql[start..end].parse().expect("digits"));
            }
            i = end;
        } else {
            i += 1;
        }
    }
    out
}

/// The top-level `SelectStmt` of the parse tree, out of libpg_query's protobuf.
fn parse_select(sql: &str) -> Result<pg_query::protobuf::SelectStmt, TestCaseError> {
    let parsed = pg_query::parse(sql)
        .map_err(|e| TestCaseError::fail(format!("pg_query rejected: {e}\nsql: {sql}")))?;
    let node = parsed
        .protobuf
        .stmts
        .first()
        .and_then(|raw| raw.stmt.as_ref())
        .and_then(|stmt| stmt.node.as_ref());
    match node {
        Some(pg_query::NodeEnum::SelectStmt(s)) => Ok((**s).clone()),
        other => Err(TestCaseError::fail(format!(
            "expected one SelectStmt, parse tree held {other:?}\nsql: {sql}"
        ))),
    }
}

/// Run every invariant against one generated tree.
fn check_ast(ast: &QueryAst) -> Result<(), TestCaseError> {
    let mut ctx = Ctx::default();
    let q = build_select(ast, &mut ctx);

    let (sql, args) = q
        .build()
        .map_err(|e| TestCaseError::fail(format!("build() failed: {e}\nast: {ast:?}")))?;

    // Invariant 1a — the grammar accepts it.
    if let Err(e) = keelson_sqlcheck::check(Dialect::Psql, &sql) {
        return Err(TestCaseError::fail(format!(
            "libpg_query rejected the generated SQL\n  error: {e}\n  sql:   {sql}\n  ast:   {ast:?}"
        )));
    }

    // Invariant 1b — a real engine accepts it, when one is compiled in.
    // (`PREPARE` against PostgreSQL 17 under `live-docker`; a panic here is
    // caught by proptest and shrunk like any other failure.)
    if live::available().contains(&Dialect::Psql) {
        live::assert_valid(Dialect::Psql, &sql);
    }

    // Invariant 2 — the parse tree contains exactly the clauses asked for,
    // and no others. Field names are pg_query 6's `protobuf::SelectStmt`.
    let stmt = parse_select(&sql)?;
    prop_assert_eq!(
        stmt.target_list.len(),
        ast.cols.len(),
        "select-list arity differs\nsql: {}",
        sql
    );
    prop_assert_eq!(stmt.from_clause.len(), 1, "one FROM item\nsql: {}", sql);
    prop_assert_eq!(
        stmt.where_clause.is_some(),
        ast.where_.is_some(),
        "WHERE presence differs\nsql: {}",
        sql
    );
    prop_assert_eq!(
        stmt.sort_clause.len(),
        usize::from(ast.order.is_some()),
        "ORDER BY presence differs\nsql: {}",
        sql
    );
    prop_assert_eq!(
        stmt.limit_count.is_some(),
        ast.limit.is_some(),
        "LIMIT presence differs\nsql: {}",
        sql
    );
    prop_assert_eq!(
        stmt.limit_offset.is_some(),
        ast.offset.is_some(),
        "OFFSET presence differs\nsql: {}",
        sql
    );
    prop_assert!(
        stmt.group_clause.is_empty()
            && stmt.having_clause.is_none()
            && stmt.window_clause.is_empty()
            && stmt.with_clause.is_none()
            && stmt.locking_clause.is_empty()
            && stmt.distinct_clause.is_empty()
            && stmt.larg.is_none()
            && stmt.rarg.is_none(),
        "a clause that was never asked for appeared\nsql: {}",
        sql
    );

    // Invariant 3 — placeholder integrity. The numbering must be exactly
    // 1..=n in emission order, and the values must bind in the order the
    // leaves rendered. This is the silent-failure class: everything above
    // still passes when $4 and $5 swap.
    let seq = placeholder_sequence(&sql);
    let want: Vec<usize> = (1..=args.len()).collect();
    prop_assert_eq!(
        seq,
        want,
        "placeholders are not 1..=n in emission order\nsql: {}",
        sql
    );
    prop_assert_eq!(
        &args,
        &ctx.expected,
        "arguments out of order against the render walk\nsql: {}",
        sql
    );

    // Invariant 4 — determinism: a second build and a clone's build both
    // reproduce the same SQL and arguments.
    let (sql2, args2) = q.build().expect("second build");
    prop_assert_eq!(&sql, &sql2, "two builds differ");
    prop_assert_eq!(&args, &args2, "two builds bind differently");
    let clone = q.clone();
    let (sql3, args3) = clone.build().expect("clone build");
    prop_assert_eq!(&sql, &sql3, "a clone renders differently");
    prop_assert_eq!(&args, &args3, "a clone binds differently");

    Ok(())
}

// ===========================================================================
// The properties
// ===========================================================================

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        // Failures are promoted into `mod promoted` by hand (see the module
        // doc), not replayed from a lossy seed file in the tree.
        failure_persistence: Some(Box::new(FileFailurePersistence::Off)),
        .. ProptestConfig::default()
    })]

    /// The whole statement: random clause subset, every clause holding a
    /// random typed tree, sub-queries in the select list, `FROM`, and `WHERE`.
    #[test]
    fn a_generated_query_tree_holds_every_invariant(ast in query_strat(3)) {
        check_ast(&ast)?;
    }

    /// Depth over breadth: one predicate, two levels deeper than the statement
    /// property reaches, in the clause where expression nesting is richest.
    #[test]
    fn a_deeply_nested_predicate_keeps_its_placeholders_in_order(
        w in levels(5).bool_.pop().unwrap(),
        tab in any::<u8>(),
        limit in proptest::option::of(Just(CountExpr::Arg)),
    ) {
        let ast = QueryAst {
            from: FromAst::Table(tab),
            cols: vec![ColExpr::I(IntExpr::Col(0))],
            where_: Some(w),
            order: None,
            // A trailing bare `LIMIT $n` catches an inner counter that forgot
            // to advance the outer one.
            limit,
            offset: None,
        };
        check_ast(&ast)?;
    }
}

/// Failing cases found by the generator, promoted to hand-written regression
/// tests with expectations **derived from the grammar** (never from the
/// builder). None yet: the properties above have not caught the builder out.
/// When one does, the shrunken tree from the proptest report gets rebuilt here
/// explicitly, its expected SQL written from the cited production, and the
/// test named after the bug.
mod promoted {}
