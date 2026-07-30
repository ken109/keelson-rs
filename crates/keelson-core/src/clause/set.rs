use crate::expr::{Expr, IntoExpr, IntoExprList};
use crate::writer::{Expression, SqlWriter};

/// The assignment list of an `UPDATE`, or of `DO UPDATE` / `ON DUPLICATE KEY
/// UPDATE`.
///
/// **The `SET` keyword is not written here**, and this is the one clause in the
/// module that does not write its own. The reason is grammatical: MySQL's
/// `INSERT … ON DUPLICATE KEY UPDATE col = val` takes exactly this list with no
/// `SET` in front of it, while `UPDATE t SET …` and PostgreSQL's
/// `DO UPDATE SET …` both need one. Whatever contains the list therefore supplies
/// the keyword.
///
/// An assignment is a whole expression rather than a column/value pair, because
/// PostgreSQL's multi-column form `(a, b) = (SELECT x, y FROM …)` is one
/// assignment with a row on each side, and a pair could not hold it.
#[derive(Debug, Clone, Default)]
pub struct Set {
    /// The assignments, in order.
    pub exprs: Vec<Expr>,
}

impl Set {
    /// Append one assignment.
    pub fn append_set(&mut self, expr: impl IntoExpr) {
        self.exprs.push(expr.into_expr());
    }

    /// Append several assignments.
    pub fn append_sets(&mut self, exprs: impl IntoExprList) {
        self.exprs.extend(exprs.into_expr_list());
    }

    /// Whether the clause is absent.
    pub fn is_empty(&self) -> bool {
        self.exprs.is_empty()
    }
}

impl Expression for Set {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.write_slice(&self.exprs, "", ", ", "");
    }
}

/// Anything with an assignment list: an `UPDATE`, or a
/// [`ConflictClause`](super::ConflictClause).
pub trait HasSet {
    /// The assignment list to modify.
    fn set_mut(&mut self) -> &mut Set;
}

impl HasSet for Set {
    fn set_mut(&mut self) -> &mut Set {
        self
    }
}

#[cfg(test)]
mod tests {
    use keelson_sqlcheck::testing::assert_frag_sql;

    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::expr::{Chain, arg, quote};
    use crate::value::Value;
    use crate::writer::build;

    /// The assignment list carries no `SET` keyword, so the frame supplies it.
    const FRAME: &str = "UPDATE users SET {} WHERE \"id\" = 1";

    #[test]
    fn an_empty_set_writes_nothing() {
        // Not framed: `UPDATE users SET` with nothing after it is not a statement
        // in any dialect, so there is no statement for an empty assignment list to
        // be judged inside. What it renders is all there is to check.
        assert_eq!(build(&Numbered, &Set::default()).unwrap().0, "");
        assert!(Set::default().is_empty());
    }

    #[test]
    fn assignments_are_comma_separated_and_carry_no_keyword() {
        let mut s = Set::default();
        s.append_set(quote("name").eq(arg("kubo")));
        s.append_set(quote("age").eq(arg(3i32)));

        let (sql, args) = build(&Numbered, &s).unwrap();
        // The parentheses come from the comparison chain, which is what makes an
        // assignment and a condition the same kind of thing. That also means this
        // one cannot be framed as an `UPDATE`: PostgreSQL's `set_clause` is
        // `column_name = expression`, and `("name" = $1)` is a parenthesised
        // *comparison*, so a dialect's `set` mod builds an unparenthesised
        // `"a" = $1` instead. Judged as the select list it *is* valid in.
        assert_frag_sql(
            r#"SELECT {} FROM users"#,
            &sql,
            r#"("name" = $1), ("age" = $2)"#,
        );
        assert_eq!(args, vec![Value::Text("kubo".into()), Value::I32(3)]);
    }

    #[test]
    fn a_multi_column_assignment_is_one_assignment() {
        // PostgreSQL 17 sql-update:
        //   ( column_name [, ...] ) = ( { expression } [, ...] )
        let mut s = Set::default();
        s.append_sets((
            Expr::binary(
                Expr::group((quote("name"), quote("email"))),
                "=",
                Expr::group((arg("kubo"), arg("kubo@example.com"))),
            ),
            Expr::raw(r#""age" = DEFAULT"#),
        ));
        let (sql, args) = build(&Numbered, &s).unwrap();
        assert_frag_sql(
            FRAME,
            &sql,
            r#"("name", "email") = ($1, $2), "age" = DEFAULT"#,
        );
        assert_eq!(args.len(), 2);
    }
}
