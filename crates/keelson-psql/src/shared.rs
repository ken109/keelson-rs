//! The mods, written once against the `Has*` traits and re-exported per statement.
//!
//! Nothing in here names a query type. A mod is `Mod<Q>` for every `Q` that
//! implements the clause trait it needs, so `where_` is one function that serves
//! `SELECT`, `UPDATE`, `DELETE` *and* the `DO UPDATE` body of an `ON CONFLICT` —
//! and refuses to compile against an `INSERT`, which has no `WHERE`.
//!
//! Two shapes recur.
//!
//! **A plain mod** is a function returning `impl Mod<Q>`, built from
//! [`mod_fn`].
//!
//! **A chain** is a struct that is itself a mod and has builder methods — bob's
//! `FromChain`, `JoinChain`, `CTEChain`, `LockChain`, `OrderBy`. It exists wherever
//! a clause has decorations that must be set together rather than one mod at a time:
//! `from(..).as_("u").only()` replaces the whole from-item once, so no later mod can
//! silently wipe an earlier one.
//!
//! **A slot** is how one chain type reaches different fields of different queries.
//! `select::from` and `delete::from` are the same chain with a different
//! [`TableSlot`]; the marker is a type parameter, so which field is written is
//! decided at compile time and there is one implementation of the builder methods.

use std::borrow::Cow;
use std::marker::PhantomData;

use keelson_core::clause::{
    Combine, ConflictClause, ConflictTarget, Cte, CteCycle, CteSearch, Fetch, HasCombines,
    HasConflict, HasFetch, HasGroupBy, HasHaving, HasJoins, HasLimit, HasLocks, HasOffset,
    HasOrderBy, HasReturning, HasSelectList, HasSet, HasTableRef, HasValues, HasWhere, HasWindows,
    HasWith, Join, JoinKind, Lock, LockStrength, LockWait, NamedWindow, NullsPosition, OrderBy,
    OrderDef, OrderDirection, SearchOrder, SetOp, TableFunctions, TableRef, Values, Window,
};
use keelson_core::expr::{Expr, IntoExpr, IntoExprList, IntoIdent};
use keelson_core::{Mod, mod_fn};

use crate::extras::{Incomplete, LateralBareName, Sample, SampledTable};
use crate::function::TableFunction;
use crate::statement::{HasExtraTables, HasTargetTable};

// ---------------------------------------------------------------------------
// WITH
// ---------------------------------------------------------------------------

/// A common table expression under construction.
///
/// `with("recent", body)` is already a complete mod; the methods add the optional
/// parts of PostgreSQL's `with_query` production.
#[derive(Debug, Clone)]
pub struct CteChain {
    cte: Cte,
}

/// `WITH "name" AS (body)`.
///
/// `body` is any expression, so a hand-written fragment works; a query goes in
/// directly, because the four query types implement
/// [`IntoExpr`]. It is *not* parenthesised here —
/// [`Cte`] supplies the parentheses.
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

    /// `AS MATERIALIZED (…)` — compute it once, whatever the planner would prefer.
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

    /// `SEARCH BREADTH FIRST BY cols SET col`.
    #[must_use]
    pub fn search_breadth(
        mut self,
        set: impl Into<Cow<'static, str>>,
        columns: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) -> CteChain {
        self.cte.search = CteSearch::new(SearchOrder::Breadth, columns, set);
        self
    }

    /// `SEARCH DEPTH FIRST BY cols SET col`.
    ///
    /// bob's `SearchBreadth` sets `SearchDepth` too, which is a copy-paste slip; the
    /// two are distinct here.
    #[must_use]
    pub fn search_depth(
        mut self,
        set: impl Into<Cow<'static, str>>,
        columns: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) -> CteChain {
        self.cte.search = CteSearch::new(SearchOrder::Depth, columns, set);
        self
    }

    /// `CYCLE cols SET mark USING path`.
    #[must_use]
    pub fn cycle(
        mut self,
        set: impl Into<Cow<'static, str>>,
        using: impl Into<Cow<'static, str>>,
        columns: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) -> CteChain {
        let cycle = CteCycle::new(columns, set, using);
        self.cte.cycle = CteCycle {
            to: self.cte.cycle.to,
            default_val: self.cte.cycle.default_val,
            ..cycle
        };
        self
    }

    /// `… SET mark TO value DEFAULT value USING …`.
    ///
    /// Both halves at once, because the grammar spells them as one optional group.
    /// PostgreSQL requires *constants* here — `TO AexprConst DEFAULT AexprConst` —
    /// so use [`s`](crate::s) or [`raw`](crate::raw), never [`arg`](crate::arg).
    #[must_use]
    pub fn cycle_value(mut self, to: impl IntoExpr, default: impl IntoExpr) -> CteChain {
        self.cte.cycle.to = Some(to.into_expr());
        self.cte.cycle.default_val = Some(default.into_expr());
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
/// A property of the whole list, not of one entry: it is what makes every name in
/// the list visible to every entry.
pub fn recursive<Q: HasWith>(recursive: bool) -> impl Mod<Q> {
    mod_fn(move |q: &mut Q| q.with_mut().set_recursive(recursive))
}

// ---------------------------------------------------------------------------
// The projection
// ---------------------------------------------------------------------------

/// Add to the select list. Several calls accumulate; with none, `*` is written.
pub fn columns<Q: HasSelectList>(columns: impl IntoExprList) -> impl Mod<Q> {
    let columns = columns.into_expr_list();
    mod_fn(move |q: &mut Q| q.select_list_mut().append_select(columns))
}

/// Add to the *preload* select list, which renders after
/// [`columns`] but is counted separately.
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
/// Implemented by the three markers below rather than by a query, so the builder
/// methods are written once and `select::from` / `update::table` / `select::from_also`
/// differ only in a type parameter.
pub trait TableSlot<Q> {
    /// Put `table` where this slot means.
    fn place(q: &mut Q, table: TableRef);
}

/// The from-item slot: a `SELECT`'s `FROM`, an `INSERT`'s target, an `UPDATE`'s
/// `FROM`, a `DELETE`'s `USING`.
#[derive(Debug, Clone, Copy, Default)]
pub struct FromSlot;

/// The target slot: the table an `UPDATE` writes to or a `DELETE` removes from.
#[derive(Debug, Clone, Copy, Default)]
pub struct TargetSlot;

/// The additional-from-items slot, for the second and later entries of a
/// comma-separated `FROM`/`USING` list.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExtraSlot;

impl<Q: HasTableRef> TableSlot<Q> for FromSlot {
    fn place(q: &mut Q, mut table: TableRef) {
        // Joins already appended to the slot survive, so `from(..)` written after
        // `inner_join(..)` is not a way to silently lose them. bob's `SetTable`
        // keeps them for the same reason.
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

/// A from-item under construction: the table plus every decoration PostgreSQL's
/// `from_item` allows in front of the joins.
///
/// ```text
/// [ ONLY ] table_name [ * ] [ [ AS ] alias [ ( column_alias [, ...] ) ] ]
///          [ TABLESAMPLE sampling_method ( argument [, ...] ) [ REPEATABLE ( seed ) ] ]
/// [ LATERAL ] function_name ( … ) [ WITH ORDINALITY ] [ [ AS ] alias [ ( … ) ] ]
/// ```
#[derive(Debug, Clone)]
pub struct TableChain<S> {
    table: TableRef,
    sample: Option<Sample>,
    slot: PhantomData<S>,
}

fn table_chain<S>(table: impl IntoExpr) -> TableChain<S> {
    TableChain {
        table: TableRef::new(table),
        sample: None,
        slot: PhantomData,
    }
}

/// A from-item: `FROM <table>`.
pub fn from_item(table: impl IntoExpr) -> TableChain<FromSlot> {
    table_chain(table)
}

/// A further comma-separated from-item. A comma there means `CROSS JOIN`.
pub fn extra_from_item(table: impl IntoExpr) -> TableChain<ExtraSlot> {
    table_chain(table)
}

/// The statement's target table.
pub fn target_table(table: impl IntoExpr) -> TableChain<TargetSlot> {
    table_chain(table)
}

/// A from-item that is one or more set-returning function calls.
///
/// One function is written plainly; two or more become `ROWS FROM (f(), g())`,
/// because `ROWS FROM (f())` and `f()` mean the same thing and the shorter form is
/// what a person writes.
///
/// Each item is a [`Function`](crate::Function) or a [`TableFunction`] — the
/// latter is what [`Function::columns`](crate::Function::columns)/
/// [`Function::as_table`](crate::Function::as_table) return, carrying the
/// `func_alias_clause` a record-returning function needs. A list mixing the two
/// converts the plain ones with `TableFunction::from`.
///
/// *No* functions is not a from-item: an empty [`TableFunctions`] renders nothing,
/// which would leave the `FROM ` in front of it dangling and make `build()` hand
/// back unparseable SQL with no error at all. See
/// [`Error::Incomplete`](keelson_core::Error::Incomplete).
pub fn from_functions<F>(functions: impl IntoIterator<Item = F>) -> TableChain<FromSlot>
where
    F: Into<TableFunction>,
{
    let list: Vec<Expr> = functions
        .into_iter()
        .map(|f| f.into().into_expr())
        .collect();
    if list.is_empty() {
        return table_chain(Expr::custom(Incomplete("the functions of a from-item")));
    }
    table_chain(Expr::custom(TableFunctions::new(list)))
}

impl<S> TableChain<S> {
    /// `AS "alias"`.
    #[must_use]
    pub fn as_(mut self, alias: impl Into<Cow<'static, str>>) -> TableChain<S> {
        self.table.set_alias(alias);
        self
    }

    /// Column aliases: `AS "t" ("a", "b")`. For an `INSERT` this is the insert
    /// column list instead.
    #[must_use]
    pub fn columns(
        mut self,
        columns: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) -> TableChain<S> {
        self.table.set_columns(columns);
        self
    }

    /// `ONLY` — do not include rows from inheriting tables.
    #[must_use]
    pub fn only(mut self) -> TableChain<S> {
        self.table.only = true;
        self
    }

    /// `LATERAL` — let this item refer to columns of the ones before it.
    ///
    /// Only grammatical in front of a sub-query or function item; on a bare
    /// table or CTE name this records a `build()` error instead, because
    /// `FROM LATERAL "posts"` is a syntax error with nothing to mean.
    #[must_use]
    pub fn lateral(mut self) -> TableChain<S> {
        self.table = lateral_table(self.table);
        self
    }

    /// `WITH ORDINALITY` — add a `bigint` column numbering the rows.
    #[must_use]
    pub fn with_ordinality(mut self) -> TableChain<S> {
        self.table.with_ordinality = true;
        self
    }

    /// `TABLESAMPLE method (args)`, e.g. `tablesample("BERNOULLI", 10)`.
    #[must_use]
    pub fn tablesample(
        mut self,
        method: impl Into<Cow<'static, str>>,
        args: impl IntoExprList,
    ) -> TableChain<S> {
        self.sample = Some(Sample {
            method: method.into(),
            args: args.into_expr_list(),
            repeatable: None,
        });
        self
    }

    /// `REPEATABLE (seed)` — the same sample every time.
    ///
    /// Ignored without a [`tablesample`](Self::tablesample), because
    /// `REPEATABLE` is a modifier of one and means nothing alone.
    #[must_use]
    pub fn repeatable(mut self, seed: impl IntoExpr) -> TableChain<S> {
        if let Some(sample) = &mut self.sample {
            sample.repeatable = Some(seed.into_expr());
        }
        self
    }
}

/// Mark a table reference `LATERAL`, refusing the one item shape the grammar
/// has no sentence for: a bare table or CTE name ([`Expr::Ident`]). The item
/// is wrapped in [`LateralBareName`], which records the error `build()`
/// surfaces — catching the mistake at the `.lateral()` call rather than
/// letting valid-looking SQL leave with `LATERAL "posts"` in it.
fn lateral_table(mut table: TableRef) -> TableRef {
    table.lateral = true;
    if matches!(table.expression, Some(Expr::Ident(_))) {
        let name = table.expression.take().expect("just matched Some");
        table.expression = Some(Expr::custom(LateralBareName(name)));
    }
    table
}

/// Fold an alias, column aliases and a sampling clause into one expression.
///
/// See [`SampledTable`]: `TABLESAMPLE` has to be written after the alias, and
/// [`TableRef`] has no slot there.
fn finish_table(mut table: TableRef, sample: Option<Sample>) -> TableRef {
    // Both conditions are checked before anything is moved out: taking the
    // expression as part of a tuple pattern would empty the table reference even on
    // the overwhelmingly common no-sampling path.
    let Some(sample) = sample else {
        return table;
    };
    let Some(expression) = table.expression.take() else {
        return table;
    };
    table.expression = Some(Expr::custom(SampledTable {
        table: expression,
        alias: table.alias.take(),
        columns: std::mem::take(&mut table.columns),
        sample,
    }));
    table
}

impl<Q, S: TableSlot<Q>> Mod<Q> for TableChain<S> {
    fn apply(self, q: &mut Q) {
        S::place(q, finish_table(self.table, self.sample));
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
    sample: Option<Sample>,
}

fn join_chain(kind: JoinKind, to: impl IntoExpr) -> JoinChain {
    JoinChain {
        join: Join::new(kind, TableRef::new(to)),
        sample: None,
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

/// `RIGHT JOIN <table>`.
pub fn right_join(table: impl IntoExpr) -> JoinChain {
    join_chain(JoinKind::Right, table)
}

/// `FULL JOIN <table>`.
pub fn full_join(table: impl IntoExpr) -> JoinChain {
    join_chain(JoinKind::Full, table)
}

/// `CROSS JOIN <table>`.
///
/// A narrower chain than the others: a cross join takes neither `ON`, `USING` nor
/// `NATURAL`, so those methods do not exist on it.
pub fn cross_join(table: impl IntoExpr) -> CrossJoinChain {
    CrossJoinChain(join_chain(JoinKind::Cross, table))
}

impl JoinChain {
    /// `AS "alias"` on the joined table.
    #[must_use]
    pub fn as_(mut self, alias: impl Into<Cow<'static, str>>) -> JoinChain {
        self.join.to.set_alias(alias);
        self
    }

    /// Column aliases on the joined table.
    #[must_use]
    pub fn columns(
        mut self,
        columns: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) -> JoinChain {
        self.join.to.set_columns(columns);
        self
    }

    /// `ONLY` on the joined table.
    #[must_use]
    pub fn only(mut self) -> JoinChain {
        self.join.to.only = true;
        self
    }

    /// `LATERAL` on the joined item — which is what lets a joined sub-query see
    /// the columns of the item it is joined to.
    ///
    /// Only grammatical in front of a sub-query or function item; on a bare
    /// table or CTE name this records a `build()` error instead, because
    /// `JOIN LATERAL "posts"` is a syntax error with nothing to mean.
    #[must_use]
    pub fn lateral(mut self) -> JoinChain {
        self.join.to = lateral_table(self.join.to);
        self
    }

    /// `WITH ORDINALITY` on the joined function.
    #[must_use]
    pub fn with_ordinality(mut self) -> JoinChain {
        self.join.to.with_ordinality = true;
        self
    }

    /// `TABLESAMPLE method (args)` on the joined table.
    #[must_use]
    pub fn tablesample(
        mut self,
        method: impl Into<Cow<'static, str>>,
        args: impl IntoExprList,
    ) -> JoinChain {
        self.sample = Some(Sample {
            method: method.into(),
            args: args.into_expr_list(),
            repeatable: None,
        });
        self
    }

    /// `REPEATABLE (seed)` on the joined table's sampling clause.
    #[must_use]
    pub fn repeatable(mut self, seed: impl IntoExpr) -> JoinChain {
        if let Some(sample) = &mut self.sample {
            sample.repeatable = Some(seed.into_expr());
        }
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

    /// `USING (…) AS "alias"` — name the row of merged join columns
    /// (PostgreSQL 16+), so `"alias"."id"` refers to the merged column.
    ///
    /// Belongs to the `USING` clause: without [`using`](Self::using) columns
    /// this records a `build()` error, because there is no merged row for the
    /// alias to name.
    #[must_use]
    pub fn using_alias(mut self, alias: impl Into<Cow<'static, str>>) -> JoinChain {
        self.join.using_alias = Some(alias.into());
        self
    }
}

impl From<JoinChain> for Join {
    fn from(chain: JoinChain) -> Join {
        let JoinChain { mut join, sample } = chain;
        join.to = finish_table(join.to, sample);
        join
    }
}

impl<Q: HasJoins> Mod<Q> for JoinChain {
    fn apply(self, q: &mut Q) {
        q.joins_mut().push(self.into());
    }
}

/// A `CROSS JOIN` under construction — [`JoinChain`] without the condition
/// methods, because a cross join has no condition.
#[derive(Debug, Clone)]
pub struct CrossJoinChain(JoinChain);

impl CrossJoinChain {
    /// `AS "alias"` on the joined table.
    #[must_use]
    pub fn as_(self, alias: impl Into<Cow<'static, str>>) -> CrossJoinChain {
        CrossJoinChain(self.0.as_(alias))
    }

    /// Column aliases on the joined table.
    #[must_use]
    pub fn columns(
        self,
        columns: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) -> CrossJoinChain {
        CrossJoinChain(self.0.columns(columns))
    }

    /// `ONLY` on the joined table.
    #[must_use]
    pub fn only(self) -> CrossJoinChain {
        CrossJoinChain(self.0.only())
    }

    /// `LATERAL` on the joined table.
    #[must_use]
    pub fn lateral(self) -> CrossJoinChain {
        CrossJoinChain(self.0.lateral())
    }

    /// `WITH ORDINALITY` on the joined function.
    #[must_use]
    pub fn with_ordinality(self) -> CrossJoinChain {
        CrossJoinChain(self.0.with_ordinality())
    }

    /// `TABLESAMPLE method (args)` on the cross-joined table.
    ///
    /// Grammatical here because both operands of `gram.y`'s
    /// `table_ref CROSS JOIN table_ref` are `table_ref`s, and a `table_ref` is
    /// `relation_expr opt_alias_clause tablesample_clause`.
    #[must_use]
    pub fn tablesample(
        self,
        method: impl Into<Cow<'static, str>>,
        args: impl IntoExprList,
    ) -> CrossJoinChain {
        CrossJoinChain(self.0.tablesample(method, args))
    }

    /// `REPEATABLE (seed)` on the cross-joined table's sampling clause.
    ///
    /// Ignored without a [`tablesample`](Self::tablesample), like
    /// [`TableChain::repeatable`].
    #[must_use]
    pub fn repeatable(self, seed: impl IntoExpr) -> CrossJoinChain {
        CrossJoinChain(self.0.repeatable(seed))
    }
}

impl From<CrossJoinChain> for Join {
    fn from(chain: CrossJoinChain) -> Join {
        chain.0.into()
    }
}

impl<Q: HasJoins> Mod<Q> for CrossJoinChain {
    fn apply(self, q: &mut Q) {
        self.0.apply(q);
    }
}

impl TableChain<ExtraSlot> {
    /// A join hanging off *this* comma-separated item rather than off the
    /// leading one: `FROM "a", "b" INNER JOIN "c" ON …`.
    ///
    /// Grammatical because gram.y's `from_list` is `table_ref (',' table_ref)*`
    /// and *every* `table_ref` — not just the first — may be a `joined_table`.
    /// The join binds tighter than the comma, so `"b" INNER JOIN "c"` is one
    /// from-item. Takes the same [`JoinChain`]/[`CrossJoinChain`] the standalone
    /// join mods are — those mods reach the leading item through [`HasJoins`],
    /// which is why the extra items take theirs by method instead. Several
    /// calls chain several joins onto this item, exactly as several standalone
    /// mods do onto the leading one.
    #[must_use]
    pub fn join(mut self, join: impl Into<Join>) -> TableChain<ExtraSlot> {
        self.table.joins.push(join.into());
        self
    }
}

// ---------------------------------------------------------------------------
// WHERE / HAVING / GROUP BY
// ---------------------------------------------------------------------------

/// `WHERE condition`. Several calls are `AND`-joined; use [`or`](crate::or) for
/// the other connective.
pub fn where_<Q: HasWhere>(condition: impl IntoExpr) -> impl Mod<Q> {
    let condition = condition.into_expr();
    mod_fn(move |q: &mut Q| q.where_mut().append_where(condition))
}

/// `WHERE CURRENT OF "cursor"` — the row a cursor is positioned on.
///
/// An alternative to a condition rather than an addition to one, so it is the only
/// `WHERE` a statement using it should have.
pub fn where_current_of<Q: HasWhere>(cursor: impl Into<Cow<'static, str>>) -> impl Mod<Q> {
    let cursor = Expr::join((Expr::raw("CURRENT OF"), Expr::ident(cursor.into())));
    mod_fn(move |q: &mut Q| q.where_mut().append_where(cursor))
}

/// `HAVING condition`. Several calls are `AND`-joined.
pub fn having<Q: HasHaving>(condition: impl IntoExpr) -> impl Mod<Q> {
    let condition = condition.into_expr();
    mod_fn(move |q: &mut Q| q.having_mut().append_having(condition))
}

/// Add a grouping element: a plain expression, or a
/// [`rollup`](crate::rollup)/[`cube`](crate::cube)/[`grouping_sets`](crate::grouping_sets).
pub fn group_by<Q: HasGroupBy>(group: impl IntoExpr) -> impl Mod<Q> {
    let group = group.into_expr();
    mod_fn(move |q: &mut Q| q.group_by_mut().append_group(group))
}

/// `GROUP BY DISTINCT …` — de-duplicate the grouping sets a `CUBE` or `ROLLUP`
/// expands to. `ALL` is the default and is not representable.
pub fn group_by_distinct<Q: HasGroupBy>(distinct: bool) -> impl Mod<Q> {
    mod_fn(move |q: &mut Q| q.group_by_mut().distinct = distinct)
}

// ---------------------------------------------------------------------------
// WINDOW
// ---------------------------------------------------------------------------

/// `WINDOW "name" AS (definition)`, the definition built from `psql::window::*`
/// and `psql::frame::*` mods.
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
    /// `ASC`.
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

    /// `USING <operator>` — sort by a named `<`-like or `>`-like operator.
    /// PostgreSQL-only, which is why [`OrderDirection`] is not a two-variant enum.
    #[must_use]
    pub fn using(mut self, operator: impl Into<Cow<'static, str>>) -> OrderChain<S> {
        self.def.direction = Some(OrderDirection::Using(operator.into()));
        self
    }

    /// `NULLS FIRST`.
    #[must_use]
    pub fn nulls_first(mut self) -> OrderChain<S> {
        self.def.nulls = Some(NullsPosition::First);
        self
    }

    /// `NULLS LAST`.
    #[must_use]
    pub fn nulls_last(mut self) -> OrderChain<S> {
        self.def.nulls = Some(NullsPosition::Last);
        self
    }

    /// `COLLATE "name"`, written between the expression and the direction.
    #[must_use]
    pub fn collate(mut self, name: impl Into<Cow<'static, str>>) -> OrderChain<S> {
        self.def.collation = Some(name.into());
        self
    }
}

impl<Q, S: OrderSlot<Q>> Mod<Q> for OrderChain<S> {
    fn apply(self, q: &mut Q) {
        // `OrderBy` stores expressions, and an `OrderDef` reaches one as
        // `Expr::Custom` — the same route every struct-shaped clause item takes.
        // Nothing groups it, so `ORDER BY "name" DESC` keeps its shape.
        S::slot(q).append_order(Expr::custom(self.def));
    }
}

// ---------------------------------------------------------------------------
// LIMIT / OFFSET / FETCH
// ---------------------------------------------------------------------------

/// `LIMIT count`.
///
/// A number is a literal — `limit(20)` gives `LIMIT 20` — because
/// [`IntoExpr`] makes it one. `limit(arg(20))` binds
/// it instead.
pub fn limit<Q: HasLimit>(count: impl IntoExpr) -> impl Mod<Q> {
    let count = count.into_expr();
    mod_fn(move |q: &mut Q| q.limit_mut().set_limit(count))
}

/// `LIMIT ALL` — explicitly no limit, which is what the grammar's other
/// alternative is for.
pub fn limit_all<Q: HasLimit>() -> impl Mod<Q> {
    mod_fn(move |q: &mut Q| q.limit_mut().set_limit(Expr::raw("ALL")))
}

/// `OFFSET start`.
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

/// Which `FETCH` a [`FetchChain`] sets.
pub trait FetchSlot<Q> {
    /// The clause to set.
    fn slot(q: &mut Q) -> &mut Fetch;
}

/// The statement's own `FETCH`.
#[derive(Debug, Clone, Copy, Default)]
pub struct DirectFetch;

/// The `FETCH` that applies to the result of a set operation.
#[derive(Debug, Clone, Copy, Default)]
pub struct CombinedFetch;

impl<Q: HasFetch> FetchSlot<Q> for DirectFetch {
    fn slot(q: &mut Q) -> &mut Fetch {
        q.fetch_mut()
    }
}

impl<Q: HasCombines> FetchSlot<Q> for CombinedFetch {
    fn slot(q: &mut Q) -> &mut Fetch {
        &mut q.combines_mut().fetch
    }
}

/// A `FETCH` clause under construction.
#[derive(Debug, Clone)]
pub struct FetchChain<S> {
    fetch: Fetch,
    slot: PhantomData<S>,
}

/// `FETCH NEXT count ROWS ONLY` — the standard spelling of `LIMIT`, and the only
/// one that can ask for ties.
pub fn fetch(count: impl IntoExpr) -> FetchChain<DirectFetch> {
    FetchChain {
        fetch: Fetch::new(count),
        slot: PhantomData,
    }
}

/// `FETCH` over the result of a set operation rather than over this query.
pub fn fetch_combined(count: impl IntoExpr) -> FetchChain<CombinedFetch> {
    FetchChain {
        fetch: Fetch::new(count),
        slot: PhantomData,
    }
}

impl<S> FetchChain<S> {
    /// `ROWS WITH TIES` instead of `ROWS ONLY`: also return the rows that tie with
    /// the last one under the `ORDER BY`, which the statement must therefore have.
    #[must_use]
    pub fn with_ties(mut self) -> FetchChain<S> {
        self.fetch.with_ties = true;
        self
    }
}

impl<Q, S: FetchSlot<Q>> Mod<Q> for FetchChain<S> {
    fn apply(self, q: &mut Q) {
        *S::slot(q) = self.fetch;
    }
}

// ---------------------------------------------------------------------------
// Locking
// ---------------------------------------------------------------------------

/// A `FOR …` locking clause under construction.
#[derive(Debug, Clone)]
pub struct LockChain {
    lock: Lock,
}

/// `FOR UPDATE` — the strongest lock.
pub fn for_update() -> LockChain {
    LockChain {
        lock: Lock::new(LockStrength::Update),
    }
}

/// `FOR NO KEY UPDATE` — weaker than `FOR UPDATE`; does not block a foreign-key
/// reference. PostgreSQL only.
pub fn for_no_key_update() -> LockChain {
    LockChain {
        lock: Lock::new(LockStrength::NoKeyUpdate),
    }
}

/// `FOR SHARE`.
pub fn for_share() -> LockChain {
    LockChain {
        lock: Lock::new(LockStrength::Share),
    }
}

/// `FOR KEY SHARE` — the weakest. PostgreSQL only.
pub fn for_key_share() -> LockChain {
    LockChain {
        lock: Lock::new(LockStrength::KeyShare),
    }
}

impl LockChain {
    /// `OF "t"` — restrict the lock to these tables of the statement. Names, not
    /// expressions, so they are quoted.
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

/// `UNION (query)` — rows of either, duplicates removed.
pub fn union<Q: HasCombines>(query: impl IntoExpr) -> impl Mod<Q> {
    combine(SetOp::Union, false, query)
}

/// `UNION ALL (query)` — rows of either, duplicates kept.
pub fn union_all<Q: HasCombines>(query: impl IntoExpr) -> impl Mod<Q> {
    combine(SetOp::Union, true, query)
}

/// `INTERSECT (query)` — rows of both.
pub fn intersect<Q: HasCombines>(query: impl IntoExpr) -> impl Mod<Q> {
    combine(SetOp::Intersect, false, query)
}

/// `INTERSECT ALL (query)`.
pub fn intersect_all<Q: HasCombines>(query: impl IntoExpr) -> impl Mod<Q> {
    combine(SetOp::Intersect, true, query)
}

/// `EXCEPT (query)` — rows of this query that are not in the other.
pub fn except<Q: HasCombines>(query: impl IntoExpr) -> impl Mod<Q> {
    combine(SetOp::Except, false, query)
}

/// `EXCEPT ALL (query)`.
pub fn except_all<Q: HasCombines>(query: impl IntoExpr) -> impl Mod<Q> {
    combine(SetOp::Except, true, query)
}

// ---------------------------------------------------------------------------
// RETURNING
// ---------------------------------------------------------------------------

/// `RETURNING a, b`. `returning("*")` is an ordinary entry.
///
/// Whether this clause is present is what decides whether a mutation is run as a
/// query or as an exec.
pub fn returning<Q: HasReturning>(expressions: impl IntoExprList) -> impl Mod<Q> {
    let expressions = expressions.into_expr_list();
    mod_fn(move |q: &mut Q| q.returning_mut().append_returnings(expressions))
}

// ---------------------------------------------------------------------------
// SET
// ---------------------------------------------------------------------------

/// One assignment, written out: `set(quote("a").eq(arg(1)))`.
///
/// A whole expression rather than a column/value pair, because PostgreSQL's
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

/// Assign to a column: `set_col("a")`, never `set_col(("t", "a"))`.
///
/// PostgreSQL refuses a qualified assignment target — *"SET target columns cannot
/// be qualified with the relation name"* — in an `UPDATE` and in
/// `ON CONFLICT DO UPDATE` alike; `gram.y`'s `set_target: ColId opt_indirection`
/// reads the qualifier as the column name, so the statement parses and then fails
/// analysis. The parameter is [`IntoIdent`] for the dialects whose grammar does
/// allow the qualified form (MySQL's does), not as a suggestion to use it here.
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

    /// `"col" = $n` — bind `value` as an argument.
    pub fn to_arg<Q: HasSet>(self, value: impl keelson_core::ToValue) -> impl Mod<Q> {
        set(Expr::binary(self.column, "=", Expr::arg(value)))
    }
}

/// `"col" = EXCLUDED."col"` for each column — the body of an upsert.
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
                    Expr::raw(" = EXCLUDED."),
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
/// Replaces any rows already added, because the two are alternatives in the
/// grammar rather than things that combine.
pub fn values_from_query<Q: HasValues>(query: impl IntoExpr) -> impl Mod<Q> {
    let query = query.into_expr();
    mod_fn(move |q: &mut Q| *q.values_mut() = Values::from_query(query))
}

// ---------------------------------------------------------------------------
// ON CONFLICT
// ---------------------------------------------------------------------------

/// An `ON CONFLICT` clause under construction.
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
pub fn on_conflict(columns: impl IntoExprList) -> ConflictChain {
    ConflictChain {
        target: ConflictTarget::on_columns(columns),
    }
}

/// `ON CONFLICT ON CONSTRAINT "name"` — name the constraint instead of inferring
/// it. Cannot be combined with a column list, and PostgreSQL says so.
pub fn on_conflict_on_constraint(name: impl Into<Cow<'static, str>>) -> ConflictChain {
    ConflictChain {
        target: ConflictTarget::on_constraint(name),
    }
}

impl ConflictChain {
    /// The **index** predicate: `ON CONFLICT (a) WHERE …`.
    ///
    /// Matched against a partial unique index's own definition, not evaluated per
    /// row — which is why it is a method here and not the `where_` mod that filters
    /// which conflicting rows get updated. It hangs off the parenthesised column
    /// list and cannot stand without one.
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
    /// [`ConflictClause`], which implements
    /// [`HasSet`] and [`HasWhere`]: `set`, `set_col`, `set_excluded` and `where_`
    /// all apply, and that `where_` is the row filter.
    pub fn do_update(self, body: impl Mod<ConflictClause>) -> ConflictMod {
        let mut clause = ConflictClause::do_update();
        clause.target = self.target;
        body.apply(&mut clause);
        ConflictMod { clause }
    }
}

/// A finished `ON CONFLICT` clause, ready to apply.
#[derive(Debug, Clone)]
pub struct ConflictMod {
    clause: ConflictClause,
}

impl<Q: HasConflict> Mod<Q> for ConflictMod {
    fn apply(self, q: &mut Q) {
        q.conflict_mut().set_conflict(Expr::custom(self.clause));
    }
}
