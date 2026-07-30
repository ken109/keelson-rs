use std::fmt;

/// The result type used throughout keelson.
pub type Result<T> = std::result::Result<T, Error>;

/// Everything that can go wrong while building a query.
///
/// Building is pure string work, so the set is small: almost every failure is
/// either "this dialect cannot express that" or "the caller wired the pieces up
/// inconsistently". Execution errors live in the backend crates.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The dialect has no syntax for named argument placeholders.
    ///
    /// bob calls this `ErrNoNamedArgs` and models it by having a separate
    /// `DialectWithNamed` interface; we keep one trait and fail here instead.
    NoNamedArgs,

    /// A raw clause's `?` placeholders and its argument list disagree.
    ///
    /// The message is byte-compatible with bob's `rawError` so ported tests can
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
