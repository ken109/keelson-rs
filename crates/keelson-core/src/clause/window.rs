use std::borrow::Cow;

use crate::expr::{Expr, IntoExprList};
use crate::writer::{Expression, SqlWriter};

use super::frame::{Frame, HasFrame};
use super::order_by::{HasOrderBy, OrderBy};
use super::{MaybeAbsent, write_present};

/// A window definition: what goes inside `OVER (…)`, or after `WINDOW name AS`.
///
/// From PostgreSQL 17,
/// <https://www.postgresql.org/docs/17/sql-select.html#SQL-WINDOW>:
///
/// ```text
/// [ existing_window_name ] [ PARTITION BY expression [, ...] ]
/// [ ORDER BY expression [ASC | DESC | USING operator] [NULLS {FIRST|LAST}] [, ...] ]
/// [ frame_clause ]
/// ```
///
/// Every part is optional, including all of them at once: `OVER ()` and
/// `WINDOW w AS ()` are both legal and mean "the whole partition". So an empty
/// `Window` renders the empty string and the parentheses come from whatever
/// contains it.
///
/// Because it holds an [`OrderBy`] and a [`Frame`] and implements [`HasOrderBy`]
/// and [`HasFrame`], the ordinary order-by and frame mods apply to a window
/// unchanged.
#[derive(Debug, Clone, Default)]
pub struct Window {
    /// A window this one extends, copying its `PARTITION BY` and — unless this one
    /// has its own — its `ORDER BY`. Quoted on output.
    pub based_on: Option<Cow<'static, str>>,
    /// `PARTITION BY`.
    pub partition_by: Vec<Expr>,
    /// `ORDER BY`, which is what gives `RANGE` frames and `rank()` a meaning.
    pub order_by: OrderBy,
    /// The frame.
    pub frame: Frame,
}

impl Window {
    /// A window extending an existing named one.
    pub fn based_on(name: impl Into<Cow<'static, str>>) -> Self {
        Window {
            based_on: Some(name.into()),
            ..Window::default()
        }
    }

    /// Append partition expressions.
    pub fn add_partition_by(&mut self, exprs: impl IntoExprList) {
        self.partition_by.extend(exprs.into_expr_list());
    }

    /// Whether nothing was set, so that `OVER ()` is what will be written.
    pub fn is_empty(&self) -> bool {
        self.based_on.is_none()
            && self.partition_by.is_empty()
            && self.order_by.is_empty()
            && self.frame.is_empty()
    }
}

impl Expression for Window {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        // Each part writes its own separator only when something precedes it, so
        // there is never a leading or doubled space. bob pads unconditionally and
        // emits `PARTITION BY a  ORDER BY b`.
        let mut written = false;

        if let Some(based_on) = &self.based_on {
            w.push_quoted(&[based_on]);
            written = true;
        }

        if !self.partition_by.is_empty() {
            if written {
                w.push_str(" ");
            }
            w.write_slice(&self.partition_by, "PARTITION BY ", ", ", "");
            written = true;
        }

        if !self.order_by.is_empty() {
            if written {
                w.push_str(" ");
            }
            w.write_expr(&self.order_by);
            written = true;
        }

        if !self.frame.is_empty() {
            if written {
                w.push_str(" ");
            }
            w.write_expr(&self.frame);
        }
    }
}

impl HasOrderBy for Window {
    fn order_by_mut(&mut self) -> &mut OrderBy {
        &mut self.order_by
    }
}

impl HasFrame for Window {
    fn frame_mut(&mut self) -> &mut Frame {
        &mut self.frame
    }
}

/// Anything with a window definition: a [`NamedWindow`], or a dialect's function
/// builder holding the window of its `OVER`.
pub trait HasWindow {
    /// The window definition to modify.
    fn window_mut(&mut self) -> &mut Window;
}

impl HasWindow for Window {
    fn window_mut(&mut self) -> &mut Window {
        self
    }
}

/// `name AS (<definition>)` — one entry of a statement's `WINDOW` clause.
#[derive(Debug, Clone, Default)]
pub struct NamedWindow {
    /// The name, quoted on output.
    pub name: Cow<'static, str>,
    /// What the name means.
    pub definition: Window,
}

impl NamedWindow {
    /// Name a window definition.
    pub fn new(name: impl Into<Cow<'static, str>>, definition: Window) -> Self {
        NamedWindow {
            name: name.into(),
            definition,
        }
    }

    /// Whether there is no name, so that nothing will be written.
    pub fn is_empty(&self) -> bool {
        self.name.is_empty()
    }
}

impl Expression for NamedWindow {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        if self.name.is_empty() {
            // An unnamed entry cannot be referred to, so it is not an entry.
            return;
        }
        w.push_quoted(&[&self.name]);
        w.push_str(" AS (");
        w.write_expr(&self.definition);
        w.push_str(")");
    }
}

impl HasWindow for NamedWindow {
    fn window_mut(&mut self) -> &mut Window {
        &mut self.definition
    }
}

/// `WINDOW w AS (…), v AS (…)`
#[derive(Debug, Clone, Default)]
pub struct Windows {
    /// The named windows, in order. A later one may be
    /// [`based_on`](Window::based_on) an earlier one.
    pub windows: Vec<NamedWindow>,
}

impl Windows {
    /// Append a named window.
    pub fn append_window(&mut self, window: NamedWindow) {
        self.windows.push(window);
    }

    /// Whether the clause is absent.
    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }
}

impl Expression for Windows {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        write_present(w, &self.windows, "WINDOW ", ", ", "");
    }
}

/// A statement with a `WINDOW` clause.
pub trait HasWindows {
    /// The `WINDOW` clause to modify.
    fn windows_mut(&mut self) -> &mut Windows;
}

impl HasWindows for Windows {
    fn windows_mut(&mut self) -> &mut Windows {
        self
    }
}

impl MaybeAbsent for NamedWindow {
    fn is_absent(&self) -> bool {
        self.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clause::frame::{FrameExclusion, FrameMode};
    use crate::clause::order_by::{OrderDef, OrderDirection};
    use crate::dialect::testing::Numbered;
    use crate::expr::{arg, quote};
    use crate::value::Value;
    use crate::writer::build;

    #[test]
    fn an_empty_window_writes_nothing_which_is_what_over_wants() {
        assert_eq!(build(&Numbered, &Window::default()).unwrap().0, "");
        assert!(Window::default().is_empty());
        assert_eq!(build(&Numbered, &Windows::default()).unwrap().0, "");
    }

    #[test]
    fn a_window_based_on_a_name_is_just_that_name() {
        // PostgreSQL 17: `[ existing_window_name ]` is the first thing in a window
        // definition, and it may be the only thing.
        assert_eq!(
            build(&Numbered, &Window::based_on("w")).unwrap().0,
            r#""w""#
        );
    }

    #[test]
    fn the_parts_render_in_grammar_order_with_single_spaces() {
        let mut win = Window::based_on("w");
        win.add_partition_by((quote("a"), quote("b")));
        win.order_by_mut()
            .append_order(Expr::custom(OrderDef::new(quote("created_at"))));
        win.frame_mut().set_mode(FrameMode::Rows);
        win.frame_mut().set_end("CURRENT ROW");

        assert_eq!(
            build(&Numbered, &win).unwrap().0,
            r#""w" PARTITION BY "a", "b" ORDER BY "created_at" ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW"#
        );
    }

    #[test]
    fn each_part_can_appear_alone_without_stray_spaces() {
        let mut partition_only = Window::default();
        partition_only.add_partition_by(quote("a"));
        assert_eq!(
            build(&Numbered, &partition_only).unwrap().0,
            r#"PARTITION BY "a""#
        );

        let mut order_only = Window::default();
        order_only.order_by_mut().append_order(quote("a"));
        assert_eq!(build(&Numbered, &order_only).unwrap().0, r#"ORDER BY "a""#);

        let mut frame_only = Window::default();
        frame_only.frame_mut().set_exclusion(FrameExclusion::Group);
        assert_eq!(
            build(&Numbered, &frame_only).unwrap().0,
            "RANGE UNBOUNDED PRECEDING EXCLUDE GROUP"
        );
    }

    #[test]
    fn a_partition_expression_may_bind_an_argument() {
        let mut win = Window::default();
        win.add_partition_by(Expr::func("coalesce", (quote("a"), arg(0i32))));
        let (sql, args) = build(&Numbered, &win).unwrap();
        assert_eq!(sql, r#"PARTITION BY coalesce("a", $1)"#);
        assert_eq!(args, vec![Value::I32(0)]);
    }

    #[test]
    fn named_windows_are_comma_separated_under_one_keyword() {
        let mut w1 = Window::default();
        w1.add_partition_by(quote("dept"));
        w1.order_by_mut().append_order(Expr::custom(OrderDef {
            direction: Some(OrderDirection::Desc),
            ..OrderDef::new(quote("salary"))
        }));

        let mut ws = Windows::default();
        ws.append_window(NamedWindow::new("w", w1));
        // The empty definition is legal: `v AS ()` is the whole partition.
        ws.append_window(NamedWindow::new("v", Window::default()));

        assert_eq!(
            build(&Numbered, &ws).unwrap().0,
            r#"WINDOW "w" AS (PARTITION BY "dept" ORDER BY "salary" DESC), "v" AS ()"#
        );
    }

    #[test]
    fn an_unnamed_entry_takes_the_keyword_and_its_comma_with_it() {
        // A `WINDOW` with nothing after it, or a dangling comma, is a syntax error
        // rather than untidiness, so an absent entry is skipped separator and all.
        let mut ws = Windows::default();
        ws.append_window(NamedWindow::default());
        assert!(NamedWindow::default().is_empty());
        assert_eq!(build(&Numbered, &ws).unwrap().0, "");

        ws.append_window(NamedWindow::new("w", Window::default()));
        ws.append_window(NamedWindow::default());
        assert_eq!(build(&Numbered, &ws).unwrap().0, r#"WINDOW "w" AS ()"#);
    }

    #[test]
    fn the_window_and_frame_traits_reach_a_named_window() {
        // The nesting that matters: a mod written for a Window applies to the
        // definition inside a NamedWindow, and a frame mod applies through both.
        let mut named = NamedWindow::new("w", Window::default());
        named.window_mut().add_partition_by(quote("a"));
        named.window_mut().frame_mut().set_mode(FrameMode::Groups);
        assert_eq!(
            build(&Numbered, &named).unwrap().0,
            r#""w" AS (PARTITION BY "a" GROUPS UNBOUNDED PRECEDING)"#
        );
    }
}
