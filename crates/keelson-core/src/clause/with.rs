use crate::writer::{Expression, SqlWriter};

use super::cte::Cte;
use super::write_present;

/// `WITH [RECURSIVE] <cte>, <cte>, …`
///
/// From PostgreSQL 17: `WITH [ RECURSIVE ] with_query [, ...]`. `RECURSIVE` is a
/// property of the whole list rather than of one entry — it makes every name in the
/// list visible to every entry, which is also what lets two CTEs refer to each
/// other.
#[derive(Debug, Clone, Default)]
pub struct With {
    /// Whether the list is `WITH RECURSIVE`.
    pub recursive: bool,
    /// The common table expressions, in order.
    pub ctes: Vec<Cte>,
}

impl With {
    /// Append one CTE.
    pub fn append_cte(&mut self, cte: Cte) {
        self.ctes.push(cte);
    }

    /// Make the list recursive.
    pub fn set_recursive(&mut self, recursive: bool) {
        self.recursive = recursive;
    }

    /// Whether the clause is absent.
    pub fn is_empty(&self) -> bool {
        self.ctes.is_empty()
    }
}

impl Expression for With {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        // `WITH RECURSIVE` with no CTEs is not a clause, so the list gates the
        // keyword — including the RECURSIVE that a mod may have set first.
        let prefix = if self.recursive {
            "WITH RECURSIVE "
        } else {
            "WITH "
        };
        write_present(w, &self.ctes, prefix, ", ", "");
    }
}

/// A statement that accepts common table expressions.
pub trait HasWith {
    /// The `WITH` clause to modify.
    fn with_mut(&mut self) -> &mut With;
}

impl HasWith for With {
    fn with_mut(&mut self) -> &mut With {
        self
    }
}

#[cfg(test)]
mod tests {
    use keelson_sqlcheck::testing::assert_frag_sql;

    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::expr::{Expr, arg};
    use crate::value::Value;
    use crate::writer::build;

    /// A `WITH` prefixes a statement, so the frame follows it rather than
    /// surrounding it. Two frames: one whose body reads the CTEs, and one for the
    /// cases where the clause writes nothing and there is no CTE to read.
    const FRAME: &str = r#"{} SELECT * FROM "a""#;
    const EMPTY_FRAME: &str = r#"{} SELECT "id" FROM users"#;

    /// A real sub-query: `SELECT "id" FROM posts WHERE "id" = $n`. The placeholder
    /// is compared against a column so PostgreSQL can give it a type.
    fn sub(v: i32) -> Expr {
        Expr::join((Expr::raw(r#"SELECT "id" FROM posts WHERE "id" ="#), arg(v)))
    }

    fn sub_sql(n: usize) -> String {
        format!(r#"SELECT "id" FROM posts WHERE "id" = ${n}"#)
    }

    fn sql(w: &With) -> String {
        build(&Numbered, w).expect("render").0
    }

    #[test]
    fn an_empty_with_writes_nothing_not_even_the_keyword() {
        assert_frag_sql(EMPTY_FRAME, &sql(&With::default()), "");
        assert!(With::default().is_empty());
    }

    #[test]
    fn recursive_alone_is_still_an_absent_clause() {
        let mut with = With::default();
        with.set_recursive(true);
        assert_frag_sql(EMPTY_FRAME, &sql(&with), "");
    }

    #[test]
    fn ctes_are_comma_separated_and_share_one_placeholder_run() {
        let mut with = With::default();
        with.append_cte(Cte::new("a", sub(1)));
        with.append_cte(Cte::new("b", sub(2)));

        let (rendered, args) = build(&Numbered, &with).unwrap();
        assert_frag_sql(
            FRAME,
            &rendered,
            &format!(r#"WITH "a" AS ({}), "b" AS ({})"#, sub_sql(1), sub_sql(2)),
        );
        assert_eq!(args, vec![Value::I32(1), Value::I32(2)]);

        // RECURSIVE is a property of the whole list, not of one CTE, and a list
        // that happens not to recurse is still allowed to carry it.
        with.set_recursive(true);
        assert_frag_sql(
            FRAME,
            &sql(&with),
            &format!(
                r#"WITH RECURSIVE "a" AS ({}), "b" AS ({})"#,
                sub_sql(1),
                sub_sql(2)
            ),
        );
    }

    #[test]
    fn an_absent_cte_takes_its_comma_with_it() {
        // `WITH "a" AS (…), ` does not parse, so a separator is written only
        // between CTEs that are both actually there.
        let mut with = With::default();
        with.append_cte(Cte::default());
        assert_frag_sql(EMPTY_FRAME, &sql(&with), "");

        with.append_cte(Cte::new("a", sub(1)));
        with.append_cte(Cte::default());
        assert_frag_sql(
            FRAME,
            &sql(&with),
            &format!(r#"WITH "a" AS ({})"#, sub_sql(1)),
        );
    }
}
