//! Mods for [`psql::merge`](crate::merge()) (PostgreSQL 15+).
//!
//! [`into`] is the table being merged into; [`using`] is the data source —
//! a table, or a [`subquery`](crate::subquery) with an alias — and [`on`] is the
//! join condition between them. The `WHEN` clauses are chains, and the chain
//! type is what enforces the grammar's split: a matched arm
//! ([`when_matched`], [`when_not_matched_by_source`]) offers `UPDATE`/`DELETE`/
//! `DO NOTHING`, a not-matched arm ([`when_not_matched`]) offers `INSERT`/
//! `DO NOTHING`, and the wrong pairing does not compile.
//!
//! ```
//! use keelson_psql as psql;
//! use keelson_psql::{Chain, arg, merge, quote};
//!
//! let q = psql::merge((
//!     merge::into(quote("tags")).as_("t"),
//!     merge::using(quote("posts")).as_("p"),
//!     merge::on(quote(("t", "id")).eq(quote(("p", "id")))),
//!     merge::when_matched().then_update(merge::set_col("name").to(quote(("p", "title")))),
//!     merge::when_not_matched()
//!         .then_insert()
//!         .columns(["id", "name"])
//!         .values((quote(("p", "id")), quote(("p", "title")))),
//! ));
//! ```
//!
//! Three spellings are PostgreSQL 17+, and each says so where it stands:
//! [`when_not_matched_by_source`], [`NotMatchedChain::by_target`], and
//! [`returning`]. `recursive` is deliberately not re-exported — PostgreSQL
//! rejects `WITH RECURSIVE` on a `MERGE`.

use keelson_core::clause::Set;
use keelson_core::expr::{Expr, IntoExpr, IntoExprList};
use keelson_core::{Mod, mod_fn};

use crate::extras::Overriding;
use crate::statement::{MergeAction, MergeInsert, MergeMatchKind, MergeQuery, MergeWhen};

pub use crate::shared::{from_item as using, returning, set, set_col, target_table as into, with};

/// The `ON` join condition between target and source. Several calls are
/// `AND`-joined, as a join's `ON` conditions are.
pub fn on(condition: impl IntoExpr) -> impl Mod<MergeQuery> {
    let condition = condition.into_expr();
    mod_fn(move |q: &mut MergeQuery| q.on.push(condition))
}

/// `WHEN MATCHED …` — the arm taken when the source row found a target row.
///
/// Not a mod until an action is chosen, because `WHEN MATCHED` with no `THEN`
/// is not a clause: call [`then_update`](MatchedChain::then_update),
/// [`then_delete`](MatchedChain::then_delete) or
/// [`then_do_nothing`](MatchedChain::then_do_nothing).
pub fn when_matched() -> MatchedChain {
    MatchedChain {
        kind: MergeMatchKind::Matched,
        condition: Vec::new(),
    }
}

/// `WHEN NOT MATCHED BY SOURCE …` (PostgreSQL 17+) — the arm taken when a
/// *target* row has no source row. It acts on that target row, so its actions
/// are the matched ones: `UPDATE`, `DELETE`, `DO NOTHING`.
pub fn when_not_matched_by_source() -> MatchedChain {
    MatchedChain {
        kind: MergeMatchKind::NotMatchedBySource,
        condition: Vec::new(),
    }
}

/// `WHEN NOT MATCHED …` — the arm taken when the source row found no target
/// row. There is nothing to update or delete, so its actions are
/// [`then_insert`](NotMatchedChain::then_insert) and
/// [`then_do_nothing`](NotMatchedChain::then_do_nothing).
pub fn when_not_matched() -> NotMatchedChain {
    NotMatchedChain {
        by_target: false,
        condition: Vec::new(),
    }
}

/// A `WHEN MATCHED` / `WHEN NOT MATCHED BY SOURCE` clause under construction —
/// the arms that act on an existing target row.
#[derive(Debug, Clone)]
pub struct MatchedChain {
    kind: MergeMatchKind,
    condition: Vec<Expr>,
}

impl MatchedChain {
    /// `AND condition` — refine when this arm applies. Several calls are
    /// `AND`-joined.
    #[must_use]
    pub fn and(mut self, condition: impl IntoExpr) -> MatchedChain {
        self.condition.push(condition.into_expr());
        self
    }

    /// `THEN UPDATE SET …` — the body is built from [`set`]/[`set_col`] mods,
    /// exactly as an `UPDATE`'s or an upsert's assignment list is.
    pub fn then_update(self, body: impl Mod<Set>) -> MergeWhenMod {
        let mut set = Set::default();
        body.apply(&mut set);
        self.finish(MergeAction::Update(set))
    }

    /// `THEN DELETE`.
    pub fn then_delete(self) -> MergeWhenMod {
        self.finish(MergeAction::Delete)
    }

    /// `THEN DO NOTHING` — take this arm and do nothing, which is different
    /// from not having the arm: a row it captures is consumed by it.
    pub fn then_do_nothing(self) -> MergeWhenMod {
        self.finish(MergeAction::DoNothing)
    }

    fn finish(self, action: MergeAction) -> MergeWhenMod {
        MergeWhenMod {
            when: MergeWhen {
                kind: self.kind,
                condition: self.condition,
                action,
            },
        }
    }
}

/// A `WHEN NOT MATCHED [BY TARGET]` clause under construction — the arm with no
/// target row, whose only actions are `INSERT` and `DO NOTHING`.
#[derive(Debug, Clone)]
pub struct NotMatchedChain {
    by_target: bool,
    condition: Vec<Expr>,
}

impl NotMatchedChain {
    /// Spell out `BY TARGET` (PostgreSQL 17+). Same meaning as leaving it off;
    /// see `docs/sql-rendering.md` on optional spellings being written only on
    /// request.
    #[must_use]
    pub fn by_target(mut self) -> NotMatchedChain {
        self.by_target = true;
        self
    }

    /// `AND condition` — refine when this arm applies. Several calls are
    /// `AND`-joined.
    #[must_use]
    pub fn and(mut self, condition: impl IntoExpr) -> NotMatchedChain {
        self.condition.push(condition.into_expr());
        self
    }

    /// `THEN INSERT …` — already a complete mod meaning `INSERT DEFAULT
    /// VALUES`; [`columns`](MergeInsertChain::columns) and
    /// [`values`](MergeInsertChain::values) fill in the fuller forms.
    pub fn then_insert(self) -> MergeInsertChain {
        MergeInsertChain {
            when: self,
            insert: MergeInsert::default(),
        }
    }

    /// `THEN DO NOTHING`.
    pub fn then_do_nothing(self) -> MergeWhenMod {
        MergeWhenMod {
            when: MergeWhen {
                kind: MergeMatchKind::NotMatched {
                    by_target: self.by_target,
                },
                condition: self.condition,
                action: MergeAction::DoNothing,
            },
        }
    }
}

/// A `THEN INSERT` under construction. The `merge_insert` production takes one
/// row — not the multi-row list an `INSERT` statement does — and with no row it
/// is `INSERT DEFAULT VALUES`.
#[derive(Debug, Clone)]
pub struct MergeInsertChain {
    when: NotMatchedChain,
    insert: MergeInsert,
}

impl MergeInsertChain {
    /// The insert column list: `INSERT ("id", "name")`.
    #[must_use]
    pub fn columns(
        mut self,
        columns: impl IntoIterator<Item = impl Into<std::borrow::Cow<'static, str>>>,
    ) -> MergeInsertChain {
        self.insert.columns = columns.into_iter().map(Into::into).collect();
        self
    }

    /// `OVERRIDING SYSTEM VALUE` — as on an `INSERT` statement.
    #[must_use]
    pub fn overriding_system(mut self) -> MergeInsertChain {
        self.insert.overriding = Some(Overriding::System);
        self
    }

    /// `OVERRIDING USER VALUE` — as on an `INSERT` statement.
    #[must_use]
    pub fn overriding_user(mut self) -> MergeInsertChain {
        self.insert.overriding = Some(Overriding::User);
        self
    }

    /// The row: `VALUES ($1, $2)`. A cell may be `DEFAULT`, which is
    /// [`raw("DEFAULT")`](crate::raw). Replaces any previously set row — the
    /// production takes exactly one.
    #[must_use]
    pub fn values(mut self, row: impl IntoExprList) -> MergeInsertChain {
        self.insert.row = row.into_expr_list();
        self
    }
}

impl Mod<MergeQuery> for MergeInsertChain {
    fn apply(self, q: &mut MergeQuery) {
        q.whens.push(MergeWhen {
            kind: MergeMatchKind::NotMatched {
                by_target: self.when.by_target,
            },
            condition: self.when.condition,
            action: MergeAction::Insert(self.insert),
        });
    }
}

/// A finished `WHEN … THEN …` clause, ready to apply.
#[derive(Debug, Clone)]
pub struct MergeWhenMod {
    when: MergeWhen,
}

impl Mod<MergeQuery> for MergeWhenMod {
    fn apply(self, q: &mut MergeQuery) {
        q.whens.push(self.when);
    }
}
