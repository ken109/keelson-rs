//! Naming: snake → Pascal, singularisation, and keyword-safe identifiers.
//!
//! Inflection is rules-plus-exceptions, exactly as bob ships it: a small set
//! of English rules here, and the config's `[inflections]` table for the
//! irregulars the rules get wrong (`people = "person"`). The rules are
//! deliberately few — a wrong guess is one config line away from being fixed,
//! and the config is the durable record of the decision.

use std::collections::BTreeMap;

use proc_macro2::{Ident, Span};

/// `post_tags` → `PostTags`. Non-alphanumeric characters split words.
pub(crate) fn pascal(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper_next = true;
    for c in name.chars() {
        if c == '_' || c == '-' || c == ' ' {
            upper_next = true;
        } else if upper_next {
            out.extend(c.to_uppercase());
            upper_next = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// Singularise the *last* word of a snake_case name: `post_tags` →
/// `post_tag`, `people` → `person` (via `irregulars`).
pub(crate) fn singular(name: &str, irregulars: &BTreeMap<String, String>) -> String {
    if let Some(s) = irregulars.get(name) {
        return s.clone();
    }
    let (head, last) = match name.rfind('_') {
        Some(i) => (&name[..=i], &name[i + 1..]),
        None => ("", name),
    };
    if let Some(s) = irregulars.get(last) {
        return format!("{head}{s}");
    }
    format!("{head}{}", singular_word(last))
}

/// The rule half: strip a plural suffix from one English word.
fn singular_word(word: &str) -> String {
    for (suffix, replacement) in [
        ("ies", "y"),   // categories → category
        ("sses", "ss"), // addresses → address
        ("shes", "sh"), // dishes → dish
        ("ches", "ch"), // branches → branch
        ("xes", "x"),   // boxes → box
        ("zes", "z"),   // sizes → size? no: prizes → prize handled below by "s"
        ("uses", "us"), // statuses → status
        ("oes", "o"),   // heroes → hero
        ("ss", "ss"),   // status quo: a word ending in ss is not plural
    ] {
        if let Some(stem) = word.strip_suffix(suffix) {
            // "zes" is ambiguous (sizes/prizes want "size"/"prize"); prefer
            // dropping only the "s" for that one.
            if suffix == "zes" {
                return format!("{stem}ze");
            }
            return format!("{stem}{replacement}");
        }
    }
    word.strip_suffix('s').unwrap_or(word).to_owned()
}

/// A `proc_macro2::Ident` for a schema-supplied name, raw-prefixed when the
/// name collides with a Rust keyword (`type` → `r#type`).
pub(crate) fn ident(name: &str) -> Ident {
    if syn::parse_str::<Ident>(name).is_ok() {
        Ident::new(name, Span::call_site())
    } else {
        // Keywords (`type`, `where`, …). `crate`/`self`/`super`/`Self`
        // cannot even be raw; suffix those.
        if matches!(name, "crate" | "self" | "super" | "Self" | "_") {
            Ident::new(&format!("{name}_"), Span::call_site())
        } else {
            Ident::new_raw(name, Span::call_site())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pascal_splits_on_underscores() {
        assert_eq!(pascal("users"), "Users");
        assert_eq!(pascal("post_tags"), "PostTags");
        assert_eq!(pascal("a"), "A");
    }

    #[test]
    fn singular_handles_the_common_rules() {
        let none = BTreeMap::new();
        assert_eq!(singular("users", &none), "user");
        assert_eq!(singular("posts", &none), "post");
        assert_eq!(singular("post_tags", &none), "post_tag");
        assert_eq!(singular("categories", &none), "category");
        assert_eq!(singular("statuses", &none), "status");
        assert_eq!(singular("addresses", &none), "address");
        assert_eq!(singular("branches", &none), "branch");
        assert_eq!(singular("boxes", &none), "box");
        assert_eq!(singular("sizes", &none), "size");
        assert_eq!(singular("access", &none), "access", "not plural at all");
    }

    #[test]
    fn irregulars_come_from_config_and_apply_to_the_last_word() {
        let mut irr = BTreeMap::new();
        irr.insert("people".to_owned(), "person".to_owned());
        assert_eq!(singular("people", &irr), "person");
        assert_eq!(singular("sales_people", &irr), "sales_person");
    }

    #[test]
    fn keywords_become_raw_idents() {
        assert_eq!(ident("type").to_string(), "r#type");
        assert_eq!(ident("users").to_string(), "users");
        assert_eq!(ident("self").to_string(), "self_");
    }
}
