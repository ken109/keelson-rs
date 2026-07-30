//! The mods, written once against the `Has*` traits and re-exported per statement.
//!
//! Nothing in here names a query type. A mod is `Mod<Q>` for every `Q` that
//! implements the clause trait it needs, so `where_` is one function serving
//! `SELECT`, `UPDATE` and `DELETE` — and refusing to compile against an `INSERT`,
//! which has no `WHERE`.
//!
//! Three shapes recur.
//!
//! **A plain mod** is a function returning `impl Mod<Q>`, built from
//! [`keelson_core::mod_fn`].
//!
//! **A chain** is a struct that is itself a mod and has builder methods. It exists
//! wherever a clause has decorations that must be set together rather than one mod
//! at a time: `from(..).as_("u").use_index(["PRIMARY"])` replaces the whole
//! from-item once, so no later mod can silently wipe an earlier one.
//!
//! **A slot** is how one chain type reaches different fields of different queries.
//! `select::from` and `delete::using` are the same chain with a different
//! [`TableSlot`]; the marker is a type parameter, so which field is written is
//! decided at compile time and there is one implementation of the builder methods.

use std::borrow::Cow;
use std::marker::PhantomData;

use keelson_core::clause::{
    Combine, Cte, GroupByWith, HasCombines, HasGroupBy, HasHaving, HasJoins, HasLimit, HasLocks,
    HasOffset, HasOrderBy, HasSelectList, HasSet, HasTableRef, HasValues, HasWhere, HasWindows,
    HasWith, IndexHint, IndexHintKind, IndexHintScope, Join, JoinKind, Lock, LockStrength,
    LockWait, NamedWindow, OrderBy, OrderDef, OrderDirection, Set, SetOp, TableRef, Values, Window,
};
use keelson_core::expr::{Expr, IntoExpr, IntoExprList, IntoIdent};
use keelson_core::{Expression, Mod, SqlWriter, mod_fn};

use crate::extras::{
    HasDuplicateKeyUpdate, HasHints, HasModifiers, HasRowAlias, Modifier, RowAlias, row_value,
    values_of,
};
use crate::statement::{HasDeleteTables, HasExtraTables, HasTargetTable};

// ---------------------------------------------------------------------------
// WITH
// ---------------------------------------------------------------------------

/// A common table expression under construction.
///
/// `with("recent", body)` is already a complete mod; the one method adds the only
/// optional part MySQL's `with_query` has.
///
/// MySQL's production is
/// `cte_name [(col_name [, col_name] ...)] AS (subquery)` (*15.2.20*) — there is no
/// `MATERIALIZED`, no `SEARCH` and no `CYCLE`, so this chain has none of the
/// methods PostgreSQL's does.
#[derive(Debug, Clone)]
pub struct CteChain {
    cte: Cte,
}

/// `WITH \`name\` AS (body)`.
///
/// `body` is any expression, so a hand-written fragment works and a query goes in
/// directly, because the query types implement
/// [`keelson_core::expr::IntoExpr`]. It is *not* parenthesised here —
/// [`Cte`] supplies the parentheses.
pub fn with(name: impl Into<Cow<'static, str>>, body: impl IntoExpr) -> CteChain {
    CteChain {
        cte: Cte::new(name, body),
    }
}

impl CteChain {
    /// Name the CTE's output columns: ``WITH `c` (`a`, `b`) AS (…)``.
    #[must_use]
    pub fn columns(
        mut self,
        columns: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) -> CteChain {
        self.cte.columns = columns.into_iter().map(Into::into).collect();
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
/// A property of the whole list, not of one entry. MySQL requires it whenever any
/// CTE in the list refers to itself.
pub fn recursive<Q: HasWith>(recursive: bool) -> impl Mod<Q> {
    mod_fn(move |q: &mut Q| q.with_mut().set_recursive(recursive))
}

// ---------------------------------------------------------------------------
// Optimizer hints
// ---------------------------------------------------------------------------

/// One optimizer hint, written verbatim inside the `/*+ … */` comment:
/// `optimizer_hint("BKA(users, posts)")`.
///
/// The hint language has its own grammar with some forty names, several of which
/// take a query-block-qualified table list. Modelling it would be a second dialect
/// inside this one, so the general form is this and only the fixed-shape hints get
/// their own mods below.
pub fn optimizer_hint<Q: HasHints>(hint: impl Into<Cow<'static, str>>) -> impl Mod<Q> {
    let hint = hint.into();
    mod_fn(move |q: &mut Q| q.hints_mut().append_hint(hint))
}

/// `MAX_EXECUTION_TIME(n)` — give up after `n` milliseconds. `SELECT` only, and
/// MySQL ignores it on anything else.
pub fn max_execution_time<Q: HasHints>(millis: u64) -> impl Mod<Q> {
    optimizer_hint(format!("MAX_EXECUTION_TIME({millis})"))
}

/// `SET_VAR(name = value)` — change a system variable for this statement alone.
pub fn set_var<Q: HasHints>(assignment: impl Into<Cow<'static, str>>) -> impl Mod<Q> {
    optimizer_hint(format!("SET_VAR({})", assignment.into()))
}

/// `QB_NAME(name)` — name this query block so a later hint can qualify a table
/// with it.
pub fn qb_name<Q: HasHints>(name: impl Into<Cow<'static, str>>) -> impl Mod<Q> {
    optimizer_hint(format!("QB_NAME({})", name.into()))
}

/// `RESOURCE_GROUP(name)` — run the statement in a named resource group.
pub fn resource_group<Q: HasHints>(name: impl Into<Cow<'static, str>>) -> impl Mod<Q> {
    optimizer_hint(format!("RESOURCE_GROUP({})", name.into()))
}

// ---------------------------------------------------------------------------
// Statement modifiers
// ---------------------------------------------------------------------------

fn modifier<Q: HasModifiers>(modifier: Modifier) -> impl Mod<Q> {
    mod_fn(move |q: &mut Q| q.modifiers_mut().append_modifier(modifier))
}

/// `DISTINCT` — drop duplicate result rows. There is no `DISTINCT ON`; that is
/// PostgreSQL's.
pub fn distinct<Q: HasModifiers>() -> impl Mod<Q> {
    modifier(Modifier::Distinct)
}

/// `DISTINCTROW`, MySQL's synonym for `DISTINCT`.
pub fn distinct_row<Q: HasModifiers>() -> impl Mod<Q> {
    modifier(Modifier::DistinctRow)
}

/// `LOW_PRIORITY` — wait until no client is reading the table.
pub fn low_priority<Q: HasModifiers>() -> impl Mod<Q> {
    modifier(Modifier::LowPriority)
}

/// `HIGH_PRIORITY` — jump ahead of pending writes.
pub fn high_priority<Q: HasModifiers>() -> impl Mod<Q> {
    modifier(Modifier::HighPriority)
}

/// `DELAYED` — accepted for backward compatibility; MySQL 8 treats it as an
/// ordinary `INSERT` and raises a warning.
pub fn delayed<Q: HasModifiers>() -> impl Mod<Q> {
    modifier(Modifier::Delayed)
}

/// `QUICK` — skip merging index leaves while deleting.
pub fn quick<Q: HasModifiers>() -> impl Mod<Q> {
    modifier(Modifier::Quick)
}

/// `IGNORE` — downgrade the errors that would abort the statement to warnings.
pub fn ignore<Q: HasModifiers>() -> impl Mod<Q> {
    modifier(Modifier::Ignore)
}

/// The `STRAIGHT_JOIN` *modifier*: join every table in the order written.
///
/// Not the same thing as [`straight_join`], which is a join operator applying to
/// one pair of tables.
pub fn straight<Q: HasModifiers>() -> impl Mod<Q> {
    modifier(Modifier::StraightJoin)
}

/// `SQL_SMALL_RESULT` — the result is small; use an in-memory temporary table.
pub fn sql_small_result<Q: HasModifiers>() -> impl Mod<Q> {
    modifier(Modifier::SmallResult)
}

/// `SQL_BIG_RESULT` — the result is large; sort rather than build an index.
pub fn sql_big_result<Q: HasModifiers>() -> impl Mod<Q> {
    modifier(Modifier::BigResult)
}

/// `SQL_BUFFER_RESULT` — force the result into a temporary table, releasing table
/// locks sooner.
pub fn sql_buffer_result<Q: HasModifiers>() -> impl Mod<Q> {
    modifier(Modifier::BufferResult)
}

/// `SQL_NO_CACHE` — do not touch the query cache.
pub fn sql_no_cache<Q: HasModifiers>() -> impl Mod<Q> {
    modifier(Modifier::NoCache)
}

/// `SQL_CALC_FOUND_ROWS` — count the rows a `LIMIT` discarded, for `FOUND_ROWS()`.
pub fn sql_calc_found_rows<Q: HasModifiers>() -> impl Mod<Q> {
    modifier(Modifier::CalcFoundRows)
}

// ---------------------------------------------------------------------------
// The projection
// ---------------------------------------------------------------------------

/// Add to the select list. Several calls accumulate; with none, `*` is written.
pub fn columns<Q: HasSelectList>(columns: impl IntoExprList) -> impl Mod<Q> {
    let columns = columns.into_expr_list();
    mod_fn(move |q: &mut Q| q.select_list_mut().append_select(columns))
}

/// Add to the *preload* select list, which renders after [`columns`] but is counted
/// separately, so a relation loader can tell which of the returned columns belong
/// to the root object.
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
/// are written once and `select::from` / `update::table` / `delete::from` differ
/// only in a type parameter.
pub trait TableSlot<Q> {
    /// Put `table` where this slot means.
    fn place(q: &mut Q, table: TableRef);
}

/// The from-item slot: a `SELECT`'s `FROM`, an `INSERT`'s or `REPLACE`'s target, a
/// `DELETE`'s `USING`.
#[derive(Debug, Clone, Copy, Default)]
pub struct FromSlot;

/// The target slot: the `table_references` an `UPDATE` writes to.
#[derive(Debug, Clone, Copy, Default)]
pub struct TargetSlot;

/// The additional-table slot, for the second and later entries of a
/// comma-separated table list.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExtraSlot;

/// The `DELETE FROM` list, which also collects the statement's partitions.
#[derive(Debug, Clone, Copy, Default)]
pub struct DeleteSlot;

impl<Q: HasTableRef> TableSlot<Q> for FromSlot {
    fn place(q: &mut Q, mut table: TableRef) {
        // Joins already appended to the slot survive, so `from(..)` written after
        // `inner_join(..)` is not a way to silently lose them.
        table.joins.append(&mut q.table_ref_mut().joins);
        *q.table_ref_mut() = table;
    }
}

impl<Q: HasTargetTable> TableSlot<Q> for TargetSlot {
    fn place(q: &mut Q, mut table: TableRef) {
        table.joins.append(&mut q.target_table_mut().joins);
        *q.target_table_mut() = table;
    }
}

impl<Q: HasExtraTables> TableSlot<Q> for ExtraSlot {
    fn place(q: &mut Q, table: TableRef) {
        q.extra_tables_mut().push(table);
    }
}

impl<Q: HasDeleteTables> TableSlot<Q> for DeleteSlot {
    fn place(q: &mut Q, mut table: TableRef) {
        // `DELETE` writes PARTITION after the alias and once for the whole
        // statement, so the chain's list is moved out of the table reference. See
        // `HasDeleteTables`.
        let partitions = std::mem::take(&mut table.partitions);
        q.delete_partitions_mut().extend(partitions);
        q.delete_tables_mut().push(table);
    }
}

/// A table reference under construction: the table plus every decoration MySQL's
/// `table_factor` allows.
///
/// ```text
/// tbl_name [PARTITION (partition_names)] [[AS] alias] [index_hint_list]
/// [LATERAL] table_subquery [AS] alias [(col_list)]
/// ```
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

/// A further comma-separated table reference. A comma there means `CROSS JOIN`.
pub fn extra_from_item(table: impl IntoExpr) -> TableChain<ExtraSlot> {
    table_chain(table)
}

/// The `table_references` an `UPDATE` writes to.
pub fn target_table(table: impl IntoExpr) -> TableChain<TargetSlot> {
    table_chain(table)
}

/// A table a `DELETE` removes rows from. Several calls give
/// `DELETE FROM t1, t2 …`.
pub fn delete_table(table: impl IntoExpr) -> TableChain<DeleteSlot> {
    table_chain(table)
}

impl<S> TableChain<S> {
    /// `AS \`alias\``.
    #[must_use]
    pub fn as_(mut self, alias: impl Into<Cow<'static, str>>) -> TableChain<S> {
        self.table.set_alias(alias);
        self
    }

    /// Column aliases: `AS \`t\` (\`a\`, \`b\`)`, which MySQL 8.0.19 allows on a
    /// derived table. For an `INSERT` or `REPLACE` this is the column list instead.
    #[must_use]
    pub fn columns(
        mut self,
        columns: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) -> TableChain<S> {
        self.table.set_columns(columns);
        self
    }

    /// `LATERAL` — let a derived table refer to columns of the items before it
    /// (MySQL 8.0.14).
    ///
    /// Only grammatical in front of a derived table; on a bare table or CTE
    /// name this records a `build()` error instead, because
    /// ``FROM LATERAL `posts` `` is a syntax error with nothing to mean.
    #[must_use]
    pub fn lateral(mut self) -> TableChain<S> {
        self.table = lateral_table(self.table);
        self
    }

    /// `PARTITION (\`p0\`, \`p1\`)` — read only these partitions.
    #[must_use]
    pub fn partition(
        mut self,
        partitions: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) -> TableChain<S> {
        self.table.append_partition(partitions);
        self
    }

    /// `USE INDEX (…)` — consider only these indexes. An empty list is meaningful:
    /// `USE INDEX ()` tells MySQL to use none.
    #[must_use]
    pub fn use_index(
        self,
        indexes: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) -> TableChain<S> {
        self.index_hint(IndexHintKind::Use, indexes)
    }

    /// `IGNORE INDEX (…)` — do not consider these indexes.
    #[must_use]
    pub fn ignore_index(
        self,
        indexes: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) -> TableChain<S> {
        self.index_hint(IndexHintKind::Ignore, indexes)
    }

    /// `FORCE INDEX (…)` — a table scan is not acceptable.
    #[must_use]
    pub fn force_index(
        self,
        indexes: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) -> TableChain<S> {
        self.index_hint(IndexHintKind::Force, indexes)
    }

    /// `FOR JOIN` on the hint just added.
    ///
    /// Ignored if no hint has been added: a scope with nothing to scope means
    /// nothing, and `FOR JOIN` is not a clause of its own.
    #[must_use]
    pub fn for_join(self) -> TableChain<S> {
        self.hint_scope(IndexHintScope::Join)
    }

    /// `FOR ORDER BY` on the hint just added.
    #[must_use]
    pub fn for_order_by(self) -> TableChain<S> {
        self.hint_scope(IndexHintScope::OrderBy)
    }

    /// `FOR GROUP BY` on the hint just added.
    #[must_use]
    pub fn for_group_by(self) -> TableChain<S> {
        self.hint_scope(IndexHintScope::GroupBy)
    }

    fn index_hint(
        mut self,
        kind: IndexHintKind,
        indexes: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) -> TableChain<S> {
        self.table.append_index_hint(IndexHint::new(kind, indexes));
        self
    }

    fn hint_scope(mut self, scope: IndexHintScope) -> TableChain<S> {
        if let Some(hint) = self.table.index_hints.last_mut() {
            hint.for_ = Some(scope);
        }
        self
    }
}

impl<Q, S: TableSlot<Q>> Mod<Q> for TableChain<S> {
    fn apply(self, q: &mut Q) {
        S::place(q, self.table);
    }
}

/// Mark a table reference `LATERAL`, refusing the one item shape the grammar
/// has no sentence for: a bare table or CTE name ([`Expr::Ident`]).
///
/// MySQL's `LATERAL` (8.0.14, *15.2.15.9 Lateral Derived Tables*) is
/// grammatical only in front of a derived table — the manual's production is
/// `LATERAL table_subquery [AS] alias` and nothing else takes the keyword, and
/// there is nothing for it to mean on a name anyway (a base table cannot
/// reference the items before it). The item is wrapped in [`LateralBareName`],
/// which records the error `build()` surfaces — catching the mistake at the
/// `.lateral()` call rather than letting valid-looking SQL leave with
/// ``LATERAL `posts` `` in it.
///
/// Only [`Expr::Ident`] items are judged. A raw fragment could be anything —
/// progressive enhancement means hand-written SQL is trusted — and derived
/// tables arrive as other variants.
fn lateral_table(mut table: TableRef) -> TableRef {
    table.lateral = true;
    if matches!(table.expression, Some(Expr::Ident(_))) {
        let name = table.expression.take().expect("just matched Some");
        table.expression = Some(Expr::custom(LateralBareName(name)));
    }
    table
}

/// A from-item that was marked `LATERAL` but is a bare table or CTE name.
///
/// The chain methods swap this in when `.lateral()` is called on such an item,
/// so the mistake is caught where it is made; the item still renders, keeping
/// the debug print honest, while `build()` refuses. The same judgment, for the
/// same reason, as keelson-psql's `LateralBareName`.
#[derive(Debug)]
struct LateralBareName(Expr);

impl Expression for LateralBareName {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.record_error(keelson_core::Error::other(
            "LATERAL is set on a bare table or CTE name, but LATERAL can precede only a derived table",
        ));
        w.write_expr(&self.0);
    }
}

// ---------------------------------------------------------------------------
// Joins
// ---------------------------------------------------------------------------

/// A join under construction.
///
/// `ON` and `USING` are alternatives and `NATURAL` excludes both; nothing enforces
/// that here, because the caller picks one method and the grammar is what says so.
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

/// `LEFT JOIN <table>`. MySQL requires an `ON` or `USING` on this one.
pub fn left_join(table: impl IntoExpr) -> JoinChain {
    join_chain(JoinKind::Left, table)
}

/// `RIGHT JOIN <table>`. MySQL requires an `ON` or `USING` on this one.
pub fn right_join(table: impl IntoExpr) -> JoinChain {
    join_chain(JoinKind::Right, table)
}

/// `CROSS JOIN <table>`.
///
/// In MySQL `JOIN`, `CROSS JOIN` and `INNER JOIN` are syntactic equivalents, so
/// unlike PostgreSQL this one *does* take an `ON` or `USING` — hence a
/// [`PlainJoinChain`] rather than a stripped-down one. What it does not take is
/// `NATURAL`, which the grammar allows only on `INNER`, `LEFT` and `RIGHT`.
pub fn cross_join(table: impl IntoExpr) -> PlainJoinChain {
    PlainJoinChain(join_chain(JoinKind::Cross, table))
}

/// `STRAIGHT_JOIN <table>` — an `INNER JOIN` that forbids the optimizer from
/// reading the right table first.
///
/// The join operator, not the [`straight`] modifier.
pub fn straight_join(table: impl IntoExpr) -> PlainJoinChain {
    PlainJoinChain(join_chain(JoinKind::Custom("STRAIGHT_JOIN".into()), table))
}

impl JoinChain {
    /// `AS \`alias\`` on the joined table.
    #[must_use]
    pub fn as_(mut self, alias: impl Into<Cow<'static, str>>) -> JoinChain {
        self.join.to.set_alias(alias);
        self
    }

    /// Column aliases on the joined derived table.
    #[must_use]
    pub fn columns(
        mut self,
        columns: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) -> JoinChain {
        self.join.to.set_columns(columns);
        self
    }

    /// `LATERAL` on the joined item — what lets a joined derived table see the
    /// columns of the item it is joined to.
    ///
    /// Only grammatical in front of a derived table; on a bare table or CTE
    /// name this records a `build()` error instead, because
    /// ``JOIN LATERAL `posts` `` is a syntax error with nothing to mean.
    #[must_use]
    pub fn lateral(mut self) -> JoinChain {
        self.join.to = lateral_table(self.join.to);
        self
    }

    /// `PARTITION (…)` on the joined table.
    #[must_use]
    pub fn partition(
        mut self,
        partitions: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) -> JoinChain {
        self.join.to.append_partition(partitions);
        self
    }

    /// `USE INDEX (…)` on the joined table.
    #[must_use]
    pub fn use_index(
        self,
        indexes: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) -> JoinChain {
        self.index_hint(IndexHintKind::Use, indexes)
    }

    /// `IGNORE INDEX (…)` on the joined table.
    #[must_use]
    pub fn ignore_index(
        self,
        indexes: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) -> JoinChain {
        self.index_hint(IndexHintKind::Ignore, indexes)
    }

    /// `FORCE INDEX (…)` on the joined table.
    #[must_use]
    pub fn force_index(
        self,
        indexes: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) -> JoinChain {
        self.index_hint(IndexHintKind::Force, indexes)
    }

    /// `FOR JOIN` on the hint just added.
    #[must_use]
    pub fn for_join(self) -> JoinChain {
        self.hint_scope(IndexHintScope::Join)
    }

    /// `FOR ORDER BY` on the hint just added.
    #[must_use]
    pub fn for_order_by(self) -> JoinChain {
        self.hint_scope(IndexHintScope::OrderBy)
    }

    /// `FOR GROUP BY` on the hint just added.
    #[must_use]
    pub fn for_group_by(self) -> JoinChain {
        self.hint_scope(IndexHintScope::GroupBy)
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

    /// `USING (\`a\`, \`b\`)` — join on equally named columns, merging them.
    #[must_use]
    pub fn using(
        mut self,
        columns: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) -> JoinChain {
        self.join.append_using(columns);
        self
    }

    fn index_hint(
        mut self,
        kind: IndexHintKind,
        indexes: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) -> JoinChain {
        self.join
            .to
            .append_index_hint(IndexHint::new(kind, indexes));
        self
    }

    fn hint_scope(mut self, scope: IndexHintScope) -> JoinChain {
        if let Some(hint) = self.join.to.index_hints.last_mut() {
            hint.for_ = Some(scope);
        }
        self
    }
}

impl<Q: HasJoins> Mod<Q> for JoinChain {
    fn apply(self, q: &mut Q) {
        q.joins_mut().push(self.join);
    }
}

/// A `CROSS JOIN` or `STRAIGHT_JOIN` under construction — [`JoinChain`] without
/// [`natural`](JoinChain::natural), which MySQL's grammar allows only on `INNER`,
/// `LEFT` and `RIGHT`.
#[derive(Debug, Clone)]
pub struct PlainJoinChain(JoinChain);

impl PlainJoinChain {
    /// `AS \`alias\`` on the joined table.
    #[must_use]
    pub fn as_(self, alias: impl Into<Cow<'static, str>>) -> PlainJoinChain {
        PlainJoinChain(self.0.as_(alias))
    }

    /// Column aliases on the joined derived table.
    #[must_use]
    pub fn columns(
        self,
        columns: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) -> PlainJoinChain {
        PlainJoinChain(self.0.columns(columns))
    }

    /// `LATERAL` on the joined table.
    #[must_use]
    pub fn lateral(self) -> PlainJoinChain {
        PlainJoinChain(self.0.lateral())
    }

    /// `PARTITION (…)` on the joined table.
    #[must_use]
    pub fn partition(
        self,
        partitions: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) -> PlainJoinChain {
        PlainJoinChain(self.0.partition(partitions))
    }

    /// `USE INDEX (…)` on the joined table.
    #[must_use]
    pub fn use_index(
        self,
        indexes: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) -> PlainJoinChain {
        PlainJoinChain(self.0.use_index(indexes))
    }

    /// `IGNORE INDEX (…)` on the joined table.
    #[must_use]
    pub fn ignore_index(
        self,
        indexes: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) -> PlainJoinChain {
        PlainJoinChain(self.0.ignore_index(indexes))
    }

    /// `FORCE INDEX (…)` on the joined table.
    #[must_use]
    pub fn force_index(
        self,
        indexes: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) -> PlainJoinChain {
        PlainJoinChain(self.0.force_index(indexes))
    }

    /// `ON condition`.
    #[must_use]
    pub fn on(self, condition: impl IntoExpr) -> PlainJoinChain {
        PlainJoinChain(self.0.on(condition))
    }

    /// `ON (a = b)`.
    #[must_use]
    pub fn on_eq(self, a: impl IntoExpr, b: impl IntoExpr) -> PlainJoinChain {
        PlainJoinChain(self.0.on_eq(a, b))
    }

    /// `USING (\`a\`, \`b\`)`.
    #[must_use]
    pub fn using(
        self,
        columns: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) -> PlainJoinChain {
        PlainJoinChain(self.0.using(columns))
    }
}

impl<Q: HasJoins> Mod<Q> for PlainJoinChain {
    fn apply(self, q: &mut Q) {
        self.0.apply(q);
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
pub fn having<Q: HasHaving>(condition: impl IntoExpr) -> impl Mod<Q> {
    let condition = condition.into_expr();
    mod_fn(move |q: &mut Q| q.having_mut().append_having(condition))
}

/// Add a grouping expression.
///
/// MySQL's `grouping_element` is a plain expression: there is no `ROLLUP(…)`,
/// `CUBE(…)` or `GROUPING SETS(…)` element, and no `GROUP BY DISTINCT`. The only
/// modifier is [`with_rollup`], and it applies to the whole clause.
pub fn group_by<Q: HasGroupBy>(group: impl IntoExpr) -> impl Mod<Q> {
    let group = group.into_expr();
    mod_fn(move |q: &mut Q| q.group_by_mut().append_group(group))
}

/// `GROUP BY … WITH ROLLUP` — add the super-aggregate rows.
pub fn with_rollup<Q: HasGroupBy>() -> impl Mod<Q> {
    mod_fn(move |q: &mut Q| q.group_by_mut().with = Some(GroupByWith::Rollup))
}

// ---------------------------------------------------------------------------
// WINDOW
// ---------------------------------------------------------------------------

/// `WINDOW \`name\` AS (definition)`, the definition built from `mysql::window::*`
/// and `mysql::frame::*` mods.
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

/// Which `ORDER BY` an [`OrderChain`] appends to.
pub trait OrderSlot<Q> {
    /// The clause to append to.
    fn slot(q: &mut Q) -> &mut OrderBy;
}

/// The statement's — or window's — own `ORDER BY`.
#[derive(Debug, Clone, Copy, Default)]
pub struct DirectOrder;

/// The `ORDER BY` that applies to the result of a set operation.
#[derive(Debug, Clone, Copy, Default)]
pub struct CombinedOrder;

impl<Q: HasOrderBy> OrderSlot<Q> for DirectOrder {
    fn slot(q: &mut Q) -> &mut OrderBy {
        q.order_by_mut()
    }
}

impl<Q: HasCombines> OrderSlot<Q> for CombinedOrder {
    fn slot(q: &mut Q) -> &mut OrderBy {
        &mut q.combines_mut().order_by
    }
}

/// One sort key under construction.
///
/// MySQL's `ORDER BY` takes a direction and — through the expression — a collation,
/// and nothing else: there is no `NULLS FIRST`/`NULLS LAST` and no
/// `USING operator`, so those methods do not exist here.
#[derive(Debug, Clone)]
pub struct OrderChain<S> {
    def: OrderDef,
    slot: PhantomData<S>,
}

/// `ORDER BY expression`.
pub fn order_by(expression: impl IntoExpr) -> OrderChain<DirectOrder> {
    OrderChain {
        def: OrderDef::new(expression),
        slot: PhantomData,
    }
}

/// `ORDER BY` over the result of a `UNION`/`INTERSECT`/`EXCEPT`, rather than over
/// this query.
pub fn order_by_combined(expression: impl IntoExpr) -> OrderChain<CombinedOrder> {
    OrderChain {
        def: OrderDef::new(expression),
        slot: PhantomData,
    }
}

impl<S> OrderChain<S> {
    /// `ASC` — the default, written out.
    #[must_use]
    pub fn asc(mut self) -> OrderChain<S> {
        self.def.direction = Some(OrderDirection::Asc);
        self
    }

    /// `DESC`.
    #[must_use]
    pub fn desc(mut self) -> OrderChain<S> {
        self.def.direction = Some(OrderDirection::Desc);
        self
    }

    /// `COLLATE \`name\``, written between the expression and the direction.
    ///
    /// The collation name is quoted as an identifier, which MySQL accepts wherever
    /// a `collation_name` is expected.
    #[must_use]
    pub fn collate(mut self, name: impl Into<Cow<'static, str>>) -> OrderChain<S> {
        self.def.collation = Some(name.into());
        self
    }
}

impl<Q, S: OrderSlot<Q>> Mod<Q> for OrderChain<S> {
    fn apply(self, q: &mut Q) {
        // `OrderBy` stores expressions, and an `OrderDef` reaches one as
        // `Expr::Custom` — the route every struct-shaped clause item takes. Nothing
        // groups it, so ``ORDER BY `name` DESC`` keeps its shape.
        S::slot(q).append_order(Expr::custom(self.def));
    }
}

// ---------------------------------------------------------------------------
// LIMIT / OFFSET
// ---------------------------------------------------------------------------

/// `LIMIT count`.
///
/// A number is a literal — `limit(20)` gives `LIMIT 20` — because
/// [`keelson_core::expr::IntoExpr`] makes it one. `limit(arg(20))` binds
/// it instead, which MySQL permits in a prepared statement.
pub fn limit<Q: HasLimit>(count: impl IntoExpr) -> impl Mod<Q> {
    let count = count.into_expr();
    mod_fn(move |q: &mut Q| q.limit_mut().set_limit(count))
}

/// `OFFSET start`.
///
/// MySQL spells `LIMIT` and `OFFSET` as one clause, so this needs a [`limit`] to
/// parse. There is no `LIMIT ALL`.
pub fn offset<Q: HasOffset>(start: impl IntoExpr) -> impl Mod<Q> {
    let start = start.into_expr();
    mod_fn(move |q: &mut Q| q.offset_mut().set_offset(start))
}

/// `LIMIT` over the result of a set operation rather than over this query.
pub fn limit_combined<Q: HasCombines>(count: impl IntoExpr) -> impl Mod<Q> {
    let count = count.into_expr();
    mod_fn(move |q: &mut Q| q.combines_mut().limit.set_limit(count))
}

/// `OFFSET` over the result of a set operation rather than over this query.
pub fn offset_combined<Q: HasCombines>(start: impl IntoExpr) -> impl Mod<Q> {
    let start = start.into_expr();
    mod_fn(move |q: &mut Q| q.combines_mut().offset.set_offset(start))
}

// ---------------------------------------------------------------------------
// Locking
// ---------------------------------------------------------------------------

/// A `FOR …` locking clause under construction.
#[derive(Debug, Clone)]
pub struct LockChain {
    lock: Lock,
}

/// `FOR UPDATE` — lock the rows for writing.
///
/// MySQL has only two strengths. `FOR NO KEY UPDATE` and `FOR KEY SHARE` are
/// PostgreSQL's, and there is nothing here that produces them.
pub fn for_update() -> LockChain {
    LockChain {
        lock: Lock::new(LockStrength::Update),
    }
}

/// `FOR SHARE` (MySQL 8.0) — lock the rows for reading.
/// [`select::lock_in_share_mode`](crate::select::lock_in_share_mode) is the older
/// spelling.
pub fn for_share() -> LockChain {
    LockChain {
        lock: Lock::new(LockStrength::Share),
    }
}

impl LockChain {
    /// `OF \`t\`` — restrict the lock to these tables of the statement.
    #[must_use]
    pub fn of(
        mut self,
        tables: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) -> LockChain {
        self.lock.append_table(tables);
        self
    }

    /// `NOWAIT` — fail rather than wait for a locked row.
    #[must_use]
    pub fn no_wait(mut self) -> LockChain {
        self.lock.wait = Some(LockWait::NoWait);
        self
    }

    /// `SKIP LOCKED` — leave a locked row out of the result.
    #[must_use]
    pub fn skip_locked(mut self) -> LockChain {
        self.lock.wait = Some(LockWait::SkipLocked);
        self
    }
}

impl<Q: HasLocks> Mod<Q> for LockChain {
    fn apply(self, q: &mut Q) {
        q.locks_mut().append_lock(self.lock);
    }
}

// ---------------------------------------------------------------------------
// Set operations
// ---------------------------------------------------------------------------

fn combine<Q: HasCombines>(op: SetOp, all: bool, query: impl IntoExpr) -> impl Mod<Q> {
    let mut c = Combine::new(op, query);
    c.all = all;
    mod_fn(move |q: &mut Q| q.combines_mut().append_combine(c))
}

/// `UNION (query)` — rows of either, duplicates removed. `UNION DISTINCT` is the
/// same thing spelled out, and is not representable because it adds nothing.
pub fn union<Q: HasCombines>(query: impl IntoExpr) -> impl Mod<Q> {
    combine(SetOp::Union, false, query)
}

/// `UNION ALL (query)` — rows of either, duplicates kept.
pub fn union_all<Q: HasCombines>(query: impl IntoExpr) -> impl Mod<Q> {
    combine(SetOp::Union, true, query)
}

/// `INTERSECT (query)` — rows of both (MySQL 8.0.31).
pub fn intersect<Q: HasCombines>(query: impl IntoExpr) -> impl Mod<Q> {
    combine(SetOp::Intersect, false, query)
}

/// `INTERSECT ALL (query)`.
pub fn intersect_all<Q: HasCombines>(query: impl IntoExpr) -> impl Mod<Q> {
    combine(SetOp::Intersect, true, query)
}

/// `EXCEPT (query)` — rows of this query that are not in the other (MySQL 8.0.31).
pub fn except<Q: HasCombines>(query: impl IntoExpr) -> impl Mod<Q> {
    combine(SetOp::Except, false, query)
}

/// `EXCEPT ALL (query)`.
pub fn except_all<Q: HasCombines>(query: impl IntoExpr) -> impl Mod<Q> {
    combine(SetOp::Except, true, query)
}

// ---------------------------------------------------------------------------
// SET
// ---------------------------------------------------------------------------

/// One assignment, written out: ``set(quote("a").eq(arg(1)))``.
///
/// A whole expression rather than a column/value pair, so that the left-hand side
/// can be qualified — which a multiple-table `UPDATE` needs.
pub fn set<Q: HasSet>(assignment: impl IntoExpr) -> impl Mod<Q> {
    let assignment = assignment.into_expr();
    mod_fn(move |q: &mut Q| q.set_mut().append_set(assignment))
}

/// The left-hand side of an assignment: `set_col("a").to(arg(1))`.
///
/// Not a mod on its own — an assignment with no value is not one — so
/// [`to`](SetChain::to) or [`to_arg`](SetChain::to_arg) has to be called.
#[derive(Debug, Clone)]
pub struct SetChain {
    column: Expr,
}

/// Assign to a column. `set_col(("t", "a"))` qualifies it, which a multiple-table
/// `UPDATE` requires.
pub fn set_col(column: impl IntoIdent) -> SetChain {
    SetChain {
        column: Expr::ident(column),
    }
}

impl SetChain {
    /// ``\`col\` = value``, where `value` is an expression.
    pub fn to<Q: HasSet>(self, value: impl IntoExpr) -> impl Mod<Q> {
        set(Expr::binary(self.column, "=", value))
    }

    /// ``\`col\` = ?`` — bind `value` as an argument.
    pub fn to_arg<Q: HasSet>(self, value: impl keelson_core::ToValue) -> impl Mod<Q> {
        set(Expr::binary(self.column, "=", Expr::arg(value)))
    }
}

/// ``\`col\` = VALUES(\`col\`)`` for each column — the pre-8.0.19 body of an upsert.
///
/// Only meaningful inside [`on_duplicate_key_update`]. MySQL deprecates this form
/// in favour of [`set_row`], but 8.4 still accepts it.
pub fn set_values<Q: HasSet>(
    columns: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
) -> impl Mod<Q> {
    let assignments: Vec<Expr> = columns
        .into_iter()
        .map(Into::into)
        .filter(|c: &Cow<'static, str>| !c.is_empty())
        .map(|c| Expr::binary(Expr::ident(c.clone()), "=", values_of(c)))
        .collect();
    mod_fn(move |q: &mut Q| q.set_mut().append_sets(assignments))
}

/// ``\`col\` = \`alias\`.\`col\``` for each column — the 8.0.19 body of an upsert,
/// naming the incoming row through the alias set by
/// [`insert::as_`](crate::insert::as_).
pub fn set_row<Q: HasSet>(
    alias: impl Into<Cow<'static, str>>,
    columns: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
) -> impl Mod<Q> {
    let alias = alias.into();
    let assignments: Vec<Expr> = columns
        .into_iter()
        .map(Into::into)
        .filter(|c: &Cow<'static, str>| !c.is_empty())
        .map(|c| Expr::binary(Expr::ident(c.clone()), "=", row_value(alias.clone(), c)))
        .collect();
    mod_fn(move |q: &mut Q| q.set_mut().append_sets(assignments))
}

// ---------------------------------------------------------------------------
// VALUES
// ---------------------------------------------------------------------------

/// One row of `VALUES`. Several calls append several rows.
///
/// A cell may be `DEFAULT`, which is [`raw("DEFAULT")`](crate::raw).
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
/// rather than things that combine. A CTE for the sub-query goes *on the
/// sub-query* — MySQL puts the `WITH` after the `INSERT`, never before it.
pub fn values_from_query<Q: HasValues>(query: impl IntoExpr) -> impl Mod<Q> {
    let query = query.into_expr();
    mod_fn(move |q: &mut Q| *q.values_mut() = Values::from_query(query))
}

// ---------------------------------------------------------------------------
// The INSERT row alias and ON DUPLICATE KEY UPDATE
// ---------------------------------------------------------------------------

/// `AS \`alias\`` — name the row being inserted (MySQL 8.0.19), so that
/// `ON DUPLICATE KEY UPDATE` can refer to it by name.
#[derive(Debug, Clone)]
pub struct RowAliasChain {
    alias: RowAlias,
}

/// `AS \`alias\`` on an `INSERT`. Use [`set_row`] to assign from it.
pub fn as_(alias: impl Into<Cow<'static, str>>) -> RowAliasChain {
    RowAliasChain {
        alias: RowAlias::new(alias),
    }
}

impl RowAliasChain {
    /// Per-column aliases: `AS \`new\` (\`a\`, \`b\`)`.
    #[must_use]
    pub fn columns(
        mut self,
        columns: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) -> RowAliasChain {
        self.alias.columns = columns.into_iter().map(Into::into).collect();
        self
    }
}

impl<Q: HasRowAlias> Mod<Q> for RowAliasChain {
    fn apply(self, q: &mut Q) {
        *q.row_alias_mut() = self.alias;
    }
}

/// `ON DUPLICATE KEY UPDATE assignment_list` — MySQL's upsert.
///
/// The body is built from mods against a bare
/// [`keelson_core::clause::Set`], which implements
/// [`keelson_core::clause::HasSet`] reflexively: [`set`], [`set_col`],
/// [`set_values`] and [`set_row`] all apply, and they are the same functions that
/// build an `INSERT … SET`.
pub fn on_duplicate_key_update<Q: HasDuplicateKeyUpdate>(body: impl Mod<Set>) -> impl Mod<Q> {
    let mut set = Set::default();
    body.apply(&mut set);
    mod_fn(move |q: &mut Q| q.duplicate_key_update_mut().append_sets(set.exprs))
}
