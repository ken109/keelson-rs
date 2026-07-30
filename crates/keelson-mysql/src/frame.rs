//! Mods for a window frame — which rows around the current one a window function
//! sees.
//!
//! From <https://dev.mysql.com/doc/refman/8.4/en/window-functions-frames.html>:
//!
//! ```text
//! frame_clause:
//!     frame_units frame_extent
//! frame_units:
//!     {ROWS | RANGE}
//! frame_extent:
//!     {frame_start | frame_between}
//! frame_between:
//!     BETWEEN frame_start AND frame_end
//! frame_start, frame_end: {
//!     CURRENT ROW
//!   | UNBOUNDED PRECEDING
//!   | UNBOUNDED FOLLOWING
//!   | expr PRECEDING
//!   | expr FOLLOWING
//! }
//! ```
//!
//! **MySQL has no `GROUPS` mode and no `EXCLUDE` clause.** Both are PostgreSQL's, so
//! neither exists here — that is what "a construct a dialect lacks must simply not
//! exist for it" means in practice.
//!
//! Two of the grammar's defaults are relied on rather than written: the mode
//! defaults to `RANGE`, and `frame_start` defaults to `UNBOUNDED PRECEDING`. So a
//! `to_*` mod on its own gives a complete `BETWEEN UNBOUNDED PRECEDING AND …`, and
//! `BETWEEN` appears exactly when there is an end bound.
//!
//! ```
//! use keelson_mysql::{f, frame};
//!
//! // COUNT(*) OVER (ROWS BETWEEN 3 PRECEDING AND CURRENT ROW)
//! let e = f("COUNT", "*").over((
//!     frame::rows(),
//!     frame::from_preceding(3),
//!     frame::to_current_row(),
//! ));
//! ```

use keelson_core::clause::{FrameMode, HasFrame};
use keelson_core::expr::{Expr, IntoExpr};
use keelson_core::{Mod, mod_fn};

fn mode<Q: HasFrame>(mode: FrameMode) -> impl Mod<Q> {
    mod_fn(move |q: &mut Q| q.frame_mut().set_mode(mode))
}

fn start<Q: HasFrame>(bound: Expr) -> impl Mod<Q> {
    mod_fn(move |q: &mut Q| q.frame_mut().set_start(bound))
}

fn end<Q: HasFrame>(bound: Expr) -> impl Mod<Q> {
    mod_fn(move |q: &mut Q| q.frame_mut().set_end(bound))
}

/// `expr PRECEDING` / `expr FOLLOWING`.
///
/// The offset is an expression, so a bound argument works and `? PRECEDING` is what
/// a paged window needs.
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

/// `UNBOUNDED PRECEDING` as the start bound — the default, written out.
pub fn from_unbounded_preceding<Q: HasFrame>() -> impl Mod<Q> {
    start(Expr::raw("UNBOUNDED PRECEDING"))
}

/// `expr PRECEDING` as the start bound.
pub fn from_preceding<Q: HasFrame>(offset: impl IntoExpr) -> impl Mod<Q> {
    start(offset_bound(offset, "PRECEDING"))
}

/// `CURRENT ROW` as the start bound.
pub fn from_current_row<Q: HasFrame>() -> impl Mod<Q> {
    start(Expr::raw("CURRENT ROW"))
}

/// `expr FOLLOWING` as the start bound.
pub fn from_following<Q: HasFrame>(offset: impl IntoExpr) -> impl Mod<Q> {
    start(offset_bound(offset, "FOLLOWING"))
}

/// `UNBOUNDED FOLLOWING` as the start bound.
///
/// Rejected by MySQL — a frame cannot start at the end — but the grammar's
/// `frame_start` production lists it, so it is representable and the server is what
/// says no.
pub fn from_unbounded_following<Q: HasFrame>() -> impl Mod<Q> {
    start(Expr::raw("UNBOUNDED FOLLOWING"))
}

/// `expr PRECEDING` as the end bound. Turns the frame into a `BETWEEN`.
pub fn to_preceding<Q: HasFrame>(offset: impl IntoExpr) -> impl Mod<Q> {
    end(offset_bound(offset, "PRECEDING"))
}

/// `CURRENT ROW` as the end bound. Turns the frame into a `BETWEEN`.
pub fn to_current_row<Q: HasFrame>() -> impl Mod<Q> {
    end(Expr::raw("CURRENT ROW"))
}

/// `expr FOLLOWING` as the end bound. Turns the frame into a `BETWEEN`.
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
