//! Window mods: what goes inside `OVER (…)` or `WINDOW w AS (…)`.
//!
//! The frame bounds come in pairs. `from_*` sets the start, `to_*` sets the end,
//! and setting an end is what turns `RANGE …` into `RANGE BETWEEN … AND …`.

use keelson_core::clause::{
    FRAME_MODE_GROUPS, FRAME_MODE_RANGE, FRAME_MODE_ROWS, HasFrame, Window,
};
use keelson_core::{DynExpr, Mod, Result, SqlWriter, dyn_expr, expr_fn, mod_fn};

use crate::into_expr::IntoExpr;
use crate::mods::OrderMod;

/// `OVER (w)`: reuse a window defined by the query's `WINDOW` clause.
pub fn based_on(name: impl Into<String>) -> impl Mod<Window> {
    let name = name.into();
    mod_fn(move |w: &mut Window| w.set_based_on(name))
}

/// `PARTITION BY expression`.
pub fn partition_by(expression: impl IntoExpr) -> impl Mod<Window> {
    let expression = expression.into_expr();
    mod_fn(move |w: &mut Window| w.add_partition_by([expression]))
}

/// One `ORDER BY` term of the window.
pub fn order_by(expression: impl IntoExpr) -> OrderMod {
    OrderMod::new(expression.into_expr())
}

/// Frame by value range: `RANGE …`.
pub fn range<Q: HasFrame>() -> impl Mod<Q> {
    mod_fn(move |q: &mut Q| q.frame_mut().set_mode(FRAME_MODE_RANGE))
}

/// Frame by row count: `ROWS …`.
pub fn rows<Q: HasFrame>() -> impl Mod<Q> {
    mod_fn(move |q: &mut Q| q.frame_mut().set_mode(FRAME_MODE_ROWS))
}

/// Frame by peer group: `GROUPS …`.
pub fn groups<Q: HasFrame>() -> impl Mod<Q> {
    mod_fn(move |q: &mut Q| q.frame_mut().set_mode(FRAME_MODE_GROUPS))
}

/// `… UNBOUNDED PRECEDING`.
pub fn from_unbounded_preceding<Q: HasFrame>() -> impl Mod<Q> {
    mod_fn(move |q: &mut Q| q.frame_mut().set_start(dyn_expr("UNBOUNDED PRECEDING")))
}

/// `… n PRECEDING`.
pub fn from_preceding<Q: HasFrame>(offset: impl IntoExpr) -> impl Mod<Q> {
    let bound = suffixed(offset.into_expr(), " PRECEDING");
    mod_fn(move |q: &mut Q| q.frame_mut().set_start(bound))
}

/// `… CURRENT ROW`.
pub fn from_current_row<Q: HasFrame>() -> impl Mod<Q> {
    mod_fn(move |q: &mut Q| q.frame_mut().set_start(dyn_expr("CURRENT ROW")))
}

/// `… n FOLLOWING`.
pub fn from_following<Q: HasFrame>(offset: impl IntoExpr) -> impl Mod<Q> {
    let bound = suffixed(offset.into_expr(), " FOLLOWING");
    mod_fn(move |q: &mut Q| q.frame_mut().set_start(bound))
}

/// `BETWEEN … AND n PRECEDING`.
pub fn to_preceding<Q: HasFrame>(offset: impl IntoExpr) -> impl Mod<Q> {
    let bound = suffixed(offset.into_expr(), " PRECEDING");
    mod_fn(move |q: &mut Q| q.frame_mut().set_end(bound))
}

/// `BETWEEN … AND CURRENT ROW`.
pub fn to_current_row<Q: HasFrame>() -> impl Mod<Q> {
    mod_fn(move |q: &mut Q| q.frame_mut().set_end(dyn_expr("CURRENT ROW")))
}

/// `BETWEEN … AND n FOLLOWING`.
pub fn to_following<Q: HasFrame>(offset: impl IntoExpr) -> impl Mod<Q> {
    let bound = suffixed(offset.into_expr(), " FOLLOWING");
    mod_fn(move |q: &mut Q| q.frame_mut().set_end(bound))
}

/// `BETWEEN … AND UNBOUNDED FOLLOWING`.
pub fn to_unbounded_following<Q: HasFrame>() -> impl Mod<Q> {
    mod_fn(move |q: &mut Q| q.frame_mut().set_end(dyn_expr("UNBOUNDED FOLLOWING")))
}

/// `EXCLUDE NO OTHERS`.
pub fn exclude_no_others<Q: HasFrame>() -> impl Mod<Q> {
    mod_fn(move |q: &mut Q| q.frame_mut().set_exclusion("NO OTHERS"))
}

/// `EXCLUDE CURRENT ROW`.
pub fn exclude_current_row<Q: HasFrame>() -> impl Mod<Q> {
    mod_fn(move |q: &mut Q| q.frame_mut().set_exclusion("CURRENT ROW"))
}

/// `EXCLUDE GROUP`.
pub fn exclude_group<Q: HasFrame>() -> impl Mod<Q> {
    mod_fn(move |q: &mut Q| q.frame_mut().set_exclusion("GROUP"))
}

/// `EXCLUDE TIES`.
pub fn exclude_ties<Q: HasFrame>() -> impl Mod<Q> {
    mod_fn(move |q: &mut Q| q.frame_mut().set_exclusion("TIES"))
}

/// A frame bound is an expression plus a keyword, and the offset may bind an
/// argument, so the two are glued at write time rather than as strings.
fn suffixed(offset: DynExpr, suffix: &'static str) -> DynExpr {
    dyn_expr(expr_fn(move |w: &mut SqlWriter<'_>| -> Result<()> {
        w.write_expr(&offset)?;
        w.push_str(suffix);
        Ok(())
    }))
}
