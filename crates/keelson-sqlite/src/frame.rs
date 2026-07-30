//! Mods for a window frame — which rows around the current one a window function
//! sees.
//!
//! From <https://www.sqlite.org/syntax/frame-spec.html>:
//!
//! ```text
//! { RANGE | ROWS | GROUPS }
//!   { UNBOUNDED PRECEDING | expr PRECEDING | CURRENT ROW
//!   | BETWEEN { UNBOUNDED PRECEDING | expr PRECEDING | CURRENT ROW | expr FOLLOWING }
//!         AND { expr PRECEDING | CURRENT ROW | expr FOLLOWING | UNBOUNDED FOLLOWING } }
//!   [ EXCLUDE { NO OTHERS | CURRENT ROW | GROUP | TIES } ]
//! ```
//!
//! Read that inner list carefully: `UNBOUNDED FOLLOWING` appears only as a
//! frame-*end* and `UNBOUNDED PRECEDING` only as a frame-*start*. So there is no
//! `from_unbounded_following` and no `to_unbounded_preceding` here — PostgreSQL's
//! grammar lists both in both positions and leaves the server to refuse them,
//! SQLite's diagram does not, and a construct a dialect's grammar lacks should not
//! be representable.
//!
//! Two of the grammar's defaults are relied on rather than written: the mode
//! defaults to `RANGE`, and the start bound to `UNBOUNDED PRECEDING`. So a `to_*`
//! mod on its own gives a complete `BETWEEN UNBOUNDED PRECEDING AND …`, and
//! `BETWEEN` appears exactly when there is an end bound.
//!
//! ```
//! use keelson_sqlite::{arg, f, frame};
//!
//! // count(*) OVER (ROWS BETWEEN ?1 PRECEDING AND CURRENT ROW EXCLUDE TIES)
//! let e = f("count", "*").over((
//!     frame::rows(),
//!     frame::from_preceding(arg(3i32)),
//!     frame::to_current_row(),
//!     frame::exclude_ties(),
//! ));
//! ```

use keelson_core::clause::{FrameExclusion, FrameMode, HasFrame};
use keelson_core::expr::{Expr, IntoExpr};
use keelson_core::{Mod, mod_fn};

fn mode<Q: HasFrame>(mode: FrameMode) -> impl Mod<Q> {
    mod_fn(move |q: &mut Q| q.frame_mut().set_mode(mode))
}

fn exclusion<Q: HasFrame>(exclusion: FrameExclusion) -> impl Mod<Q> {
    mod_fn(move |q: &mut Q| q.frame_mut().set_exclusion(exclusion))
}

fn start<Q: HasFrame>(bound: Expr) -> impl Mod<Q> {
    mod_fn(move |q: &mut Q| q.frame_mut().set_start(bound))
}

fn end<Q: HasFrame>(bound: Expr) -> impl Mod<Q> {
    mod_fn(move |q: &mut Q| q.frame_mut().set_end(bound))
}

/// `offset PRECEDING` / `offset FOLLOWING`.
///
/// The offset is an expression, so a bound argument works: `?1 PRECEDING` is legal
/// and is what a paged window needs.
fn offset_bound(offset: impl IntoExpr, keyword: &'static str) -> Expr {
    Expr::join((offset, Expr::raw(keyword)))
}

/// `RANGE` — the offsets are values compared against the `ORDER BY` key. The
/// grammar's default, so this only ever documents intent.
pub fn range<Q: HasFrame>() -> impl Mod<Q> {
    mode(FrameMode::Range)
}

/// `ROWS` — the offsets are row counts.
pub fn rows<Q: HasFrame>() -> impl Mod<Q> {
    mode(FrameMode::Rows)
}

/// `GROUPS` — the offsets are counts of peer groups. Requires an `ORDER BY` on the
/// window.
pub fn groups<Q: HasFrame>() -> impl Mod<Q> {
    mode(FrameMode::Groups)
}

/// `UNBOUNDED PRECEDING` as the start bound — the default, written out.
pub fn from_unbounded_preceding<Q: HasFrame>() -> impl Mod<Q> {
    start(Expr::raw("UNBOUNDED PRECEDING"))
}

/// `offset PRECEDING` as the start bound.
pub fn from_preceding<Q: HasFrame>(offset: impl IntoExpr) -> impl Mod<Q> {
    start(offset_bound(offset, "PRECEDING"))
}

/// `CURRENT ROW` as the start bound.
pub fn from_current_row<Q: HasFrame>() -> impl Mod<Q> {
    start(Expr::raw("CURRENT ROW"))
}

/// `offset FOLLOWING` as the start bound. Only legal inside a `BETWEEN`, so pair it
/// with a `to_*` mod.
pub fn from_following<Q: HasFrame>(offset: impl IntoExpr) -> impl Mod<Q> {
    start(offset_bound(offset, "FOLLOWING"))
}

/// `offset PRECEDING` as the end bound. Turns the frame into a `BETWEEN`.
pub fn to_preceding<Q: HasFrame>(offset: impl IntoExpr) -> impl Mod<Q> {
    end(offset_bound(offset, "PRECEDING"))
}

/// `CURRENT ROW` as the end bound. Turns the frame into a `BETWEEN`.
pub fn to_current_row<Q: HasFrame>() -> impl Mod<Q> {
    end(Expr::raw("CURRENT ROW"))
}

/// `offset FOLLOWING` as the end bound. Turns the frame into a `BETWEEN`.
pub fn to_following<Q: HasFrame>(offset: impl IntoExpr) -> impl Mod<Q> {
    end(offset_bound(offset, "FOLLOWING"))
}

/// `UNBOUNDED FOLLOWING` as the end bound. Turns the frame into a `BETWEEN`.
pub fn to_unbounded_following<Q: HasFrame>() -> impl Mod<Q> {
    end(Expr::raw("UNBOUNDED FOLLOWING"))
}

/// `EXCLUDE NO OTHERS` — the default, written out.
pub fn exclude_no_others<Q: HasFrame>() -> impl Mod<Q> {
    exclusion(FrameExclusion::NoOthers)
}

/// `EXCLUDE CURRENT ROW`.
pub fn exclude_current_row<Q: HasFrame>() -> impl Mod<Q> {
    exclusion(FrameExclusion::CurrentRow)
}

/// `EXCLUDE GROUP` — the current row and all its peers.
pub fn exclude_group<Q: HasFrame>() -> impl Mod<Q> {
    exclusion(FrameExclusion::Group)
}

/// `EXCLUDE TIES` — the current row's peers, but not the row itself.
pub fn exclude_ties<Q: HasFrame>() -> impl Mod<Q> {
    exclusion(FrameExclusion::Ties)
}
