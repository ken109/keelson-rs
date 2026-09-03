//! What the two analysers carry between them: a `FROM` item, and the answer
//! one expression yielded.
//!
//! The nullability and type rules are one catalogue — N1 to N16, written out
//! in [this module's parent](super) — applied by two walkers, because
//! PostgreSQL and SQLite publish parse trees with nothing in common. The
//! walkers cannot merge: the same rule reaches a different answer on each
//! engine (N10 and N11 yield `bool` on PostgreSQL and `i64` on SQLite, which
//! has no boolean type), so a shared walker would be a shared body of
//! per-dialect branches.
//!
//! What *can* be one thing is the vocabulary the rules are stated in.
//! [`Inferred`] especially: it was written out twice, identically, and a
//! field added to one copy — an inference the other engine also owes an
//! answer for — would have been a silent divergence between the two lanes
//! rather than a compile error in the second.
//!
//! `Scope` deliberately stays in each analyser. Both have one, with the same
//! four fields and methods of the same names, but the methods take that
//! engine's own parse tree: making it one type makes the two `impl` blocks
//! collide, which is the type system saying what is true — these are two
//! walkers answering similar questions, not one walker with two skins.

use crate::schema::TableDef;

/// One item the query's `FROM`/`JOIN` names.
#[derive(Debug)]
pub(crate) struct Source {
    /// The name the query refers to it by (alias if there is one).
    pub(crate) key: String,
    /// The introspected table, when the item is a real table.
    pub(crate) table: Option<TableDef>,
    /// Rule N2: a left-joined table's columns are nullable whatever the DDL
    /// says.
    pub(crate) outer: bool,
}

/// What one expression yielded.
#[derive(Debug, Clone)]
pub(crate) struct Inferred {
    /// `None` when the generator will not guess — the caller must annotate.
    pub(crate) rust_type: Option<String>,
    pub(crate) nullable: bool,
    /// True when [`nullable`](Self::nullable) is owed to an outer join only.
    pub(crate) outer_join: bool,
    /// The nullability with the outer join taken back out.
    pub(crate) inner_nullable: bool,
    /// Which rule in the catalogue decided, e.g. `"N13"`. It reaches the
    /// generated code's doc comments, so a reader can look up why a field is
    /// an `Option`.
    pub(crate) rule: &'static str,
    /// The output name the engine would give this expression with no alias.
    pub(crate) name: Option<String>,
}

impl Inferred {
    pub(crate) fn new(rust_type: Option<String>, nullable: bool, rule: &'static str) -> Inferred {
        Inferred {
            rust_type,
            nullable,
            outer_join: false,
            inner_nullable: nullable,
            rule,
            name: None,
        }
    }

    pub(crate) fn known(t: &str, nullable: bool, rule: &'static str) -> Inferred {
        Inferred::new(Some(t.to_owned()), nullable, rule)
    }

    pub(crate) fn unknown(rule: &'static str) -> Inferred {
        Inferred::new(None, true, rule)
    }

    pub(crate) fn named(mut self, name: &str) -> Inferred {
        self.name = Some(name.to_owned());
        self
    }
}
