use crate::error::Result;
use crate::writer::{DynExpr, Expression, SqlWriter};

pub const FRAME_MODE_RANGE: &str = "RANGE";
pub const FRAME_MODE_ROWS: &str = "ROWS";
pub const FRAME_MODE_GROUPS: &str = "GROUPS";

/// A window frame: `RANGE BETWEEN <start> AND <end> EXCLUDE <exclusion>`.
///
/// `defined` is separate from the other fields because an all-default frame is
/// still a *written* frame — `RANGE UNBOUNDED PRECEDING` — and only the mods know
/// whether the caller asked for one. Every setter flips it, so use them rather
/// than assigning the fields.
#[derive(Debug, Clone, Default)]
pub struct Frame {
    /// Whether any part of the frame was set by the caller.
    pub defined: bool,
    /// One of the `FRAME_MODE_*` constants; empty renders as `RANGE`.
    pub mode: String,
    /// Empty renders as `UNBOUNDED PRECEDING`.
    pub start: Option<DynExpr>,
    /// When present the frame becomes a `BETWEEN … AND …`.
    pub end: Option<DynExpr>,
    /// `NO OTHERS`, `CURRENT ROW`, `GROUP` or `TIES`.
    pub exclusion: String,
}

impl Frame {
    pub fn set_mode(&mut self, mode: impl Into<String>) {
        self.defined = true;
        self.mode = mode.into();
    }

    pub fn set_start(&mut self, start: DynExpr) {
        self.defined = true;
        self.start = Some(start);
    }

    pub fn set_end(&mut self, end: DynExpr) {
        self.defined = true;
        self.end = Some(end);
    }

    pub fn set_exclusion(&mut self, exclusion: impl Into<String>) {
        self.defined = true;
        self.exclusion = exclusion.into();
    }
}

impl Expression for Frame {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        w.push_str(if self.mode.is_empty() {
            FRAME_MODE_RANGE
        } else {
            &self.mode
        });
        w.push_str(" ");

        if self.end.is_some() {
            w.push_str("BETWEEN ");
        }

        match &self.start {
            Some(start) => w.write_expr(start)?,
            None => w.push_str("UNBOUNDED PRECEDING"),
        }

        if let Some(end) = &self.end {
            w.push_str(" AND ");
            w.write_expr(end)?;
        }

        if !self.exclusion.is_empty() {
            w.push_str(" EXCLUDE ");
            w.push_str(&self.exclusion);
        }

        Ok(())
    }
}

/// A [`Window`](super::Window) whose frame can be modified.
pub trait HasFrame {
    fn frame_mut(&mut self) -> &mut Frame;
}

impl HasFrame for Frame {
    fn frame_mut(&mut self) -> &mut Frame {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::writer::{build, dyn_expr, expr_fn};

    #[test]
    fn a_default_frame_still_has_a_mode_and_a_start() {
        assert_eq!(
            build(&Numbered, &Frame::default()).unwrap().0,
            "RANGE UNBOUNDED PRECEDING"
        );
    }

    #[test]
    fn setting_anything_marks_the_frame_defined() {
        let mut f = Frame::default();
        assert!(!f.defined);
        f.set_mode(FRAME_MODE_ROWS);
        assert!(f.defined);
    }

    #[test]
    fn an_end_turns_the_frame_into_a_between() {
        let mut f = Frame::default();
        f.set_mode(FRAME_MODE_ROWS);
        f.set_end(dyn_expr("CURRENT ROW"));
        assert_eq!(
            build(&Numbered, &f).unwrap().0,
            "ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW"
        );
    }

    #[test]
    fn bounds_may_bind_arguments_and_are_numbered_left_to_right() {
        let bound = |n: i32, dir: &'static str| {
            dyn_expr(expr_fn(move |w: &mut SqlWriter<'_>| {
                w.push_arg(n);
                w.push_str(dir);
                Ok(())
            }))
        };

        let mut f = Frame::default();
        f.set_mode(FRAME_MODE_GROUPS);
        f.set_start(bound(1, " PRECEDING"));
        f.set_end(bound(2, " FOLLOWING"));
        f.set_exclusion("TIES");

        let (sql, args) = build(&Numbered, &f).unwrap();
        assert_eq!(
            sql,
            "GROUPS BETWEEN $1 PRECEDING AND $2 FOLLOWING EXCLUDE TIES"
        );
        assert_eq!(
            args.len(),
            2,
            "bob mis-indexes the end bound here; we do not"
        );
    }
}
