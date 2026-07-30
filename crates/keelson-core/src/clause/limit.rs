use crate::expr::{Expr, IntoExpr};
use crate::writer::{Expression, SqlWriter};

/// `LIMIT n`
///
/// The count is an expression, not a number: SQLite accepts an arbitrary
/// expression there, and every dialect accepts a bound argument. `limit(10)` gives
/// the literal `LIMIT 10` because a number converts to
/// [`Expr::Raw`](crate::expr::Expr::Raw); `limit(arg(10))` binds it instead.
/// Narrowing that back down to a literal is a dialect mod's job.
#[derive(Debug, Clone, Default)]
pub struct Limit {
    /// How many rows.
    pub count: Option<Expr>,
}

impl Limit {
    /// Set the count.
    pub fn set_limit(&mut self, count: impl IntoExpr) {
        self.count = Some(count.into_expr());
    }

    /// Whether the clause is absent.
    pub fn is_empty(&self) -> bool {
        self.count.is_none()
    }
}

impl Expression for Limit {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.write_if_some(self.count.as_ref(), "LIMIT ", "");
    }
}

/// A statement with a `LIMIT`.
pub trait HasLimit {
    /// The `LIMIT` clause to modify.
    fn limit_mut(&mut self) -> &mut Limit;
}

impl HasLimit for Limit {
    fn limit_mut(&mut self) -> &mut Limit {
        self
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

    /// `LIMIT` is a fragment; this is the statement it trails.
    const FRAME: &str = r#"SELECT "id" FROM users {}"#;

    fn sql(l: &Limit) -> String {
        build(&Numbered, l).expect("render").0
    }

    #[test]
    fn an_unset_limit_writes_nothing() {
        assert_frag_sql(FRAME, &sql(&Limit::default()), "");
        assert!(Limit::default().is_empty());
    }

    #[test]
    fn a_count_is_a_literal_unless_it_is_bound() {
        let mut l = Limit::default();
        l.set_limit(10i64);
        let (rendered, args) = build(&Numbered, &l).unwrap();
        assert_frag_sql(FRAME, &rendered, "LIMIT 10");
        assert!(args.is_empty(), "a number is a literal, not an argument");

        l.set_limit(arg(10i64));
        let (rendered, args) = build(&Numbered, &l).unwrap();
        assert_frag_sql(FRAME, &rendered, "LIMIT $1");
        assert_eq!(args, vec![Value::I64(10)]);
    }

    #[test]
    fn a_count_may_be_any_expression() {
        // SQLite: "LIMIT expr" takes a full expression. PostgreSQL 17 sql-select
        // spells the same slot `LIMIT { count | ALL }` where count is an
        // a_expr, so a scalar sub-query is one there too.
        let mut l = Limit::default();
        l.set_limit("(SELECT count(*) FROM users)");
        assert_frag_sql(FRAME, &sql(&l), "LIMIT (SELECT count(*) FROM users)");
    }
}
