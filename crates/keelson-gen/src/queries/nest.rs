//! bob's nested-row naming, ported: what an output column's *name* says about
//! where it lands in the row struct.
//!
//! | output column | lands in |
//! |---|---|
//! | `title` | a plain field |
//! | `author__name` | `row.author.name` — a to-one nested struct, `Option` when the whole side can be absent |
//! | `tags.name` | `row.tags[i].name` — a to-many nested `Vec` |
//!
//! `-- prefix: <text>` replaces both defaults for one query: any column whose
//! name starts with the prefix belongs to the nested field the prefix names,
//! and the nesting is to-many when the prefix ends in `.` (bob's
//! `--prefix:videos.`), to-one otherwise.

use crate::queries::ir::Nesting;

/// Split an output column name into its nesting and its field name.
pub fn split(name: &str, prefix: Option<&str>) -> (Nesting, String) {
    if let Some(p) = prefix {
        if let Some(rest) = name.strip_prefix(p)
            && !rest.is_empty()
        {
            let base = p.trim_end_matches(['.', '_']).to_owned();
            let nesting = if p.ends_with('.') {
                Nesting::ToMany(base)
            } else {
                Nesting::ToOne(base)
            };
            return (nesting, rest.to_owned());
        }
        return (Nesting::Flat, name.to_owned());
    }
    if let Some((head, rest)) = name.split_once("__")
        && !head.is_empty()
        && !rest.is_empty()
    {
        return (Nesting::ToOne(head.to_owned()), rest.to_owned());
    }
    if let Some((head, rest)) = name.split_once('.')
        && !head.is_empty()
        && !rest.is_empty()
    {
        return (Nesting::ToMany(head.to_owned()), rest.to_owned());
    }
    (Nesting::Flat, name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_double_underscore_for_to_one_and_dot_for_to_many() {
        assert_eq!(split("title", None), (Nesting::Flat, "title".to_owned()));
        assert_eq!(
            split("author__name", None),
            (Nesting::ToOne("author".to_owned()), "name".to_owned())
        );
        assert_eq!(
            split("tags.name", None),
            (Nesting::ToMany("tags".to_owned()), "name".to_owned())
        );
    }

    #[test]
    fn a_prefix_annotation_switches_both() {
        assert_eq!(
            split("videos.id", Some("videos.")),
            (Nesting::ToMany("videos".to_owned()), "id".to_owned())
        );
        assert_eq!(
            split("owner_id", Some("videos.")),
            (Nesting::Flat, "owner_id".to_owned()),
            "a column outside the prefix stays flat, even with `_` in its name"
        );
        assert_eq!(
            split("author_name", Some("author_")),
            (Nesting::ToOne("author".to_owned()), "name".to_owned())
        );
    }

    #[test]
    fn a_leading_or_trailing_separator_is_not_a_split() {
        assert_eq!(split("__x", None), (Nesting::Flat, "__x".to_owned()));
        assert_eq!(split("x__", None), (Nesting::Flat, "x__".to_owned()));
    }
}
