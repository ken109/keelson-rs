//! `#[derive(FromRow)]` — the row-mapping half.
//!
//! Emits exactly the hand-written shape `keelson_exec::FromRow` documents and
//! keelson-gen emits: one `row.take("column")?` per field, by name, consuming
//! the value so `String`s and blobs move instead of cloning.

use proc_macro2::TokenStream;
use quote::quote;
use syn::spanned::Spanned as _;
use syn::{Data, DeriveInput, Error, Fields, Result};

use crate::attr::{combine, field_options, reject_lifetimes};

pub(crate) fn derive(input: DeriveInput) -> Result<TokenStream> {
    let mut errors: Option<Error> = None;

    let named_type = input.ident.to_string();
    if let Err(e) = reject_lifetimes(&input.generics, |lt| {
        format!(
            "`#[derive(FromRow)]` cannot map onto a borrowed struct, and `{named_type}` has the \
             lifetime parameter `{}`. A row is decoded into owned `Value`s and every field is \
             taken out of it by value, so a field borrowing from the row could not outlive the \
             mapping. Own the fields (`String`, `Vec<u8>`, …)",
            lt.lifetime
        )
    }) {
        combine(&mut errors, e);
    }

    // Nothing is understood on the struct itself; say so rather than ignoring
    // it. (`reject_options`' message is written for `Bind`, so the container
    // case gets its own.)
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("keelson")) {
        combine(
            &mut errors,
            Error::new_spanned(
                attr,
                "`#[derive(FromRow)]` takes no options on the struct itself — `rename` and \
                 `flatten` describe one field, so they go on the field",
            ),
        );
    }

    let named = match named_fields(&input) {
        Ok(f) => f,
        Err(e) => {
            combine(&mut errors, e);
            return Err(errors.expect("just combined"));
        }
    };

    // field ident -> the column it reads, so a collision can name both.
    let mut taken: Vec<(String, syn::Ident, proc_macro2::Span)> = Vec::new();
    let mut inits = Vec::new();

    for field in named {
        let ident = field.ident.clone().expect("named fields");
        let ty = &field.ty;

        let opts = match field_options(&field.attrs) {
            Ok(o) => o,
            Err(e) => {
                combine(&mut errors, e);
                continue;
            }
        };

        match (opts.flatten, opts.rename) {
            (Some(flatten), Some((_, rename))) => {
                let mut e = Error::new(
                    rename,
                    "`rename` names one column and `flatten` reads many, so a field cannot have \
                     both. Drop `rename` here, and put it on the fields of the flattened struct \
                     if their names differ from the columns",
                );
                e.combine(Error::new(flatten, "`flatten` is here"));
                combine(&mut errors, e);
            }
            (Some(_), None) => {
                // A flattened field reads the same row, so its own `FromRow`
                // impl does the work and the columns it consumes are its own
                // business.
                inits.push(quote! {
                    #ident: <#ty as ::keelson_exec::FromRow>::from_row(row)?
                });
            }
            (None, rename) => {
                let (column, span) = match rename {
                    Some((c, span)) => (c, span),
                    // A raw identifier reads the column it is named after:
                    // `r#type` is the field `type` is spelled as, and no
                    // database has a column called `r#type`.
                    None => (
                        ident.to_string().trim_start_matches("r#").to_owned(),
                        ident.span(),
                    ),
                };
                if let Some((_, first, first_span)) =
                    taken.iter().find(|(c, _, _)| *c == column).cloned()
                {
                    let mut e = Error::new(
                        span,
                        format!(
                            "two fields read the column \"{column}\": `{first}` and `{ident}`. \
                             Reading a column consumes it — the value moves out of the row and \
                             NULL is left behind — so the second field would always decode \
                             NULL. Rename one of them, or read the column once and copy the \
                             value after mapping"
                        ),
                    );
                    e.combine(Error::new(
                        first_span,
                        format!("`{first}` already reads \"{column}\""),
                    ));
                    combine(&mut errors, e);
                    continue;
                }
                taken.push((column.clone(), ident.clone(), span));
                inits.push(quote! {
                    #ident: ::keelson_exec::Row::take::<#ty>(row, #column)?
                });
            }
        }
    }

    if let Some(e) = errors {
        return Err(e);
    }

    let name = &input.ident;
    let mut generics = input.generics.clone();
    if !generics.params.is_empty() {
        // Bounds on the field types, not on the parameters: `Option<T>` needs
        // `Option<T>: FromValue`, which core's blanket impl already gives for
        // `T: FromValue`, and stating it this way is exact.
        let where_clause = generics.make_where_clause();
        let mut seen: Vec<String> = Vec::new();
        for field in named {
            let ty = &field.ty;
            let opts = field_options(&field.attrs).unwrap_or_default();
            let predicate: syn::WherePredicate = if opts.flatten.is_some() {
                syn::parse_quote!(#ty: ::keelson_exec::FromRow)
            } else {
                syn::parse_quote!(#ty: ::keelson_core::FromValue)
            };
            let key = quote!(#predicate).to_string();
            if !seen.contains(&key) {
                seen.push(key);
                where_clause.predicates.push(predicate);
            }
        }
    }
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    Ok(quote! {
        #[automatically_derived]
        impl #impl_generics ::keelson_exec::FromRow for #name #ty_generics #where_clause {
            fn from_row(
                row: &mut ::keelson_exec::Row,
            ) -> ::core::result::Result<Self, ::keelson_exec::ExecError> {
                ::core::result::Result::Ok(#name {
                    #(#inits,)*
                })
            }
        }
    })
}

/// The struct's named fields — or the reason this type has none.
fn named_fields(
    input: &DeriveInput,
) -> Result<&syn::punctuated::Punctuated<syn::Field, syn::Token![,]>> {
    let name = &input.ident;

    let fields = match &input.data {
        Data::Struct(s) => &s.fields,
        Data::Enum(_) => {
            return Err(Error::new_spanned(
                name,
                format!(
                    "`#[derive(FromRow)]` maps a row onto a struct with named fields, and \
                     `{name}` is an enum. Which variant a row is depends on a discriminator \
                     only you can name, so read that column first and build the variant in a \
                     hand-written `impl keelson_exec::FromRow for {name}`"
                ),
            ));
        }
        Data::Union(_) => {
            return Err(Error::new_spanned(
                name,
                format!(
                    "`#[derive(FromRow)]` maps a row onto a struct with named fields, and \
                     `{name}` is a union"
                ),
            ));
        }
    };

    match fields {
        Fields::Named(f) => Ok(&f.named),
        Fields::Unnamed(f) => {
            let arity = f.unnamed.len();
            let example = f
                .unnamed
                .iter()
                .map(|f| quote!(#f).to_string())
                .collect::<Vec<_>>()
                .join(", ");
            Err(Error::new(
                f.span(),
                format!(
                    "`#[derive(FromRow)]` matches fields to columns by name, and `{name}` has \
                     unnamed fields. Give them names, or drop the struct: tuples of arity \
                     {arity} already implement `FromRow` positionally, so `fetch_all::<({example})>` \
                     reads the same row"
                ),
            ))
        }
        Fields::Unit => Err(Error::new_spanned(
            name,
            format!(
                "`#[derive(FromRow)]` needs fields to map columns onto, and `{name}` has none. A \
                 mapper that reads nothing from the row would hide a `SELECT` returning columns \
                 nobody looks at"
            ),
        )),
    }
}
