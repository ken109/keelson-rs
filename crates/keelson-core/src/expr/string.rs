use crate::error::Result;
use crate::writer::{Expression, SqlWriter};

/// A single-quoted SQL string literal.
///
/// Nothing is escaped, exactly as in bob: this is for literals the query author
/// wrote, not for user input. User input belongs in an [`Args`](super::Args).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawString(String);

impl RawString {
    /// A literal for `s`.
    pub fn new(s: impl Into<String>) -> Self {
        RawString(s.into())
    }

    /// The literal's contents, without the quotes.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A single-quoted string literal — bob's `S`.
pub fn s(literal: impl Into<String>) -> RawString {
    RawString::new(literal)
}

impl Expression for RawString {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        w.push_str("'");
        w.push_str(&self.0);
        w.push_str("'");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::writer::build;

    #[test]
    fn a_literal_is_single_quoted() {
        let (sql, args) = build(&Numbered, &s("a string")).unwrap();
        assert_eq!(sql, "'a string'");
        assert!(args.is_empty(), "a literal binds nothing");
    }
}
