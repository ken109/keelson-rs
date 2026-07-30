use std::borrow::Cow;

use keelson_core::clause::Set;
use keelson_core::expr::{Expr, IntoExpr};
use keelson_core::{Expression, Query, SqlWriter};

// ---------------------------------------------------------------------------
// Statement modifiers
// ---------------------------------------------------------------------------

/// One of MySQL's statement modifiers — the keywords between a statement's first
/// word and its real content.
///
/// MySQL spreads these across four productions:
///
/// ```text
/// SELECT [ALL | DISTINCT | DISTINCTROW] [HIGH_PRIORITY] [STRAIGHT_JOIN]
///        [SQL_SMALL_RESULT] [SQL_BIG_RESULT] [SQL_BUFFER_RESULT]
///        [SQL_NO_CACHE] [SQL_CALC_FOUND_ROWS] …
/// INSERT [LOW_PRIORITY | DELAYED | HIGH_PRIORITY] [IGNORE] …
/// UPDATE [LOW_PRIORITY] [IGNORE] …
/// DELETE [LOW_PRIORITY] [QUICK] [IGNORE] …
/// ```
///
/// **The order below is the grammar's order**, and [`Modifiers`] keeps its list
/// sorted by it. That is the whole reason this is an enum rather than bob's
/// `[]string`: bob appends the keywords in whatever order the mods were written,
/// so `im.Ignore(), im.HighPriority()` produces `INSERT IGNORE HIGH_PRIORITY`,
/// which MySQL rejects. Here mod order cannot affect the output.
///
/// Which modifiers a statement *permits* is decided by which of them its mod
/// module re-exports, exactly as with the clause traits.
///
/// `ALL` is the default in `SELECT` and adds nothing, so it is not representable —
/// the absence of [`Distinct`](Modifier::Distinct) is what it means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Modifier {
    /// `DISTINCT` — drop duplicate result rows.
    Distinct,
    /// `DISTINCTROW`, MySQL's synonym for `DISTINCT`.
    DistinctRow,
    /// `LOW_PRIORITY` — wait for readers before writing.
    LowPriority,
    /// `HIGH_PRIORITY` — jump the queue ahead of pending writes.
    HighPriority,
    /// `DELAYED` — accepted and deprecated; MySQL treats it as `INSERT` and warns.
    Delayed,
    /// `QUICK` — do not merge index leaves while deleting.
    Quick,
    /// `IGNORE` — turn errors that would abort the statement into warnings.
    Ignore,
    /// `STRAIGHT_JOIN` — join tables in the order they are written.
    StraightJoin,
    /// `SQL_SMALL_RESULT` — the result set is small; use an in-memory temp table.
    SmallResult,
    /// `SQL_BIG_RESULT` — the result set is large; sort rather than use an index.
    BigResult,
    /// `SQL_BUFFER_RESULT` — force the result into a temporary table.
    BufferResult,
    /// `SQL_NO_CACHE` — do not read or write the query cache.
    NoCache,
    /// `SQL_CALC_FOUND_ROWS` — count the rows a `LIMIT` discarded.
    CalcFoundRows,
}

impl Modifier {
    /// The keyword, as written.
    pub fn as_str(self) -> &'static str {
        match self {
            Modifier::Distinct => "DISTINCT",
            Modifier::DistinctRow => "DISTINCTROW",
            Modifier::LowPriority => "LOW_PRIORITY",
            Modifier::HighPriority => "HIGH_PRIORITY",
            Modifier::Delayed => "DELAYED",
            Modifier::Quick => "QUICK",
            Modifier::Ignore => "IGNORE",
            Modifier::StraightJoin => "STRAIGHT_JOIN",
            Modifier::SmallResult => "SQL_SMALL_RESULT",
            Modifier::BigResult => "SQL_BIG_RESULT",
            Modifier::BufferResult => "SQL_BUFFER_RESULT",
            Modifier::NoCache => "SQL_NO_CACHE",
            Modifier::CalcFoundRows => "SQL_CALC_FOUND_ROWS",
        }
    }
}

/// The modifiers of one statement, kept in grammar order.
#[derive(Debug, Clone, Default)]
pub struct Modifiers {
    /// The modifiers, sorted and duplicate-free.
    pub modifiers: Vec<Modifier>,
}

impl Modifiers {
    /// Add a modifier, keeping the list sorted. A repeat is a no-op — writing
    /// `IGNORE IGNORE` says nothing the first one did not.
    pub fn append_modifier(&mut self, modifier: Modifier) {
        if let Err(at) = self.modifiers.binary_search(&modifier) {
            self.modifiers.insert(at, modifier);
        }
    }

    /// Whether there are no modifiers.
    pub fn is_empty(&self) -> bool {
        self.modifiers.is_empty()
    }
}

impl Expression for Modifiers {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        for (i, modifier) in self.modifiers.iter().enumerate() {
            if i > 0 {
                w.push_str(" ");
            }
            w.push_str(modifier.as_str());
        }
    }
}

/// A statement that takes MySQL's modifier keywords.
pub trait HasModifiers {
    /// The modifiers to add to.
    fn modifiers_mut(&mut self) -> &mut Modifiers;
}

impl HasModifiers for Modifiers {
    fn modifiers_mut(&mut self) -> &mut Modifiers {
        self
    }
}

// ---------------------------------------------------------------------------
// Optimizer hints
// ---------------------------------------------------------------------------

/// The `/*+ … */` optimizer-hint comment that may follow a statement's first
/// keyword (*10.9.2 Optimizer Hints*).
///
/// Each hint is written verbatim: the hint language is its own grammar, and
/// modelling all forty-odd hint names would be a second dialect inside this one.
/// The handful with a fixed shape have their own mods; everything else goes
/// through [`optimizer_hint`](crate::shared::optimizer_hint).
#[derive(Debug, Clone, Default)]
pub struct Hints {
    /// The hint bodies, in the order they were added.
    pub hints: Vec<Cow<'static, str>>,
}

impl Hints {
    /// Append a hint body, without the surrounding `/*+ */`.
    pub fn append_hint(&mut self, hint: impl Into<Cow<'static, str>>) {
        self.hints.push(hint.into());
    }

    /// Whether there are no hints.
    pub fn is_empty(&self) -> bool {
        self.hints.is_empty()
    }
}

impl Expression for Hints {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        if self.hints.is_empty() {
            return;
        }
        w.push_str("/*+ ");
        for (i, hint) in self.hints.iter().enumerate() {
            if i > 0 {
                w.push_str(" ");
            }
            w.push_str(hint);
        }
        w.push_str(" */");
    }
}

/// A statement that takes optimizer hints — all four of them do.
pub trait HasHints {
    /// The hints to add to.
    fn hints_mut(&mut self) -> &mut Hints;
}

impl HasHints for Hints {
    fn hints_mut(&mut self) -> &mut Hints {
        self
    }
}

// ---------------------------------------------------------------------------
// INSERT's row alias
// ---------------------------------------------------------------------------

/// `AS row_alias [(col_alias, …)]`, the name an `INSERT`'s new row is given
/// (MySQL 8.0.19).
///
/// It exists so that `ON DUPLICATE KEY UPDATE` can refer to the incoming values
/// by name instead of through the deprecated `VALUES()` function:
///
/// ```text
/// INSERT INTO t (a, b) VALUES (?, ?) AS `new` ON DUPLICATE KEY UPDATE b = `new`.b
/// ```
#[derive(Debug, Clone, Default)]
pub struct RowAlias {
    /// The row alias, quoted on output.
    pub name: Option<Cow<'static, str>>,
    /// Per-column aliases, quoted on output.
    pub columns: Vec<Cow<'static, str>>,
}

impl RowAlias {
    /// A row alias with no column aliases.
    pub fn new(name: impl Into<Cow<'static, str>>) -> RowAlias {
        RowAlias {
            name: Some(name.into()),
            columns: Vec::new(),
        }
    }

    /// Whether there is no alias. Column aliases alone are not a clause: the
    /// grammar hangs them off the row alias.
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
    }
}

impl Expression for RowAlias {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        let Some(name) = &self.name else {
            return;
        };
        w.push_str("AS ");
        w.push_quoted(&[name]);
        if !self.columns.is_empty() {
            w.push_str(" (");
            for (i, column) in self.columns.iter().enumerate() {
                if i > 0 {
                    w.push_str(", ");
                }
                w.push_quoted(&[column]);
            }
            w.push_str(")");
        }
    }
}

/// A statement that names its incoming row — only `INSERT` does.
pub trait HasRowAlias {
    /// The row alias to set.
    fn row_alias_mut(&mut self) -> &mut RowAlias;
}

impl HasRowAlias for RowAlias {
    fn row_alias_mut(&mut self) -> &mut RowAlias {
        self
    }
}

/// A statement with an `ON DUPLICATE KEY UPDATE` assignment list.
///
/// A separate trait from [`HasSet`](keelson_core::clause::HasSet) because an
/// `INSERT` has *two* assignment lists — the `INSERT … SET` row source and this
/// one — and a mod has to be able to say which it means. `HasSet` is the row
/// source; the body of `on_duplicate_key_update` is a bare
/// [`Set`](keelson_core::clause::Set), which implements `HasSet` reflexively, so
/// the same `set`/`set_col` mods serve both.
pub trait HasDuplicateKeyUpdate {
    /// The `ON DUPLICATE KEY UPDATE` assignments.
    fn duplicate_key_update_mut(&mut self) -> &mut Set;
}

// ---------------------------------------------------------------------------
// Sub-queries
// ---------------------------------------------------------------------------

/// A whole query standing in an expression slot, rendered in **its own** dialect.
///
/// [`SqlWriter::write_with_dialect`] keeps one shared argument list and
/// placeholder counter, so a sub-query re-indexes into its container for free —
/// which for MySQL means its arguments land in the right *positions* even though
/// every placeholder looks identical.
#[derive(Debug)]
struct QueryExpr<Q>(Q);

impl<Q: Query> Expression for QueryExpr<Q> {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.write_with_dialect(self.0.dialect(), &self.0);
    }
}

/// A query as an expression, **not** parenthesised.
///
/// The form for slots that supply their own parentheses — a `WITH` body, a
/// set-operation operand, `IN (…)`, `INSERT … SELECT`. Use [`subquery`] where the
/// parentheses belong to the sub-query itself, as in a `FROM` item.
pub fn query(q: impl Query + 'static) -> Expr {
    Expr::custom(QueryExpr(q))
}

/// A parenthesised sub-query: `(SELECT …)`.
///
/// What a `FROM` item or a scalar sub-expression needs. MySQL additionally
/// requires an alias on a derived table, which is
/// [`select::from(..).as_(..)`](crate::select::from).
pub fn subquery(q: impl Query + 'static) -> Expr {
    Expr::group(query(q))
}

// ---------------------------------------------------------------------------
// ON DUPLICATE KEY UPDATE value sources
// ---------------------------------------------------------------------------

/// `VALUES(`col`)` — the value the `INSERT` proposed for `col`.
///
/// The pre-8.0.19 way to reach the incoming row inside
/// `ON DUPLICATE KEY UPDATE`. MySQL deprecates it in favour of a row alias, which
/// is [`row_value`]; both are still accepted by 8.4.
///
/// Note that `VALUES()` means something entirely different anywhere else — it is
/// the ordinary `VALUES` row constructor — so this belongs only inside an
/// `ON DUPLICATE KEY UPDATE` body.
pub fn values_of(column: impl Into<Cow<'static, str>>) -> Expr {
    Expr::func("VALUES", Expr::ident(column.into()))
}

/// ``` `alias`.`col` ``` — the incoming row's column, through the row alias set by
/// [`insert::as_`](crate::insert::as_).
pub fn row_value(
    alias: impl Into<Cow<'static, str>>,
    column: impl Into<Cow<'static, str>>,
) -> Expr {
    Expr::ident([alias.into(), column.into()])
}

/// `MATCH (cols) AGAINST (expr [modifier])` — a full-text search predicate
/// (*14.9 Full-Text Search Functions*).
///
/// The modifier is written verbatim after the search string, so
/// `IN NATURAL LANGUAGE MODE`, `IN BOOLEAN MODE` and `WITH QUERY EXPANSION` all
/// work; `None` leaves it out, which is natural-language mode.
#[derive(Debug)]
pub(crate) struct Match {
    pub(crate) columns: Vec<Expr>,
    pub(crate) against: Expr,
    pub(crate) modifier: Option<Cow<'static, str>>,
}

impl Expression for Match {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.push_str("MATCH (");
        w.write_slice(&self.columns, "", ", ", "");
        w.push_str(") AGAINST (");
        w.write_expr(&self.against);
        if let Some(modifier) = &self.modifier {
            w.push_str(" ");
            w.push_str(modifier);
        }
        w.push_str(")");
    }
}

/// `MATCH (cols) AGAINST (search)` in natural-language mode.
pub fn match_against(
    columns: impl keelson_core::expr::IntoExprList,
    search: impl IntoExpr,
) -> Expr {
    Expr::custom(Match {
        columns: columns.into_expr_list(),
        against: search.into_expr(),
        modifier: None,
    })
}

/// `MATCH (cols) AGAINST (search IN BOOLEAN MODE)`, or any other search modifier
/// written out.
pub fn match_against_mode(
    columns: impl keelson_core::expr::IntoExprList,
    search: impl IntoExpr,
    modifier: impl Into<Cow<'static, str>>,
) -> Expr {
    Expr::custom(Match {
        columns: columns.into_expr_list(),
        against: search.into_expr(),
        modifier: Some(modifier.into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Mysql, quote, s};
    use keelson_core::build;

    fn sql(e: impl Expression) -> String {
        build(&Mysql, &e).expect("render").0
    }

    /// The grammar's order, not the caller's. `SELECT DISTINCT HIGH_PRIORITY …`
    /// parses; `SELECT HIGH_PRIORITY DISTINCT …` does not.
    #[test]
    fn modifiers_render_in_grammar_order_whatever_order_they_were_added_in() {
        let mut m = Modifiers::default();
        m.append_modifier(Modifier::CalcFoundRows);
        m.append_modifier(Modifier::HighPriority);
        m.append_modifier(Modifier::Distinct);
        m.append_modifier(Modifier::StraightJoin);
        assert_eq!(
            sql(m),
            "DISTINCT HIGH_PRIORITY STRAIGHT_JOIN SQL_CALC_FOUND_ROWS"
        );
    }

    #[test]
    fn a_repeated_modifier_is_written_once() {
        let mut m = Modifiers::default();
        m.append_modifier(Modifier::Ignore);
        m.append_modifier(Modifier::Ignore);
        assert_eq!(sql(m), "IGNORE");
    }

    #[test]
    fn an_empty_modifier_list_writes_nothing() {
        assert_eq!(sql(Modifiers::default()), "");
        assert!(Modifiers::default().is_empty());
    }

    /// *10.9.2*: `SELECT /*+ MAX_EXECUTION_TIME(1000) */ …`.
    #[test]
    fn hints_are_wrapped_in_one_comment_and_space_separated() {
        let mut h = Hints::default();
        assert_eq!(sql(h.clone()), "");
        h.append_hint("MAX_EXECUTION_TIME(1000)");
        assert_eq!(sql(h.clone()), "/*+ MAX_EXECUTION_TIME(1000) */");
        h.append_hint("QB_NAME(outer)");
        assert_eq!(sql(h), "/*+ MAX_EXECUTION_TIME(1000) QB_NAME(outer) */");
    }

    #[test]
    fn a_row_alias_quotes_its_name_and_its_columns() {
        assert_eq!(sql(RowAlias::default()), "");
        assert_eq!(sql(RowAlias::new("new")), "AS `new`");
        assert_eq!(
            sql(RowAlias {
                name: Some("new".into()),
                columns: vec!["a".into(), "b".into()],
            }),
            "AS `new` (`a`, `b`)"
        );
        // Column aliases alone are not a clause.
        assert_eq!(
            sql(RowAlias {
                name: None,
                columns: vec!["a".into()],
            }),
            ""
        );
    }

    #[test]
    fn the_two_upsert_value_sources_render_as_the_manual_writes_them() {
        assert_eq!(sql(values_of("name")), "VALUES(`name`)");
        assert_eq!(sql(row_value("new", "name")), "`new`.`name`");
    }

    /// *14.9*: `MATCH (col1, col2) AGAINST (expr [search_modifier])`.
    #[test]
    fn match_against_puts_the_modifier_inside_the_against_parentheses() {
        assert_eq!(
            sql(match_against(quote("title"), s("rust"))),
            "MATCH (`title`) AGAINST ('rust')"
        );
        assert_eq!(
            sql(match_against_mode(
                (quote("title"), quote("status")),
                s("+rust -go"),
                "IN BOOLEAN MODE"
            )),
            "MATCH (`title`, `status`) AGAINST ('+rust -go' IN BOOLEAN MODE)"
        );
    }
}
