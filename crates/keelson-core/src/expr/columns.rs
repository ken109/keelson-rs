use crate::error::Result;
use crate::writer::{Expression, SqlWriter};

/// A table's column list, quoted, qualified and aliased.
///
/// Renders as `"users"."id" AS "id", "users"."name" AS "name"`. The aliases are
/// what let a query that joins two tables be read back into two structs: the
/// prefix disambiguates columns whose names collide, so generated loading code
/// can find `pilots.id` under `pilot_id` without parsing the SQL.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ColumnsExpr {
    parent: Vec<String>,
    names: Vec<String>,
    agg_func: [String; 2],
    alias_prefix: String,
    alias_disabled: bool,
}

impl ColumnsExpr {
    /// A column set. Empty names are dropped.
    pub fn new<S: Into<String>>(names: impl IntoIterator<Item = S>) -> Self {
        ColumnsExpr {
            names: non_empty(names),
            ..Default::default()
        }
    }

    /// The column names, in order.
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// Wrap each column in a function call, e.g. `("MAX(", ")")`.
    pub fn with_agg_func(mut self, before: impl Into<String>, after: impl Into<String>) -> Self {
        self.agg_func = [before.into(), after.into()];
        self
    }

    /// Qualify the columns, e.g. `("public", "users")`.
    pub fn with_parent<S: Into<String>>(mut self, parent: impl IntoIterator<Item = S>) -> Self {
        self.parent = parent.into_iter().map(Into::into).collect();
        self
    }

    /// Prefix every alias, so `id` becomes `AS "pilot_id"`.
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.alias_prefix = prefix.into();
        self
    }

    /// Emit `AS "prefix_name"` after each column. On by default.
    pub fn enable_alias(mut self) -> Self {
        self.alias_disabled = false;
        self
    }

    /// Emit the columns bare, with no `AS`.
    pub fn disable_alias(mut self) -> Self {
        self.alias_disabled = true;
        self
    }

    /// Keep only these columns, preserving the set's own order.
    pub fn only<S: AsRef<str>>(mut self, cols: impl IntoIterator<Item = S>) -> Self {
        let keep: Vec<String> = cols.into_iter().map(|c| c.as_ref().to_owned()).collect();
        self.names.retain(|n| keep.iter().any(|k| k == n));
        self
    }

    /// Drop these columns.
    pub fn except<S: AsRef<str>>(mut self, cols: impl IntoIterator<Item = S>) -> Self {
        let drop: Vec<String> = cols.into_iter().map(|c| c.as_ref().to_owned()).collect();
        self.names.retain(|n| !drop.iter().any(|d| d == n));
        self
    }
}

fn non_empty<S: Into<String>>(items: impl IntoIterator<Item = S>) -> Vec<String> {
    items
        .into_iter()
        .map(Into::into)
        .filter(|s| !s.is_empty())
        .collect()
}

impl Expression for ColumnsExpr {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        if self.names.is_empty() {
            return Ok(());
        }

        let mut path: Vec<&str> = self.parent.iter().map(String::as_str).collect();
        path.push("");

        for (i, col) in self.names.iter().enumerate() {
            if i > 0 {
                w.push_str(", ");
            }

            w.push_str(&self.agg_func[0]);
            // The qualifier is fixed; only the last segment changes per column.
            let last = path.len() - 1;
            path[last] = col;
            w.push_quoted(&path);
            w.push_str(&self.agg_func[1]);

            if !self.alias_disabled {
                w.push_str(" AS ");
                let alias = format!("{}{col}", self.alias_prefix);
                w.push_quoted(&[&alias]);
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::writer::build;

    fn cols() -> ColumnsExpr {
        ColumnsExpr::new(["id", "name"])
    }

    #[test]
    fn columns_are_quoted_qualified_and_aliased() {
        let (sql, args) = build(&Numbered, &cols().with_parent(["users"])).unwrap();
        assert_eq!(sql, r#""users"."id" AS "id", "users"."name" AS "name""#);
        assert!(args.is_empty(), "a column list binds nothing");
    }

    #[test]
    fn without_a_parent_the_columns_are_bare() {
        let (sql, _) = build(&Numbered, &cols()).unwrap();
        assert_eq!(sql, r#""id" AS "id", "name" AS "name""#);
    }

    #[test]
    fn the_prefix_only_touches_the_alias() {
        let (sql, _) = build(
            &Numbered,
            &cols().with_parent(["users"]).with_prefix("user_"),
        )
        .unwrap();
        assert_eq!(
            sql,
            r#""users"."id" AS "user_id", "users"."name" AS "user_name""#
        );
    }

    #[test]
    fn aliases_can_be_switched_off_and_back_on() {
        let (sql, _) = build(&Numbered, &cols().disable_alias()).unwrap();
        assert_eq!(sql, r#""id", "name""#);

        let (sql, _) = build(&Numbered, &cols().disable_alias().enable_alias()).unwrap();
        assert_eq!(sql, r#""id" AS "id", "name" AS "name""#);
    }

    #[test]
    fn an_agg_func_wraps_each_column_individually() {
        let (sql, _) = build(
            &Numbered,
            &cols().with_agg_func("MAX(", ")").disable_alias(),
        )
        .unwrap();
        assert_eq!(sql, r#"MAX("id"), MAX("name")"#);
    }

    #[test]
    fn only_and_except_keep_the_original_order() {
        let c = ColumnsExpr::new(["a", "b", "c"]);
        assert_eq!(c.clone().only(["c", "a"]).names(), ["a", "c"]);
        assert_eq!(c.clone().except(["b"]).names(), ["a", "c"]);
        assert_eq!(c.except(["a", "b", "c"]).names().len(), 0);
    }

    #[test]
    fn empty_names_are_dropped_and_an_empty_set_writes_nothing() {
        assert_eq!(ColumnsExpr::new(["a", "", "b"]).names(), ["a", "b"]);
        let (sql, _) = build(&Numbered, &ColumnsExpr::new(Vec::<String>::new())).unwrap();
        assert_eq!(sql, "");
    }

    #[test]
    fn an_empty_parent_segment_is_skipped() {
        let (sql, _) = build(
            &Numbered,
            &ColumnsExpr::new(["id"])
                .with_parent(["", "users"])
                .disable_alias(),
        )
        .unwrap();
        assert_eq!(sql, r#""users"."id""#);
    }
}
