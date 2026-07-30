//! `#[keelson(...)]` parsing, shared by both derives.
//!
//! One helper attribute serves every keelson derive, the way `#[serde(...)]`
//! does — so this module owns the whole vocabulary and every rejection
//! message. The rule the messages follow: name the offending key, say what is
//! accepted instead, and point the span at the key rather than at the item.

use proc_macro2::Span;
use syn::spanned::Spanned as _;
use syn::{Attribute, Error, Lit, LitStr, Meta, Result, Token};

/// The attribute every keelson derive reads.
const KEELSON: &str = "keelson";

/// What a `#[derive(FromRow)]` field can say about itself.
#[derive(Debug, Default)]
pub(crate) struct FieldOptions {
    /// `rename = "..."`: the column to read instead of the field's own name.
    pub(crate) rename: Option<(String, Span)>,
    /// `flatten`: read the field's own type out of the same row.
    pub(crate) flatten: Option<Span>,
}

/// Collect the `#[keelson(...)]` options on one `FromRow` field.
///
/// Every attribute is visited even when an earlier one failed, so a field with
/// two mistakes reports both.
pub(crate) fn field_options(attrs: &[Attribute]) -> Result<FieldOptions> {
    let mut opts = FieldOptions::default();
    let mut errors: Option<Error> = None;

    for attr in attrs.iter().filter(|a| a.path().is_ident(KEELSON)) {
        if let Err(e) = one_attr(attr, &mut opts) {
            combine(&mut errors, e);
        }
    }

    match errors {
        Some(e) => Err(e),
        None => Ok(opts),
    }
}

fn one_attr(attr: &Attribute, opts: &mut FieldOptions) -> Result<()> {
    require_list(attr)?;

    attr.parse_nested_meta(|meta| {
        let span = meta.path.span();

        if meta.path.is_ident("rename") {
            if !meta.input.peek(Token![=]) {
                return Err(Error::new(
                    span,
                    "`rename` needs the column name to read: `#[keelson(rename = \"user_id\")]`",
                ));
            }
            let name = match meta.value()?.parse::<Lit>()? {
                Lit::Str(s) => s,
                other => {
                    return Err(Error::new(
                        other.span(),
                        "`rename` takes a string literal — the column name as the database \
                         spells it, e.g. `#[keelson(rename = \"user_id\")]`",
                    ));
                }
            };
            check_rename(&name)?;
            if let Some((_, first)) = opts.rename.replace((name.value(), span)) {
                let mut e = Error::new(span, "`rename` is given twice on this field; keep one");
                e.combine(Error::new(first, "the first `rename` is here"));
                return Err(e);
            }
        } else if meta.path.is_ident("flatten") {
            if meta.input.peek(Token![=]) {
                return Err(Error::new(
                    span,
                    "`flatten` takes no value — it reads the field's own type out of the same \
                     row. Write `#[keelson(flatten)]`",
                ));
            }
            opts.flatten = Some(span);
        } else if meta.path.is_ident("prefix") {
            return Err(Error::new(span, PREFIX));
        } else {
            let key = quote_path(&meta.path);
            return Err(Error::new(
                span,
                format!(
                    "unknown keelson option `{key}`. `#[derive(FromRow)]` understands \
                     `rename = \"column\"` and `flatten` on a field, and nothing on the struct \
                     itself"
                ),
            ));
        }
        Ok(())
    })
}

/// The one option this crate names and refuses, so the refusal is discoverable
/// from the place a user would reach for it.
const PREFIX: &str = "`prefix` is not supported. Stripping a prefix means rebuilding the row \
     under different column names, and the failure a user then sees names the stripped column \
     (\"no column \\\"id\\\"\") rather than the real one (\"author_id\") — a worse error than \
     the one it saves. Use `#[keelson(flatten)]` with a nested struct whose fields carry \
     `#[keelson(rename = \"author_id\")]`, which reads the same row and reports real column \
     names";

/// `#[derive(Bind)]` accepts no options at all; this is where it says so.
///
/// The whole `keelson` namespace is refused rather than only the unknown keys:
/// `rename` and `flatten` describe a row of columns, and a newtype is one
/// column. A type that derives both `Bind` and `FromRow` is fine — just keep
/// its field attribute-free.
pub(crate) fn reject_options(attrs: &[Attribute], what: &str) -> Result<()> {
    let mut errors: Option<Error> = None;

    for attr in attrs.iter().filter(|a| a.path().is_ident(KEELSON)) {
        let e = match require_list(attr) {
            Err(e) => e,
            Ok(()) => {
                let mut found: Option<Error> = None;
                let _ = attr.parse_nested_meta(|meta| {
                    let key = quote_path(&meta.path);
                    combine(
                        &mut found,
                        Error::new(
                            meta.path.span(),
                            format!(
                                "`#[derive(Bind)]` takes no options, so `{key}` on {what} does \
                                 nothing. A newtype is a single column: `rename` and `flatten` \
                                 are `#[derive(FromRow)]` options and belong on a struct that \
                                 maps a whole row"
                            ),
                        ),
                    );
                    // Swallow the value, if any, so the walk reaches every key.
                    if meta.input.peek(Token![=]) {
                        let _ = meta.value().and_then(|v| v.parse::<Lit>());
                    }
                    Ok(())
                });
                match found {
                    Some(e) => e,
                    None => Error::new_spanned(attr, "`#[derive(Bind)]` takes no options"),
                }
            }
        };
        combine(&mut errors, e);
    }

    match errors {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Refuse a type that borrows, with a message the caller writes.
///
/// Worth a rejection of its own rather than leaving it to the compiler: both
/// derives put the field's bound in a `where` clause, and an impl whose `where`
/// clause can never hold is *accepted* by rustc — it simply never applies. So
/// a borrowed type would derive cleanly and then fail at the first call site,
/// which is precisely the "inference swamp at some distant call site" the
/// override mechanism exists to prevent. It cannot succeed either way:
/// `FromValue` builds an owned value out of a `Value`, and every keelson
/// public type is lifetime-free by design.
pub(crate) fn reject_lifetimes(
    generics: &syn::Generics,
    message: impl Fn(&syn::LifetimeParam) -> String,
) -> Result<()> {
    let mut errors: Option<Error> = None;
    for lt in generics.lifetimes() {
        combine(&mut errors, Error::new(lt.lifetime.span(), message(lt)));
    }
    match errors {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// `#[keelson]` and `#[keelson = "..."]` are not shapes any derive accepts.
fn require_list(attr: &Attribute) -> Result<()> {
    match attr.meta {
        Meta::List(_) => Ok(()),
        _ => Err(Error::new_spanned(
            attr,
            "expected a list of options: `#[keelson(...)]`, e.g. \
             `#[keelson(rename = \"user_id\")]`",
        )),
    }
}

fn check_rename(name: &LitStr) -> Result<()> {
    if name.value().is_empty() {
        return Err(Error::new(
            name.span(),
            "`rename` cannot be empty — no result set has a column with no name. Drop the \
             attribute to read the column named after the field",
        ));
    }
    Ok(())
}

/// A path as the user wrote it, for a message: `rename`, `foo::bar`.
fn quote_path(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

/// Accumulate errors so one derive reports every problem it found, not the
/// first.
pub(crate) fn combine(slot: &mut Option<Error>, e: Error) {
    match slot {
        Some(existing) => existing.combine(e),
        None => *slot = Some(e),
    }
}
