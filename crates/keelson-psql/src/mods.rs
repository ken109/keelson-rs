//! The mods that are more than one setter.
//!
//! bob spells these as function types with methods — `FromChain`, `JoinChain`,
//! `CTEChain`, `LockChain`, `OrderBy` — so that `sm.From("users").As("u")` and
//! `sm.LeftJoin("t").Using("id")` read as one phrase. Here they are structs that
//! collect a clause and are themselves [`Mod`]s, so the builder methods return
//! `Self` and nothing has to be finalised.
//!
//! They are shared across statement kinds through the `Has*` traits, which is
//! why `sm` and (later) `um` / `dm` can hand back the same types.

use keelson_core::clause::{
    CROSS_JOIN, Cte, CteSearch, HasJoins, HasLocks, HasOrderBy, HasTableRef, HasWith, Join, Lock,
    OrderBy, OrderDef, SEARCH_BREADTH, SEARCH_DEPTH, TableRef,
};
use keelson_core::expr::ExprBuilder;
use keelson_core::{DynExpr, Expression, Mod, dyn_expr};

use crate::expr::Expr;
use crate::into_expr::{Exprs, IntoExpr, Names};
use crate::query::Query;
use crate::select::HasCombinedOrder;

/// The `FROM` item, with the modifiers PostgreSQL allows on one.
#[derive(Debug, Clone, Default)]
pub struct FromMod {
    table: TableRef,
}

impl FromMod {
    /// A `FROM` item over an already-erased expression.
    pub fn new(table: DynExpr) -> Self {
        FromMod {
            table: TableRef::new(table),
        }
    }

    /// `… AS "alias"`.
    pub fn as_(mut self, alias: impl Into<String>) -> Self {
        self.table.alias = alias.into();
        self
    }

    /// `… AS "alias" ("a", "b")` — the column aliases go with an alias.
    pub fn columns(mut self, columns: impl Names) -> Self {
        self.table.columns = columns.into_names();
        self
    }

    /// `FROM ONLY table`: do not descend into inheriting tables.
    pub fn only(mut self) -> Self {
        self.table.only = true;
        self
    }

    /// `FROM LATERAL …`: let the item see columns of earlier items.
    pub fn lateral(mut self) -> Self {
        self.table.lateral = true;
        self
    }

    /// `… WITH ORDINALITY`: add a row-number column to a set-returning function.
    pub fn with_ordinality(mut self) -> Self {
        self.table.with_ordinality = true;
        self
    }
}

impl<Q: HasTableRef> Mod<Q> for FromMod {
    fn apply(self, q: &mut Q) {
        let table = q.table_ref_mut();

        table.expression = self.table.expression;
        // Column aliases are meaningless without an alias, so they travel with
        // one — same reason bob guards the `SetTableAlias` call.
        if !self.table.alias.is_empty() {
            table.set_table_alias(self.table.alias, self.table.columns);
        }
        table.only = self.table.only;
        table.lateral = self.table.lateral;
        table.with_ordinality = self.table.with_ordinality;
    }
}

/// A join, with its target's modifiers and its join condition.
#[derive(Debug, Clone, Default)]
pub struct JoinMod {
    join: Join,
}

impl JoinMod {
    /// A join of `kind` — one of the `*_JOIN` constants — to `table`.
    pub fn new(kind: impl Into<String>, table: DynExpr) -> Self {
        JoinMod {
            join: Join {
                kind: kind.into(),
                to: TableRef::new(table),
                ..Join::default()
            },
        }
    }

    /// `… AS "alias"`.
    pub fn as_(mut self, alias: impl Into<String>) -> Self {
        self.join.to.alias = alias.into();
        self
    }

    /// `… AS "alias" ("a", "b")`.
    pub fn columns(mut self, columns: impl Names) -> Self {
        self.join.to.columns = columns.into_names();
        self
    }

    /// `JOIN ONLY table`.
    pub fn only(mut self) -> Self {
        self.join.to.only = true;
        self
    }

    /// `JOIN LATERAL …`.
    pub fn lateral(mut self) -> Self {
        self.join.to.lateral = true;
        self
    }

    /// `… WITH ORDINALITY`.
    pub fn with_ordinality(mut self) -> Self {
        self.join.to.with_ordinality = true;
        self
    }

    /// `NATURAL JOIN …`: join on every same-named column.
    pub fn natural(mut self) -> Self {
        self.join.natural = true;
        self
    }

    /// `… ON a AND b`.
    pub fn on(mut self, conditions: impl Exprs) -> Self {
        self.join.on.extend(conditions.into_exprs());
        self
    }

    /// `… ON a = b`.
    pub fn on_eq(mut self, a: impl Expression + 'static, b: impl Expression + 'static) -> Self {
        let eq: Expr = Expr::from_expr(a);
        self.join.on.push(dyn_expr(eq.eq(b)));
        self
    }

    /// `… USING ("id")`.
    pub fn using(mut self, columns: impl Names) -> Self {
        self.join.using = columns.into_names();
        self
    }
}

impl<Q: HasJoins> Mod<Q> for JoinMod {
    fn apply(self, q: &mut Q) {
        q.joins_mut().push(self.join);
    }
}

/// A `CROSS JOIN`, which takes no join condition.
///
/// Separate from [`JoinMod`] for the same reason bob separates it: `ON` and
/// `USING` are not valid here, and the type is where that is said.
#[derive(Debug, Clone, Default)]
pub struct CrossJoinMod {
    join: Join,
}

impl CrossJoinMod {
    /// `CROSS JOIN table`.
    pub fn new(table: DynExpr) -> Self {
        CrossJoinMod {
            join: Join {
                kind: CROSS_JOIN.into(),
                to: TableRef::new(table),
                ..Join::default()
            },
        }
    }

    /// `… AS "alias"`.
    pub fn as_(mut self, alias: impl Into<String>) -> Self {
        self.join.to.alias = alias.into();
        self
    }

    /// `… AS "alias" ("a", "b")`.
    pub fn columns(mut self, columns: impl Names) -> Self {
        self.join.to.columns = columns.into_names();
        self
    }

    /// `CROSS JOIN ONLY table`.
    pub fn only(mut self) -> Self {
        self.join.to.only = true;
        self
    }

    /// `CROSS JOIN LATERAL …`.
    pub fn lateral(mut self) -> Self {
        self.join.to.lateral = true;
        self
    }

    /// `… WITH ORDINALITY`.
    pub fn with_ordinality(mut self) -> Self {
        self.join.to.with_ordinality = true;
        self
    }
}

impl<Q: HasJoins> Mod<Q> for CrossJoinMod {
    fn apply(self, q: &mut Q) {
        q.joins_mut().push(self.join);
    }
}

/// One common table expression.
///
/// Applying it without [`as_`](Self::as_) leaves the CTE without a body, which
/// fails at build time rather than silently writing `AS ()`.
#[derive(Debug, Clone, Default)]
pub struct CteMod {
    cte: Cte,
}

impl CteMod {
    /// `name (columns…) AS (…)`.
    pub fn new(name: impl Into<String>, columns: impl Names) -> Self {
        CteMod {
            cte: Cte {
                columns: columns.into_names(),
                ..Cte::new(name)
            },
        }
    }

    /// The CTE's body.
    ///
    /// Stored [bare](Query::into_bare): the parentheses are the CTE's own.
    pub fn as_<Q: Expression + 'static>(mut self, query: Query<Q>) -> Self {
        self.cte.query = Some(dyn_expr(query.into_bare()));
        self
    }

    /// `AS MATERIALIZED (…)`: force the CTE to be evaluated once.
    pub fn materialized(mut self) -> Self {
        self.cte.materialized = Some(true);
        self
    }

    /// `AS NOT MATERIALIZED (…)`: allow the CTE to be folded into its uses.
    pub fn not_materialized(mut self) -> Self {
        self.cte.materialized = Some(false);
        self
    }

    /// `SEARCH BREADTH FIRST BY … SET …`.
    pub fn search_breadth(mut self, set: impl Into<String>, columns: impl Names) -> Self {
        self.cte.search = CteSearch {
            order: SEARCH_BREADTH.into(),
            columns: columns.into_names(),
            set: set.into(),
        };
        self
    }

    /// `SEARCH DEPTH FIRST BY … SET …`.
    pub fn search_depth(mut self, set: impl Into<String>, columns: impl Names) -> Self {
        self.cte.search = CteSearch {
            order: SEARCH_DEPTH.into(),
            columns: columns.into_names(),
            set: set.into(),
        };
        self
    }

    /// `CYCLE columns SET set USING using`.
    pub fn cycle(
        mut self,
        set: impl Into<String>,
        using: impl Into<String>,
        columns: impl Names,
    ) -> Self {
        self.cte.cycle.set = set.into();
        self.cte.cycle.using = using.into();
        self.cte.cycle.columns = columns.into_names();
        self
    }

    /// The `TO … DEFAULT …` values of a `CYCLE` clause.
    pub fn cycle_value(mut self, value: impl IntoExpr, default: impl IntoExpr) -> Self {
        self.cte.cycle.set_val = Some(value.into_expr());
        self.cte.cycle.default_val = Some(default.into_expr());
        self
    }
}

impl<Q: HasWith> Mod<Q> for CteMod {
    fn apply(self, q: &mut Q) {
        q.with_mut().append_cte(dyn_expr(self.cte));
    }
}

/// A row-level lock: `FOR UPDATE OF users SKIP LOCKED`.
#[derive(Debug, Clone, Default)]
pub struct LockMod {
    lock: Lock,
}

impl LockMod {
    /// A lock of `strength` — one of the `LOCK_STRENGTH_*` constants — over
    /// `tables`, or over every table in the query when there are none.
    pub fn new(strength: impl Into<String>, tables: impl Names) -> Self {
        LockMod {
            lock: Lock {
                strength: strength.into(),
                tables: tables.into_names(),
                wait: String::new(),
            },
        }
    }

    /// `NOWAIT`: fail rather than block.
    pub fn no_wait(mut self) -> Self {
        self.lock.wait = keelson_core::clause::LOCK_WAIT_NO_WAIT.into();
        self
    }

    /// `SKIP LOCKED`: skip rows another transaction holds.
    pub fn skip_locked(mut self) -> Self {
        self.lock.wait = keelson_core::clause::LOCK_WAIT_SKIP_LOCKED.into();
        self
    }
}

impl<Q: HasLocks> Mod<Q> for LockMod {
    fn apply(self, q: &mut Q) {
        q.locks_mut().append_lock(dyn_expr(self.lock));
    }
}

/// One `ORDER BY` term and its direction, nulls placement and collation.
#[derive(Debug, Clone, Default)]
pub struct OrderMod {
    def: OrderDef,
}

impl OrderMod {
    /// Order by an already-erased expression.
    pub fn new(expression: DynExpr) -> Self {
        OrderMod {
            def: OrderDef::new(expression),
        }
    }

    /// `… ASC`.
    pub fn asc(mut self) -> Self {
        self.def.direction = "ASC".into();
        self
    }

    /// `… DESC`.
    pub fn desc(mut self) -> Self {
        self.def.direction = "DESC".into();
        self
    }

    /// `… USING <`, for an ordering operator that has no `ASC`/`DESC` spelling.
    pub fn using(mut self, operator: impl AsRef<str>) -> Self {
        self.def.direction = format!("USING {}", operator.as_ref());
        self
    }

    /// `… NULLS FIRST`.
    pub fn nulls_first(mut self) -> Self {
        self.def.nulls = "FIRST".into();
        self
    }

    /// `… NULLS LAST`.
    pub fn nulls_last(mut self) -> Self {
        self.def.nulls = "LAST".into();
        self
    }

    /// `… COLLATE "collation"`.
    pub fn collate(mut self, collation: impl Into<String>) -> Self {
        self.def.collation = collation.into();
        self
    }

    /// The finished term, for a mod that has to file it somewhere unusual.
    pub fn into_order_def(self) -> OrderDef {
        self.def
    }
}

impl<Q: HasOrderBy> Mod<Q> for OrderMod {
    fn apply(self, q: &mut Q) {
        q.order_by_mut().append_order(dyn_expr(self.def));
    }
}

/// An `ORDER BY` term that applies to the result of a set operation.
///
/// Same builders as [`OrderMod`], different slot: the query has two `ORDER BY`
/// clauses and this one is written after the `UNION`.
#[derive(Debug, Clone, Default)]
pub struct CombinedOrderMod {
    inner: OrderMod,
}

impl CombinedOrderMod {
    /// Order the combined result by an already-erased expression.
    pub fn new(expression: DynExpr) -> Self {
        CombinedOrderMod {
            inner: OrderMod::new(expression),
        }
    }

    /// `… ASC`.
    pub fn asc(mut self) -> Self {
        self.inner = self.inner.asc();
        self
    }

    /// `… DESC`.
    pub fn desc(mut self) -> Self {
        self.inner = self.inner.desc();
        self
    }

    /// `… USING <`.
    pub fn using(mut self, operator: impl AsRef<str>) -> Self {
        self.inner = self.inner.using(operator);
        self
    }

    /// `… NULLS FIRST`.
    pub fn nulls_first(mut self) -> Self {
        self.inner = self.inner.nulls_first();
        self
    }

    /// `… NULLS LAST`.
    pub fn nulls_last(mut self) -> Self {
        self.inner = self.inner.nulls_last();
        self
    }

    /// `… COLLATE "collation"`.
    pub fn collate(mut self, collation: impl Into<String>) -> Self {
        self.inner = self.inner.collate(collation);
        self
    }
}

impl<Q: HasCombinedOrder> Mod<Q> for CombinedOrderMod {
    fn apply(self, q: &mut Q) {
        let slot: &mut OrderBy = q.combined_order_mut();
        slot.append_order(dyn_expr(self.inner.into_order_def()));
    }
}
