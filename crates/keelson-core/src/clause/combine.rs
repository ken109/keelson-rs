use crate::error::Error;
use crate::expr::{Expr, IntoExpr};
use crate::writer::{Expression, SqlWriter};

use super::fetch::Fetch;
use super::limit::Limit;
use super::offset::Offset;
use super::order_by::OrderBy;
use super::{MaybeAbsent, write_present};

/// Every set operation chained onto one statement, **and the `ORDER BY` / `LIMIT`
/// / `OFFSET` / `FETCH` that belong to the combination rather than to any one
/// operand**.
///
/// That second half is the part worth getting right. PostgreSQL 17,
/// <https://www.postgresql.org/docs/17/sql-select.html>:
///
/// > `select_statement UNION [ALL] select_statement`
/// >
/// > `ORDER BY` and `LIMIT` … can be attached to a subexpression if it is enclosed
/// > in parentheses. Without parentheses, these clauses will be taken to apply to
/// > the result of the `UNION`, not to its right-hand input expression.
///
/// So a statement with set operations has two sets of trailing clauses that render
/// in different places, and the difference is invisible in the SQL text except
/// through the parentheses:
///
/// ```text
/// (SELECT … LIMIT 1) UNION ALL (SELECT …) ORDER BY 1 LIMIT 5
///  ^ the leading query's own LIMIT           ^ the combination's
/// ```
///
/// The leading query's clauses stay where the query writes them, and are wrapped;
/// the combination's live here and are written after the last operand.
/// [`parenthesises_leading_query`](Self::parenthesises_leading_query) is the
/// condition for the wrapping.
///
/// No keyword of its own: every [`Combine`] starts with its operator.
#[derive(Debug, Clone, Default)]
pub struct Combines {
    /// The operations, applied left to right.
    pub queries: Vec<Combine>,
    /// `ORDER BY` over the result of the combination.
    pub order_by: OrderBy,
    /// `LIMIT` over the result of the combination.
    pub limit: Limit,
    /// `OFFSET` over the result of the combination.
    pub offset: Offset,
    /// `FETCH` over the result of the combination.
    pub fetch: Fetch,
}

impl Combines {
    /// Append one set operation.
    pub fn append_combine(&mut self, combine: Combine) {
        self.queries.push(combine);
    }

    /// Whether anything at all is combined.
    pub fn is_empty(&self) -> bool {
        self.queries.is_empty()
            && self.order_by.is_empty()
            && self.limit.is_empty()
            && self.offset.is_empty()
            && self.fetch.is_empty()
    }

    /// Whether the statement in front of these operations has to be parenthesised.
    ///
    /// It does exactly when it carries a trailing clause of its own —
    /// `ORDER BY`, `LIMIT`, `OFFSET`, `FETCH` or a locking clause — *and*
    /// something is combined onto it, because without the parentheses that clause
    /// would silently move to the whole combination. `leading_has_tail_clauses` is
    /// what only the query type can know.
    ///
    /// With nothing combined the parentheses would be legal but pointless, so they
    /// are left out; that is also why an all-default `Combines` changes nothing
    /// about how a statement renders.
    pub fn parenthesises_leading_query(&self, leading_has_tail_clauses: bool) -> bool {
        !self.queries.is_empty() && leading_has_tail_clauses
    }
}

impl Expression for Combines {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        // Each part supplies its own separator only when something precedes it, so
        // a combination that is nothing but a trailing `LIMIT` — which is legal,
        // and is how a set operation's own limit is set before its operands are —
        // does not come out with a leading space.
        let mut written = !self.queries.is_empty();
        write_present(w, &self.queries, "", " ", "");

        for (present, clause) in [
            (!self.order_by.is_empty(), &self.order_by as &dyn Expression),
            (!self.limit.is_empty(), &self.limit),
            (!self.offset.is_empty(), &self.offset),
            (!self.fetch.is_empty(), &self.fetch),
        ] {
            if !present {
                continue;
            }
            if written {
                w.push_str(" ");
            }
            w.write_expr(clause);
            written = true;
        }
    }
}

/// A statement other statements can be combined onto.
pub trait HasCombines {
    /// The set operations to modify.
    fn combines_mut(&mut self) -> &mut Combines;
}

impl HasCombines for Combines {
    fn combines_mut(&mut self) -> &mut Combines {
        self
    }
}

/// `UNION [ALL] (<query>)`
///
/// The operand is always parenthesised. It does not have to be — `a UNION b UNION
/// c` is legal and left-associative — but a parenthesised operand cannot be
/// re-associated by a later `ORDER BY`, and it is the only way an operand that has
/// its own `LIMIT` can be written at all.
#[derive(Debug, Clone, Default)]
pub struct Combine {
    /// Which set operation. `None` is how a default-constructed `Combine` stays
    /// absent.
    pub op: Option<SetOp>,
    /// The right-hand operand, rendered inside parentheses.
    pub query: Option<Expr>,
    /// `ALL`: keep duplicate rows instead of removing them.
    pub all: bool,
}

impl Combine {
    /// A set operation of `op` against `query`, without `ALL`.
    pub fn new(op: SetOp, query: impl IntoExpr) -> Self {
        Combine {
            op: Some(op),
            query: Some(query.into_expr()),
            all: false,
        }
    }

    /// Whether this operation is absent.
    pub fn is_empty(&self) -> bool {
        self.op.is_none() && self.query.is_none()
    }
}

impl Expression for Combine {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        if self.is_empty() {
            return;
        }

        // Half-filled is a caller error rather than an absent clause, and there is
        // no rendering that could be right, so it is recorded instead of guessed.
        let Some(op) = &self.op else {
            w.record_error(Error::Incomplete("the operator of a set operation"));
            return;
        };
        let Some(query) = &self.query else {
            w.record_error(Error::Incomplete("the query of a set operation"));
            return;
        };

        w.push_str(op.as_str());
        w.push_str(if self.all { " ALL (" } else { " (" });
        w.write_expr(query);
        w.push_str(")");
    }
}

/// Which set operation combines two queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetOp {
    /// `UNION` — rows of either.
    Union,
    /// `INTERSECT` — rows of both. Binds tighter than `UNION` and `EXCEPT`.
    Intersect,
    /// `EXCEPT` — rows of the left that are not in the right.
    Except,
}

impl SetOp {
    /// The keyword, as written.
    pub fn as_str(self) -> &'static str {
        match self {
            SetOp::Union => "UNION",
            SetOp::Intersect => "INTERSECT",
            SetOp::Except => "EXCEPT",
        }
    }
}

impl MaybeAbsent for Combine {
    fn is_absent(&self) -> bool {
        self.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::expr::arg;
    use crate::value::Value;
    use crate::writer::build;

    /// `SELECT $n`, standing in for a whole sub-query.
    fn sub(v: i32) -> Expr {
        Expr::join((Expr::raw("SELECT"), arg(v)))
    }

    #[test]
    fn an_empty_combines_writes_nothing() {
        assert_eq!(build(&Numbered, &Combines::default()).unwrap().0, "");
        assert_eq!(build(&Numbered, &Combine::default()).unwrap().0, "");
        assert!(Combines::default().is_empty());
        assert!(Combine::default().is_empty());
    }

    #[test]
    fn all_goes_between_the_operator_and_the_operand() {
        let mut c = Combine::new(SetOp::Union, sub(1));
        assert_eq!(build(&Numbered, &c).unwrap().0, "UNION (SELECT $1)");

        c.all = true;
        assert_eq!(build(&Numbered, &c).unwrap().0, "UNION ALL (SELECT $1)");
    }

    #[test]
    fn every_operator_has_its_spelling() {
        for (op, keyword) in [
            (SetOp::Union, "UNION"),
            (SetOp::Intersect, "INTERSECT"),
            (SetOp::Except, "EXCEPT"),
        ] {
            assert_eq!(
                build(&Numbered, &Combine::new(op, Expr::raw("SELECT 1")))
                    .unwrap()
                    .0,
                format!("{keyword} (SELECT 1)")
            );
        }
    }

    #[test]
    fn a_half_filled_combine_is_a_recorded_failure_not_a_broken_fragment() {
        let no_op = Combine {
            query: Some(sub(1)),
            ..Combine::default()
        };
        assert_eq!(
            build(&Numbered, &no_op).unwrap_err().to_string(),
            "query is missing the operator of a set operation"
        );

        let no_query = Combine {
            op: Some(SetOp::Except),
            ..Combine::default()
        };
        assert_eq!(
            build(&Numbered, &no_query).unwrap_err().to_string(),
            "query is missing the query of a set operation"
        );
    }

    #[test]
    fn chained_operations_keep_one_placeholder_run() {
        let mut cs = Combines::default();
        cs.append_combine(Combine::new(SetOp::Union, sub(1)));
        cs.append_combine(Combine::new(SetOp::Intersect, sub(2)));

        let (sql, args) = build(&Numbered, &cs).unwrap();
        assert_eq!(sql, "UNION (SELECT $1) INTERSECT (SELECT $2)");
        assert_eq!(args, vec![Value::I32(1), Value::I32(2)]);
    }

    #[test]
    fn the_combinations_own_tail_clauses_follow_the_last_operand() {
        // PostgreSQL 17: an unparenthesised trailing ORDER BY / LIMIT applies to
        // the result of the UNION, which is exactly what these fields are for.
        let mut cs = Combines::default();
        cs.append_combine(Combine::new(SetOp::Union, sub(1)));
        cs.order_by.append_order("1");
        cs.limit.set_limit(10i64);
        cs.offset.set_offset(5i64);

        assert_eq!(
            build(&Numbered, &cs).unwrap().0,
            "UNION (SELECT $1) ORDER BY 1 LIMIT 10 OFFSET 5"
        );
    }

    #[test]
    fn a_tail_clause_alone_is_still_a_non_empty_combines() {
        // It has to be: `SELECT … ORDER BY` written through the combination's
        // ORDER BY with nothing combined would otherwise be silently dropped.
        let mut cs = Combines::default();
        cs.fetch.set_fetch(2i64);
        assert!(!cs.is_empty());
        assert_eq!(build(&Numbered, &cs).unwrap().0, "FETCH NEXT 2 ROWS ONLY");
    }

    #[test]
    fn the_leading_query_is_wrapped_only_when_both_conditions_hold() {
        let mut cs = Combines::default();
        assert!(
            !cs.parenthesises_leading_query(true),
            "nothing combined: the parentheses would say nothing"
        );

        cs.append_combine(Combine::new(SetOp::Union, sub(1)));
        assert!(
            !cs.parenthesises_leading_query(false),
            "no tail clause on the leading query: nothing to protect"
        );
        assert!(cs.parenthesises_leading_query(true));
    }
}
