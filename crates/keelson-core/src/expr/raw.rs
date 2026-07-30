use std::borrow::Cow;

use crate::error::{Error, Result};
use crate::value::{ToValue, Value};
use crate::writer::{DynExpr, Expression, SqlWriter, dyn_expr};

/// SQL written out verbatim, with no placeholder rewriting and no arguments.
///
/// This is the keyword-fragment building block: [`AND`](super::AND),
/// [`IS NULL`](super::IS_NULL) and friends are `Raw`. Use [`Clause`] when the
/// fragment carries `?` placeholders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Raw(pub Cow<'static, str>);

impl Raw {
    /// A raw fragment. `&'static str` borrows; `String` moves.
    pub fn new(sql: impl Into<Cow<'static, str>>) -> Self {
        Raw(sql.into())
    }

    /// The fragment as written.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Expression for Raw {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        w.push_str(&self.0);
        Ok(())
    }
}

/// One replacement for a `?` in a [`Clause`].
///
/// A `?` either binds a value or splices in a nested expression, which is how
/// `IN (?)` can take a whole argument list and expand to `IN ($3, $4, $5)`.
#[derive(Debug, Clone)]
pub enum RawArg {
    /// Bind a value: the `?` becomes the dialect's placeholder.
    Value(Value),
    /// Render an expression in place of the `?`, consuming as many placeholder
    /// positions as it binds arguments.
    Expr(DynExpr),
}

impl RawArg {
    /// A bound value.
    pub fn value(v: impl ToValue) -> Self {
        RawArg::Value(v.to_value())
    }

    /// A nested expression.
    pub fn expr(e: impl Expression + 'static) -> Self {
        RawArg::Expr(dyn_expr(e))
    }
}

/// Raw SQL whose `?` placeholders are rewritten to the dialect's own syntax,
/// with the supplied arguments interleaved.
///
/// `?` is the authoring syntax regardless of dialect, so the same string works
/// everywhere. Write `\?` for a literal question mark.
///
/// The number of `?` must match the number of arguments; a mismatch is
/// [`Error::RawArgCount`] rather than a silently wrong query. The SQL is still
/// written before the error surfaces, matching bob.
#[derive(Debug, Clone, Default)]
pub struct Clause {
    query: String,
    args: Vec<RawArg>,
}

impl Clause {
    /// A clause with no arguments yet.
    pub fn new(query: impl Into<String>) -> Self {
        Clause {
            query: query.into(),
            args: Vec::new(),
        }
    }

    /// Append the given replacements.
    pub fn with_args(mut self, args: impl IntoIterator<Item = RawArg>) -> Self {
        self.args.extend(args);
        self
    }

    /// Append bound values, for the common all-values case.
    pub fn with_values<V: ToValue>(mut self, vals: impl IntoIterator<Item = V>) -> Self {
        self.args
            .extend(vals.into_iter().map(|v| RawArg::Value(v.to_value())));
        self
    }

    /// Append one bound value.
    pub fn with_value(mut self, v: impl ToValue) -> Self {
        self.args.push(RawArg::value(v));
        self
    }

    /// Append one nested expression.
    pub fn with_expr(mut self, e: impl Expression + 'static) -> Self {
        self.args.push(RawArg::expr(e));
        self
    }

    /// The clause as written, still using `?`.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The replacements, in order.
    pub fn args(&self) -> &[RawArg] {
        &self.args
    }
}

/// A [`Clause`] from a query and its replacements.
pub fn clause(query: impl Into<String>, args: impl IntoIterator<Item = RawArg>) -> Clause {
    Clause::new(query).with_args(args)
}

impl Expression for Clause {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        let placeholders = self.write_rewritten(w)?;

        if placeholders != self.args.len() {
            return Err(Error::RawArgCount {
                placeholders,
                args: self.args.len(),
                clause: self.query.clone(),
            });
        }

        Ok(())
    }
}

impl Clause {
    /// Write the query with every unescaped `?` replaced, returning how many
    /// placeholders were found.
    ///
    /// Both `?` and `\` are ASCII, so byte offsets are always on char
    /// boundaries and the scan stays a byte scan.
    fn write_rewritten(&self, w: &mut SqlWriter<'_>) -> Result<usize> {
        let mut rest: &str = &self.query;
        let mut found = 0usize;

        loop {
            let Some(mark) = rest.find('?') else {
                w.push_str(rest);
                return Ok(found);
            };

            // An escape only applies to the `?` we are looking at; a `\?` later
            // in the string is somebody else's problem.
            if let Some(escape) = rest.find("\\?").filter(|escape| mark > *escape) {
                w.push_str(&rest[..escape]);
                w.push_str("?");
                rest = &rest[escape + 2..];
                continue;
            }

            w.push_str(&rest[..mark]);

            match self.args.get(found) {
                Some(RawArg::Expr(e)) => w.write_expr(e)?,
                Some(RawArg::Value(v)) => w.push_arg(v.clone()),
                // Too few arguments. Keep writing so the caller sees the whole
                // statement in the error, then fail on the count.
                None => w.push_arg(Value::Null),
            }

            found += 1;
            rest = &rest[mark + 1..];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{arg_group, args, quote};
    use super::*;
    use crate::dialect::testing::{Named, Numbered};
    use crate::error::Error;
    use crate::writer::build;

    /// The seven cases from bob's `expr/raw_test.go`, which the recorded golden
    /// fixtures pin as dialect-agnostic (`?1` placeholders, `"` quoting).
    #[test]
    fn plain_clause_has_no_placeholders() {
        let (sql, args) = build(&Named, &Clause::new("SELECT a, b FROM alphabet")).unwrap();
        assert_eq!(sql, "SELECT a, b FROM alphabet");
        assert!(args.is_empty());
    }

    #[test]
    fn escaped_question_marks_are_literal() {
        let c = Clause::new(r#"SELECT a, b FROM "alphabet\?" WHERE c = ? AND d <= ?"#)
            .with_values([1i32, 2]);
        let (sql, args) = build(&Named, &c).unwrap();
        assert_eq!(
            sql,
            r#"SELECT a, b FROM "alphabet?" WHERE c = ?1 AND d <= ?2"#
        );
        assert_eq!(args, vec![Value::I32(1), Value::I32(2)]);
    }

    #[test]
    fn a_count_mismatch_is_an_error_but_the_sql_is_still_written() {
        let c = Clause::new("SELECT a, b FROM alphabet WHERE c = ? AND d <= ?");
        let err = build(&Named, &c).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Bad Statement: has 2 placeholders but 0 args: \
             SELECT a, b FROM alphabet WHERE c = ? AND d <= ?"
        );
        assert!(matches!(
            err,
            Error::RawArgCount {
                placeholders: 2,
                args: 0,
                ..
            }
        ));
    }

    #[test]
    fn numbered_args_are_rewritten_in_order() {
        let c = Clause::new("SELECT a, b FROM alphabet WHERE c = ? AND d <= ?").with_values([1, 2]);
        let (sql, args) = build(&Named, &c).unwrap();
        assert_eq!(sql, "SELECT a, b FROM alphabet WHERE c = ?1 AND d <= ?2");
        assert_eq!(args, vec![Value::I32(1), Value::I32(2)]);
    }

    #[test]
    fn an_expression_arg_expands_to_several_placeholders() {
        let c = clause(
            "SELECT a, b FROM alphabet WHERE c IN (?) AND d <= ?",
            [RawArg::expr(args([5, 6, 7])), RawArg::value(2)],
        );
        let (sql, args) = build(&Named, &c).unwrap();
        assert_eq!(
            sql,
            "SELECT a, b FROM alphabet WHERE c IN (?1, ?2, ?3) AND d <= ?4"
        );
        assert_eq!(
            args,
            vec![Value::I32(5), Value::I32(6), Value::I32(7), Value::I32(2)]
        );
    }

    #[test]
    fn an_arg_group_brings_its_own_parentheses() {
        let c = clause(
            "SELECT a, b FROM alphabet WHERE c IN ? AND d <= ?",
            [RawArg::expr(arg_group([5, 6, 7])), RawArg::value(2)],
        );
        let (sql, args) = build(&Named, &c).unwrap();
        assert_eq!(
            sql,
            "SELECT a, b FROM alphabet WHERE c IN (?1, ?2, ?3) AND d <= ?4"
        );
        assert_eq!(args.len(), 4);
    }

    #[test]
    fn an_expression_arg_that_binds_nothing_shifts_the_rest_down() {
        let c = clause(
            "SELECT a, b FROM alphabet WHERE c = ? AND d <= ?",
            [RawArg::expr(quote("AA")), RawArg::value(2)],
        );
        let (sql, args) = build(&Named, &c).unwrap();
        assert_eq!(
            sql,
            r#"SELECT a, b FROM alphabet WHERE c = "AA" AND d <= ?1"#
        );
        assert_eq!(args, vec![Value::I32(2)]);
    }

    #[test]
    fn rewriting_uses_the_dialect_placeholder_syntax() {
        let c = Clause::new("a = ? AND b = ?").with_values([1, 2]);
        let (sql, _) = build(&Numbered, &c).unwrap();
        assert_eq!(sql, "a = $1 AND b = $2");
    }

    #[test]
    fn a_trailing_placeholder_terminates_the_scan() {
        let c = Clause::new("a = ?").with_value(1);
        let (sql, _) = build(&Numbered, &c).unwrap();
        assert_eq!(sql, "a = $1");
    }

    #[test]
    fn an_escape_after_a_real_placeholder_is_not_mistaken_for_one() {
        let c = Clause::new(r"a = ? AND b LIKE '\?'").with_value(1);
        let (sql, args) = build(&Numbered, &c).unwrap();
        assert_eq!(sql, "a = $1 AND b LIKE '?'");
        assert_eq!(args.len(), 1);
    }

    #[test]
    fn too_many_args_is_also_an_error() {
        let c = Clause::new("a = ?").with_values([1, 2]);
        assert!(matches!(
            build(&Numbered, &c),
            Err(Error::RawArgCount {
                placeholders: 1,
                args: 2,
                ..
            })
        ));
    }

    #[test]
    fn a_clause_spliced_into_a_query_continues_the_numbering() {
        let c = Clause::new("a = ? AND b = ?").with_values([1, 2]);
        let (sql, _) = crate::writer::build_from(&Numbered, 4, &c).unwrap();
        assert_eq!(sql, "a = $4 AND b = $5");
    }

    #[test]
    fn raw_is_verbatim() {
        let (sql, args) = build(&Numbered, &Raw::new("COUNT(*)")).unwrap();
        assert_eq!(sql, "COUNT(*)");
        assert!(args.is_empty());
    }
}
