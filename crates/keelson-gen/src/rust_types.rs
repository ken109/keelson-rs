//! What the emitters know about the Rust types they are about to write.
//!
//! Three emitters — [`emit::model`](crate::emit), [`emit::factory`](crate::emit)
//! and [`queries::emit`](crate::queries) — each have to answer the same two
//! questions about a type named in the schema or in a `-- param:` annotation:
//! *is it `Copy`* (so a read needs no `.clone()`), and *is it a type we have
//! never heard of* (so a `.clone()` on it might be a `clone_on_copy` lint in
//! the reader's crate). All three used to carry their own copy of the list,
//! one of them spelled as a `matches!` rather than a `const`, with a comment
//! saying it was kept in step by hand. It was — but nothing checked, and the
//! cost of a drift is generated code that lints or fails to compile in
//! somebody else's build.
//!
//! The lists are private here. Callers ask [`is_copy`] and
//! [`needs_copy_allow`], which is the whole interface: whether a type is on a
//! list is this module's business, and a fourth emitter that needs the same
//! answer gets it without learning that lists are how it is stored.

use proc_macro2::TokenStream;
use quote::quote;

use crate::error::{GenError, Result};
use crate::names::ident;
use crate::resolve::ModelColumn;

/// Rust types the generator knows are `Copy`, so a read of one needs no
/// `.clone()`.
///
/// Every entry is a type keelson itself can produce: the primitives, and the
/// optional first-class `Value` types behind keelson-core's `chrono`, `uuid`
/// and `decimal` features. A `[[types.override]]` may name anything at all,
/// which is why [`needs_copy_allow`] exists.
const KNOWN_COPY: &[&str] = &[
    "i8",
    "i16",
    "i32",
    "i64",
    "i128",
    "isize",
    "u8",
    "u16",
    "u32",
    "u64",
    "u128",
    "usize",
    "bool",
    "f32",
    "f64",
    "char",
    "uuid::Uuid",
    "chrono::NaiveDate",
    "chrono::NaiveTime",
    "chrono::NaiveDateTime",
    "chrono::DateTime<chrono::Utc>",
    "rust_decimal::Decimal",
];

/// Rust types the generator knows are *not* `Copy` — cloning one is never a
/// `clone_on_copy` candidate, so no `allow` is needed.
const KNOWN_CLONE: &[&str] = &["String", "Vec<u8>", "serde_json::Value"];

/// Whether a read of this type can be a plain field access.
pub(crate) fn is_copy(rust_type: &str) -> bool {
    KNOWN_COPY.contains(&rust_type)
}

/// Whether cloning this type needs an `#[allow(clippy::clone_on_copy)]`
/// around it.
///
/// True exactly for the types on neither list — an override type the
/// generator has never seen. It may or may not be `Copy`; the generated code
/// clones it either way, and the allow keeps that from lining the reader's
/// build with warnings for a decision they did not make.
pub(crate) fn needs_copy_allow(rust_type: &str) -> bool {
    !is_copy(rust_type) && !KNOWN_CLONE.contains(&rust_type)
}

/// A configured type name → the `syn::Type` to write, or a configuration
/// error naming where it came from.
pub(crate) fn parse_type(rust_type: &str, what: &str) -> Result<syn::Type> {
    syn::parse_str(rust_type)
        .map_err(|e| GenError::Config(format!("{what}: `{rust_type}` is not a Rust type: {e}")))
}

/// `recv.field` / `recv.field.clone()` as the column's type demands.
pub(crate) fn key_access(recv: TokenStream, c: &ModelColumn) -> TokenStream {
    let f = ident(&c.field);
    if is_copy(&c.rust_type) {
        quote!(#recv.#f)
    } else {
        quote!(#recv.#f.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A type on both lists would make [`needs_copy_allow`] depend on which
    /// list is consulted first — a bug that reads as correct in either
    /// half.
    #[test]
    fn the_two_lists_are_disjoint() {
        for t in KNOWN_COPY {
            assert!(!KNOWN_CLONE.contains(t), "`{t}` is on both lists");
        }
    }

    /// Every listed name is written into generated code as a type. A typo
    /// here does not fail until somebody compiles the output.
    #[test]
    fn every_listed_name_is_a_type() {
        for t in KNOWN_COPY.iter().chain(KNOWN_CLONE) {
            assert!(parse_type(t, "list entry").is_ok(), "`{t}` is not a type");
        }
    }

    #[test]
    fn an_unknown_type_is_cloned_under_an_allow() {
        assert!(!is_copy("my_crate::Money"));
        assert!(needs_copy_allow("my_crate::Money"));
        // Known either way: no allow.
        assert!(!needs_copy_allow("i32"));
        assert!(!needs_copy_allow("String"));
    }
}
