//! `SELECT` query mods.
//!
//! Every function here returns a [`Mod`], so they compose as a tuple:
//! `psql::select((sm::from("users"), sm::limit(10)))`. The ones with more than
//! one setting return a builder from [`crate::mods`] that is itself a mod, so
//! `sm::left_join("t").using("id")` needs no terminator.
//!
//! Most are generic over the `Has*` trait for the clause they touch rather than
//! over `SelectQuery`, which is what lets the same mod serve `UPDATE`'s `FROM`
//! or a window's `ORDER BY`.

use std::sync::Arc;

use keelson_core::clause::{
    Combine, EXCEPT, FULL_JOIN, Fetch, HasCombines, HasFetch, HasGroupBy, HasHaving, HasLimit,
    HasOffset, HasSelectList, HasWhere, HasWindows, HasWith, INNER_JOIN, INTERSECT, LEFT_JOIN,
    LOCK_STRENGTH_KEY_SHARE, LOCK_STRENGTH_NO_KEY_UPDATE, LOCK_STRENGTH_SHARE,
    LOCK_STRENGTH_UPDATE, NamedWindow, RIGHT_JOIN, UNION, Window,
};
use keelson_core::{BuildMod, Expression, Mod, dyn_expr, mod_fn};

use crate::function::{Function, Functions};
use crate::into_expr::{Exprs, IntoExpr, Names};
use crate::mods::{CombinedOrderMod, CrossJoinMod, CteMod, FromMod, JoinMod, LockMod, OrderMod};
use crate::query::Query;
use crate::select::{
    HasCombinedFetch, HasCombinedLimit, HasCombinedOffset, HasDistinct, SelectQuery,
};

/// One common table expression: `sm::with("adults", ()).as_(query)`.
pub fn with(name: impl Into<String>, columns: impl Names) -> CteMod {
    CteMod::new(name, columns)
}

/// `WITH RECURSIVE` instead of `WITH`.
pub fn recursive<Q: HasWith>(recursive: bool) -> impl Mod<Q> {
    mod_fn(move |q: &mut Q| q.with_mut().set_recursive(recursive))
}

/// `SELECT DISTINCT`, or `SELECT DISTINCT ON (…)` when given expressions.
///
/// `sm::distinct(())` is the bare form. Not calling it at all is what leaves the
/// query without a `DISTINCT`.
pub fn distinct<Q: HasDistinct>(on: impl Exprs) -> impl Mod<Q> {
    let on = on.into_exprs();
    mod_fn(move |q: &mut Q| q.distinct_mut().on = Some(on))
}

/// Add to the selected columns.
pub fn columns<Q: HasSelectList>(columns: impl Exprs) -> impl Mod<Q> {
    let columns = columns.into_exprs();
    mod_fn(move |q: &mut Q| q.select_list_mut().append_select(columns))
}

/// The `FROM` item: a table name, a sub-query, a function.
pub fn from(table: impl IntoExpr) -> FromMod {
    FromMod::new(table.into_expr())
}

/// `FROM` a set-returning function, or several of them as `ROWS FROM (…)`.
pub fn from_function(functions: impl IntoIterator<Item = Function>) -> FromMod {
    let functions: Vec<Function> = functions.into_iter().collect();

    // One function needs no `ROWS FROM` wrapper, and bob does not give it one.
    if let [single] = functions.as_slice() {
        return FromMod::new(dyn_expr(single.clone()));
    }
    FromMod::new(dyn_expr(Functions(functions)))
}

/// `INNER JOIN`.
pub fn inner_join(table: impl IntoExpr) -> JoinMod {
    join(INNER_JOIN, table)
}

/// `LEFT JOIN`.
pub fn left_join(table: impl IntoExpr) -> JoinMod {
    join(LEFT_JOIN, table)
}

/// `RIGHT JOIN`.
pub fn right_join(table: impl IntoExpr) -> JoinMod {
    join(RIGHT_JOIN, table)
}

/// `FULL JOIN`.
pub fn full_join(table: impl IntoExpr) -> JoinMod {
    join(FULL_JOIN, table)
}

/// `CROSS JOIN`, which takes no `ON` or `USING`.
pub fn cross_join(table: impl IntoExpr) -> CrossJoinMod {
    CrossJoinMod::new(table.into_expr())
}

/// A join of an explicit kind, for a spelling with no helper of its own.
pub fn join(kind: impl Into<String>, table: impl IntoExpr) -> JoinMod {
    JoinMod::new(kind, table.into_expr())
}

/// One `WHERE` condition. Several are `AND`ed.
pub fn where_<Q: HasWhere>(condition: impl IntoExpr) -> impl Mod<Q> {
    let condition = condition.into_expr();
    mod_fn(move |q: &mut Q| q.where_mut().append_where(condition))
}

/// One `HAVING` condition. Several are `AND`ed.
pub fn having<Q: HasHaving>(condition: impl IntoExpr) -> impl Mod<Q> {
    let condition = condition.into_expr();
    mod_fn(move |q: &mut Q| q.having_mut().append_having(condition))
}

/// One `GROUP BY` term.
pub fn group_by<Q: HasGroupBy>(group: impl IntoExpr) -> impl Mod<Q> {
    let group = group.into_expr();
    mod_fn(move |q: &mut Q| q.group_by_mut().append_group(group))
}

/// `GROUP BY DISTINCT`.
pub fn group_by_distinct<Q: HasGroupBy>(distinct: bool) -> impl Mod<Q> {
    mod_fn(move |q: &mut Q| q.group_by_mut().set_group_by_distinct(distinct))
}

/// A named window: `sm::window("w", (wm::partition_by("dept"), wm::order_by("salary")))`.
pub fn window<Q: HasWindows>(
    name: impl Into<String>,
    window_mods: impl Mod<Window>,
) -> impl Mod<Q> {
    let mut definition = Window::default();
    window_mods.apply(&mut definition);

    let named = NamedWindow {
        name: name.into(),
        definition,
    };
    mod_fn(move |q: &mut Q| q.windows_mut().append_window(dyn_expr(named)))
}

/// One `ORDER BY` term.
pub fn order_by(expression: impl IntoExpr) -> OrderMod {
    OrderMod::new(expression.into_expr())
}

/// `LIMIT`. A number is a literal; `psql::arg(n)` is a placeholder.
pub fn limit<Q: HasLimit>(count: impl IntoExpr) -> impl Mod<Q> {
    let count = count.into_expr();
    mod_fn(move |q: &mut Q| q.limit_mut().set_limit(count))
}

/// `OFFSET`.
pub fn offset<Q: HasOffset>(count: impl IntoExpr) -> impl Mod<Q> {
    let count = count.into_expr();
    mod_fn(move |q: &mut Q| q.offset_mut().set_offset(count))
}

/// `FETCH NEXT n ROWS ONLY`, or `ROWS WITH TIES`.
pub fn fetch<Q: HasFetch>(count: impl IntoExpr, with_ties: bool) -> impl Mod<Q> {
    let count = count.into_expr();
    mod_fn(move |q: &mut Q| {
        q.fetch_mut().set_fetch(Fetch {
            count: Some(count),
            with_ties,
        })
    })
}

/// `UNION (…)`.
pub fn union<Q: HasCombines, S: Expression + 'static>(query: Query<S>) -> impl Mod<Q> {
    combine(UNION, query, false)
}

/// `UNION ALL (…)`.
pub fn union_all<Q: HasCombines, S: Expression + 'static>(query: Query<S>) -> impl Mod<Q> {
    combine(UNION, query, true)
}

/// `INTERSECT (…)`.
pub fn intersect<Q: HasCombines, S: Expression + 'static>(query: Query<S>) -> impl Mod<Q> {
    combine(INTERSECT, query, false)
}

/// `INTERSECT ALL (…)`.
pub fn intersect_all<Q: HasCombines, S: Expression + 'static>(query: Query<S>) -> impl Mod<Q> {
    combine(INTERSECT, query, true)
}

/// `EXCEPT (…)`.
pub fn except<Q: HasCombines, S: Expression + 'static>(query: Query<S>) -> impl Mod<Q> {
    combine(EXCEPT, query, false)
}

/// `EXCEPT ALL (…)`.
pub fn except_all<Q: HasCombines, S: Expression + 'static>(query: Query<S>) -> impl Mod<Q> {
    combine(EXCEPT, query, true)
}

/// A set operation of an explicit strategy.
fn combine<Q: HasCombines, S: Expression + 'static>(
    strategy: &'static str,
    query: Query<S>,
    all: bool,
) -> impl Mod<Q> {
    // Bare: the parentheses belong to the `Combine` clause.
    let combine = Combine {
        strategy: strategy.into(),
        query: Some(dyn_expr(query.into_bare())),
        all,
    };
    mod_fn(move |q: &mut Q| q.combines_mut().append_combine(combine))
}

/// `FOR UPDATE`, optionally `OF table, …`.
pub fn for_update(tables: impl Names) -> LockMod {
    LockMod::new(LOCK_STRENGTH_UPDATE, tables)
}

/// `FOR NO KEY UPDATE`.
pub fn for_no_key_update(tables: impl Names) -> LockMod {
    LockMod::new(LOCK_STRENGTH_NO_KEY_UPDATE, tables)
}

/// `FOR SHARE`.
pub fn for_share(tables: impl Names) -> LockMod {
    LockMod::new(LOCK_STRENGTH_SHARE, tables)
}

/// `FOR KEY SHARE`.
pub fn for_key_share(tables: impl Names) -> LockMod {
    LockMod::new(LOCK_STRENGTH_KEY_SHARE, tables)
}

/// `ORDER BY` applied to the result of a `UNION` / `INTERSECT` / `EXCEPT`.
pub fn order_combined(expression: impl IntoExpr) -> CombinedOrderMod {
    CombinedOrderMod::new(expression.into_expr())
}

/// `LIMIT` applied to the result of a set operation.
pub fn limit_combined<Q: HasCombinedLimit>(count: impl IntoExpr) -> impl Mod<Q> {
    let count = count.into_expr();
    mod_fn(move |q: &mut Q| q.combined_limit_mut().set_limit(count))
}

/// `OFFSET` applied to the result of a set operation.
pub fn offset_combined<Q: HasCombinedOffset>(count: impl IntoExpr) -> impl Mod<Q> {
    let count = count.into_expr();
    mod_fn(move |q: &mut Q| q.combined_offset_mut().set_offset(count))
}

/// `FETCH` applied to the result of a set operation.
pub fn fetch_combined<Q: HasCombinedFetch>(count: impl IntoExpr, with_ties: bool) -> impl Mod<Q> {
    let count = count.into_expr();
    mod_fn(move |q: &mut Q| {
        q.combined_fetch_mut().set_fetch(Fetch {
            count: Some(count),
            with_ties,
        })
    })
}

/// A mod that runs on every build rather than now — bob's contextual mod.
pub fn build_mod(m: Arc<dyn BuildMod<SelectQuery>>) -> impl Mod<SelectQuery> {
    mod_fn(move |q: &mut SelectQuery| q.append_build_mod(m))
}
