use std::fmt;

/// Which statement a query renders.
///
/// Carried so the execution layer can decide whether to expect rows without
/// parsing the SQL back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum QueryType {
    /// Not one of the four, e.g. a raw statement or a `CREATE TABLE`.
    #[default]
    Unknown,
    Select,
    Insert,
    Update,
    Delete,
}

impl fmt::Display for QueryType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            QueryType::Unknown => "UNKNOWN",
            QueryType::Select => "SELECT",
            QueryType::Insert => "INSERT",
            QueryType::Update => "UPDATE",
            QueryType::Delete => "DELETE",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_matches_the_sql_keyword() {
        assert_eq!(QueryType::Select.to_string(), "SELECT");
        assert_eq!(QueryType::Insert.to_string(), "INSERT");
        assert_eq!(QueryType::Update.to_string(), "UPDATE");
        assert_eq!(QueryType::Delete.to_string(), "DELETE");
        assert_eq!(QueryType::Unknown.to_string(), "UNKNOWN");
        assert_eq!(QueryType::default(), QueryType::Unknown);
    }
}
