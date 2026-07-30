use std::fmt;

/// Everything that can go wrong between a connection string and the emitted
/// files. One enum, no `anyhow`: callers (the CLI, tests, build scripts)
/// match on the kind.
#[derive(Debug)]
pub enum GenError {
    /// The TOML configuration failed to parse or contradicts itself.
    Config(String),
    /// The catalog queries failed or returned something unusable.
    Introspect(String),
    /// A column's database type has no default mapping and no configured
    /// override — the honest failure `docs/type-mappings.md` prescribes.
    UnmappedType {
        /// `table.column` the failure names.
        column: String,
        /// The declared database type that had no mapping.
        db_type: String,
    },
    /// A feature the generator deliberately does not cover yet (MySQL
    /// emission, composite-column foreign keys, …).
    Unsupported(String),
    /// Filesystem trouble writing the output.
    Io(std::io::Error),
}

impl fmt::Display for GenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GenError::Config(msg) => write!(f, "config: {msg}"),
            GenError::Introspect(msg) => write!(f, "introspection: {msg}"),
            GenError::UnmappedType { column, db_type } => write!(
                f,
                "no type mapping for {column} (db type `{db_type}`); \
                 add a [types.map] or [[types.override]] entry"
            ),
            GenError::Unsupported(msg) => write!(f, "unsupported: {msg}"),
            GenError::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for GenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            GenError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for GenError {
    fn from(e: std::io::Error) -> Self {
        GenError::Io(e)
    }
}

/// The crate-wide result.
pub type Result<T> = std::result::Result<T, GenError>;
