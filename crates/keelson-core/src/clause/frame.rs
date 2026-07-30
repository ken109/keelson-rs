use crate::expr::{Expr, IntoExpr};
use crate::writer::{Expression, SqlWriter};

/// A window frame: which rows around the current one the window function sees.
///
/// From PostgreSQL 17,
/// <https://www.postgresql.org/docs/17/sql-expressions.html#SYNTAX-WINDOW-FUNCTIONS>:
///
/// ```text
/// { RANGE | ROWS | GROUPS } frame_start [ frame_exclusion ]
/// { RANGE | ROWS | GROUPS } BETWEEN frame_start AND frame_end [ frame_exclusion ]
///
/// frame_start, frame_end:
///     UNBOUNDED PRECEDING | offset PRECEDING | CURRENT ROW
///   | offset FOLLOWING    | UNBOUNDED FOLLOWING
/// frame_exclusion:
///     EXCLUDE CURRENT ROW | EXCLUDE GROUP | EXCLUDE TIES | EXCLUDE NO OTHERS
/// ```
///
/// Two defaults from that grammar are baked in, because they are what the keywords
/// mean rather than what this library prefers:
///
/// - the mode defaults to `RANGE`, and
/// - `frame_start` defaults to `UNBOUNDED PRECEDING`.
///
/// So setting only [`exclusion`](Self::exclusion) still renders a complete frame,
/// `RANGE UNBOUNDED PRECEDING EXCLUDE TIES`. **`BETWEEN` appears exactly when
/// there is an end bound** — that is the whole difference between the two
/// productions, and writing it without one is a syntax error.
///
/// bob carries a separate `Defined bool` that its setters flip, which can disagree
/// with the fields. Here "defined" is derived: a frame is absent when all four
/// parts are, which is also what makes [`Frame::default()`](Default) render
/// nothing.
#[derive(Debug, Clone, Default)]
pub struct Frame {
    /// What the offsets count in. `None` renders as `RANGE`.
    pub mode: Option<FrameMode>,
    /// The start bound. `None` renders as `UNBOUNDED PRECEDING`.
    pub start: Option<Expr>,
    /// The end bound. Its presence is what turns the frame into a `BETWEEN`.
    pub end: Option<Expr>,
    /// Rows to leave out of the frame even though they are inside it.
    pub exclusion: Option<FrameExclusion>,
}

impl Frame {
    /// A frame in `mode`, from `UNBOUNDED PRECEDING`.
    pub fn new(mode: FrameMode) -> Self {
        Frame {
            mode: Some(mode),
            ..Frame::default()
        }
    }

    /// Set the mode.
    pub fn set_mode(&mut self, mode: FrameMode) {
        self.mode = Some(mode);
    }

    /// Set the start bound.
    pub fn set_start(&mut self, start: impl IntoExpr) {
        self.start = Some(start.into_expr());
    }

    /// Set the end bound, making the frame a `BETWEEN`.
    pub fn set_end(&mut self, end: impl IntoExpr) {
        self.end = Some(end.into_expr());
    }

    /// Set the exclusion.
    pub fn set_exclusion(&mut self, exclusion: FrameExclusion) {
        self.exclusion = Some(exclusion);
    }

    /// Whether no part of the frame was set, so that nothing will be written.
    pub fn is_empty(&self) -> bool {
        self.mode.is_none()
            && self.start.is_none()
            && self.end.is_none()
            && self.exclusion.is_none()
    }
}

impl Expression for Frame {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        if self.is_empty() {
            return;
        }

        w.push_str(self.mode.unwrap_or(FrameMode::Range).as_str());
        w.push_str(" ");

        if self.end.is_some() {
            w.push_str("BETWEEN ");
        }

        match &self.start {
            Some(start) => w.write_expr(start),
            None => w.push_str("UNBOUNDED PRECEDING"),
        }

        if let Some(end) = &self.end {
            w.push_str(" AND ");
            w.write_expr(end);
        }

        if let Some(exclusion) = &self.exclusion {
            w.push_str(" EXCLUDE ");
            w.push_str(exclusion.as_str());
        }
    }
}

/// Anything with a window frame — a [`Window`](super::Window) definition.
pub trait HasFrame {
    /// The frame to modify.
    fn frame_mut(&mut self) -> &mut Frame;
}

impl HasFrame for Frame {
    fn frame_mut(&mut self) -> &mut Frame {
        self
    }
}

/// What a frame's offsets count in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameMode {
    /// `RANGE` — offsets are values compared against the `ORDER BY` key. The
    /// default in the grammar.
    Range,
    /// `ROWS` — offsets are row counts.
    Rows,
    /// `GROUPS` — offsets are counts of peer groups.
    Groups,
}

impl FrameMode {
    /// The keyword, as written.
    pub fn as_str(self) -> &'static str {
        match self {
            FrameMode::Range => "RANGE",
            FrameMode::Rows => "ROWS",
            FrameMode::Groups => "GROUPS",
        }
    }
}

/// Which rows a frame leaves out despite their being inside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameExclusion {
    /// `EXCLUDE NO OTHERS` — the default; excludes nothing.
    NoOthers,
    /// `EXCLUDE CURRENT ROW`.
    CurrentRow,
    /// `EXCLUDE GROUP` — the current row and all its peers.
    Group,
    /// `EXCLUDE TIES` — the current row's peers, but not the row itself.
    Ties,
}

impl FrameExclusion {
    /// The keyword, as written after `EXCLUDE`.
    pub fn as_str(self) -> &'static str {
        match self {
            FrameExclusion::NoOthers => "NO OTHERS",
            FrameExclusion::CurrentRow => "CURRENT ROW",
            FrameExclusion::Group => "GROUP",
            FrameExclusion::Ties => "TIES",
        }
    }
}

#[cfg(test)]
mod tests {
    use keelson_sqlcheck::testing::assert_frag_sql;

    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::expr::arg;
    use crate::value::Value;
    use crate::writer::build;

    /// A frame clause is the tail of a window definition. The frame carries an
    /// `ORDER BY` because `GROUPS` mode and the `EXCLUDE` variants need one.
    const FRAME: &str = r#"SELECT count(*) OVER (ORDER BY "id" {}) FROM users"#;

    fn sql(f: &Frame) -> String {
        build(&Numbered, f).expect("render").0
    }

    #[test]
    fn an_untouched_frame_writes_nothing() {
        assert_frag_sql(FRAME, &sql(&Frame::default()), "");
        assert!(Frame::default().is_empty());
    }

    #[test]
    fn a_mode_alone_gets_the_grammars_default_start() {
        // `ROWS` is not a frame_clause on its own; frame_start is mandatory and
        // UNBOUNDED PRECEDING is what it defaults to.
        assert_frag_sql(
            FRAME,
            &sql(&Frame::new(FrameMode::Rows)),
            "ROWS UNBOUNDED PRECEDING",
        );
    }

    #[test]
    fn an_exclusion_alone_still_produces_a_complete_frame() {
        // Both defaults apply at once: RANGE, and UNBOUNDED PRECEDING.
        let mut f = Frame::default();
        f.set_exclusion(FrameExclusion::Ties);
        assert!(!f.is_empty());
        assert_frag_sql(FRAME, &sql(&f), "RANGE UNBOUNDED PRECEDING EXCLUDE TIES");
    }

    #[test]
    fn a_start_alone_is_the_single_bound_form_with_no_between() {
        // The first production: no BETWEEN, no AND.
        let mut f = Frame::new(FrameMode::Rows);
        f.set_start("CURRENT ROW");
        assert_frag_sql(FRAME, &sql(&f), "ROWS CURRENT ROW");
    }

    #[test]
    fn an_end_bound_is_what_introduces_between() {
        // The second production. The end alone still means BETWEEN, with the
        // default start.
        let mut f = Frame::new(FrameMode::Rows);
        f.set_end("CURRENT ROW");
        assert_frag_sql(
            FRAME,
            &sql(&f),
            "ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW",
        );

        f.set_start("UNBOUNDED PRECEDING");
        assert_frag_sql(
            FRAME,
            &sql(&f),
            "ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW",
        );
    }

    #[test]
    fn bounds_may_bind_arguments_and_are_numbered_left_to_right() {
        // `offset PRECEDING` with a bound offset. bob adds the wrong length to
        // `start` for the end bound and re-uses the start's index; there is one
        // counter here, so it cannot happen.
        let mut f = Frame::new(FrameMode::Groups);
        f.set_start(Expr::join((arg(1i32), Expr::raw("PRECEDING"))));
        f.set_end(Expr::join((arg(2i32), Expr::raw("FOLLOWING"))));
        f.set_exclusion(FrameExclusion::CurrentRow);

        let (rendered, args) = build(&Numbered, &f).unwrap();
        assert_frag_sql(
            FRAME,
            &rendered,
            "GROUPS BETWEEN $1 PRECEDING AND $2 FOLLOWING EXCLUDE CURRENT ROW",
        );
        assert_eq!(args, vec![Value::I32(1), Value::I32(2)]);
    }

    #[test]
    fn every_mode_and_exclusion_has_its_spelling() {
        for (mode, keyword) in [
            (FrameMode::Range, "RANGE"),
            (FrameMode::Rows, "ROWS"),
            (FrameMode::Groups, "GROUPS"),
        ] {
            assert_frag_sql(
                FRAME,
                &sql(&Frame::new(mode)),
                &format!("{keyword} UNBOUNDED PRECEDING"),
            );
        }

        for (exclusion, keyword) in [
            (FrameExclusion::NoOthers, "NO OTHERS"),
            (FrameExclusion::CurrentRow, "CURRENT ROW"),
            (FrameExclusion::Group, "GROUP"),
            (FrameExclusion::Ties, "TIES"),
        ] {
            let mut f = Frame::new(FrameMode::Rows);
            f.set_exclusion(exclusion);
            assert_frag_sql(
                FRAME,
                &sql(&f),
                &format!("ROWS UNBOUNDED PRECEDING EXCLUDE {keyword}"),
            );
        }
    }
}
