use keelson_core::{Dialect, SqlWriter};

/// The SQLite dialect: `?1` placeholders, `:name` named arguments, `"` quoting.
///
/// SQLite is the one dialect keelson targets that has *both* numbered positional
/// placeholders and named ones (<https://www.sqlite.org/lang_expr.html#varparam>),
/// so it is the only [`Dialect`] that overrides
/// [`write_named_arg`](Dialect::write_named_arg). `?NNN` is used rather than a bare
/// `?` because SQLite numbers an unadorned `?` implicitly and mixing the two forms
/// is how a statement quietly binds the wrong argument.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Sqlite;

impl Dialect for Sqlite {
    fn write_arg(&self, w: &mut SqlWriter<'_>, position: usize) {
        w.push_str("?");
        w.push_str(&position.to_string());
    }

    /// Quote `s` as a delimited identifier.
    ///
    /// An embedded `"` is doubled. SQLite accepts `[x]` and `` `x` `` as well, for
    /// compatibility with SQL Server and MySQL, but `"x"` is the standard form and
    /// the only one this dialect writes.
    fn write_quoted(&self, w: &mut SqlWriter<'_>, s: &str) {
        w.push_str("\"");
        if s.contains('"') {
            w.push_str(&s.replace('"', "\"\""));
        } else {
            w.push_str(s);
        }
        w.push_str("\"");
    }

    fn write_named_arg(&self, w: &mut SqlWriter<'_>, name: &str) {
        w.push_str(":");
        w.push_str(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use keelson_core::{Value, build, expr};

    #[test]
    fn placeholders_are_numbered_and_identifiers_are_double_quoted() {
        let e = expr::Expr::join((expr::quote(("users", "id")), expr::arg(1i32)));
        let (sql, args) = build(&Sqlite, &e).unwrap();
        assert_eq!(sql, r#""users"."id" ?1"#);
        assert_eq!(args, vec![Value::I32(1)]);
    }

    #[test]
    fn an_embedded_quote_is_doubled() {
        let (sql, _) = build(&Sqlite, &expr::quote(r#"we"ird"#)).unwrap();
        assert_eq!(sql, r#""we""ird""#);
    }

    /// The one dialect that has them. A named argument binds nothing and does not
    /// advance the positional counter.
    #[test]
    fn named_arguments_are_supported_and_consume_no_position() {
        let e = expr::Expr::join((expr::arg(1i32), expr::named("cutoff"), expr::arg(2i32)));
        let (sql, args) = build(&Sqlite, &e).unwrap();
        assert_eq!(sql, "?1 :cutoff ?2");
        assert_eq!(args, vec![Value::I32(1), Value::I32(2)]);
    }
}
