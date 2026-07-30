use std::fmt;

/// The result type used throughout keelson.
pub type Result<T> = std::result::Result<T, Error>;

/// Everything that can go wrong while building a query.
///
/// Building is pure string work, so the set is small: almost every failure is
/// either "this dialect cannot express that" or "the caller wired the pieces up
/// inconsistently". Execution errors live in the backend crates.
///
/// Rendering itself is infallible — [`Expression::write_sql`](crate::Expression::write_sql)
/// returns nothing. The rare failure is recorded on the
/// [`SqlWriter`](crate::SqlWriter) and surfaced once, by
/// [`build`](crate::build).
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The dialect has no syntax for named argument placeholders.
    ///
    /// bob models this by having a separate `DialectWithNamed` interface and
    /// type-asserting on it; we keep one trait whose default
    /// [`write_named_arg`](crate::Dialect::write_named_arg) records this instead.
    NoNamedArgs,

    /// A raw clause's `?` placeholders and its argument list disagree.
    ///
    /// The message is byte-compatible with bob's `rawError` so a ported test can
    /// compare it directly.
    RawArgCount {
        /// How many `?` the clause contains.
        placeholders: usize,
        /// How many arguments were supplied.
        args: usize,
        /// The offending clause, for the message.
        clause: String,
    },

    /// A [`Value`](crate::Value) could not be read as the requested Rust type.
    TypeMismatch {
        /// The Rust type that was asked for.
        expected: &'static str,
        /// The `Value` variant that was actually present.
        found: &'static str,
    },

    /// A query is missing a clause it cannot be rendered without.
    Incomplete(&'static str),

    /// A dialect-specific or generated-code failure that has no shared shape.
    Other(String),
}

impl Error {
    /// Shorthand for [`Error::TypeMismatch`].
    pub fn type_mismatch(expected: &'static str, found: &'static str) -> Self {
        Error::TypeMismatch { expected, found }
    }

    /// Shorthand for [`Error::RawArgCount`].
    pub fn raw_arg_count(placeholders: usize, args: usize, clause: impl Into<String>) -> Self {
        Error::RawArgCount {
            placeholders,
            args,
            clause: clause.into(),
        }
    }

    /// Shorthand for [`Error::Other`].
    pub fn other(msg: impl Into<String>) -> Self {
        Error::Other(msg.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NoNamedArgs => f.write_str("Dialect does not support named arguments"),
            Error::RawArgCount {
                placeholders,
                args,
                clause,
            } => write!(
                f,
                "Bad Statement: has {placeholders} placeholders but {args} args: {clause}"
            ),
            Error::TypeMismatch { expected, found } => {
                write!(f, "cannot read {found} as {expected}")
            }
            Error::Incomplete(what) => write!(f, "query is missing {what}"),
            Error::Other(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_arg_error_reads_like_bobs() {
        assert_eq!(
            Error::NoNamedArgs.to_string(),
            "Dialect does not support named arguments"
        );
    }

    #[test]
    fn raw_arg_count_message_is_byte_compatible_with_bob() {
        // bob: "Bad Statement: has 2 placeholders but 0 args: <clause>"
        let e = Error::raw_arg_count(2, 0, "SELECT a, b FROM alphabet WHERE c = ? AND d <= ?");
        assert_eq!(
            e.to_string(),
            "Bad Statement: has 2 placeholders but 0 args: SELECT a, b FROM alphabet WHERE c = ? AND d <= ?"
        );
    }

    #[test]
    fn is_a_std_error() {
        fn takes(_: &dyn std::error::Error) {}
        takes(&Error::Incomplete("a table"));
    }
}
