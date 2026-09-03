//! The random-query generator the property lanes share.
//!
//! Tier C is one lane per dialect, and each asks a different question of the
//! same random nesting: PostgreSQL checks `$n` numbering against a real parse
//! tree, SQLite checks `?n` against a real in-process engine, MySQL checks
//! that the bound values arrive in render order — the invariant a bare `?`
//! makes invisible everywhere else.
//!
//! What is *not* different is the nesting. The typed AST below, the tables it
//! draws columns from, and the strategies that grow it were written out three
//! times, identically — some three hundred lines each, arrived at by copying.
//! Copies drift silently here: nothing fails when one lane stops generating a
//! shape, it just quietly stops looking. (One had already drifted in a small
//! way: the MySQL lane's `IntExpr::Arg` was documented as "SQLite types a
//! parameter from its value".)
//!
//! # What a lane still owns
//!
//! Everything about *rendering*. A lane turns this AST into its own dialect's
//! query — `int_expr`, `text_expr`, `bool_expr`, `build_select` — and states
//! its own invariants. Where a lane's grammar makes a shape unrepresentable
//! it narrows the strategy itself: SQLite's `OFFSET` is part of the
//! limit-clause, so its `query_strat` cannot generate an offset without a
//! limit, and a refusal there is correct behaviour rather than a
//! counterexample.
//!
//! The AST is therefore the union of shapes *some* dialect can render, and
//! carries no dialect's syntax: `BoolExpr::Cmp` is a comparison, not `>` and
//! not `$n`.

use keelson_core::Value;
use keelson_core::expr::{Expr, RawArg, arg, quote};
use proptest::prelude::*;

/// One table of the shared test schema (`tests/schema/`), split by column type so the generator
/// can only ever produce well-typed references. Every table has at least one
/// int column; text and bool columns may be absent and the generator degrades
/// deterministically when they are.
#[derive(Clone, Copy, Debug)]
pub struct Table {
    pub name: &'static str,
    pub int_cols: &'static [&'static str],
    pub text_cols: &'static [&'static str],
    pub bool_cols: &'static [&'static str],
}

pub const TABLES: [Table; 5] = [
    Table {
        name: "users",
        int_cols: &["id", "age"],
        text_cols: &["name", "email"],
        bool_cols: &["is_active"],
    },
    Table {
        name: "posts",
        int_cols: &["id", "user_id", "views"],
        text_cols: &["title", "status"],
        bool_cols: &[],
    },
    Table {
        name: "comments",
        int_cols: &["id", "post_id", "user_id"],
        text_cols: &["body"],
        bool_cols: &[],
    },
    Table {
        name: "tags",
        int_cols: &["id"],
        text_cols: &["name"],
        bool_cols: &[],
    },
    Table {
        name: "post_tags",
        int_cols: &["post_id", "tag_id"],
        text_cols: &[],
        bool_cols: &[],
    },
];

pub fn table(ix: u8) -> Table {
    TABLES[ix as usize % TABLES.len()]
}

pub const TEXT_LITERALS: [&str; 4] = ["alpha", "beta", "gamma", "delta"];

/// What column references resolve against at one query level: a relation name
/// to qualify with (a table's own name, or a derived table's alias) and the
/// columns it exposes. Sub-queries switch scope to their own table and never
/// correlate, so every reference is resolvable by construction.
#[derive(Clone, Debug)]
pub struct Scope {
    pub rel: String,
    pub cols: Table,
}

impl Scope {
    pub fn of_table(t: Table) -> Scope {
        Scope {
            rel: t.name.to_string(),
            cols: t,
        }
    }

    pub fn int_col(&self, ix: u8) -> Expr {
        let cols = self.cols.int_cols;
        quote((self.rel.clone(), cols[ix as usize % cols.len()]))
    }

    /// `None` when the scope's table has no text column (post_tags).
    pub fn text_col(&self, ix: u8) -> Option<Expr> {
        let cols = self.cols.text_cols;
        if cols.is_empty() {
            return None;
        }
        Some(quote((self.rel.clone(), cols[ix as usize % cols.len()])))
    }

    pub fn bool_col(&self, ix: u8) -> Option<Expr> {
        let cols = self.cols.bool_cols;
        if cols.is_empty() {
            return None;
        }
        Some(quote((self.rel.clone(), cols[ix as usize % cols.len()])))
    }
}

/// Column indices are abstract (`u8` reduced modulo the columns available in
/// scope at conversion time) so one generated tree is valid under every scope —
/// which is also what lets the same sub-tree recur under a different table.
#[derive(Clone, Debug)]
pub enum IntExpr {
    /// An int column of the scope.
    Col(u8),
    /// A small non-negative literal, written as a raw fragment.
    Lit(u8),
    /// A bound argument. How it is written is the lane's business.
    Arg,
    Add(Box<IntExpr>, Box<IntExpr>),
    Sub(Box<IntExpr>, Box<IntExpr>),
    /// `abs(e)`.
    Abs(Box<IntExpr>),
    /// `coalesce(a, b)`.
    Coalesce(Box<IntExpr>, Box<IntExpr>),
    /// `length(t)` — the int view of a text expression.
    Length(Box<TextExpr>),
    /// `CASE WHEN c THEN a ELSE b END`.
    Case(Box<BoolExpr>, Box<IntExpr>, Box<IntExpr>),
    /// `template("(COALESCE(?, 0) + ?)", …)` — a raw fragment whose first `?`
    /// binds a value and whose second splices a nested expression, so the
    /// template rewriter participates in cross-level numbering.
    Template(Box<IntExpr>),
    /// A scalar sub-query: `(SELECT count(*) FROM t [WHERE c])`.
    CountSub(u8, Option<Box<BoolExpr>>),
}

#[derive(Clone, Debug)]
pub enum TextExpr {
    /// A text column of the scope; degrades to a literal where none exists.
    Col(u8),
    /// One of a fixed set of harmless literals (no quotes, no `$`).
    Lit(u8),
    /// A bound argument. How it is written is the lane's business.
    Arg,
    /// Concatenation, however the lane spells it — `||` on PostgreSQL and
    /// SQLite, `CONCAT(a, b)` on MySQL, which reads `||` as logical OR.
    Concat(Box<TextExpr>, Box<TextExpr>),
    Lower(Box<TextExpr>),
    Upper(Box<TextExpr>),
    Coalesce(Box<TextExpr>, Box<TextExpr>),
    Case(Box<BoolExpr>, Box<TextExpr>, Box<TextExpr>),
}

#[derive(Clone, Copy, Debug)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Lte,
    Gt,
    Gte,
}

#[derive(Clone, Debug)]
pub enum BoolExpr {
    /// A bool column of the scope; `TRUE`/`FALSE` as a raw fragment where the
    /// table has none.
    Leaf(u8),
    Cmp(CmpOp, Box<IntExpr>, Box<IntExpr>),
    TextEq(Box<TextExpr>, Box<TextExpr>),
    Like(Box<TextExpr>, Box<TextExpr>),
    /// `col > <arg>` — the one place a bare placeholder is generated,
    /// because the column types it.
    ColCmpArg(u8),
    /// `"col" IN (e, …)`, one to three elements.
    InList(u8, Vec<IntExpr>),
    /// `"col" IN (SELECT "c" FROM t [WHERE …])`.
    InSub {
        col: u8,
        tab: u8,
        inner_col: u8,
        filter: Option<Box<BoolExpr>>,
    },
    /// `EXISTS (SELECT 1 FROM t WHERE …)`.
    Exists(u8, Box<BoolExpr>),
    And(Box<BoolExpr>, Box<BoolExpr>),
    Or(Box<BoolExpr>, Box<BoolExpr>),
    Not(Box<BoolExpr>),
    IsNull(Box<IntExpr>),
    Between(Box<IntExpr>, Box<IntExpr>, Box<IntExpr>),
}

/// A `FROM` item: a base table, or a derived table
/// `(SELECT * FROM inner [WHERE …]) AS "dN"` — recursion through `FROM` itself.
#[derive(Clone, Debug)]
pub enum FromAst {
    Table(u8),
    Derived(Box<FromAst>, Option<Box<BoolExpr>>),
}

#[derive(Clone, Debug)]
pub enum ColExpr {
    I(IntExpr),
    T(TextExpr),
}

/// A `LIMIT`/`OFFSET` count: a literal, or a bare `$n` (the clause types it).
#[derive(Clone, Debug)]
pub enum CountExpr {
    Lit(u8),
    Arg,
}

/// The generated statement. Clause fields mirror `SelectQuery`'s render order,
/// which is what invariant 2 checks against the parse tree.
#[derive(Clone, Debug)]
pub struct QueryAst {
    pub from: FromAst,
    pub cols: Vec<ColExpr>,
    pub where_: Option<BoolExpr>,
    pub order: Option<(IntExpr, bool)>,
    pub limit: Option<CountExpr>,
    pub offset: Option<CountExpr>,
}

/// Conversion walks the AST in exactly the order the writer will render it —
/// select list, `FROM` (with everything inside a derived table), `WHERE`,
/// `ORDER BY`, `LIMIT`, `OFFSET`, and left-to-right within every expression —
/// handing each argument leaf a fresh sentinel value as it goes. The produced
/// `args` must then equal `expected` exactly: that is the order-of-binding
/// half of placeholder integrity, which `$n` counting alone cannot see.
#[derive(Debug, Default)]
pub struct Ctx {
    pub next: i32,
    pub expected: Vec<Value>,
}

impl Ctx {
    pub fn int_arg(&mut self) -> Expr {
        let v = self.next;
        self.next += 1;
        self.expected.push(Value::I32(v));
        arg(v)
    }

    pub fn int_raw_arg(&mut self) -> RawArg {
        let v = self.next;
        self.next += 1;
        self.expected.push(Value::I32(v));
        RawArg::value(v)
    }

    pub fn text_arg(&mut self) -> Expr {
        let v = format!("s{}", self.next);
        self.next += 1;
        self.expected.push(Value::Text(v.clone()));
        arg(v)
    }
}

/// The scope a `FROM` item exposes to the query around it. Derived aliases are
/// named by nesting depth (`d0`, `d1`, …), which the conversion below mirrors,
/// so this stays a pure function.
pub fn scope_of(from: &FromAst, depth: usize) -> Scope {
    match from {
        FromAst::Table(i) => Scope::of_table(table(*i)),
        FromAst::Derived(inner, _) => Scope {
            rel: format!("d{depth}"),
            cols: scope_of(inner, depth + 1).cols,
        },
    }
}

pub fn leaf_int() -> BoxedStrategy<IntExpr> {
    prop_oneof![
        any::<u8>().prop_map(IntExpr::Col),
        (0u8..100).prop_map(IntExpr::Lit),
        Just(IntExpr::Arg),
    ]
    .boxed()
}

pub fn leaf_text() -> BoxedStrategy<TextExpr> {
    prop_oneof![
        any::<u8>().prop_map(TextExpr::Col),
        any::<u8>().prop_map(TextExpr::Lit),
        Just(TextExpr::Arg),
    ]
    .boxed()
}

pub fn leaf_bool() -> BoxedStrategy<BoolExpr> {
    prop_oneof![
        any::<u8>().prop_map(BoolExpr::Leaf),
        any::<u8>().prop_map(BoolExpr::ColCmpArg),
    ]
    .boxed()
}

#[derive(Debug)]
pub struct Levels {
    pub int: Vec<BoxedStrategy<IntExpr>>,
    pub text: Vec<BoxedStrategy<TextExpr>>,
    pub bool_: Vec<BoxedStrategy<BoolExpr>>,
}

pub fn levels(depth: usize) -> Levels {
    let mut l = Levels {
        int: vec![leaf_int()],
        text: vec![leaf_text()],
        bool_: vec![leaf_bool()],
    };
    for d in 1..=depth {
        let (i, t, b) = (
            l.int[d - 1].clone(),
            l.text[d - 1].clone(),
            l.bool_[d - 1].clone(),
        );

        let int = prop_oneof![
            3 => leaf_int(),
            2 => (i.clone(), i.clone())
                .prop_map(|(a, b)| IntExpr::Add(Box::new(a), Box::new(b))),
            1 => (i.clone(), i.clone())
                .prop_map(|(a, b)| IntExpr::Sub(Box::new(a), Box::new(b))),
            1 => i.clone().prop_map(|e| IntExpr::Abs(Box::new(e))),
            1 => (i.clone(), i.clone())
                .prop_map(|(a, b)| IntExpr::Coalesce(Box::new(a), Box::new(b))),
            1 => t.clone().prop_map(|e| IntExpr::Length(Box::new(e))),
            1 => (b.clone(), i.clone(), i.clone())
                .prop_map(|(c, a, e)| IntExpr::Case(Box::new(c), Box::new(a), Box::new(e))),
            1 => i.clone().prop_map(|e| IntExpr::Template(Box::new(e))),
            1 => (any::<u8>(), proptest::option::of(b.clone().prop_map(Box::new)))
                .prop_map(|(t, w)| IntExpr::CountSub(t, w)),
        ]
        .boxed();

        let text = prop_oneof![
            3 => leaf_text(),
            2 => (t.clone(), t.clone())
                .prop_map(|(a, b)| TextExpr::Concat(Box::new(a), Box::new(b))),
            1 => t.clone().prop_map(|e| TextExpr::Lower(Box::new(e))),
            1 => t.clone().prop_map(|e| TextExpr::Upper(Box::new(e))),
            1 => (t.clone(), t.clone())
                .prop_map(|(a, b)| TextExpr::Coalesce(Box::new(a), Box::new(b))),
            1 => (b.clone(), t.clone(), t.clone())
                .prop_map(|(c, a, e)| TextExpr::Case(Box::new(c), Box::new(a), Box::new(e))),
        ]
        .boxed();

        let cmp_op = prop_oneof![
            Just(CmpOp::Eq),
            Just(CmpOp::Ne),
            Just(CmpOp::Lt),
            Just(CmpOp::Lte),
            Just(CmpOp::Gt),
            Just(CmpOp::Gte),
        ];
        let bool_ = prop_oneof![
            2 => leaf_bool(),
            3 => (cmp_op, i.clone(), i.clone())
                .prop_map(|(op, a, b)| BoolExpr::Cmp(op, Box::new(a), Box::new(b))),
            1 => (t.clone(), t.clone())
                .prop_map(|(a, b)| BoolExpr::TextEq(Box::new(a), Box::new(b))),
            1 => (t.clone(), t.clone())
                .prop_map(|(a, b)| BoolExpr::Like(Box::new(a), Box::new(b))),
            1 => (any::<u8>(), proptest::collection::vec(i.clone(), 1..=3))
                .prop_map(|(c, items)| BoolExpr::InList(c, items)),
            1 => (
                any::<u8>(),
                any::<u8>(),
                any::<u8>(),
                proptest::option::of(b.clone().prop_map(Box::new)),
            )
                .prop_map(|(col, tab, inner_col, filter)| BoolExpr::InSub {
                    col,
                    tab,
                    inner_col,
                    filter,
                }),
            1 => (any::<u8>(), b.clone())
                .prop_map(|(t, w)| BoolExpr::Exists(t, Box::new(w))),
            2 => (b.clone(), b.clone())
                .prop_map(|(a, c)| BoolExpr::And(Box::new(a), Box::new(c))),
            1 => (b.clone(), b.clone())
                .prop_map(|(a, c)| BoolExpr::Or(Box::new(a), Box::new(c))),
            1 => b.clone().prop_map(|e| BoolExpr::Not(Box::new(e))),
            1 => i.clone().prop_map(|e| BoolExpr::IsNull(Box::new(e))),
            1 => (i.clone(), i.clone(), i.clone())
                .prop_map(|(e, a, c)| BoolExpr::Between(Box::new(e), Box::new(a), Box::new(c))),
        ]
        .boxed();

        l.int.push(int);
        l.text.push(text);
        l.bool_.push(bool_);
    }
    l
}

/// A `FROM` item nested up to two derived tables deep.
pub fn from_strat(b: BoxedStrategy<BoolExpr>) -> BoxedStrategy<FromAst> {
    let leaf = any::<u8>().prop_map(FromAst::Table).boxed();
    let mut level = leaf.clone();
    for _ in 0..2 {
        level = prop_oneof![
            2 => leaf.clone(),
            1 => (level, proptest::option::of(b.clone().prop_map(Box::new)))
                .prop_map(|(f, w)| FromAst::Derived(Box::new(f), w)),
        ]
        .boxed();
    }
    level
}
