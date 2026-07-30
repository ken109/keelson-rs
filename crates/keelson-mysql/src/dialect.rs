use keelson_core::{Dialect, SqlWriter};

/// The MySQL dialect: `?` placeholders, backtick quoting, no named arguments.
///
/// A unit struct rather than a value to be constructed, because there is nothing
/// to configure — every query type hands out `&Mysql` from
/// [`Query::dialect`](keelson_core::Query::dialect).
///
/// The placeholder ignores its position, which is what makes MySQL the dialect
/// where a re-indexing bug is invisible in the SQL and visible only in the
/// argument list. Argument *order* is therefore the whole contract here, and the
/// tests assert it separately.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Mysql;

impl Dialect for Mysql {
    fn write_arg(&self, w: &mut SqlWriter<'_>, _position: usize) {
        w.push_str("?");
    }

    /// Quote `s` as a backtick-delimited identifier.
    ///
    /// An embedded backtick is doubled, which is how MySQL escapes one inside a
    /// quoted identifier (*9.2 Schema Object Names*). bob writes the name through
    /// unedited, so a name containing a backtick silently ends the identifier
    /// there; the fast path here is the same single `push_str` for the
    /// overwhelmingly common case, and the scan only pays for itself when a
    /// backtick is actually present.
    fn write_quoted(&self, w: &mut SqlWriter<'_>, s: &str) {
        w.push_str("`");
        if s.contains('`') {
            w.push_str(&s.replace('`', "``"));
        } else {
            w.push_str(s);
        }
        w.push_str("`");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use keelson_core::{Value, build, expr};

    #[test]
    fn placeholders_are_question_marks_and_identifiers_are_backquoted() {
        let e = expr::Expr::join((expr::quote(("users", "id")), expr::arg(1i32)));
        let (sql, args) = build(&Mysql, &e).unwrap();
        assert_eq!(sql, "`users`.`id` ?");
        assert_eq!(args, vec![Value::I32(1)]);
    }

    /// Positions are dropped, so several arguments are indistinguishable in the
    /// SQL and only the list says what was bound.
    #[test]
    fn every_placeholder_looks_the_same_and_the_order_carries_the_meaning() {
        let e = expr::Expr::join((expr::arg(1i32), expr::arg(2i32), expr::arg(3i32)));
        let (sql, args) = build(&Mysql, &e).unwrap();
        assert_eq!(sql, "? ? ?");
        assert_eq!(args, vec![Value::I32(1), Value::I32(2), Value::I32(3)]);
    }

    #[test]
    fn an_embedded_backtick_is_doubled() {
        let (sql, _) = build(&Mysql, &expr::quote("we`ird")).unwrap();
        assert_eq!(sql, "`we``ird`");
    }

    #[test]
    fn named_arguments_are_refused() {
        assert!(matches!(
            build(&Mysql, &expr::named("x")),
            Err(keelson_core::Error::NoNamedArgs)
        ));
    }
}
