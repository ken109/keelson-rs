use std::borrow::Cow;

use crate::error::Error;
use crate::expr::{Expr, IntoExpr};
use crate::writer::{Expression, SqlWriter};

use super::{MaybeAbsent, write_quoted_list};

/// One common table expression.
///
/// From PostgreSQL 17, <https://www.postgresql.org/docs/17/sql-select.html>:
///
/// ```text
/// with_query_name [ ( column_name [, ...] ) ] AS [ [ NOT ] MATERIALIZED ] ( select | … )
///     [ SEARCH { BREADTH | DEPTH } FIRST BY column_name [, ...] SET search_seq_col_name ]
///     [ CYCLE column_name [, ...] SET cycle_mark_col_name
///       [ TO cycle_mark_value DEFAULT cycle_mark_default ] USING cycle_path_col_name ]
/// ```
///
/// The SQL standard allows only a `SELECT` here; PostgreSQL also allows
/// `INSERT`/`UPDATE`/`DELETE`, which is why [`query`](Self::query) is an ordinary
/// expression and not something narrower.
#[derive(Debug, Clone, Default)]
pub struct Cte {
    /// The name the rest of the statement refers to. Quoted on output.
    pub name: Cow<'static, str>,
    /// Column names for the result. Quoted.
    pub columns: Vec<Cow<'static, str>>,
    /// The query, rendered inside parentheses.
    pub query: Option<Expr>,
    /// `MATERIALIZED` / `NOT MATERIALIZED`. `None` writes neither and leaves the
    /// choice to the planner — which is not the same as either, so this is
    /// genuinely three-valued.
    pub materialized: Option<bool>,
    /// `SEARCH …`, for a recursive CTE.
    pub search: CteSearch,
    /// `CYCLE …`, for a recursive CTE over cyclic data.
    pub cycle: CteCycle,
}

impl Cte {
    /// A named CTE over `query`.
    pub fn new(name: impl Into<Cow<'static, str>>, query: impl IntoExpr) -> Self {
        Cte {
            name: name.into(),
            query: Some(query.into_expr()),
            ..Cte::default()
        }
    }

    /// Whether this is an untouched CTE, so that nothing will be written.
    pub fn is_empty(&self) -> bool {
        self.name.is_empty() && self.query.is_none()
    }
}

impl Expression for Cte {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        if self.is_empty() {
            return;
        }

        let Some(query) = &self.query else {
            // A named CTE with no query is a caller error rather than an absent
            // clause: there is no rendering of it that parses.
            w.record_error(Error::Incomplete("the query of a CTE"));
            return;
        };

        w.push_quoted(&[&self.name]);
        write_quoted_list(w, &self.columns, " (", ", ", ")");
        w.push_str(" AS ");

        match self.materialized {
            None => {}
            Some(true) => w.push_str("MATERIALIZED "),
            Some(false) => w.push_str("NOT MATERIALIZED "),
        }

        w.push_str("(");
        w.write_expr(query);
        w.push_str(")");

        w.write_if(!self.search.is_empty(), " ", &self.search, "");
        w.write_if(!self.cycle.is_empty(), " ", &self.cycle, "");
    }
}

/// `SEARCH { BREADTH | DEPTH } FIRST BY <cols> SET <col>`
///
/// PostgreSQL rewrites a recursive CTE with this into one that carries an ordering
/// column, so the column list decides whether the clause exists at all.
#[derive(Debug, Clone, Default)]
pub struct CteSearch {
    /// Breadth-first or depth-first.
    pub order: SearchOrder,
    /// The columns that identify a row, quoted.
    pub columns: Vec<Cow<'static, str>>,
    /// The name of the ordering column to add, quoted.
    pub set: Cow<'static, str>,
}

impl CteSearch {
    /// A search clause over `columns`, adding `set`.
    pub fn new(
        order: SearchOrder,
        columns: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
        set: impl Into<Cow<'static, str>>,
    ) -> Self {
        CteSearch {
            order,
            columns: columns.into_iter().map(Into::into).collect(),
            set: set.into(),
        }
    }

    /// Whether the clause is absent.
    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }
}

impl Expression for CteSearch {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        if self.is_empty() {
            return;
        }
        if self.set.is_empty() {
            // The SET column is not optional in the grammar, and it is the whole
            // point of the clause — it is what the outer query orders by.
            w.record_error(Error::Incomplete("the SET column of a CTE SEARCH clause"));
            return;
        }

        w.push_str("SEARCH ");
        w.push_str(self.order.as_str());
        w.push_str(" FIRST BY ");
        write_quoted_list(w, &self.columns, "", ", ", "");
        w.push_str(" SET ");
        w.push_quoted(&[&self.set]);
    }
}

/// Which way a recursive CTE's `SEARCH` walks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SearchOrder {
    /// `BREADTH FIRST`.
    #[default]
    Breadth,
    /// `DEPTH FIRST`.
    Depth,
}

impl SearchOrder {
    /// The keyword, as written between `SEARCH` and `FIRST`.
    pub fn as_str(self) -> &'static str {
        match self {
            SearchOrder::Breadth => "BREADTH",
            SearchOrder::Depth => "DEPTH",
        }
    }
}

/// `CYCLE <cols> SET <col> [TO <value> DEFAULT <value>] USING <col>`
///
/// Stops a recursive CTE from looping forever over cyclic data by marking the row
/// that closes a cycle.
///
/// # The mark values must be constants
///
/// PostgreSQL's grammar spells the optional group
/// `TO AexprConst DEFAULT AexprConst` — a *literal constant*, not an expression, so
/// a bound argument in either slot is rejected by the server with a syntax error at
/// the placeholder. libpg_query confirms it. The fields are [`Expr`]s because
/// everything else in this module is, and because a constant is written
/// [`expr::literal`](crate::expr::literal) or [`expr::raw`](crate::expr::raw); a
/// dialect's `cycle` mod is where the narrowing belongs.
#[derive(Debug, Clone, Default)]
pub struct CteCycle {
    /// The columns that identify a row, quoted.
    pub columns: Vec<Cow<'static, str>>,
    /// The name of the cycle-mark column to add, quoted.
    pub set: Cow<'static, str>,
    /// The name of the path column to add, quoted.
    pub using: Cow<'static, str>,
    /// The constant the mark column takes on a cycle. The grammar pairs this with
    /// [`default_val`](Self::default_val): both or neither.
    pub to: Option<Expr>,
    /// The constant the mark column takes otherwise.
    pub default_val: Option<Expr>,
}

impl CteCycle {
    /// A cycle clause over `columns`, adding the mark column `set` and the path
    /// column `using`.
    pub fn new(
        columns: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
        set: impl Into<Cow<'static, str>>,
        using: impl Into<Cow<'static, str>>,
    ) -> Self {
        CteCycle {
            columns: columns.into_iter().map(Into::into).collect(),
            set: set.into(),
            using: using.into(),
            ..CteCycle::default()
        }
    }

    /// Whether the clause is absent.
    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }
}

impl Expression for CteCycle {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        if self.is_empty() {
            return;
        }
        if self.set.is_empty() || self.using.is_empty() {
            w.record_error(Error::Incomplete(
                "the SET and USING columns of a CTE CYCLE clause",
            ));
            return;
        }
        // `[ TO value DEFAULT value ]` is one optional group, so half of it is
        // unrenderable rather than merely unusual.
        if self.to.is_some() != self.default_val.is_some() {
            w.record_error(Error::Incomplete(
                "both TO and DEFAULT of a CTE CYCLE clause",
            ));
            return;
        }

        w.push_str("CYCLE ");
        write_quoted_list(w, &self.columns, "", ", ", "");
        w.push_str(" SET ");
        w.push_quoted(&[&self.set]);

        if let (Some(to), Some(default_val)) = (&self.to, &self.default_val) {
            w.push_str(" TO ");
            w.write_expr(to);
            w.push_str(" DEFAULT ");
            w.write_expr(default_val);
        }

        w.push_str(" USING ");
        w.push_quoted(&[&self.using]);
    }
}

impl MaybeAbsent for Cte {
    fn is_absent(&self) -> bool {
        self.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::expr::arg;
    use crate::value::Value;
    use crate::writer::build;

    /// `SELECT $1`, standing in for a sub-query.
    fn sub() -> Expr {
        Expr::join((Expr::raw("SELECT"), arg(1i32)))
    }

    #[test]
    fn an_untouched_cte_writes_nothing() {
        assert_eq!(build(&Numbered, &Cte::default()).unwrap().0, "");
        assert!(Cte::default().is_empty());
    }

    #[test]
    fn a_bare_cte_is_name_as_query() {
        let (sql, args) = build(&Numbered, &Cte::new("c", sub())).unwrap();
        assert_eq!(sql, r#""c" AS (SELECT $1)"#);
        assert_eq!(args, vec![Value::I32(1)]);
    }

    #[test]
    fn column_names_follow_the_cte_name() {
        // PostgreSQL 17: with_query_name [ ( column_name [, ...] ) ] AS ( … )
        let cte = Cte {
            columns: vec!["id".into(), "data".into()],
            ..Cte::new("c", sub())
        };
        assert_eq!(
            build(&Numbered, &cte).unwrap().0,
            r#""c" ("id", "data") AS (SELECT $1)"#
        );
    }

    #[test]
    fn materialisation_is_three_valued() {
        // `AS ( … )`, `AS MATERIALIZED ( … )` and `AS NOT MATERIALIZED ( … )` are
        // three different instructions to the planner, so None is not a synonym
        // for either of the others.
        let base = Cte::new("c", sub());
        assert_eq!(build(&Numbered, &base).unwrap().0, r#""c" AS (SELECT $1)"#);

        let yes = Cte {
            materialized: Some(true),
            ..base.clone()
        };
        assert_eq!(
            build(&Numbered, &yes).unwrap().0,
            r#""c" AS MATERIALIZED (SELECT $1)"#
        );

        let no = Cte {
            materialized: Some(false),
            ..base
        };
        assert_eq!(
            build(&Numbered, &no).unwrap().0,
            r#""c" AS NOT MATERIALIZED (SELECT $1)"#
        );
    }

    #[test]
    fn a_named_cte_with_no_query_is_a_recorded_failure() {
        let cte = Cte {
            name: "c".into(),
            ..Cte::default()
        };
        assert_eq!(
            build(&Numbered, &cte).unwrap_err().to_string(),
            "query is missing the query of a CTE"
        );
    }

    #[test]
    fn search_and_cycle_follow_the_query_and_hinge_on_their_columns() {
        let mut cte = Cte::new("c", sub());
        cte.search = CteSearch::new(SearchOrder::Depth, ["id"], "ordercol");
        // No columns, so the whole CYCLE clause stays out even though the two
        // column names are filled in.
        cte.cycle = CteCycle {
            set: "is_cycle".into(),
            using: "path".into(),
            ..CteCycle::default()
        };

        assert_eq!(
            build(&Numbered, &cte).unwrap().0,
            r#""c" AS (SELECT $1) SEARCH DEPTH FIRST BY "id" SET "ordercol""#
        );

        cte.cycle.columns = vec!["id".into()];
        assert_eq!(
            build(&Numbered, &cte).unwrap().0,
            r#""c" AS (SELECT $1) SEARCH DEPTH FIRST BY "id" SET "ordercol" CYCLE "id" SET "is_cycle" USING "path""#
        );
    }

    #[test]
    fn breadth_is_the_default_search_order() {
        let search = CteSearch::new(SearchOrder::default(), ["id"], "seq");
        assert_eq!(
            build(&Numbered, &search).unwrap().0,
            r#"SEARCH BREADTH FIRST BY "id" SET "seq""#
        );
    }

    #[test]
    fn a_search_clause_without_its_set_column_is_a_recorded_failure() {
        let search = CteSearch {
            columns: vec!["id".into()],
            ..CteSearch::default()
        };
        assert_eq!(
            build(&Numbered, &search).unwrap_err().to_string(),
            "query is missing the SET column of a CTE SEARCH clause"
        );
    }

    #[test]
    fn the_cycle_mark_values_are_written_as_one_optional_group() {
        // PostgreSQL 17: `[ TO cycle_mark_value DEFAULT cycle_mark_default ]` — one
        // bracket around both, so one is never legal without the other. Both are
        // `AexprConst` in gram.y, which is why these are literals: libpg_query
        // rejects `TO $1` outright.
        let mut cycle = CteCycle::new(["id"], "is_cycle", "path");
        cycle.to = Some(Expr::literal("Y"));
        cycle.default_val = Some(Expr::literal("N"));

        let (sql, args) = build(&Numbered, &cycle).unwrap();
        assert_eq!(
            sql,
            r#"CYCLE "id" SET "is_cycle" TO 'Y' DEFAULT 'N' USING "path""#
        );
        assert!(args.is_empty(), "a constant binds nothing");

        cycle.default_val = None;
        assert_eq!(
            build(&Numbered, &cycle).unwrap_err().to_string(),
            "query is missing both TO and DEFAULT of a CTE CYCLE clause"
        );
    }

    #[test]
    fn a_cycle_clause_without_its_added_columns_is_a_recorded_failure() {
        let cycle = CteCycle {
            columns: vec!["id".into()],
            ..CteCycle::default()
        };
        assert_eq!(
            build(&Numbered, &cycle).unwrap_err().to_string(),
            "query is missing the SET and USING columns of a CTE CYCLE clause"
        );
    }
}
