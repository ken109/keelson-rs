use crate::expr::{Expr, IntoExpr, IntoExprList};
use crate::writer::{Expression, SqlWriter};

/// `RETURNING a, b`
///
/// Whether this clause is present decides whether a mutation is run as a query or
/// as an exec, so [`has_returning`](Self::has_returning) is read by the execution
/// layer and not only by the writer. It is a synonym of `!is_empty()`, kept
/// because that is the question the caller is actually asking.
#[derive(Debug, Clone, Default)]
pub struct Returning {
    /// What to return, in order. `*` is an ordinary entry.
    pub expressions: Vec<Expr>,
}

impl Returning {
    /// Whether the statement returns rows.
    pub fn has_returning(&self) -> bool {
        !self.expressions.is_empty()
    }

    /// Append one expression.
    pub fn append_returning(&mut self, expr: impl IntoExpr) {
        self.expressions.push(expr.into_expr());
    }

    /// Append several expressions.
    pub fn append_returnings(&mut self, exprs: impl IntoExprList) {
        self.expressions.extend(exprs.into_expr_list());
    }

    /// Whether the clause is absent.
    pub fn is_empty(&self) -> bool {
        self.expressions.is_empty()
    }
}

impl Expression for Returning {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.write_slice(&self.expressions, "RETURNING ", ", ", "");
    }
}

/// A mutation that can return rows.
pub trait HasReturning {
    /// The `RETURNING` clause to modify.
    fn returning_mut(&mut self) -> &mut Returning;
}

impl HasReturning for Returning {
    fn returning_mut(&mut self) -> &mut Returning {
        self
    }
}

#[cfg(test)]
mod tests {
    use keelson_sqlcheck::testing::assert_frag_sql;

    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::expr::{Chain, quote};
    use crate::writer::build;

    /// `RETURNING` only exists on a data-modifying statement, so that is the frame.
    /// Its values are literals rather than placeholders so that the frame's
    /// numbering cannot be confused with the fragment's.
    const FRAME: &str = r#"INSERT INTO tags ("id", "name") VALUES (1, 'rust') {}"#;

    fn sql(r: &Returning) -> String {
        build(&Numbered, r).expect("render").0
    }

    #[test]
    fn an_empty_returning_writes_nothing_and_reports_no_rows() {
        let r = Returning::default();
        assert_frag_sql(FRAME, &sql(&r), "");
        assert!(!r.has_returning());
        assert!(r.is_empty());
    }

    #[test]
    fn expressions_are_comma_separated() {
        let mut r = Returning::default();
        r.append_returning(quote("id"));
        r.append_returnings((quote("name"), Expr::func("now", ()).as_("at")));
        assert_frag_sql(FRAME, &sql(&r), r#"RETURNING "id", "name", now() AS "at""#);
        assert!(r.has_returning());
    }

    #[test]
    fn a_star_is_an_ordinary_entry() {
        let mut r = Returning::default();
        r.append_returning("*");
        assert_frag_sql(FRAME, &sql(&r), "RETURNING *");
    }
}
