use std::fmt;

use crate::executor::Family;
use crate::transaction::TxConflictError;

/// Everything that can go wrong while executing a statement.
///
/// Build failures wrap [`keelson_core::Error`]; decode failures additionally
/// carry the column they happened in, because "which column" is the question a
/// failing read always raises first. Driver failures are boxed rather than
/// enumerated: their shapes belong to the backend crates.
///
/// The one driver failure that is *not* boxed is a concurrency conflict.
/// Whether to retry is the most consequential question a caller asks of an
/// error here, and the answer is the same on every engine — so it is a variant
/// a `match` can reach, not a `Box` a caller has to downcast.
#[derive(Debug)]
#[non_exhaustive]
pub enum ExecError {
    /// The query failed to build. Nothing was sent to the database.
    Build(keelson_core::Error),

    /// A column's value could not be read as the requested Rust type.
    ///
    /// `column` is the column name, or `#N` for positional access.
    Decode {
        /// The column being read.
        column: String,
        /// Why the value would not convert — usually
        /// [`TypeMismatch`](keelson_core::Error::TypeMismatch).
        source: keelson_core::Error,
    },

    /// A column name was asked of a result set that has no such column.
    MissingColumn {
        /// The name that was asked for.
        column: String,
        /// The columns that are actually present, because a typo'd name is
        /// the bug nine times out of ten.
        available: Vec<String>,
    },

    /// `fetch_one` got zero rows.
    RowNotFound,

    /// `fetch_one` or `fetch_optional` got more than one row.
    ///
    /// sqlx silently takes the first; here "one" means one, so extras are an
    /// error rather than a hidden data bug.
    TooManyRows,

    /// A [`Value`](keelson_core::Value) this backend has no binding for —
    /// an unknown [`CustomValue`](keelson_core::CustomValue), or a variant the
    /// engine cannot represent (e.g. `u64::MAX` where only signed 64-bit
    /// parameters exist). Refused loudly at bind time; never stringified and
    /// hoped for.
    UnsupportedValue {
        /// The value's [`type_name`](keelson_core::Value::type_name).
        type_name: &'static str,
        /// The backend that refused it.
        family: Family,
    },

    /// The engine refused the work because something else held what it
    /// needed: a serialization failure, a deadlock, a lock timeout, a busy
    /// database. Every one of them means the same thing to a caller — retry
    /// the transaction from the top — which is why this is a variant rather
    /// than one more boxed driver error.
    ///
    /// [`TxConflict::of`](crate::TxConflict::of) reads it out of any
    /// [`ExecError`]; matching on it directly is the same answer without the
    /// call.
    Conflict(TxConflictError),

    /// The driver reported a failure — connection, protocol, server error.
    Driver(Box<dyn std::error::Error + Send + Sync>),

    /// A failure with no shared shape.
    Other(String),
}

impl ExecError {
    /// Wrap a driver error. Backends call this; applications match on it.
    pub fn driver(e: impl std::error::Error + Send + Sync + 'static) -> Self {
        ExecError::Driver(Box::new(e))
    }

    /// Shorthand for [`ExecError::Other`].
    pub fn other(msg: impl Into<String>) -> Self {
        ExecError::Other(msg.into())
    }
}

impl From<keelson_core::Error> for ExecError {
    fn from(e: keelson_core::Error) -> Self {
        ExecError::Build(e)
    }
}

impl fmt::Display for ExecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExecError::Build(e) => write!(f, "query failed to build: {e}"),
            ExecError::Decode { column, source } => write!(f, "column \"{column}\": {source}"),
            ExecError::MissingColumn { column, available } => write!(
                f,
                "no column \"{column}\" in result set (columns: {})",
                available.join(", ")
            ),
            ExecError::RowNotFound => {
                f.write_str("no rows returned where exactly one was expected")
            }
            ExecError::TooManyRows => {
                f.write_str("more than one row returned where at most one was expected")
            }
            ExecError::UnsupportedValue { type_name, family } => {
                write!(f, "cannot bind a {type_name} value on {family}")
            }
            ExecError::Conflict(e) => write!(f, "{e}"),
            ExecError::Driver(e) => write!(f, "driver error: {e}"),
            ExecError::Other(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for ExecError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ExecError::Build(e) | ExecError::Decode { source: e, .. } => Some(e),
            ExecError::Conflict(e) => std::error::Error::source(e),
            ExecError::Driver(e) => Some(e.as_ref()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_errors_name_the_column() {
        let e = ExecError::Decode {
            column: "email".into(),
            source: keelson_core::Error::type_mismatch("String", "NULL"),
        };
        assert_eq!(
            e.to_string(),
            "column \"email\": cannot read NULL as String"
        );
    }

    #[test]
    fn missing_column_lists_what_was_there() {
        let e = ExecError::MissingColumn {
            column: "emial".into(),
            available: vec!["id".into(), "name".into(), "email".into()],
        };
        assert_eq!(
            e.to_string(),
            "no column \"emial\" in result set (columns: id, name, email)"
        );
    }

    #[test]
    fn is_a_std_error_with_a_source() {
        let e = ExecError::Build(keelson_core::Error::Incomplete("a table"));
        assert!(std::error::Error::source(&e).is_some());
    }
}
