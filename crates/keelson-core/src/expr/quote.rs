use crate::error::Result;
use crate::writer::{Expression, SqlWriter};

/// A dot-joined, quoted identifier such as `"users"."id"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quoted(Vec<String>);

impl Quoted {
    /// An identifier from its parts. Empty parts are dropped, so a caller can
    /// pass an unset qualifier without branching.
    pub fn new(parts: impl IntoIterator<Item = String>) -> Self {
        Quoted(parts.into_iter().filter(|p| !p.is_empty()).collect())
    }

    /// The parts, outermost first.
    pub fn parts(&self) -> &[String] {
        &self.0
    }
}

impl Expression for Quoted {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        let parts: Vec<&str> = self.0.iter().map(String::as_str).collect();
        w.push_quoted(&parts);
        Ok(())
    }
}

/// Anything that names an identifier: one part or several.
///
/// This is what lets `quote("age")` and `quote(("users", "id"))` share an entry
/// point instead of forcing a slice literal at every call site.
pub trait QuoteParts {
    /// The identifier's parts, outermost first.
    fn into_quote_parts(self) -> Vec<String>;
}

/// A quoted, dot-joined identifier.
///
/// ```
/// # use keelson_core::expr::quote;
/// let one = quote("age");
/// let two = quote(("users", "id"));
/// let many = quote(["public", "users", "id"]);
/// # assert_eq!(one.parts(), ["age"]);
/// # assert_eq!(two.parts(), ["users", "id"]);
/// # assert_eq!(many.parts().len(), 3);
/// ```
pub fn quote(parts: impl QuoteParts) -> Quoted {
    Quoted::new(parts.into_quote_parts())
}

impl QuoteParts for &str {
    fn into_quote_parts(self) -> Vec<String> {
        vec![self.to_owned()]
    }
}

impl QuoteParts for String {
    fn into_quote_parts(self) -> Vec<String> {
        vec![self]
    }
}

impl QuoteParts for &String {
    fn into_quote_parts(self) -> Vec<String> {
        vec![self.clone()]
    }
}

impl<T: Into<String>, const N: usize> QuoteParts for [T; N] {
    fn into_quote_parts(self) -> Vec<String> {
        self.into_iter().map(Into::into).collect()
    }
}

impl<T: Into<String>> QuoteParts for Vec<T> {
    fn into_quote_parts(self) -> Vec<String> {
        self.into_iter().map(Into::into).collect()
    }
}

impl QuoteParts for &[&str] {
    fn into_quote_parts(self) -> Vec<String> {
        self.iter().map(|s| (*s).to_owned()).collect()
    }
}

macro_rules! quote_parts_for_tuple {
    ($($name:ident),+) => {
        impl<$($name: Into<String>),+> QuoteParts for ($($name,)+) {
            fn into_quote_parts(self) -> Vec<String> {
                #[allow(non_snake_case)]
                let ($($name,)+) = self;
                vec![$($name.into()),+]
            }
        }
    };
}

quote_parts_for_tuple!(A);
quote_parts_for_tuple!(A, B);
quote_parts_for_tuple!(A, B, C);
quote_parts_for_tuple!(A, B, C, D);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::{Numbered, Positional};
    use crate::writer::build;

    #[test]
    fn parts_are_dot_joined_and_quoted() {
        let (sql, args) = build(&Numbered, &quote(("users", "id"))).unwrap();
        assert_eq!(sql, r#""users"."id""#);
        assert!(args.is_empty());

        let (sql, _) = build(&Positional, &quote("age")).unwrap();
        assert_eq!(sql, "`age`");
    }

    #[test]
    fn empty_parts_are_dropped_at_construction() {
        let q = quote(["", "users", "", "id"]);
        assert_eq!(q.parts(), ["users", "id"]);
        let (sql, _) = build(&Numbered, &q).unwrap();
        assert_eq!(sql, r#""users"."id""#);
    }

    #[test]
    fn an_empty_identifier_writes_nothing() {
        let (sql, _) = build(&Numbered, &quote(Vec::<String>::new())).unwrap();
        assert_eq!(sql, "");
    }

    #[test]
    fn every_parts_shape_is_accepted() {
        assert_eq!(quote(String::from("a")).parts(), ["a"]);
        let owned = String::from("a");
        assert_eq!(quote(&owned).parts(), ["a"]);
        assert_eq!(quote(("a",)).parts(), ["a"]);
        assert_eq!(quote(("a", "b", "c")).parts().len(), 3);
        assert_eq!(quote(("a", "b", "c", "d")).parts().len(), 4);
        assert_eq!(quote(vec!["a", "b"]).parts().len(), 2);
        assert_eq!(quote(&["a", "b"][..]).parts().len(), 2);
    }
}
