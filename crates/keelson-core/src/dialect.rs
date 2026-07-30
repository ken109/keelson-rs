use crate::error::{Error, Result};

/// The per-database syntax an expression needs in order to render itself.
///
/// The three dialects keelson ports differ only in these three decisions:
///
/// | dialect | placeholder | quote | named |
/// |---|---|---|---|
/// | PostgreSQL | `$1` | `"id"` | unsupported |
/// | MySQL | `?` (position ignored) | `` `id` `` | unsupported |
/// | SQLite | `?1` | `"id"` | `:name` |
///
/// bob splits named-argument support into a second `DialectWithNamed` interface
/// and type-asserts on it. We keep one trait with a defaulted method instead, so
/// resolution stays static.
///
/// The writing methods take `&mut String` and cannot fail: the only sink is the
/// [`SqlWriter`](crate::SqlWriter)'s own buffer, and `String` writes are
/// infallible. bob's `io.StringWriter` errors are discarded for the same reason.
pub trait Dialect: std::fmt::Debug + Send + Sync {
    /// Write the placeholder for the argument at `position` (1-based).
    fn write_arg(&self, w: &mut String, position: usize);

    /// Write `s` as a quoted identifier, including the quote characters.
    fn write_quoted(&self, w: &mut String, s: &str);

    /// Write the placeholder for a named argument.
    ///
    /// Defaults to [`Error::NoNamedArgs`]; only SQLite overrides it.
    fn write_named_arg(&self, _w: &mut String, _name: &str) -> Result<()> {
        Err(Error::NoNamedArgs)
    }
}

impl<D: Dialect + ?Sized> Dialect for &D {
    fn write_arg(&self, w: &mut String, position: usize) {
        (**self).write_arg(w, position);
    }

    fn write_quoted(&self, w: &mut String, s: &str) {
        (**self).write_quoted(w, s);
    }

    fn write_named_arg(&self, w: &mut String, name: &str) -> Result<()> {
        (**self).write_named_arg(w, name)
    }
}

/// Dialects used by this crate's own tests.
///
/// Not exported: each dialect crate ships the real thing. These exist so the
/// core primitives can be tested without depending on a dialect crate, and so
/// that unit tests inside `crate::expr` — whose golden cases are
/// dialect-agnostic — have something to render with.
#[cfg(test)]
pub(crate) mod testing {
    use super::Dialect;
    use crate::error::Result;

    /// `$1` placeholders and `"` quoting, like PostgreSQL.
    #[derive(Debug, Clone, Copy)]
    pub(crate) struct Numbered;

    impl Dialect for Numbered {
        fn write_arg(&self, w: &mut String, position: usize) {
            w.push('$');
            w.push_str(&position.to_string());
        }

        fn write_quoted(&self, w: &mut String, s: &str) {
            w.push('"');
            w.push_str(s);
            w.push('"');
        }
    }

    /// `?` placeholders and backtick quoting, like MySQL: the position is
    /// dropped, which is what makes re-indexing bugs invisible there.
    #[derive(Debug, Clone, Copy)]
    pub(crate) struct Positional;

    impl Dialect for Positional {
        fn write_arg(&self, w: &mut String, _position: usize) {
            w.push('?');
        }

        fn write_quoted(&self, w: &mut String, s: &str) {
            w.push('`');
            w.push_str(s);
            w.push('`');
        }
    }

    /// `?1` placeholders plus `:name`, like SQLite.
    #[derive(Debug, Clone, Copy)]
    pub(crate) struct Named;

    impl Dialect for Named {
        fn write_arg(&self, w: &mut String, position: usize) {
            w.push('?');
            w.push_str(&position.to_string());
        }

        fn write_quoted(&self, w: &mut String, s: &str) {
            w.push('"');
            w.push_str(s);
            w.push('"');
        }

        fn write_named_arg(&self, w: &mut String, name: &str) -> Result<()> {
            w.push(':');
            w.push_str(name);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{Named, Numbered, Positional};
    use super::*;

    #[test]
    fn named_args_are_refused_unless_the_dialect_opts_in() {
        let mut s = String::new();
        assert!(matches!(
            Numbered.write_named_arg(&mut s, "id"),
            Err(Error::NoNamedArgs)
        ));
        assert!(matches!(
            Positional.write_named_arg(&mut s, "id"),
            Err(Error::NoNamedArgs)
        ));

        let mut s = String::new();
        Named.write_named_arg(&mut s, "id").unwrap();
        assert_eq!(s, ":id");
    }

    #[test]
    fn dyn_dialect_is_usable() {
        let d: &dyn Dialect = &Numbered;
        let mut s = String::new();
        d.write_arg(&mut s, 3);
        d.write_quoted(&mut s, "age");
        assert_eq!(s, "$3\"age\"");
    }
}
