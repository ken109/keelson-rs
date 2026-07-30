//! Mods for a window definition — what goes inside `OVER (…)` or after
//! `WINDOW name AS`.
//!
//! From <https://www.sqlite.org/syntax/window-defn.html>:
//!
//! ```text
//! ( [ base-window-name ] [ PARTITION BY expr [, ...] ]
//!   [ ORDER BY ordering-term [, ...] ] [ frame-spec ] )
//! ```
//!
//! The frame is [`sqlite::frame`](crate::frame), a separate module because the same
//! mods apply to a bare [`Frame`](keelson_core::clause::Frame) as well as to a
//! window.
//!
//! Every part is optional, all of them at once included: `OVER ()` is legal and
//! means the whole partition, which is [`over(())`](crate::Function::over).
//!
//! ```
//! use keelson_sqlite::{f, frame, quote, window};
//!
//! // sum("views") OVER (PARTITION BY "user_id" ORDER BY "id" ROWS UNBOUNDED PRECEDING)
//! let e = f("sum", quote("views")).over((
//!     window::partition_by(quote("user_id")),
//!     window::order_by(quote("id")),
//!     frame::rows(),
//! ));
//! ```

use std::borrow::Cow;

use keelson_core::clause::HasWindow;
use keelson_core::expr::IntoExprList;
use keelson_core::{Mod, mod_fn};

pub use crate::shared::order_by;

/// Extend an existing named window, taking its `PARTITION BY` and — unless this one
/// has its own — its `ORDER BY`.
///
/// SQLite refuses this when the base window has a frame specification, which is why
/// [`Function::over_name`](crate::Function::over_name) exists: that writes
/// `OVER "w"`, a reference, where this builds `OVER ("w" …)`, a copy.
pub fn based_on<Q: HasWindow>(name: impl Into<Cow<'static, str>>) -> impl Mod<Q> {
    let name = name.into();
    mod_fn(move |q: &mut Q| q.window_mut().based_on = Some(name))
}

/// `PARTITION BY a, b`. Several calls accumulate.
pub fn partition_by<Q: HasWindow>(expressions: impl IntoExprList) -> impl Mod<Q> {
    let expressions = expressions.into_expr_list();
    mod_fn(move |q: &mut Q| q.window_mut().add_partition_by(expressions))
}
