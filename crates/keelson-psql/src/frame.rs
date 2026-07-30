//! Mods for a window frame — which rows around the current one a window function
//! sees.
//!
//! From PostgreSQL 17,
//! <https://www.postgresql.org/docs/17/sql-expressions.html#SYNTAX-WINDOW-FUNCTIONS>:
//!
//! ```text
//! { RANGE | ROWS | GROUPS } frame_start [ frame_exclusion ]
//! { RANGE | ROWS | GROUPS } BETWEEN frame_start AND frame_end [ frame_exclusion ]
//!
//! frame_start, frame_end:
//!     UNBOUNDED PRECEDING | offset PRECEDING | CURRENT ROW
//!   | offset FOLLOWING    | UNBOUNDED FOLLOWING
//! frame_exclusion:
//!     EXCLUDE CURRENT ROW | EXCLUDE GROUP | EXCLUDE TIES | EXCLUDE NO OTHERS
//! ```
//!
//! Two of the grammar's defaults are relied on rather than written: the mode
//! defaults to `RANGE`, and `frame_start` defaults to `UNBOUNDED PRECEDING`. So a
//! `to_*` mod on its own gives a complete `BETWEEN UNBOUNDED PRECEDING AND …`, and
//! `BETWEEN` appears exactly when there is an end bound.
//!
//! ```
//! use keelson_psql::{arg, f, frame, quote};
//!
//! // count(*) OVER (ROWS BETWEEN $1 PRECEDING AND CURRENT ROW EXCLUDE TIES)
//! let e = f("count", "*").over((
//!     frame::rows(),
//!     frame::from_preceding(arg(3i32)),
//!     frame::to_current_row(),
//!     frame::exclude_ties(),
//! ));
//! # let _ = quote("x");
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
/// The offset is an expression, so a bound argument works: `$1 PRECEDING` is legal
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

/// `offset FOLLOWING` as the start bound.
pub fn from_following<Q: HasFrame>(offset: impl IntoExpr) -> impl Mod<Q> {
    start(offset_bound(offset, "FOLLOWING"))
}

/// `UNBOUNDED FOLLOWING` as the start bound.
///
/// Rejected by PostgreSQL — a frame cannot start at the end — but the grammar's
/// `frame_start` production lists it, so it is representable and the server is what
/// says no.
pub fn from_unbounded_following<Q: HasFrame>() -> impl Mod<Q> {
    start(Expr::raw("UNBOUNDED FOLLOWING"))
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

/// `UNBOUNDED PRECEDING` as the end bound.
///
/// Like [`from_unbounded_following`], in the grammar and refused by the server.
pub fn to_unbounded_preceding<Q: HasFrame>() -> impl Mod<Q> {
    end(Expr::raw("UNBOUNDED PRECEDING"))
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
