//! The mods, written once against the `Has*` traits and re-exported per statement.
//!
//! Nothing in here names a query type. A mod is `Mod<Q>` for every `Q` that
//! implements the clause trait it needs, so `where_` is one function that serves
//! `SELECT`, `UPDATE`, `DELETE` *and* the `DO UPDATE` body of an upsert — and
//! refuses to compile against an `INSERT`, which has no `WHERE`.
//!
//! Three shapes recur.
//!
//! **A plain mod** is a function returning `impl Mod<Q>`, built from
//! [`mod_fn`](keelson_core::mod_fn).
//!
//! **A chain** is a struct that is itself a mod and has builder methods. It exists
//! wherever a clause has decorations that must be set together rather than one mod
//! at a time: `from(..).as_("u").not_indexed()` replaces the whole from-item once,
//! so no later mod can silently wipe an earlier one.
//!
//! **A slot** is how one chain type reaches different fields of different queries.
//! `select::from` and `update::table` are the same chain with a different
//! [`TableSlot`]; the marker is a type parameter, so which field is written is
//! decided at compile time and there is one implementation of the builder methods.

use std::borrow::Cow;
use std::marker::PhantomData;

use keelson_core::clause::{
    ConflictClause, ConflictTarget, Cte, HasGroupBy, HasHaving, HasJoins, HasLimit, HasOffset,
    HasOrderBy, HasReturning, HasSelectList, HasSet, HasTableRef, HasValues, HasWhere, HasWindows,
    HasWith, IndexedBy, Join, JoinKind, NamedWindow, NullsPosition, OrderDef, OrderDirection,
    TableRef, Values, Window,
};
use keelson_core::expr::{Expr, IntoExpr, IntoExprList, IntoIdent};
use keelson_core::{Mod, mod_fn};

use crate::extras::{Compound, CompoundOp, HasCompounds, HasOr, HasUpserts, Or};
use crate::statement::{HasExtraTables, HasTargetTable};

// ---------------------------------------------------------------------------
// WITH
// ---------------------------------------------------------------------------

/// A common table expression under construction.
///
/// `with("recent", body)` is already a complete mod; the methods add the optional
/// parts of SQLite's `common-table-expression` production
/// (<https://www.sqlite.org/syntax/common-table-expression.html>):
///
/// ```text
/// table-name [ ( column-name [, ...] ) ] AS [ [ NOT ] MATERIALIZED ] ( select-stmt )
/// ```
///
/// PostgreSQL's `SEARCH` and `CYCLE` sub-clauses have no counterpart in SQLite, so
/// they are not reachable from here. Nor is a data-modifying CTE: SQLite's grammar
/// admits only a `select-stmt` between the parentheses.
#[derive(Debug, Clone)]
pub struct CteChain {
    cte: Cte,
}

/// `WITH "name" AS (body)`.
///
/// `body` is any expression, so a hand-written fragment works, and a
/// [`SelectQuery`](crate::SelectQuery) goes in directly because it implements
/// [`IntoExpr`]. It is *not* parenthesised here — [`Cte`] supplies the parentheses.
pub fn with(name: impl Into<Cow<'static, str>>, body: impl IntoExpr) -> CteChain {
    CteChain {
        cte: Cte::new(name, body),
    }
}

impl CteChain {
    /// Name the CTE's output columns: `WITH "c" ("a", "b") AS (…)`.
    #[must_use]
    pub fn columns(
        mut self,
        columns: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) -> CteChain {
        self.cte.columns = columns.into_iter().map(Into::into).collect();
        self
    }

    /// `AS MATERIALIZED (…)` — compute it once into a transient table. SQLite 3.35
    /// and later.
    #[must_use]
    pub fn materialized(mut self) -> CteChain {
        self.cte.materialized = Some(true);
        self
    }

    /// `AS NOT MATERIALIZED (…)` — allow it to be folded into the outer query.
    #[must_use]
    pub fn not_materialized(mut self) -> CteChain {
        self.cte.materialized = Some(false);
        self
    }
}

impl<Q: HasWith> Mod<Q> for CteChain {
    fn apply(self, q: &mut Q) {
        q.with_mut().append_cte(self.cte);
    }
}

/// `WITH RECURSIVE` rather than `WITH`.
///
/// A property of the whole list, not of one entry.
pub fn recursive<Q: HasWith>(recursive: bool) -> impl Mod<Q> {
    mod_fn(move |q: &mut Q| q.with_mut().set_recursive(recursive))
}

// ---------------------------------------------------------------------------
// OR <conflict-algorithm>
// ---------------------------------------------------------------------------

fn or_algorithm<Q: HasOr>(or: Or) -> impl Mod<Q> {
    mod_fn(move |q: &mut Q| *q.or_mut() = Some(or))
}

/// `OR ROLLBACK` — a constraint violation aborts the whole transaction.
pub fn or_rollback<Q: HasOr>() -> impl Mod<Q> {
    or_algorithm(Or::Rollback)
}

/// `OR ABORT` — abort this statement and undo it, but keep the transaction. The
/// default, written out.
pub fn or_abort<Q: HasOr>() -> impl Mod<Q> {
    or_algorithm(Or::Abort)
}

/// `OR REPLACE` — delete the rows that conflict, then proceed. This is what
/// SQLite's `REPLACE INTO` is short for.
pub fn or_replace<Q: HasOr>() -> impl Mod<Q> {
    or_algorithm(Or::Replace)
}

/// `OR FAIL` — stop at the offending row, keeping the changes made before it.
pub fn or_fail<Q: HasOr>() -> impl Mod<Q> {
    or_algorithm(Or::Fail)
}

/// `OR IGNORE` — skip the offending row and carry on with the rest.
pub fn or_ignore<Q: HasOr>() -> impl Mod<Q> {
    or_algorithm(Or::Ignore)
}

// ---------------------------------------------------------------------------
// The result columns
// ---------------------------------------------------------------------------

/// Add to the result columns. Several calls accumulate; with none, `*` is written.
pub fn columns<Q: HasSelectList>(columns: impl IntoExprList) -> impl Mod<Q> {
    let columns = columns.into_expr_list();
    mod_fn(move |q: &mut Q| q.select_list_mut().append_select(columns))
}

/// Add to the *preload* result columns, which render after [`columns`] but are
/// counted separately.
///
/// This exists for a relation loader: the mapper needs to know how many of the
/// returned columns belong to the root object, which is
/// [`SelectList::count_select_cols`](keelson_core::clause::SelectList::count_select_cols).
pub fn preload_columns<Q: HasSelectList>(columns: impl IntoExprList) -> impl Mod<Q> {
    let columns = columns.into_expr_list();
    mod_fn(move |q: &mut Q| q.select_list_mut().append_preload_select(columns))
}

// ---------------------------------------------------------------------------
// Table references
// ---------------------------------------------------------------------------

/// Where a [`TableChain`] puts the table reference it has built.
///
/// Implemented by the markers below rather than by a query, so the builder methods
/// are written once and `select::from` / `update::table` / `select::from_also`
/// differ only in a type parameter.
pub trait TableSlot<Q> {
    /// Put `table` where this slot means.
    fn place(q: &mut Q, table: TableRef);
}

/// The from-item slot: a `SELECT`'s or an `UPDATE`'s `FROM`.
#[derive(Debug, Clone, Copy, Default)]
pub struct FromSlot;

/// The target slot: the table an `UPDATE` writes to or a `DELETE` removes from.
#[derive(Debug, Clone, Copy, Default)]
pub struct TargetSlot;

/// The additional-from-items slot, for the second and later entries of a
/// comma-separated `FROM` list.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExtraSlot;

impl<Q: HasTableRef> TableSlot<Q> for FromSlot {
    fn place(q: &mut Q, mut table: TableRef) {
        // Joins already appended to the slot survive, so `from(..)` written after
        // `inner_join(..)` is not a way to silently lose them.
        table.joins.append(&mut q.table_ref_mut().joins);
        *q.table_ref_mut() = table;
    }
}

impl<Q: HasTargetTable> TableSlot<Q> for TargetSlot {
    fn place(q: &mut Q, table: TableRef) {
        *q.target_table_mut() = table;
    }
}

impl<Q: HasExtraTables> TableSlot<Q> for ExtraSlot {
    fn place(q: &mut Q, table: TableRef) {
        q.extra_tables_mut().push(table);
    }
}

/// A `table-or-subquery` — or a `qualified-table-name` — under construction.
///
/// ```text
/// [ schema. ] table-name [ AS alias ] [ INDEXED BY index-name | NOT INDEXED ]
/// [ schema. ] table-function ( expr [, ...] ) [ AS alias ]
/// ( select-stmt ) [ AS alias ]
/// ```
///
/// That is the whole list of decorations SQLite allows on a from-item, and it is
/// shorter than PostgreSQL's by everything: no `ONLY`, no `LATERAL`, no
/// `WITH ORDINALITY`, no `TABLESAMPLE`, and **no column-alias list** — `t (a, b)`
/// is PostgreSQL's, and SQLite has only `AS alias`. A table-valued function needs
/// no mod of its own either, because a call is already an expression:
/// `from(f("pragma_table_info", s("users")))`.
#[derive(Debug, Clone)]
pub struct TableChain<S> {
    table: TableRef,
    slot: PhantomData<S>,
}

fn table_chain<S>(table: impl IntoExpr) -> TableChain<S> {
    TableChain {
        table: TableRef::new(table),
        slot: PhantomData,
    }
}

/// A from-item: `FROM <table>`.
pub fn from_item(table: impl IntoExpr) -> TableChain<FromSlot> {
    table_chain(table)
}

/// A further comma-separated from-item. A comma is one of SQLite's `join-operator`s
/// and means the same as `CROSS JOIN`.
pub fn extra_from_item(table: impl IntoExpr) -> TableChain<ExtraSlot> {
    table_chain(table)
}

/// The statement's target table: what an `UPDATE` writes to, what a `DELETE`
/// removes from.
pub fn target_table(table: impl IntoExpr) -> TableChain<TargetSlot> {
    table_chain(table)
}

impl<S> TableChain<S> {
    /// `AS "alias"`.
    #[must_use]
    pub fn as_(mut self, alias: impl Into<Cow<'static, str>>) -> TableChain<S> {
        self.table.set_alias(alias);
        self
    }

    /// `INDEXED BY "name"` — refuse to plan this item any other way.
    ///
    /// A tuning hint that SQLite treats as a hard constraint: it is an error if the
    /// named index cannot be used. Applies to a table name, not to a sub-query or a
    /// table-valued function.
    #[must_use]
    pub fn indexed_by(mut self, name: impl Into<Cow<'static, str>>) -> TableChain<S> {
        self.table.indexed_by = Some(IndexedBy::Index(name.into()));
        self
    }

    /// `NOT INDEXED` — plan this item without any index the planner would otherwise
    /// have chosen.
    #[must_use]
    pub fn not_indexed(mut self) -> TableChain<S> {
        self.table.indexed_by = Some(IndexedBy::NotIndexed);
        self
    }
}

impl<Q, S: TableSlot<Q>> Mod<Q> for TableChain<S> {
    fn apply(self, q: &mut Q) {
        S::place(q, self.table);
    }
}

/// An `INSERT`'s target under construction: `INTO "t" AS "alias" ("a", "b")`.
///
/// A different chain from [`TableChain`] because the grammars differ. An `INSERT`
/// target is a plain table name with an alias and an **insert column list**; a
/// from-item is a `qualified-table-name` with an alias and an **index directive**.
/// Neither decoration belongs on the other, so neither is offered there.
#[derive(Debug, Clone)]
pub struct IntoChain {
    table: TableRef,
}

/// `INSERT INTO <table>`.
pub fn into_table(table: impl IntoExpr) -> IntoChain {
    IntoChain {
        table: TableRef::new(table),
    }
}

impl IntoChain {
    /// `AS "alias"` — the name `excluded`-free references to the target row use
    /// inside an upsert.
    #[must_use]
    pub fn as_(mut self, alias: impl Into<Cow<'static, str>>) -> IntoChain {
        self.table.set_alias(alias);
        self
    }

    /// The insert column list: `INTO "t" ("a", "b")`.
    #[must_use]
    pub fn columns(
        mut self,
        columns: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) -> IntoChain {
        self.table.set_columns(columns);
        self
    }
}

impl<Q: HasTableRef> Mod<Q> for IntoChain {
    fn apply(self, q: &mut Q) {
        *q.table_ref_mut() = self.table;
    }
}

// ---------------------------------------------------------------------------
// Joins
// ---------------------------------------------------------------------------

/// A join under construction.
///
/// From <https://www.sqlite.org/syntax/join-clause.html>, the `join-constraint`
/// — `ON expr` or `USING (cols)` — is a production of its own, applied after
/// whichever `join-operator` was used. So **`cross_join` takes `on` and `using`
/// too**, which is the point of a `CROSS JOIN` in SQLite: it is an inner join that
/// additionally forbids the planner from reordering the two tables. PostgreSQL's
/// `CROSS JOIN` admits no condition at all and correspondingly has a narrower
/// chain type; SQLite needs no such split.
///
/// `NATURAL` excludes both; nothing enforces that here, because the caller picks
/// one method and the grammar is what says so.
#[derive(Debug, Clone)]
pub struct JoinChain {
    join: Join,
}

fn join_chain(kind: JoinKind, to: impl IntoExpr) -> JoinChain {
    JoinChain {
        join: Join::new(kind, TableRef::new(to)),
    }
}

/// `INNER JOIN <table>`.
pub fn inner_join(table: impl IntoExpr) -> JoinChain {
    join_chain(JoinKind::Inner, table)
}

/// `LEFT JOIN <table>`.
pub fn left_join(table: impl IntoExpr) -> JoinChain {
    join_chain(JoinKind::Left, table)
}

/// `RIGHT JOIN <table>` — SQLite 3.39 and later.
///
/// Included after checking: SQLite gained right and full outer joins in 3.39
/// (2022), and the linked-in engine accepts both. An older SQLite will not.
pub fn right_join(table: impl IntoExpr) -> JoinChain {
    join_chain(JoinKind::Right, table)
}

/// `FULL JOIN <table>` — SQLite 3.39 and later.
pub fn full_join(table: impl IntoExpr) -> JoinChain {
    join_chain(JoinKind::Full, table)
}

/// `CROSS JOIN <table>` — an inner join that also pins the join order.
///
/// Takes `ON`/`USING` like any other, because in SQLite that is exactly what it is
/// for: writing `CROSS JOIN` instead of `JOIN` is how the query planner is told not
/// to swap the two tables around.
pub fn cross_join(table: impl IntoExpr) -> JoinChain {
    join_chain(JoinKind::Cross, table)
}

impl JoinChain {
    /// `AS "alias"` on the joined table.
    #[must_use]
    pub fn as_(mut self, alias: impl Into<Cow<'static, str>>) -> JoinChain {
        self.join.to.set_alias(alias);
        self
    }

    /// `INDEXED BY "name"` on the joined table.
    #[must_use]
    pub fn indexed_by(mut self, name: impl Into<Cow<'static, str>>) -> JoinChain {
        self.join.to.indexed_by = Some(IndexedBy::Index(name.into()));
        self
    }

    /// `NOT INDEXED` on the joined table.
    #[must_use]
    pub fn not_indexed(mut self) -> JoinChain {
        self.join.to.indexed_by = Some(IndexedBy::NotIndexed);
        self
    }

    /// `NATURAL <kind> JOIN` — derive the join columns from the two items' names.
    #[must_use]
    pub fn natural(mut self) -> JoinChain {
        self.join.natural = true;
        self
    }

    /// `ON condition`. Several conditions are `AND`-joined.
    #[must_use]
    pub fn on(mut self, condition: impl IntoExpr) -> JoinChain {
        self.join.append_on(condition);
        self
    }

    /// `ON (a = b)`, the overwhelmingly common shape.
    #[must_use]
    pub fn on_eq(self, a: impl IntoExpr, b: impl IntoExpr) -> JoinChain {
        self.on(Expr::binary(a, "=", b).grouped())
    }

    /// `USING ("a", "b")` — join on equally named columns, merging them.
    #[must_use]
    pub fn using(
        mut self,
        columns: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) -> JoinChain {
        self.join.append_using(columns);
        self
    }
}

impl<Q: HasJoins> Mod<Q> for JoinChain {
    fn apply(self, q: &mut Q) {
        q.joins_mut().push(self.join);
    }
}

// ---------------------------------------------------------------------------
// WHERE / HAVING / GROUP BY
// ---------------------------------------------------------------------------

/// `WHERE condition`. Several calls are `AND`-joined; use [`or`](crate::or) for the
/// other connective.
pub fn where_<Q: HasWhere>(condition: impl IntoExpr) -> impl Mod<Q> {
    let condition = condition.into_expr();
    mod_fn(move |q: &mut Q| q.where_mut().append_where(condition))
}

/// `HAVING condition`. Several calls are `AND`-joined.
///
/// SQLite allows a `HAVING` with no `GROUP BY`, which then applies to the single
/// implicit group.
pub fn having<Q: HasHaving>(condition: impl IntoExpr) -> impl Mod<Q> {
    let condition = condition.into_expr();
    mod_fn(move |q: &mut Q| q.having_mut().append_having(condition))
}

/// Add a grouping expression.
///
/// SQLite's `GROUP BY expr [, ...]` takes plain expressions and nothing else: there
/// is no `DISTINCT` modifier, and no `ROLLUP`, `CUBE` or `GROUPING SETS`.
pub fn group_by<Q: HasGroupBy>(group: impl IntoExpr) -> impl Mod<Q> {
    let group = group.into_expr();
    mod_fn(move |q: &mut Q| q.group_by_mut().append_group(group))
}

// ---------------------------------------------------------------------------
// WINDOW
// ---------------------------------------------------------------------------

/// `WINDOW "name" AS (definition)`, the definition built from
/// [`window`](crate::window) and [`frame`](crate::frame) mods.
///
/// A later window may be [`window::based_on`](crate::window::based_on) an earlier
/// one.
pub fn window<Q: HasWindows>(
    name: impl Into<Cow<'static, str>>,
    definition: impl Mod<Window>,
) -> impl Mod<Q> {
    let mut w = Window::default();
    definition.apply(&mut w);
    let named = NamedWindow::new(name, w);
    mod_fn(move |q: &mut Q| q.windows_mut().append_window(named))
}

// ---------------------------------------------------------------------------
// ORDER BY
// ---------------------------------------------------------------------------

/// One `ordering-term` under construction.
///
/// From <https://www.sqlite.org/syntax/ordering-term.html>:
///
/// ```text
/// expr [ COLLATE collation-name ] [ ASC | DESC ] [ NULLS { FIRST | LAST } ]
/// ```
///
/// There is no `USING <operator>` — that is PostgreSQL's, and is why
/// [`OrderDirection`](keelson_core::clause::OrderDirection) has a third variant
/// this dialect never builds. `NULLS FIRST`/`LAST` needs SQLite 3.30 or later.
///
/// A single mod serves the statement's `ORDER BY` and a window's alike, because
/// SQLite has only one `ORDER BY` per statement: in a compound select it belongs to
/// the whole compound, so there is no second slot for
/// [`keelson_psql`]'s `order_by_combined` to write into.
#[derive(Debug, Clone)]
pub struct OrderChain {
    def: OrderDef,
}

/// `ORDER BY expression`.
pub fn order_by(expression: impl IntoExpr) -> OrderChain {
    OrderChain {
        def: OrderDef::new(expression),
    }
}

impl OrderChain {
    /// `ASC`.
    #[must_use]
    pub fn asc(mut self) -> OrderChain {
        self.def.direction = Some(OrderDirection::Asc);
        self
    }

    /// `DESC`.
    #[must_use]
    pub fn desc(mut self) -> OrderChain {
        self.def.direction = Some(OrderDirection::Desc);
        self
    }

    /// `NULLS FIRST`.
    #[must_use]
    pub fn nulls_first(mut self) -> OrderChain {
        self.def.nulls = Some(NullsPosition::First);
        self
    }

    /// `NULLS LAST`.
    #[must_use]
    pub fn nulls_last(mut self) -> OrderChain {
        self.def.nulls = Some(NullsPosition::Last);
        self
    }

    /// `COLLATE "name"`, written between the expression and the direction.
    #[must_use]
    pub fn collate(mut self, name: impl Into<Cow<'static, str>>) -> OrderChain {
        self.def.collation = Some(name.into());
        self
    }
}

impl<Q: HasOrderBy> Mod<Q> for OrderChain {
    fn apply(self, q: &mut Q) {
        // `OrderBy` stores expressions, and an `OrderDef` reaches one as
        // `Expr::Custom`. Nothing groups it, so `ORDER BY "name" DESC` keeps its
        // shape.
        q.order_by_mut().append_order(Expr::custom(self.def));
    }
}

// ---------------------------------------------------------------------------
// LIMIT / OFFSET
// ---------------------------------------------------------------------------

/// `LIMIT count`.
///
/// SQLite takes a whole expression here, not just a literal or a parameter, so a
/// sub-select works. A plain number is a literal — `limit(20)` gives `LIMIT 20` —
/// because [`IntoExpr`] makes it one; `limit(arg(20))` binds it instead.
///
/// There is no `LIMIT ALL`: that is PostgreSQL's spelling for "no limit", and
/// SQLite's is a negative count.
pub fn limit<Q: HasLimit>(count: impl IntoExpr) -> impl Mod<Q> {
    let count = count.into_expr();
    mod_fn(move |q: &mut Q| q.limit_mut().set_limit(count))
}

/// `OFFSET start`.
///
/// SQLite's grammar is `LIMIT expr [ ( OFFSET | , ) expr ]`, so this is part of the
/// `LIMIT` clause rather than one of its own: an offset with no [`limit`] is a
/// recorded [`Error::Incomplete`](keelson_core::Error::Incomplete) at build time,
/// not a statement the database will reject later.
pub fn offset<Q: HasOffset>(start: impl IntoExpr) -> impl Mod<Q> {
    let start = start.into_expr();
    mod_fn(move |q: &mut Q| q.offset_mut().set_offset(start))
}

// ---------------------------------------------------------------------------
// Compound SELECTs
// ---------------------------------------------------------------------------

fn compound<Q: HasCompounds>(op: CompoundOp, query: impl IntoExpr) -> impl Mod<Q> {
    let c = Compound::new(op, query);
    mod_fn(move |q: &mut Q| q.compounds_mut().append_compound(c))
}

/// `UNION <select-core>` — rows of either, duplicates removed.
///
/// The operand is written bare: a parenthesised select is not a compound operand in
/// SQLite. Pass a [`SelectQuery`](crate::SelectQuery) directly, or
/// [`query`](crate::query) for one built elsewhere — **not**
/// [`subquery`](crate::subquery), which adds the parentheses that would make this a
/// syntax error.
pub fn union<Q: HasCompounds>(query: impl IntoExpr) -> impl Mod<Q> {
    compound(CompoundOp::Union, query)
}

/// `UNION ALL <select-core>` — rows of either, duplicates kept.
///
/// The only compound operator SQLite offers `ALL` on.
pub fn union_all<Q: HasCompounds>(query: impl IntoExpr) -> impl Mod<Q> {
    compound(CompoundOp::UnionAll, query)
}

/// `INTERSECT <select-core>` — rows of both. There is no `INTERSECT ALL`.
pub fn intersect<Q: HasCompounds>(query: impl IntoExpr) -> impl Mod<Q> {
    compound(CompoundOp::Intersect, query)
}

/// `EXCEPT <select-core>` — rows of this query that are not in the other. There is
/// no `EXCEPT ALL`.
pub fn except<Q: HasCompounds>(query: impl IntoExpr) -> impl Mod<Q> {
    compound(CompoundOp::Except, query)
}

// ---------------------------------------------------------------------------
// RETURNING
// ---------------------------------------------------------------------------

/// `RETURNING a, b`. `returning("*")` is an ordinary entry.
///
/// SQLite 3.35 and later, on `INSERT`, `UPDATE` and `DELETE`. Whether this clause
/// is present is what decides whether a mutation is run as a query or as an exec.
pub fn returning<Q: HasReturning>(expressions: impl IntoExprList) -> impl Mod<Q> {
    let expressions = expressions.into_expr_list();
    mod_fn(move |q: &mut Q| q.returning_mut().append_returnings(expressions))
}

// ---------------------------------------------------------------------------
// SET
// ---------------------------------------------------------------------------

/// One assignment, written out: `set(quote("a").eq(arg(1)))`.
///
/// A whole expression rather than a column/value pair, because SQLite's
/// multi-column form `(a, b) = (SELECT x, y FROM …)` is one assignment with a row on
/// each side.
pub fn set<Q: HasSet>(assignment: impl IntoExpr) -> impl Mod<Q> {
    let assignment = assignment.into_expr();
    mod_fn(move |q: &mut Q| q.set_mut().append_set(assignment))
}

/// The left-hand side of an assignment: `set_col("a").to(arg(1))`.
///
/// Not a mod on its own — an assignment with no value is not one — so
/// [`to`](Self::to) or [`to_arg`](Self::to_arg) has to be called.
#[derive(Debug, Clone)]
pub struct SetChain {
    column: Expr,
}

/// Assign to a column. `set_col(("t", "a"))` qualifies it, which `UPDATE` forbids
/// but an upsert's `DO UPDATE` permits.
pub fn set_col(column: impl IntoIdent) -> SetChain {
    SetChain {
        column: Expr::ident(column),
    }
}

impl SetChain {
    /// `"col" = value`, where `value` is an expression.
    pub fn to<Q: HasSet>(self, value: impl IntoExpr) -> impl Mod<Q> {
        set(Expr::binary(self.column, "=", value))
    }

    /// `"col" = ?n` — bind `value` as an argument.
    pub fn to_arg<Q: HasSet>(self, value: impl keelson_core::ToValue) -> impl Mod<Q> {
        set(Expr::binary(self.column, "=", Expr::arg(value)))
    }
}

/// `"col" = excluded."col"` for each column — the body of an upsert.
///
/// The pseudo-table is `excluded` in lower case and unquoted, which is how
/// <https://www.sqlite.org/lang_upsert.html> spells it.
pub fn set_excluded<Q: HasSet>(
    columns: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
) -> impl Mod<Q> {
    let assignments: Vec<Expr> = columns
        .into_iter()
        .map(Into::into)
        .filter(|c: &Cow<'static, str>| !c.is_empty())
        .map(|c| {
            Expr::join_with(
                "",
                (
                    Expr::ident(c.clone()),
                    Expr::raw(" = excluded."),
                    Expr::ident(c),
                ),
            )
        })
        .collect();
    mod_fn(move |q: &mut Q| q.set_mut().append_sets(assignments))
}

// ---------------------------------------------------------------------------
// VALUES
// ---------------------------------------------------------------------------

/// One row of `VALUES`. Several calls append several rows.
///
/// Applies to a `SELECT` as well as to an `INSERT`, because SQLite's
/// `VALUES (…), (…)` is a `select-core` in its own right — the same mod builds the
/// row source of an insert and a standalone `VALUES` statement.
///
/// A cell may **not** be `DEFAULT`: unlike PostgreSQL, SQLite has no per-cell
/// default keyword, only the whole-row `DEFAULT VALUES` that an `INSERT` with no
/// rows at all produces.
pub fn values<Q: HasValues>(row: impl IntoExprList) -> impl Mod<Q> {
    let row = row.into_expr_list();
    mod_fn(move |q: &mut Q| q.values_mut().append_values(row))
}

/// Several rows of `VALUES` at once.
pub fn rows<Q: HasValues, R: IntoExprList>(rows: impl IntoIterator<Item = R>) -> impl Mod<Q> {
    let rows: Vec<Vec<Expr>> = rows.into_iter().map(IntoExprList::into_expr_list).collect();
    mod_fn(move |q: &mut Q| {
        let values = q.values_mut();
        for row in rows {
            values.append_values(row);
        }
    })
}

/// Insert the results of a query: `INSERT INTO t (cols) SELECT …`.
///
/// Replaces any rows already added, because the two are alternatives in the grammar
/// rather than things that combine. The query is written bare, so pass a query or
/// [`query`](crate::query) rather than [`subquery`](crate::subquery).
pub fn values_from_query<Q: HasValues>(query: impl IntoExpr) -> impl Mod<Q> {
    let query = query.into_expr();
    mod_fn(move |q: &mut Q| *q.values_mut() = Values::from_query(query))
}

// ---------------------------------------------------------------------------
// ON CONFLICT — the upsert-clause
// ---------------------------------------------------------------------------

/// An `upsert-clause` under construction.
///
/// Not a mod until an action is chosen — `ON CONFLICT` with no
/// `DO NOTHING`/`DO UPDATE` is not a clause — so [`do_nothing`](Self::do_nothing)
/// or [`do_update`](Self::do_update) has to be called.
#[derive(Debug, Clone)]
pub struct ConflictChain {
    target: ConflictTarget,
}

/// `ON CONFLICT (columns)` — infer the unique index from a column list.
///
/// `on_conflict(())` targets any conflict at all, which only `DO NOTHING` accepts.
/// There is no `ON CONSTRAINT` form: SQLite infers the index from columns or not at
/// all, so PostgreSQL's `on_conflict_on_constraint` has no counterpart here.
pub fn on_conflict(columns: impl IntoExprList) -> ConflictChain {
    ConflictChain {
        target: ConflictTarget::on_columns(columns),
    }
}

impl ConflictChain {
    /// The **index** predicate: `ON CONFLICT (a) WHERE …`.
    ///
    /// Matched against a partial unique index's own definition, not evaluated per
    /// row — which is why it is a method here and not the [`where_`] mod that
    /// filters which conflicting rows get updated. It hangs off the parenthesised
    /// column list and cannot stand without one.
    #[must_use]
    pub fn where_(mut self, predicate: impl IntoExpr) -> ConflictChain {
        self.target.where_mut().append_where(predicate);
        self
    }

    /// `DO NOTHING` — skip the conflicting row.
    pub fn do_nothing(self) -> ConflictMod {
        let mut clause = ConflictClause::do_nothing();
        clause.target = self.target;
        ConflictMod { clause }
    }

    /// `DO UPDATE SET …` — the upsert.
    ///
    /// The body is built from mods against
    /// [`ConflictClause`](keelson_core::clause::ConflictClause), which implements
    /// [`HasSet`] and [`HasWhere`]: [`set`], [`set_col`], [`set_excluded`] and
    /// [`where_`] all apply, and that `where_` is the row filter.
    pub fn do_update(self, body: impl Mod<ConflictClause>) -> ConflictMod {
        let mut clause = ConflictClause::do_update();
        clause.target = self.target;
        body.apply(&mut clause);
        ConflictMod { clause }
    }
}

/// A finished `upsert-clause`, ready to apply.
///
/// Appends rather than replaces: SQLite 3.35 and later accept several upsert
/// clauses on one `INSERT`, tried in order, with only the last allowed to omit its
/// conflict target.
#[derive(Debug, Clone)]
pub struct ConflictMod {
    clause: ConflictClause,
}

impl<Q: HasUpserts> Mod<Q> for ConflictMod {
    fn apply(self, q: &mut Q) {
        q.upserts_mut().push(self.clause);
    }
}
