//! `#[derive(Bind)]` — the newtype half.
//!
//! Emits [`ToValue`] and [`FromValue`] by delegating to the single inner
//! field. Those two are exactly what `keelson_exec::Bind` is a blanket alias
//! for, so a derived newtype satisfies the bound generated code asserts.

use proc_macro2::TokenStream;
use quote::quote;
use syn::spanned::Spanned as _;
use syn::{Data, DeriveInput, Error, Index, Result, Type};

use crate::attr::{combine, reject_lifetimes, reject_options};

pub(crate) fn derive(input: DeriveInput) -> Result<TokenStream> {
    let mut errors: Option<Error> = None;

    if let Err(e) = reject_options(&input.attrs, "the struct") {
        combine(&mut errors, e);
    }

    let named = input.ident.to_string();
    if let Err(e) = reject_lifetimes(&input.generics, |lt| {
        format!(
            "`#[derive(Bind)]` cannot bind a borrowed type, and `{named}` has the lifetime \
             parameter `{}`. `FromValue` builds an owned value out of a `Value` — the row it \
             came from is gone by then — so a borrowing type could never read back, and the \
             impl this would emit could never apply. Wrap owned data (`String`, `Vec<u8>`, …); \
             no public keelson type carries a lifetime, for the same reason",
            lt.lifetime
        )
    }) {
        combine(&mut errors, e);
    }

    let field = match single_field(&input) {
        Ok(f) => Some(f),
        Err(e) => {
            combine(&mut errors, e);
            None
        }
    };

    if let Some(f) = &field
        && let Err(e) = reject_options(f.attrs, "the field")
    {
        combine(&mut errors, e);
    }

    if let Some(e) = errors {
        return Err(e);
    }
    let Field {
        member,
        ty,
        attrs: _,
    } = field.expect("no error means a field");

    let name = &input.ident;
    let mut generics = input.generics.clone();
    // A concrete inner type needs no bound: the failure then lands on the
    // field's own type, which is where a user can fix it. A generic one does,
    // or the compiler's "consider restricting" suggestion points into
    // generated code nobody can edit.
    if !generics.params.is_empty() {
        generics
            .make_where_clause()
            .predicates
            .push(syn::parse_quote!(#ty: ::keelson_core::ToValue + ::keelson_core::FromValue));
    }
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let construct = match &member {
        syn::Member::Named(f) => quote!(#name { #f: inner }),
        syn::Member::Unnamed(_) => quote!(#name(inner)),
    };

    Ok(quote! {
        #[automatically_derived]
        impl #impl_generics ::keelson_core::ToValue for #name #ty_generics #where_clause {
            fn to_value(self) -> ::keelson_core::Value {
                <#ty as ::keelson_core::ToValue>::to_value(self.#member)
            }
        }

        #[automatically_derived]
        impl #impl_generics ::keelson_core::FromValue for #name #ty_generics #where_clause {
            fn from_value(
                value: ::keelson_core::Value,
            ) -> ::core::result::Result<Self, ::keelson_core::Error> {
                <#ty as ::keelson_core::FromValue>::from_value(value)
                    .map(|inner| #construct)
            }
        }
    })
}

struct Field<'a> {
    member: syn::Member,
    ty: &'a Type,
    attrs: &'a [syn::Attribute],
}

/// The one field a newtype has — or the reason this type is not a newtype.
fn single_field(input: &DeriveInput) -> Result<Field<'_>> {
    let name = &input.ident;

    let fields = match &input.data {
        Data::Struct(s) => &s.fields,
        Data::Enum(_) => {
            return Err(Error::new_spanned(
                name,
                format!(
                    "`#[derive(Bind)]` supports single-field newtype structs only, and `{name}` \
                     is an enum. A column holds one scalar, so an enum first needs a database \
                     representation chosen deliberately — text or integer, which spelling per \
                     variant, and what an unrecognised value from the database means. Write \
                     `impl keelson_core::ToValue for {name}` and `impl keelson_core::FromValue \
                     for {name}` (about ten lines, and the unknown-variant case becomes \
                     `keelson_core::Error::type_mismatch`), or derive `Bind` on a newtype over \
                     the representation and convert at the edges"
                ),
            ));
        }
        Data::Union(_) => {
            return Err(Error::new_spanned(
                name,
                format!(
                    "`#[derive(Bind)]` supports single-field newtype structs only, and `{name}` \
                     is a union. Reading a union field is unsafe and which field is live is not \
                     knowable here, so there is nothing this derive could honestly emit"
                ),
            ));
        }
    };

    let mut iter = fields.iter();
    let Some(first) = iter.next() else {
        return Err(Error::new_spanned(
            name,
            format!(
                "`#[derive(Bind)]` needs exactly one field to bind, and `{name}` has none. Give \
                 it the value it wraps: `struct {name}(i64);`"
            ),
        ));
    };

    if let Some(extra) = iter.next() {
        let count = fields.len();
        let mut e = Error::new(
            extra.span(),
            format!(
                "`#[derive(Bind)]` binds one column, so it needs exactly one field, and `{name}` \
                 has {count}. Bind each part as its own column — a struct of them is a row, not \
                 a value, and `#[derive(FromRow)]` maps that — or, if the parts really are one \
                 column, encode them by hand in `impl keelson_core::ToValue for {name}` and the \
                 matching `FromValue`"
            ),
        );
        e.combine(Error::new(
            first.span(),
            "the first field is here".to_owned(),
        ));
        return Err(e);
    }

    let member = match &first.ident {
        Some(id) => syn::Member::Named(id.clone()),
        None => syn::Member::Unnamed(Index {
            index: 0,
            span: first.span(),
        }),
    };

    Ok(Field {
        member,
        ty: &first.ty,
        attrs: &first.attrs,
    })
}
