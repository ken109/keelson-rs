use crate::error::Result;
use crate::writer::{DynExpr, Expression, SqlWriter};

use super::frame::{Frame, HasFrame};
use super::order_by::{HasOrderBy, OrderBy};

/// A window definition: what goes inside `OVER (…)` or after `WINDOW name AS`.
///
/// Because it carries an [`OrderBy`] and a [`Frame`] and implements
/// [`HasOrderBy`] and [`HasFrame`], the ordinary order-by and frame mods apply to
/// a window unchanged.
#[derive(Debug, Clone, Default)]
pub struct Window {
    /// An existing window name this one extends.
    pub based_on: String,
    pub partition_by: Vec<DynExpr>,
    pub order_by: OrderBy,
    pub frame: Frame,
}

impl Window {
    pub fn set_based_on(&mut self, name: impl Into<String>) {
        self.based_on = name.into();
    }

    pub fn add_partition_by(&mut self, conditions: impl IntoIterator<Item = DynExpr>) {
        self.partition_by.extend(conditions);
    }

    pub fn is_empty(&self) -> bool {
        self.based_on.is_empty()
            && self.partition_by.is_empty()
            && self.order_by.is_empty()
            && !self.frame.defined
    }
}

impl Expression for Window {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        if !self.based_on.is_empty() {
            w.push_str(&self.based_on);
            w.push_str(" ");
        }

        // The partition list closes with a space and the order-by opens with one,
        // so a window carrying both has two spaces between them. That is what
        // bob emits and what the fixtures record.
        w.write_slice(&self.partition_by, "PARTITION BY ", ", ", " ")?;
        w.write_if(!self.order_by.is_empty(), " ", &self.order_by, "")?;
        w.write_if(self.frame.defined, " ", &self.frame, "")?;

        Ok(())
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

/// `name AS (<definition>)` — one entry of a statement's `WINDOW` clause.
#[derive(Debug, Clone, Default)]
pub struct NamedWindow {
    pub name: String,
    pub definition: Window,
}

impl Expression for NamedWindow {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        w.push_str(&self.name);
        w.push_str(" AS (");
        w.write_expr(&self.definition)?;
        w.push_str(")");
        Ok(())
    }
}

/// `WINDOW w1 AS (…), w2 AS (…)`
#[derive(Debug, Clone, Default)]
pub struct Windows {
    pub windows: Vec<DynExpr>,
}

impl Windows {
    pub fn append_window(&mut self, window: DynExpr) {
        self.windows.push(window);
    }

    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }
}

impl Expression for Windows {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        w.write_slice(&self.windows, "WINDOW ", ", ", "")
    }
}

/// A query with a `WINDOW` clause.
pub trait HasWindows {
    fn windows_mut(&mut self) -> &mut Windows;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clause::frame::FRAME_MODE_ROWS;
    use crate::clause::order_by::OrderDef;
    use crate::dialect::testing::Numbered;
    use crate::writer::{build, dyn_expr};

    #[test]
    fn an_empty_window_writes_nothing() {
        assert_eq!(build(&Numbered, &Window::default()).unwrap().0, "");
        assert!(Window::default().is_empty());
    }

    #[test]
    fn a_window_based_on_a_name_keeps_its_trailing_space() {
        let mut win = Window::default();
        win.set_based_on("w");
        assert_eq!(build(&Numbered, &win).unwrap().0, "w ");
    }

    #[test]
    fn partition_and_order_are_separated_by_two_spaces() {
        let mut win = Window::default();
        win.add_partition_by([dyn_expr("presale_id")]);
        win.order_by_mut()
            .append_order(dyn_expr(OrderDef::new(dyn_expr("created_date"))));

        assert_eq!(
            build(&Numbered, &win).unwrap().0,
            "PARTITION BY presale_id  ORDER BY created_date"
        );
    }

    #[test]
    fn a_frame_is_only_written_once_defined() {
        let mut win = Window::default();
        win.add_partition_by([dyn_expr("a")]);
        assert_eq!(build(&Numbered, &win).unwrap().0, "PARTITION BY a ");

        win.frame_mut().set_mode(FRAME_MODE_ROWS);
        assert_eq!(
            build(&Numbered, &win).unwrap().0,
            "PARTITION BY a  ROWS UNBOUNDED PRECEDING"
        );
    }

    #[test]
    fn named_windows_are_comma_separated_under_one_keyword() {
        let mut win = Window::default();
        win.add_partition_by([dyn_expr("depname")]);

        let mut ws = Windows::default();
        ws.append_window(dyn_expr(NamedWindow {
            name: "w".into(),
            definition: win,
        }));
        ws.append_window(dyn_expr(NamedWindow {
            name: "v".into(),
            definition: Window::default(),
        }));

        assert_eq!(
            build(&Numbered, &ws).unwrap().0,
            "WINDOW w AS (PARTITION BY depname ), v AS ()"
        );
    }
}
