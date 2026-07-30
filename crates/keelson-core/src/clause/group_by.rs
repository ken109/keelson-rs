use crate::expr::{Expr, IntoExpr, IntoExprList};
use crate::writer::{Expression, SqlWriter};

/// `GROUP BY [DISTINCT] a, b [WITH ROLLUP]`
///
/// ```text
/// GROUP BY [ ALL | DISTINCT ] grouping_element [, ...]      -- PostgreSQL 17
/// GROUP BY expr [, ...] [WITH ROLLUP]                       -- MySQL 8.4
/// ```
///
/// `ALL` is the default and writing it adds nothing, so only `DISTINCT` is
/// representable.
#[derive(Debug, Clone, Default)]
pub struct GroupBy {
    /// The grouping elements. A [`GroupingSet`] is one of these.
    pub groups: Vec<Expr>,
    /// PostgreSQL's `GROUP BY DISTINCT`, which de-duplicates the grouping sets a
    /// `CUBE` or `ROLLUP` expands to.
    pub distinct: bool,
    /// MySQL's `WITH ROLLUP`.
    pub with: Option<GroupByWith>,
}

impl GroupBy {
    /// Replace the grouping elements.
    pub fn set_groups(&mut self, groups: impl IntoExprList) {
        self.groups = groups.into_expr_list();
    }

    /// Append one grouping element.
    pub fn append_group(&mut self, group: impl IntoExpr) {
        self.groups.push(group.into_expr());
    }

    /// Whether the clause is absent.
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }
}

impl Expression for GroupBy {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        // The group list gates the whole clause: `GROUP BY DISTINCT` and
        // `WITH ROLLUP` are modifiers of a list, and neither is SQL without one.
        if self.groups.is_empty() {
            return;
        }

        w.push_str("GROUP BY ");
        if self.distinct {
            w.push_str("DISTINCT ");
        }
        w.write_slice(&self.groups, "", ", ", "");

        if let Some(with) = &self.with {
            w.push_str(" WITH ");
            w.push_str(with.as_str());
        }
    }
}

/// A statement with a `GROUP BY` clause.
pub trait HasGroupBy {
    /// The `GROUP BY` clause to modify.
    fn group_by_mut(&mut self) -> &mut GroupBy;
}

impl HasGroupBy for GroupBy {
    fn group_by_mut(&mut self) -> &mut GroupBy {
        self
    }
}

/// MySQL's trailing `WITH …` modifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupByWith {
    /// `WITH ROLLUP`.
    Rollup,
    /// `WITH CUBE`.
    Cube,
}

impl GroupByWith {
    /// The keyword, as written.
    pub fn as_str(self) -> &'static str {
        match self {
            GroupByWith::Rollup => "ROLLUP",
            GroupByWith::Cube => "CUBE",
        }
    }
}

/// One grouping element that is itself a set: `ROLLUP (a, b)`, `CUBE (a, b)`,
/// `GROUPING SETS ((a), (b), ())`.
///
/// From PostgreSQL 17's `grouping_element`. Meant to be put into
/// [`GroupBy::groups`] beside plain expressions.
#[derive(Debug, Clone, Default)]
pub struct GroupingSet {
    /// Which kind of set.
    pub kind: GroupingSetKind,
    /// The elements. For `GROUPING SETS` these are themselves usually
    /// [`Expr::Group`](crate::expr::Expr::Group)s.
    pub groups: Vec<Expr>,
}

impl GroupingSet {
    /// A grouping element of `kind` over `groups`.
    pub fn new(kind: GroupingSetKind, groups: impl IntoExprList) -> Self {
        GroupingSet {
            kind,
            groups: groups.into_expr_list(),
        }
    }

    /// Whether there is nothing to group.
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }
}

impl Expression for GroupingSet {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        // Unlike bob, a keyword with no list is not written: `ROLLUP` alone is a
        // syntax error, and `GROUPING SETS (())` — the one legal empty form — is
        // written by giving it one empty `Expr::Group`.
        if self.groups.is_empty() {
            return;
        }
        w.push_str(self.kind.keyword());
        w.write_slice(&self.groups, " (", ", ", ")");
    }
}

/// Which set a [`GroupingSet`] expands to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GroupingSetKind {
    /// `GROUPING SETS (…)` — the sets are listed explicitly.
    #[default]
    GroupingSets,
    /// `CUBE (…)` — every subset.
    Cube,
    /// `ROLLUP (…)` — every prefix.
    Rollup,
}

impl GroupingSetKind {
    /// The keyword, as written.
    pub fn keyword(self) -> &'static str {
        match self {
            GroupingSetKind::GroupingSets => "GROUPING SETS",
            GroupingSetKind::Cube => "CUBE",
            GroupingSetKind::Rollup => "ROLLUP",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::expr::quote;
    use crate::writer::build;

    #[test]
    fn no_groups_means_no_clause_even_with_modifiers() {
        let g = GroupBy {
            distinct: true,
            with: Some(GroupByWith::Rollup),
            ..GroupBy::default()
        };
        assert_eq!(build(&Numbered, &g).unwrap().0, "");
        assert!(g.is_empty());
    }

    #[test]
    fn distinct_and_with_wrap_the_group_list() {
        let mut g = GroupBy::default();
        g.append_group(quote("status"));
        g.append_group("1");
        assert_eq!(build(&Numbered, &g).unwrap().0, r#"GROUP BY "status", 1"#);

        g.distinct = true;
        g.with = Some(GroupByWith::Cube);
        assert_eq!(
            build(&Numbered, &g).unwrap().0,
            r#"GROUP BY DISTINCT "status", 1 WITH CUBE"#
        );
    }

    #[test]
    fn a_grouping_set_is_one_grouping_element() {
        // PostgreSQL 17 grouping_element:
        //   ROLLUP ( { expression | ( expression [, ...] ) } [, ...] )
        let mut g = GroupBy::default();
        g.append_group(Expr::custom(GroupingSet::new(
            GroupingSetKind::Rollup,
            (quote("a"), quote("b")),
        )));
        assert_eq!(
            build(&Numbered, &g).unwrap().0,
            r#"GROUP BY ROLLUP ("a", "b")"#
        );
    }

    #[test]
    fn grouping_sets_hold_row_groups_including_the_empty_one() {
        // GROUPING SETS ( ( ) ) is the legal way to ask for the grand total. Note
        // that the empty set is `Expr::raw("()")` and *not* `Expr::group(())`,
        // which renders `(NULL)` — a one-column set over the constant NULL, which
        // is a different query. That trap is `Expr`'s, but this is where a caller
        // walks into it.
        let set = GroupingSet::new(
            GroupingSetKind::GroupingSets,
            (
                Expr::group(quote("a")),
                Expr::group((quote("a"), quote("b"))),
                Expr::raw("()"),
            ),
        );
        assert_eq!(
            build(&Numbered, &set).unwrap().0,
            r#"GROUPING SETS (("a"), ("a", "b"), ())"#
        );
    }

    #[test]
    fn an_empty_grouping_set_writes_nothing() {
        assert_eq!(build(&Numbered, &GroupingSet::default()).unwrap().0, "");
        assert!(GroupingSet::default().is_empty());
        assert_eq!(GroupingSetKind::Cube.keyword(), "CUBE");
    }
}
