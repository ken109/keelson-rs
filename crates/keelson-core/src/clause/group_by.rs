use crate::error::Result;
use crate::writer::{DynExpr, Expression, SqlWriter};

/// `GROUP BY [DISTINCT] a, b [WITH ROLLUP]`
#[derive(Debug, Clone, Default)]
pub struct GroupBy {
    pub groups: Vec<DynExpr>,
    /// PostgreSQL `GROUP BY DISTINCT`.
    pub distinct: bool,
    /// MySQL `WITH ROLLUP` / `WITH CUBE`. Empty writes nothing.
    pub with: String,
}

impl GroupBy {
    pub fn set_groups(&mut self, groups: impl IntoIterator<Item = DynExpr>) {
        self.groups = groups.into_iter().collect();
    }

    pub fn append_group(&mut self, group: DynExpr) {
        self.groups.push(group);
    }

    pub fn set_group_with(&mut self, with: impl Into<String>) {
        self.with = with.into();
    }

    pub fn set_group_by_distinct(&mut self, distinct: bool) {
        self.distinct = distinct;
    }

    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }
}

impl Expression for GroupBy {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        // `GROUP BY DISTINCT` with nothing to group by is not valid SQL, so the
        // whole clause hinges on the group list rather than on any one modifier.
        if self.groups.is_empty() {
            return Ok(());
        }

        w.push_str("GROUP BY ");
        if self.distinct {
            w.push_str("DISTINCT ");
        }
        w.write_slice(&self.groups, "", ", ", "")?;

        if !self.with.is_empty() {
            w.push_str(" WITH ");
            w.push_str(&self.with);
        }

        Ok(())
    }
}

/// A query with a `GROUP BY` clause.
pub trait HasGroupBy {
    fn group_by_mut(&mut self) -> &mut GroupBy;
}

/// `GROUPING SETS (…)`, `CUBE (…)` or `ROLLUP (…)` — one grouping element,
/// meant to be put into [`GroupBy::groups`].
#[derive(Debug, Clone, Default)]
pub struct GroupingSet {
    pub groups: Vec<DynExpr>,
    /// `GROUPING SETS`, `CUBE` or `ROLLUP`.
    pub kind: String,
}

impl Expression for GroupingSet {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        w.push_str(&self.kind);
        w.write_slice(&self.groups, " (", ", ", ")")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::writer::{build, dyn_expr};

    #[test]
    fn no_groups_means_no_clause_even_with_modifiers() {
        let g = GroupBy {
            distinct: true,
            with: "ROLLUP".into(),
            ..GroupBy::default()
        };
        assert_eq!(build(&Numbered, &g).unwrap().0, "");
    }

    #[test]
    fn distinct_and_with_wrap_the_group_list() {
        let mut g = GroupBy::default();
        g.append_group(dyn_expr("a"));
        g.append_group(dyn_expr("b"));
        assert_eq!(build(&Numbered, &g).unwrap().0, "GROUP BY a, b");

        g.set_group_by_distinct(true);
        g.set_group_with("CUBE");
        assert_eq!(
            build(&Numbered, &g).unwrap().0,
            "GROUP BY DISTINCT a, b WITH CUBE"
        );
    }

    #[test]
    fn a_grouping_set_is_one_group_element() {
        let mut g = GroupBy::default();
        g.append_group(dyn_expr(GroupingSet {
            kind: "GROUPING SETS".into(),
            groups: vec![dyn_expr("a"), dyn_expr("b")],
        }));
        assert_eq!(
            build(&Numbered, &g).unwrap().0,
            "GROUP BY GROUPING SETS (a, b)"
        );
    }
}
