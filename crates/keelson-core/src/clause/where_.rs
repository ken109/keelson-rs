use crate::expr::{Expr, IntoExpr};
use crate::writer::{Expression, SqlWriter};

/// `WHERE a AND b AND c`
///
/// Conditions accumulate and are always joined with `AND`. `OR` is built inside a
/// single condition — [`expr::or`](crate::expr::or) — because a clause that could
/// switch its own connective would make `append_where` mean something different
/// depending on call order.
#[derive(Debug, Clone, Default)]
pub struct Where {
    /// The conditions, in the order they were appended.
    pub conditions: Vec<Expr>,
}

impl Where {
    /// Append one condition.
    pub fn append_where(&mut self, condition: impl IntoExpr) {
        self.conditions.push(condition.into_expr());
    }

    /// Whether the clause is absent.
    pub fn is_empty(&self) -> bool {
        self.conditions.is_empty()
    }
}

impl Expression for Where {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.write_slice(&self.conditions, "WHERE ", " AND ", "");
    }
}

/// Anything with a `WHERE` clause.
///
/// Implemented by [`Where`] itself, and — because `ON CONFLICT` nests two of them
/// — by [`ConflictClause`](super::ConflictClause) and
/// [`ConflictTarget`](super::ConflictTarget), so that a `where_` mod written once
/// applies in all three places.
pub trait HasWhere {
    /// The `WHERE` clause to modify.
    fn where_mut(&mut self) -> &mut Where;
}

impl HasWhere for Where {
    fn where_mut(&mut self) -> &mut Where {
        self
    }
}

#[cfg(test)]
mod tests {
    use keelson_sqlcheck::testing::assert_frag_sql;

    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::expr::{Chain, arg, or, quote};
    use crate::value::Value;
    use crate::writer::build;

    /// A `WHERE` clause is not a statement, so every case below is judged inside
    /// the statement it belongs to. The names are `tests/schema/psql.sql`'s,
    /// because a real engine resolves them.
    const FRAME: &str = r#"SELECT "id" FROM users {}"#;

    fn sql(e: &impl Expression) -> String {
        build(&Numbered, e).expect("render").0
    }

    #[test]
    fn an_empty_where_writes_nothing_not_even_the_keyword() {
        // PostgreSQL 17 sql-select: `[ WHERE condition ]`. The keyword is inside
        // the brackets, so an absent clause takes it along.
        let (sql, args) = build(&Numbered, &Where::default()).unwrap();
        assert_frag_sql(FRAME, &sql, "");
        assert!(args.is_empty());
        assert!(Where::default().is_empty());
    }

    #[test]
    fn conditions_are_and_joined_and_numbered_in_order() {
        let mut wh = Where::default();
        wh.append_where(quote("age").eq(arg(30i32)));
        wh.append_where(quote("name").eq(arg("kubo")));
        // Progressive enhancement: a hand-written fragment is a condition too.
        wh.append_where("email IS NULL");

        let (sql, args) = build(&Numbered, &wh).unwrap();
        assert_frag_sql(
            FRAME,
            &sql,
            r#"WHERE ("age" = $1) AND ("name" = $2) AND email IS NULL"#,
        );
        assert_eq!(args, vec![Value::I32(30), Value::Text("kubo".into())]);
    }

    #[test]
    fn a_disjunction_is_one_condition() {
        let mut wh = Where::default();
        wh.append_where(or((
            quote("age").eq(arg(30i32)),
            quote("name").eq(arg("kubo")),
        )));
        assert_frag_sql(FRAME, &sql(&wh), r#"WHERE (("age" = $1) OR ("name" = $2))"#);
    }
}
