use crate::error::Result;
use crate::writer::{DynExpr, Expression, SqlWriter};

use super::join::Join;

/// A `from_item`: a table, a sub-select, a function call or a CTE name, plus
/// every decoration the three dialects hang off one.
///
/// The dialect-specific fields are all here rather than in three near-identical
/// copies, because a shared [`Join`] has to be able to hold one. Which of them a
/// dialect *permits* is decided by which mods that dialect exports, not by the
/// shape of this struct.
///
/// ```text
/// [ONLY] [LATERAL] expr [WITH ORDINALITY] [PARTITION (…)] [AS alias(cols)]
///        [index hints] [INDEXED BY …] [joins]
/// ```
#[derive(Debug, Clone, Default)]
pub struct TableRef {
    pub expression: Option<DynExpr>,

    pub alias: String,
    /// Column aliases. Unlike a CTE's, these are quoted.
    pub columns: Vec<String>,

    /// PostgreSQL `ONLY`.
    pub only: bool,
    /// PostgreSQL and MySQL `LATERAL`.
    pub lateral: bool,
    /// PostgreSQL `WITH ORDINALITY`.
    pub with_ordinality: bool,
    /// SQLite. `Some("")` means `NOT INDEXED`; `None` writes nothing.
    pub indexed_by: Option<String>,
    /// MySQL `PARTITION (…)`.
    pub partitions: Vec<String>,
    /// MySQL `USE`/`FORCE`/`IGNORE INDEX`.
    pub index_hints: Vec<IndexHint>,

    pub joins: Vec<Join>,
}

impl TableRef {
    pub fn new(table: DynExpr) -> Self {
        TableRef {
            expression: Some(table),
            ..TableRef::default()
        }
    }

    pub fn set_table(&mut self, table: DynExpr) {
        self.expression = Some(table);
    }

    pub fn set_table_alias(
        &mut self,
        alias: impl Into<String>,
        columns: impl IntoIterator<Item = String>,
    ) {
        self.alias = alias.into();
        self.columns = columns.into_iter().collect();
    }

    pub fn append_join(&mut self, join: Join) {
        self.joins.push(join);
    }

    pub fn append_partition(&mut self, partitions: impl IntoIterator<Item = String>) {
        self.partitions.extend(partitions);
    }

    pub fn append_index_hint(&mut self, hint: IndexHint) {
        self.index_hints.push(hint);
    }

    /// Whether there is a table at all — what a query tests before writing
    /// `FROM`.
    pub fn is_empty(&self) -> bool {
        self.expression.is_none()
    }
}

impl Expression for TableRef {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        if self.only {
            w.push_str("ONLY ");
        }
        if self.lateral {
            w.push_str("LATERAL ");
        }

        if let Some(e) = &self.expression {
            w.write_expr(e)?;
        }

        if self.with_ordinality {
            w.push_str(" WITH ORDINALITY");
        }

        w.write_slice(&self.partitions, " PARTITION (", ", ", ")")?;

        if !self.alias.is_empty() {
            w.push_str(" AS ");
            w.push_quoted(&[&self.alias]);
        }

        if !self.columns.is_empty() {
            w.push_str("(");
            for (i, c) in self.columns.iter().enumerate() {
                if i > 0 {
                    w.push_str(", ");
                }
                w.push_quoted(&[c]);
            }
            w.push_str(")");
        }

        w.write_slice(&self.index_hints, "\n", " ", "")?;

        match self.indexed_by.as_deref() {
            None => {}
            Some("") => w.push_str(" NOT INDEXED"),
            // `{:?}` on a `str` is Rust's nearest equivalent of Go's `%q`,
            // which is what bob uses here — a double-quoted, escaped literal
            // rather than a dialect-quoted identifier.
            Some(index) => w.push_str(&format!(" INDEXED BY {index:?}")),
        }

        w.write_slice(&self.joins, "\n", "\n", "")
    }
}

/// A query with a table reference — the `FROM` of a select, the `USING` of a
/// delete, the target of an update.
pub trait HasTableRef {
    fn table_ref_mut(&mut self) -> &mut TableRef;
}

impl HasTableRef for TableRef {
    fn table_ref_mut(&mut self) -> &mut TableRef {
        self
    }
}

/// MySQL's `USE | FORCE | IGNORE INDEX [FOR …] (indexes)`.
///
/// Never contributes arguments: index names are identifiers.
#[derive(Debug, Clone, Default)]
pub struct IndexHint {
    /// `USE`, `FORCE` or `IGNORE`. Empty means the hint is not written at all.
    pub kind: String,
    pub indexes: Vec<String>,
    /// `JOIN`, `ORDER BY` or `GROUP BY`.
    pub for_: String,
}

impl Expression for IndexHint {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        if self.kind.is_empty() {
            return Ok(());
        }
        w.push_str(&self.kind);
        w.push_str(" INDEX ");
        w.write_if(!self.for_.is_empty(), " FOR ", &self.for_, "")?;
        // The brackets are unconditional: `USE INDEX ()` is how MySQL is told to
        // ignore an index for a scope.
        w.push_str(" (");
        w.write_slice(&self.indexes, "", ", ", "")?;
        w.push_str(")");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clause::join::{CROSS_JOIN, INNER_JOIN};
    use crate::dialect::testing::{Named, Numbered};
    use crate::writer::{build, dyn_expr};

    fn users() -> TableRef {
        TableRef::new(dyn_expr("users"))
    }

    #[test]
    fn a_bare_table_is_just_its_expression() {
        let (sql, _) = build(&Numbered, &users()).unwrap();
        assert_eq!(sql, "users");
    }

    #[test]
    fn an_empty_table_ref_writes_nothing() {
        let (sql, _) = build(&Numbered, &TableRef::default()).unwrap();
        assert_eq!(sql, "");
        assert!(TableRef::default().is_empty());
    }

    #[test]
    fn the_alias_is_quoted_and_so_are_its_columns() {
        let mut t = users();
        t.set_table_alias("u", ["a".to_string(), "b".to_string()]);
        let (sql, _) = build(&Numbered, &t).unwrap();
        assert_eq!(sql, r#"users AS "u"("a", "b")"#);
    }

    #[test]
    fn column_aliases_without_an_alias_still_render() {
        let mut t = users();
        t.columns = vec!["p".into()];
        assert_eq!(build(&Numbered, &t).unwrap().0, r#"users("p")"#);
    }

    #[test]
    fn postgres_decorations_bracket_the_expression() {
        let mut t = users();
        t.only = true;
        t.lateral = true;
        t.with_ordinality = true;
        assert_eq!(
            build(&Numbered, &t).unwrap().0,
            "ONLY LATERAL users WITH ORDINALITY"
        );
    }

    #[test]
    fn sqlite_indexed_by_distinguishes_none_empty_and_named() {
        let mut t = users();
        assert_eq!(build(&Named, &t).unwrap().0, "users");

        t.indexed_by = Some(String::new());
        assert_eq!(build(&Named, &t).unwrap().0, "users NOT INDEXED");

        t.indexed_by = Some("users_pkey".into());
        assert_eq!(
            build(&Named, &t).unwrap().0,
            r#"users INDEXED BY "users_pkey""#
        );
    }

    #[test]
    fn mysql_partitions_come_before_the_alias() {
        let mut t = users();
        t.append_partition(["p0".to_string(), "p1".to_string()]);
        t.set_table_alias("u", []);
        assert_eq!(
            build(&Numbered, &t).unwrap().0,
            r#"users PARTITION (p0, p1) AS "u""#
        );
    }

    #[test]
    fn index_hints_are_newline_prefixed_and_space_separated() {
        let mut t = users();
        t.append_index_hint(IndexHint {
            kind: "USE".into(),
            indexes: vec!["a".into()],
            for_: String::new(),
        });
        t.append_index_hint(IndexHint {
            kind: "IGNORE".into(),
            indexes: vec!["b".into(), "c".into()],
            for_: "JOIN".into(),
        });
        let (sql, args) = build(&Numbered, &t).unwrap();
        assert_eq!(sql, "users\nUSE INDEX  (a) IGNORE INDEX  FOR JOIN (b, c)");
        assert!(
            args.is_empty(),
            "index names are identifiers, not arguments"
        );
    }

    #[test]
    fn a_hint_without_a_kind_is_skipped_entirely() {
        let mut t = users();
        t.append_index_hint(IndexHint::default());
        // The slice is non-empty, so the "\n" prefix is still written; the hint
        // itself contributes nothing. This matches bob.
        assert_eq!(build(&Numbered, &t).unwrap().0, "users\n");
    }

    #[test]
    fn joins_are_newline_separated_and_come_last() {
        let mut t = users();
        t.set_table_alias("u", []);
        t.append_join(Join {
            kind: INNER_JOIN.into(),
            to: TableRef::new(dyn_expr("pilots")),
            on: vec![dyn_expr("users.id = pilots.user_id")],
            ..Join::default()
        });
        t.append_join(Join {
            kind: CROSS_JOIN.into(),
            to: TableRef::new(dyn_expr("jets")),
            ..Join::default()
        });

        let (sql, _) = build(&Numbered, &t).unwrap();
        assert_eq!(
            sql,
            "users AS \"u\"\nINNER JOIN pilots ON users.id = pilots.user_id\nCROSS JOIN jets"
        );
    }
}
