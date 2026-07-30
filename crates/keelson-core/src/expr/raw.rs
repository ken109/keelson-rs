use std::borrow::Cow;

use crate::error::Error;
use crate::value::{ToValue, Value};
use crate::writer::SqlWriter;

use super::convert::IntoExpr;
use super::node::Expr;

/// One replacement for a `?` in an [`Expr::Template`].
///
/// A `?` either binds a value or splices in a whole expression. The second form
/// is what lets `WHERE id IN (?)` expand to `IN ($3, $4, $5)`: the nested
/// expression consumes as many placeholder positions as it binds arguments, and
/// the counter keeps going from there.
#[derive(Debug, Clone)]
pub enum RawArg {
    /// Bind a value; the `?` becomes the dialect's positional placeholder.
    Value(Value),
    /// Render an expression in place of the `?`.
    Expr(Expr),
    /// Write a named placeholder. Binds nothing, consumes no position, and
    /// records [`Error::NoNamedArgs`] on a dialect that has no named-argument
    /// syntax.
    Named(Cow<'static, str>),
}

impl RawArg {
    /// A bound value.
    pub fn value(v: impl ToValue) -> RawArg {
        RawArg::Value(v.to_value())
    }

    /// A spliced expression.
    pub fn expr(e: impl IntoExpr) -> RawArg {
        RawArg::Expr(e.into_expr())
    }

    /// A named placeholder.
    pub fn named(name: impl Into<Cow<'static, str>>) -> RawArg {
        RawArg::Named(name.into())
    }
}

/// Write `sql` with every unescaped `?` replaced by the corresponding `args`
/// entry, rewritten into the dialect's own placeholder syntax.
///
/// `\?` is a literal question mark. The rewrite is a single left-to-right byte
/// scan — both `?` and `\` are ASCII, so byte offsets are always on character
/// boundaries.
///
/// A count mismatch is recorded rather than short-circuited: the whole statement
/// is still written first so that the error names the clause the author actually
/// typed, and a missing argument binds `NULL` to keep the placeholder positions
/// after it correct. That is bob's behaviour too, and it is why the recorded
/// fixture for the mismatched case has full SQL *and* an error.
pub(super) fn write_template(w: &mut SqlWriter<'_>, sql: &str, args: &[RawArg]) {
    let mut rest = sql;
    let mut placeholders = 0usize;

    loop {
        let Some(mark) = rest.find('?') else {
            w.push_str(rest);
            break;
        };

        // An escape only matters for the `?` we are looking at. A `\?` further
        // along belongs to a later iteration, and a `\?` we have already passed
        // is no longer in `rest`.
        if let Some(escape) = rest.find("\\?").filter(|escape| mark > *escape) {
            w.push_str(&rest[..escape]);
            w.push_str("?");
            rest = &rest[escape + 2..];
            continue;
        }

        w.push_str(&rest[..mark]);

        match args.get(placeholders) {
            Some(RawArg::Value(v)) => w.push_arg(v.clone()),
            Some(RawArg::Expr(e)) => w.write_expr(e),
            Some(RawArg::Named(name)) => w.push_named_arg(name),
            None => w.push_arg(Value::Null),
        }

        placeholders += 1;
        rest = &rest[mark + 1..];
    }

    if placeholders != args.len() {
        w.record_error(Error::raw_arg_count(placeholders, args.len(), sql));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::{Numbered, TestDialect};
    use crate::writer::build;

    /// bob's `expr/raw_test.go`, which is also the source of the seven
    /// dialect-agnostic golden cases. Its dialect writes `?N`, `:name` and
    /// `"quoted"`, which is [`TestDialect`].
    mod bob_raw_test {
        use super::*;

        #[test]
        fn plain() {
            let (sql, args) =
                build(&TestDialect, &Expr::template("SELECT a, b FROM alphabet", [])).unwrap();
            assert_eq!(sql, "SELECT a, b FROM alphabet");
            assert!(args.is_empty());
        }

        #[test]
        fn escaped_args() {
            let e = Expr::template(
                r#"SELECT a, b FROM "alphabet\?" WHERE c = ? AND d <= ?"#,
                [RawArg::value(1i32), RawArg::value(2i32)],
            );
            let (sql, args) = build(&TestDialect, &e).unwrap();
            assert_eq!(
                sql,
                r#"SELECT a, b FROM "alphabet?" WHERE c = ?1 AND d <= ?2"#
            );
            assert_eq!(args, vec![Value::I32(1), Value::I32(2)]);
        }

        #[test]
        fn mismatched_args_and_placeholders() {
            let e = Expr::template("SELECT a, b FROM alphabet WHERE c = ? AND d <= ?", []);
            // The SQL is written in full before the count is checked, so the
            // error can quote the clause.
            let mut w = SqlWriter::new(&TestDialect);
            w.write_expr(&e);
            assert_eq!(
                w.sql(),
                "SELECT a, b FROM alphabet WHERE c = ?1 AND d <= ?2"
            );
            let err = w.finish().unwrap_err();
            assert_eq!(
                err.to_string(),
                "Bad Statement: has 2 placeholders but 0 args: \
                 SELECT a, b FROM alphabet WHERE c = ? AND d <= ?"
            );
        }

        #[test]
        fn numbered_args() {
            let e = Expr::template(
                "SELECT a, b FROM alphabet WHERE c = ? AND d <= ?",
                [RawArg::value(1i32), RawArg::value(2i32)],
            );
            let (sql, args) = build(&TestDialect, &e).unwrap();
            assert_eq!(sql, "SELECT a, b FROM alphabet WHERE c = ?1 AND d <= ?2");
            assert_eq!(args, vec![Value::I32(1), Value::I32(2)]);
        }

        #[test]
        fn expr_args() {
            // One `?` inside parentheses the author wrote, filled by an argument
            // list that expands to three placeholders.
            let e = Expr::template(
                "SELECT a, b FROM alphabet WHERE c IN (?) AND d <= ?",
                [
                    RawArg::expr(Expr::args([5i32, 6, 7])),
                    RawArg::value(2i32),
                ],
            );
            let (sql, args) = build(&TestDialect, &e).unwrap();
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
        fn expr_args_group() {
            // Same, with the parentheses coming from the expression instead.
            let e = Expr::template(
                "SELECT a, b FROM alphabet WHERE c IN ? AND d <= ?",
                [
                    RawArg::expr(crate::expr::arg_group([5i32, 6, 7])),
                    RawArg::value(2i32),
                ],
            );
            let (sql, args) = build(&TestDialect, &e).unwrap();
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
        fn expr_args_quote() {
            // An identifier binds nothing, so the `?` that follows it is still
            // the first placeholder.
            let e = Expr::template(
                "SELECT a, b FROM alphabet WHERE c = ? AND d <= ?",
                [
                    RawArg::expr(Expr::ident("AA")),
                    RawArg::value(2i32),
                ],
            );
            let (sql, args) = build(&TestDialect, &e).unwrap();
            assert_eq!(
                sql,
                r#"SELECT a, b FROM alphabet WHERE c = "AA" AND d <= ?1"#
            );
            assert_eq!(args, vec![Value::I32(2)]);
        }
    }

    #[test]
    fn a_lone_escape_before_a_real_placeholder() {
        let e = Expr::template(r"a\?b?c", [RawArg::value(1i32)]);
        let (sql, _) = build(&Numbered, &e).unwrap();
        assert_eq!(sql, "a?b$1c");
    }

    #[test]
    fn an_escape_after_a_real_placeholder_is_still_honoured() {
        let e = Expr::template(r"a?b\?c", [RawArg::value(1i32)]);
        let (sql, _) = build(&Numbered, &e).unwrap();
        assert_eq!(sql, "a$1b?c");
    }

    #[test]
    fn consecutive_escapes_stay_literal() {
        let e = Expr::template(r"\?\?", []);
        let (sql, args) = build(&Numbered, &e).unwrap();
        assert_eq!(sql, "??");
        assert!(args.is_empty());
    }

    #[test]
    fn a_trailing_placeholder_ends_the_scan() {
        let e = Expr::template("a = ?", [RawArg::value(9i32)]);
        assert_eq!(build(&Numbered, &e).unwrap().0, "a = $1");
    }

    #[test]
    fn too_many_args_is_also_a_mismatch() {
        let e = Expr::template("a = ?", [RawArg::value(1i32), RawArg::value(2i32)]);
        let err = build(&Numbered, &e).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Bad Statement: has 1 placeholders but 2 args: a = ?"
        );
    }

    #[test]
    fn a_named_replacement_consumes_a_placeholder_but_no_position() {
        let e = Expr::template(
            "a = ? AND b = ? AND c = ?",
            [
                RawArg::value(1i32),
                RawArg::named("b"),
                RawArg::value(3i32),
            ],
        );
        let (sql, args) = build(&TestDialect, &e).unwrap();
        assert_eq!(sql, "a = ?1 AND b = :b AND c = ?2");
        assert_eq!(args, vec![Value::I32(1), Value::I32(3)]);
    }

    #[test]
    fn a_named_replacement_fails_on_a_dialect_without_named_arguments() {
        let e = Expr::template("a = ?", [RawArg::named("a")]);
        assert!(matches!(
            build(&Numbered, &e),
            Err(Error::NoNamedArgs)
        ));
    }

    #[test]
    fn multibyte_text_around_a_placeholder_is_not_sliced_mid_character() {
        let e = Expr::template("名前 = ? AND 年齢 > ?", [
            RawArg::value("さくら"),
            RawArg::value(20i32),
        ]);
        let (sql, _) = build(&Numbered, &e).unwrap();
        assert_eq!(sql, "名前 = $1 AND 年齢 > $2");
    }

    #[test]
    fn a_template_continues_the_surrounding_numbering() {
        let outer = Expr::join([
            Expr::arg(1i32),
            Expr::template("f(?, ?)", [RawArg::value(2i32), RawArg::value(3i32)]),
            Expr::arg(4i32),
        ]);
        let (sql, args) = build(&Numbered, &outer).unwrap();
        assert_eq!(sql, "$1 f($2, $3) $4");
        assert_eq!(args.len(), 4);
    }
}
