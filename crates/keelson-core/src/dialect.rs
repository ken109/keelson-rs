use crate::error::Error;
use crate::writer::SqlWriter;

/// The per-database syntax an expression needs in order to render itself.
///
/// The three dialects keelson targets differ only in these decisions:
///
/// | dialect | placeholder | quote | named |
/// |---|---|---|---|
/// | PostgreSQL | `$1` | `"id"` | unsupported |
/// | MySQL | `?` (position ignored) | `` `id` `` | unsupported |
/// | SQLite | `?1` | `"id"` | `:name` |
///
/// bob splits named-argument support into a second `DialectWithNamed` interface
/// and type-asserts on it. We keep one trait with a defaulted method instead, so
/// resolution stays static and "this dialect cannot do that" is one recorded
/// error rather than a failed downcast.
///
/// All three methods are infallible. They append to the writer's buffer, and
/// writing into a `String` cannot fail; the one real failure — a named argument
/// asked of a dialect that has none — is *recorded* on the writer with
/// [`SqlWriter::record_error`] and surfaced later by [`build`](crate::build).
///
/// # Implementing
///
/// Write only through [`SqlWriter::push_str`]. Never call
/// [`SqlWriter::push_arg`] from inside [`write_arg`](Self::write_arg): that is
/// the method `push_arg` calls, and it is the only place the placeholder counter
/// advances.
pub trait Dialect: std::fmt::Debug + Send + Sync {
    /// Write the placeholder for the argument at `position` (1-based).
    ///
    /// A dialect with positional placeholders ignores `position` — the argument
    /// order carries the meaning instead.
    fn write_arg(&self, w: &mut SqlWriter<'_>, position: usize);

    /// Write `s` as a quoted identifier, including the quote characters.
    fn write_quoted(&self, w: &mut SqlWriter<'_>, s: &str);

    /// Write the placeholder for a named argument.
    ///
    /// Defaults to recording [`Error::NoNamedArgs`]; only SQLite overrides it.
    fn write_named_arg(&self, w: &mut SqlWriter<'_>, _name: &str) {
        w.record_error(Error::NoNamedArgs);
    }
}

impl<D: Dialect + ?Sized> Dialect for &D {
    fn write_arg(&self, w: &mut SqlWriter<'_>, position: usize) {
        (**self).write_arg(w, position);
    }

    fn write_quoted(&self, w: &mut SqlWriter<'_>, s: &str) {
        (**self).write_quoted(w, s);
    }

    fn write_named_arg(&self, w: &mut SqlWriter<'_>, name: &str) {
        (**self).write_named_arg(w, name);
    }
}

/// Stand-in dialects, for tests and for the dialect-agnostic golden cases.
///
/// Enabled by this crate's own tests and by the `testing` feature, which the
/// dialect crates switch on as a dev-dependency. The real dialects ship in
/// `keelson-psql`, `keelson-mysql` and `keelson-sqlite`; these exist so that
/// expression-level code can be tested without depending on any of them.
#[cfg(any(test, feature = "testing"))]
pub mod testing {
    use super::Dialect;
    use crate::writer::SqlWriter;

    /// `?1` placeholders, `:name` named arguments and `"` quoting.
    ///
    /// Byte-for-byte the dialect bob's own `expr` package tests with, which is
    /// what the dialect-agnostic golden cases were recorded against. Use this one
    /// unless a test is specifically about placeholder style.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct TestDialect;

    impl Dialect for TestDialect {
        fn write_arg(&self, w: &mut SqlWriter<'_>, position: usize) {
            w.push_str("?");
            w.push_usize(position);
        }

        fn write_quoted(&self, w: &mut SqlWriter<'_>, s: &str) {
            w.push_str("\"");
            w.push_str(s);
            w.push_str("\"");
        }

        fn write_named_arg(&self, w: &mut SqlWriter<'_>, name: &str) {
            w.push_str(":");
            w.push_str(name);
        }
    }

    /// `$1` placeholders and `"` quoting, like PostgreSQL. No named arguments.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct Numbered;

    impl Dialect for Numbered {
        fn write_arg(&self, w: &mut SqlWriter<'_>, position: usize) {
            w.push_str("$");
            w.push_usize(position);
        }

        fn write_quoted(&self, w: &mut SqlWriter<'_>, s: &str) {
            w.push_str("\"");
            w.push_str(s);
            w.push_str("\"");
        }
    }

    /// `?` placeholders and backtick quoting, like MySQL.
    ///
    /// The position is dropped, which is exactly what makes a re-indexing bug
    /// invisible there — so indexing tests use [`Numbered`] instead.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct Positional;

    impl Dialect for Positional {
        fn write_arg(&self, w: &mut SqlWriter<'_>, _position: usize) {
            w.push_str("?");
        }

        fn write_quoted(&self, w: &mut SqlWriter<'_>, s: &str) {
            w.push_str("`");
            w.push_str(s);
            w.push_str("`");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{Numbered, Positional, TestDialect};
    use super::*;
    use crate::error::Error;

    // These cases are about the three `Dialect` methods and nothing else, so what
    // they assert is a placeholder and a quoted identifier written back to back —
    // `$1"id"`. That is deliberately not SQL: the point is which characters each
    // dialect emits, and a statement would only hide them behind a frame. The
    // dialects' output *as SQL* is judged wherever a fragment or statement rendered
    // through them is, which is most of the rest of this crate's tests.

    /// The recorded shapes of the three real dialects, checked against the ones
    /// this trait has to be able to express.
    #[test]
    fn the_trait_expresses_all_three_real_dialects() {
        // psql: $N and "id"
        let mut w = SqlWriter::new(&Numbered);
        w.push_arg(1i32);
        w.push_quoted(&["id"]);
        assert_eq!(w.sql(), r#"$1"id""#);

        // mysql: ? and `id`
        let mut w = SqlWriter::new(&Positional);
        w.push_arg(1i32);
        w.push_quoted(&["id"]);
        assert_eq!(w.sql(), "?`id`");

        // sqlite: ?N, :name and "id"
        let mut w = SqlWriter::new(&TestDialect);
        w.push_arg(1i32);
        w.push_named_arg("name");
        w.push_quoted(&["id"]);
        assert_eq!(w.sql(), r#"?1:name"id""#);
    }

    #[test]
    fn named_args_are_refused_unless_the_dialect_opts_in() {
        for d in [&Numbered as &dyn Dialect, &Positional] {
            let mut w = SqlWriter::new(d);
            w.push_named_arg("id");
            assert!(matches!(w.error(), Some(Error::NoNamedArgs)));
        }
    }

    #[test]
    fn a_dialect_reference_is_itself_a_dialect() {
        // So that `impl Dialect for X` is usable both as `&X` and `&&X`.
        let d = &Numbered;
        let mut w = SqlWriter::new(&d);
        w.push_arg(7i32);
        assert_eq!(w.sql(), "$1");
    }
}
