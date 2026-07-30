use keelson_core::{Dialect, SqlWriter};

/// The PostgreSQL dialect: `$1` placeholders, `"` quoting, no named arguments.
///
/// A unit struct rather than a value to be constructed, because there is nothing
/// to configure — every query type hands out `&Psql` from
/// [`Query::dialect`](keelson_core::Query::dialect).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Psql;

impl Dialect for Psql {
    fn write_arg(&self, w: &mut SqlWriter<'_>, position: usize) {
        w.push_str("$");
        w.push_str(&position.to_string());
    }

    /// Quote `s` as a delimited identifier.
    ///
    /// An embedded `"` is doubled, which is how PostgreSQL escapes one inside a
    /// delimited identifier (`4.1.1 Identifiers and Key Words`). bob writes the
    /// name through unedited, so a name containing a quote silently ends the
    /// identifier there; the fast path here is the same single `push_str` for the
    /// overwhelmingly common case, and the scan only pays for itself when a quote
    /// is actually present.
    fn write_quoted(&self, w: &mut SqlWriter<'_>, s: &str) {
        w.push_str("\"");
        if s.contains('"') {
            w.push_str(&s.replace('"', "\"\""));
        } else {
            w.push_str(s);
        }
        w.push_str("\"");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use keelson_core::{Value, build, expr};

    #[test]
    fn placeholders_are_dollar_numbered_and_identifiers_are_double_quoted() {
        let e = expr::Expr::join((expr::quote(("users", "id")), expr::arg(1i32)));
        let (sql, args) = build(&Psql, &e).unwrap();
        assert_eq!(sql, r#""users"."id" $1"#);
        assert_eq!(args, vec![Value::I32(1)]);
    }

    #[test]
    fn an_embedded_quote_is_doubled() {
        let (sql, _) = build(&Psql, &expr::quote(r#"we"ird"#)).unwrap();
        assert_eq!(sql, r#""we""ird""#);
    }

    #[test]
    fn named_arguments_are_refused() {
        assert!(matches!(
            build(&Psql, &expr::named("x")),
            Err(keelson_core::Error::NoNamedArgs)
        ));
    }
}
