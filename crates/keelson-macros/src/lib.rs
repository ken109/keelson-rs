//! keelson's derive macros: [`Bind`] for newtype column types, [`FromRow`]
//! for row mapping.
//!
//! Both close the same gap. `docs/type-mappings.md` promises that the Rust
//! type of a generated column can be replaced with your own, and that what a
//! replacement must satisfy is a *trait bound in the generated code* — so a
//! type that cannot bind is a compile error, not a runtime surprise. That
//! bound is `keelson_exec::Bind`:
//!
//! ```text
//! pub trait Bind: ToValue + FromValue + Send + 'static {}
//! impl<T: ToValue + FromValue + Send + 'static> Bind for T {}
//! ```
//!
//! and for every `[[types.override]]` in a keelson-gen configuration the
//! generator emits one line asserting it:
//!
//! ```text
//! const _: () = keelson_exec::assert_bind::<crate::types::UserId>();
//! ```
//!
//! Satisfying that used to mean hand-writing `ToValue` and `FromValue`.
//! `#[derive(Bind)]` writes them for a newtype.
//!
//! # Where this sits
//!
//! The derives, and nothing else. Do not depend on this crate directly:
//! [keelson-core](https://docs.rs/keelson-core) re-exports both behind its `macros` feature,
//! which is the path `use keelson_core::Bind;` takes and the one generated code
//! is written against. The traits they implement live in keelson-core
//! (`ToValue`/`FromValue`) and [keelson-exec](https://docs.rs/keelson-exec) (`FromRow`). The
//! whole map is the [keelson](https://docs.rs/keelson) facade crate.
//!
//! # `#[derive(Bind)]`
//!
//! ```
//! use keelson_core::{Bind, FromValue as _, ToValue as _, Value};
//!
//! #[derive(Debug, Clone, PartialEq, Bind)]
//! pub struct UserId(pub i64);
//!
//! #[derive(Debug, Clone, PartialEq, Bind)]
//! pub struct Email(String);
//!
//! // The line generated code emits for an override now passes.
//! const _: () = keelson_exec::assert_bind::<UserId>();
//!
//! assert_eq!(UserId(7).to_value(), Value::I64(7));
//! assert_eq!(UserId::from_value(Value::I64(7)).unwrap(), UserId(7));
//! ```
//!
//! What it emits, and nothing else: `keelson_core::ToValue` and
//! `keelson_core::FromValue`, each delegating to the single inner field.
//! `Bind` itself is never implemented — it is a blanket alias, and naming the
//! derive after the bound it satisfies is the point. Everything the inner type
//! can do, the newtype now does: `Option<UserId>` binds as NULL-or-value
//! through core's blanket impls, the widening `FromValue` accepts (a driver
//! that hands back `I32` for a `BIGINT`) still applies, and the type is usable
//! anywhere `arg(...)` takes a value.
//!
//! ## What it accepts
//!
//! One field, named or not — `struct UserId(i64);` and
//! `struct UserId { raw: i64 }` are the same thing to this derive. Generics
//! work (`struct Tagged<T>(T)`), with the inner type's bound added to the
//! generated impls.
//!
//! ## What it refuses, and why
//!
//! - **Multi-field structs.** A column is one value. A struct of several is a
//!   *row*, which is what [`FromRow`] is for; if the parts genuinely are one
//!   column, the encoding is a decision (separator? escaping? what does a
//!   malformed value mean?) that belongs in your own `ToValue`/`FromValue`.
//! - **Enums.** Same reason, one level deeper: an enum needs a chosen
//!   database representation — text or integer, the spelling of each variant,
//!   and what an unrecognised value read back means. Every one of those is a
//!   decision this derive would have to invent, and inventing it silently is
//!   exactly the "plausible guess" keelson does not make. Write the two impls
//!   (about ten lines; the unknown-variant case is
//!   `keelson_core::Error::type_mismatch`), or derive `Bind` on a newtype over
//!   the representation.
//! - **Unions and unit structs.** Nothing to bind.
//! - **Types with a lifetime parameter.** `FromValue` builds an *owned* value
//!   out of a `Value`, so a borrowing newtype could never read back. This one
//!   is refused rather than left to the compiler for a specific reason: the
//!   bound goes in a `where` clause, and rustc *accepts* an impl whose `where`
//!   clause can never hold — it just never applies. The derive would appear to
//!   work and then fail at the first call site, which is exactly the distant,
//!   inference-swamped failure this whole mechanism exists to replace.
//! - **`#[keelson(...)]` options.** There are none. A newtype has one field
//!   and one meaning. The whole `keelson` namespace is refused here rather
//!   than only the unrecognised keys, so a `rename` that drifted onto a
//!   newtype is caught instead of silently doing nothing. Deriving `Bind` and
//!   `FromRow` on the same one-field struct still works — just leave its field
//!   attribute-free.
//!
//! Each refusal is a compile error spanned at the offending item, naming the
//! restriction and what to do instead —
//! `keelson-macros/tests/compile_fail/*.stderr` pins the exact text.
//!
//! # `#[derive(FromRow)]`
//!
//! ```
//! use std::sync::Arc;
//!
//! use keelson_core::{FromRow, Value};
//! use keelson_exec::{Column, FromRow as _, Row};
//!
//! #[derive(Debug, PartialEq, FromRow)]
//! struct Account {
//!     id: i64,
//!     // The column is `email_address`; the field is not.
//!     #[keelson(rename = "email_address")]
//!     email: String,
//!     // A nullable column must be an Option, exactly as in a hand-written impl.
//!     nickname: Option<String>,
//!     // Read out of the same row, by the nested type's own FromRow.
//!     #[keelson(flatten)]
//!     audit: Audit,
//! }
//!
//! #[derive(Debug, PartialEq, FromRow)]
//! struct Audit {
//!     created_by: i64,
//! }
//!
//! let columns: Arc<[Column]> = vec![
//!     Column::new("id"),
//!     Column::new("email_address"),
//!     Column::new("nickname"),
//!     Column::new("created_by"),
//! ]
//! .into();
//! let mut row = Row::new(
//!     columns,
//!     vec![
//!         Value::I64(1),
//!         Value::Text("ada@example.com".into()),
//!         Value::Null,
//!         Value::I64(9),
//!     ],
//! );
//!
//! assert_eq!(
//!     Account::from_row(&mut row).unwrap(),
//!     Account {
//!         id: 1,
//!         email: "ada@example.com".into(),
//!         nickname: None,
//!         audit: Audit { created_by: 9 },
//!     }
//! );
//! ```
//!
//! The emitted body is the shape `keelson_exec::FromRow` documents and
//! keelson-gen already emits by hand — one `row.take("column")?` per field, by
//! name. By name, not by position, so it survives column reordering and
//! `SELECT *` drift; `take` rather than `get`, so `String`s and blobs move out
//! of the row instead of cloning. Errors keep their column name, because
//! `Row::take` puts it there.
//!
//! ## Field options
//!
//! - `#[keelson(rename = "column")]` — read that column instead of the one
//!   named after the field.
//! - `#[keelson(flatten)]` — read the field's own type out of the same row,
//!   through its `FromRow` impl. Nested structs, in other words, and it costs
//!   one line of generated code because `FromRow::from_row` already takes the
//!   whole row.
//!
//! ## What it refuses, and why
//!
//! - **Tuple structs and unit structs.** Mapping is by name, and unnamed
//!   fields have none. Tuples up to arity 8 *already* implement `FromRow`
//!   positionally, so the alternative is to delete the struct, and the error
//!   says so with your own field types substituted in.
//! - **Enums.** Which variant a row is depends on a discriminator column only
//!   you can name.
//! - **Structs with a lifetime parameter.** A row is decoded into owned
//!   `Value`s and every field is taken out of it by value, so a field
//!   borrowing from the row could not outlive the mapping — refused for the
//!   same "an unsatisfiable `where` clause compiles" reason as above.
//! - **`rename` together with `flatten`.** One names a single column, the
//!   other reads many.
//! - **Two fields reading the same column.** `take` consumes: the value moves
//!   out and NULL is left behind, so the second field would silently decode
//!   NULL. That is a bug the derive can see, so it is a compile error rather
//!   than a mystery at runtime.
//! - **`prefix = "..."`.** Deliberately not implemented, and the error says
//!   why rather than pretending the option does not exist. Stripping a prefix
//!   means rebuilding the row under different column names before handing it
//!   to the nested `FromRow`; the nested impl then reports failures against
//!   the *stripped* names ("no column \"id\"" when the result set says
//!   "author_id"), and the available-columns list in the error is the stripped
//!   set too. An honest prefix needs a prefix-aware view inside
//!   `keelson_exec::Row`, which is a change to the execution layer, not to a
//!   macro. Until then: `flatten` plus `rename` on the nested fields is
//!   explicit, exact, and reports real column names.
//!
//! # Getting at the derives
//!
//! They are re-exported by keelson-core behind its `macros` feature, which is
//! how a user reaches them:
//!
//! ```toml
//! keelson-core = { version = "…", features = ["macros"] }
//! ```
//!
//! `use keelson_core::Bind;` then imports the *derive*; `keelson_exec::Bind`
//! is the *trait*. Importing both is fine — they live in different namespaces
//! — and the trait is rarely named directly, since it is a blanket alias.
//!
//! # What the generated code depends on
//!
//! Nothing is imported into your scope, and nothing you write must be: every
//! path the expansion names is absolute.
//!
//! - `#[derive(Bind)]` names only `::keelson_core` (`ToValue`, `FromValue`,
//!   `Value`, `Error`).
//! - `#[derive(FromRow)]` names only `::keelson_exec` (`FromRow`, `Row`,
//!   `ExecError`) — plus `::keelson_core` in the one case where a bound must
//!   be written out: a *generic* struct, whose emitted `where` clause says
//!   `FieldTy: ::keelson_core::FromValue`. A generic `FromRow` struct
//!   therefore needs keelson-core in its dependencies; a non-generic one does
//!   not.
//!
//! Both crates are dependencies you already have — `FromRow` cannot exist
//! without keelson-exec, and keelson-exec depends on keelson-core.

#![warn(missing_docs)]

mod attr;
mod bind;
mod from_row;
mod sql;

use proc_macro::TokenStream;
use syn::{DeriveInput, parse_macro_input};

/// Implement `keelson_core::ToValue` and `keelson_core::FromValue` for a
/// newtype by delegating to its single field — the pair
/// `keelson_exec::Bind` requires, and so the pair a keelson-gen column
/// override must satisfy.
///
/// ```
/// use keelson_core::{Bind, FromValue as _, ToValue as _, Value};
///
/// #[derive(Debug, PartialEq, Bind)]
/// struct UserId(i64);
///
/// const _: () = keelson_exec::assert_bind::<UserId>();
/// assert_eq!(UserId(7).to_value(), Value::I64(7));
/// assert_eq!(UserId::from_value(Value::I64(7)).unwrap(), UserId(7));
/// ```
///
/// Single-field structs only — tuple or named, generic or not. Multi-field
/// structs, enums, unions and unit structs are compile errors naming the
/// restriction; see the crate documentation for the reasoning.
#[proc_macro_derive(Bind, attributes(keelson))]
pub fn derive_bind(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    bind::derive(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Implement `keelson_exec::FromRow` by reading one column per field, by
/// name.
///
/// `#[keelson(rename = "column")]` reads a differently named column;
/// `#[keelson(flatten)]` reads a nested struct out of the same row. Named
/// fields only. See the crate documentation for the full list of what is
/// refused and why.
#[proc_macro_derive(FromRow, attributes(keelson))]
pub fn derive_from_row(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    from_row::derive(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// The scanner behind each dialect's `sql!`. Not called directly: the dialect
/// crate's `sql!` forwards to it with its own `raw_query` as the first
/// argument, which is what makes `keelson_sqlite::sql!("…")` know its dialect.
///
/// ```text
/// sql_with!(keelson_sqlite::raw_query, "SELECT … WHERE id = {user_id}")
/// //  =>   keelson_sqlite::raw_query("SELECT … WHERE id = ?").bind(user_id)
/// ```
///
/// See the `sql` module's documentation for the grammar and for what the
/// rewriting is worth.
#[doc(hidden)]
#[proc_macro]
pub fn sql_with(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as sql::Input);
    sql::expand(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
