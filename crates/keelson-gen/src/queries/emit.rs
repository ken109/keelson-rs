//! One analysed query file → one generated `.rs` module, carrying both faces.
//!
//! The generated module holds nothing but `&'static str` slices of the
//! `include_str!`ed query file, so the SQL an application ships is the SQL its
//! author wrote, byte for byte. A `const _: () = assert!(SOURCE.len() == …)`
//! at the top of every module turns "the `.sql` file changed but nobody
//! re-ran the generator" into a compile error rather than an off-by-a-clause
//! slice.

use proc_macro2::{Ident, Literal, TokenStream};
use quote::{format_ident, quote};

use crate::config::Dialect;
use crate::error::{GenError, Result};
use crate::names::{ident, pascal};
use crate::queries::ir::{Analysis, Nesting, OutputColumn, Part, Span};
use crate::queries::spec::{Cardinality, QueryFile};

/// The dialect's contribution: the crate its statement types come from, the
/// placeholder spelling, and the `Dialect` value a query hands back.
pub(crate) struct Dial {
    pub(crate) krate: TokenStream,
    pub(crate) dialect: TokenStream,
    pub(crate) placeholder: &'static str,
}

impl Dial {
    pub(crate) fn new(dialect: Dialect) -> Result<Dial> {
        Ok(match dialect {
            Dialect::Psql => Dial {
                krate: quote!(keelson_psql),
                dialect: quote!(keelson_psql::Psql),
                placeholder: "$n",
            },
            Dialect::Sqlite => Dial {
                krate: quote!(keelson_sqlite),
                dialect: quote!(keelson_sqlite::Sqlite),
                placeholder: "?n",
            },
            Dialect::Mysql => return Err(mysql_refusal()),
        })
    }
}

/// The recorded refusal, in one place so the pipeline and the analyser say the
/// same thing.
///
/// This is *not* "not implemented yet". Model emission for MySQL reads the
/// catalog, which MySQL has; typing a hand-written statement needs a parse
/// tree, and MySQL publishes none — `sqlparser` is a generic SQL parser rather
/// than MySQL's own, and the server will not describe a statement's result
/// columns without executing it. An inferred type nobody can trust is worse
/// than no code.
pub(crate) fn mysql_refusal() -> GenError {
    GenError::Unsupported(
        "MySQL has no trustworthy static parse tree to infer a hand-written statement's \
         result types from (no libpg_query equivalent, and the server will not describe a \
         statement without executing it), so `[queries]` generation is refused for it"
            .to_owned(),
    )
}

/// A nested group in the row struct, in first-appearance order.
struct Group<'a> {
    name: String,
    to_many: bool,
    columns: Vec<&'a OutputColumn>,
}

/// Whether a to-one group is `Option<Nested>`: every one of its columns owes
/// its nullability to the outer join, so the whole side is either there or not.
fn group_optional(g: &Group<'_>) -> bool {
    !g.to_many && g.columns.iter().all(|c| c.outer_join)
}

fn groups<'a>(outputs: &'a [OutputColumn]) -> Vec<Group<'a>> {
    let mut out: Vec<Group<'a>> = Vec::new();
    for c in outputs {
        let (name, to_many) = match &c.nesting {
            Nesting::Flat => continue,
            Nesting::ToOne(n) => (n.clone(), false),
            Nesting::ToMany(n) => (n.clone(), true),
        };
        match out.iter_mut().find(|g| g.name == name) {
            Some(g) => g.columns.push(c),
            None => out.push(Group {
                name,
                to_many,
                columns: vec![c],
            }),
        }
    }
    out
}

/// The Rust types this emitter knows are `Copy`, so a parameter is read
/// rather than cloned on its way into a [`keelson_core::Value`]. The same list
/// the model emitter keeps, and for the same reason: a `.clone()` on an `i32`
/// is a lint in the *reader's* crate, and generated code must not create work
/// for its reader.
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

/// Types known *not* to be `Copy`: cloning them is never a `clone_on_copy`
/// candidate, so no `allow` is needed.
const KNOWN_CLONE: &[&str] = &["String", "Vec<u8>", "serde_json::Value"];

fn ty(rust_type: &str, what: &str) -> Result<syn::Type> {
    syn::parse_str(rust_type)
        .map_err(|e| GenError::Config(format!("{what}: `{rust_type}` is not a Rust type: {e}")))
}

/// `T` or `Option<T>`.
fn field_type(rust_type: &str, nullable: bool, what: &str) -> Result<TokenStream> {
    let t = ty(rust_type, what)?;
    Ok(if nullable {
        quote!(Option<#t>)
    } else {
        quote!(#t)
    })
}

/// The literal slice expression for one span: `&SOURCE[12usize..40usize]`.
fn slice(span: Span) -> TokenStream {
    let start = Literal::usize_suffixed(span.start);
    let end = Literal::usize_suffixed(span.end);
    quote!(&SOURCE[#start..#end])
}

/// The `w.push_str(..)` / `w.push_arg(..)` body for a run of parts.
fn write_parts(parts: &[Part], arg: impl Fn(usize) -> TokenStream) -> TokenStream {
    let stmts = parts.iter().map(|p| match p {
        Part::Sql(span) => {
            let s = slice(*span);
            quote!(w.push_str(#s);)
        }
        Part::Arg(i) => {
            let a = arg(*i);
            quote!(w.push_arg(#a);)
        }
    });
    quote!(#(#stmts)*)
}

fn doc(lines: &[String]) -> TokenStream {
    let attrs = lines.iter().map(|l| {
        let l = format!(" {l}");
        quote!(#[doc = #l])
    });
    quote!(#(#attrs)*)
}

/// Render one query file's module.
pub(crate) fn module(
    file: &QueryFile,
    analyses: &[Analysis],
    include_path: &str,
    dial: &Dial,
) -> Result<TokenStream> {
    let source_len = Literal::usize_suffixed(file.source.len());
    let stale = format!(
        "{} changed after it was generated from; re-run keelson-gen",
        file.path.display()
    );
    let module_doc = format!(
        " Generated from `{}`. Each query has two faces: a query object that runs \
         the file's own SQL, and a mod that merges the same clauses into a host \
         statement. Both slice the text below, so they can never disagree.",
        file.path.display()
    );

    let mut items = Vec::new();
    for a in analyses {
        items.push(query_items(a, dial)?);
    }

    Ok(quote! {
        #![doc = #module_doc]

        /// The query file, verbatim. Every span below indexes it.
        const SOURCE: &str = include_str!(#include_path);
        const _: () = assert!(SOURCE.len() == #source_len, #stale);

        #(#items)*
    })
}

fn query_items(a: &Analysis, dial: &Dial) -> Result<TokenStream> {
    let name = &a.spec.name;
    let base = pascal(name);
    let params_ty = format_ident!("{}Params", base);
    let row_ty = format_ident!("{}Row", base);
    let query_ty = format_ident!("{}Query", base);
    let fn_name = ident(name);
    let query_fn = format_ident!("{}_query", name);
    let mod_fn_name = format_ident!("{}_mod", name);

    let user_doc = doc(&a.spec.doc);
    let params = params_struct(a, &params_ty)?;
    let rows = if a.spec.cardinality.returns_rows() {
        row_structs(a, &base, &row_ty)?
    } else {
        quote!()
    };
    let query = query_struct(a, dial, &params_ty, &query_ty, &query_fn, &user_doc);
    let verb = verb_fn(
        a, &params_ty, &row_ty, &query_ty, &fn_name, &query_fn, &user_doc,
    )?;
    let mods = mod_face(a, dial, &params_ty, &mod_fn_name, name)?;

    Ok(quote! {
        #params
        #rows
        #query
        #verb
        #mods
    })
}

// --- parameters -----------------------------------------------------------

fn params_struct(a: &Analysis, params_ty: &Ident) -> Result<TokenStream> {
    let name = &a.spec.name;
    let fields = a
        .params
        .iter()
        .map(|p| {
            let f = ident(&p.name);
            let t = ty(
                &p.rust_type,
                &format!("query `{name}` parameter `{}`", p.name),
            )?;
            let d = format!(
                " `{}{}` — {}.",
                if p.rust_type.is_empty() { "" } else { "$" },
                p.number,
                rule_text(p.rule)
            );
            Ok(quote! {
                #[doc = #d]
                pub #f: #t,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let arg_names: Vec<Ident> = a.params.iter().map(|p| ident(&p.name)).collect();
    let arg_types = a
        .params
        .iter()
        .map(|p| ty(&p.rust_type, "parameter"))
        .collect::<Result<Vec<_>>>()?;
    let to_values = a.params.iter().map(|p| {
        let f = ident(&p.name);
        if KNOWN_COPY.contains(&p.rust_type.as_str()) {
            quote!(keelson_core::ToValue::to_value(self.#f))
        } else {
            quote!(keelson_core::ToValue::to_value(self.#f.clone()))
        }
    });
    // A `-- param:` type the emitter has never heard of may or may not be
    // `Copy`; it is cloned, and the allow keeps that from lining the reader's
    // build with warnings.
    let clone_allow = a
        .params
        .iter()
        .any(|p| {
            !KNOWN_COPY.contains(&p.rust_type.as_str())
                && !KNOWN_CLONE.contains(&p.rust_type.as_str())
        })
        .then(|| quote!(#[allow(clippy::clone_on_copy)]));

    let doc_text = format!(" Parameters of `{name}`.");
    // A parameterless query's `new()` takes no arguments, which clippy's
    // `new_without_default` (on by default, and denied in plenty of builds)
    // asks for a `Default` beside. The struct has no fields in that case, so
    // deriving it is both trivially correct and the smallest answer. With
    // fields it is deliberately *not* derived: a parameter type need not
    // implement `Default`, and an all-zeroes parameter set is not a
    // meaningful value anyway.
    let derives = if arg_types.is_empty() {
        quote!(#[derive(Debug, Clone, PartialEq, Default)])
    } else {
        quote!(#[derive(Debug, Clone, PartialEq)])
    };
    let from_impl = match arg_types.as_slice() {
        // A query with no placeholders is still called with *something*; `()`
        // is what reads best at the call site.
        [] => quote! {
            impl From<()> for #params_ty {
                fn from((): ()) -> Self {
                    #params_ty::new()
                }
            }
        },
        [one] => quote! {
            impl From<#one> for #params_ty {
                fn from(v: #one) -> Self {
                    #params_ty::new(v)
                }
            }
        },
        many => {
            let idx = (0..many.len()).map(Literal::usize_unsuffixed);
            quote! {
                impl From<(#(#many,)*)> for #params_ty {
                    fn from(v: (#(#many,)*)) -> Self {
                        #params_ty::new(#(v.#idx),*)
                    }
                }
            }
        }
    };

    Ok(quote! {
        #[doc = #doc_text]
        #derives
        pub struct #params_ty {
            #(#fields)*
        }

        impl #params_ty {
            /// The parameters in placeholder order.
            pub fn new(#(#arg_names: #arg_types),*) -> Self {
                #params_ty { #(#arg_names),* }
            }

            /// The bound arguments, in placeholder order.
            ///
            /// Public deliberately: a query with no placeholders never calls
            /// this, and a *private* method nothing calls is a `dead_code`
            /// warning in a generated file the application cannot edit.
            #clone_allow
            pub fn args(&self) -> Vec<keelson_core::Value> {
                vec![#(#to_values),*]
            }
        }

        #from_impl
    })
}

fn rule_text(rule: &str) -> &'static str {
    match rule {
        "P1" => "type taken from the column it is compared with",
        "P2" => "type taken from its explicit cast",
        "P3" => "a row count",
        "A2" => "type given by a `-- param:` annotation",
        _ => "type inferred from context",
    }
}

// --- rows -----------------------------------------------------------------

fn row_structs(a: &Analysis, base: &str, row_ty: &Ident) -> Result<TokenStream> {
    let name = &a.spec.name;
    let gs = groups(&a.outputs);

    let mut nested_defs = Vec::new();
    for g in &gs {
        let nested_ty = format_ident!("{}{}", base, pascal(&g.name));
        let optional = group_optional(g);
        // Inside a group the join can make absent, a column goes back to its
        // own nullability: the `Option` that says "no joined row" is the
        // group's, and repeating it per field would say NULL twice.
        let hoisted = optional || g.to_many;
        let fields = g
            .columns
            .iter()
            .map(|c| {
                let f = ident(&c.field);
                let nullable = if hoisted {
                    c.inner_nullable
                } else {
                    c.nullable
                };
                let t = field_type(
                    &c.rust_type,
                    nullable,
                    &format!("query `{name}` column `{}`", c.name),
                )?;
                let d = format!(" `{}` — {}.", c.name, rule_doc(c, nullable));
                Ok(quote! {
                    #[doc = #d]
                    pub #f: #t,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let d = format!(" `{name}`'s nested `{}`.", g.name);
        nested_defs.push(quote! {
            #[doc = #d]
            #[derive(Debug, Clone, PartialEq)]
            pub struct #nested_ty {
                #(#fields)*
            }
        });
    }

    let mut fields = Vec::new();
    let mut decodes = Vec::new();
    let mut seen_groups: Vec<String> = Vec::new();
    for c in &a.outputs {
        match &c.nesting {
            Nesting::Flat => {
                let f = ident(&c.field);
                let t = field_type(
                    &c.rust_type,
                    c.nullable,
                    &format!("query `{name}` column `{}`", c.name),
                )?;
                let d = format!(" `{}` — {}.", c.name, rule_doc(c, c.nullable));
                fields.push(quote! {
                    #[doc = #d]
                    pub #f: #t,
                });
                let key = Literal::string(&c.name);
                decodes.push(quote!(#f: row.take(#key)?,));
            }
            Nesting::ToOne(n) | Nesting::ToMany(n) => {
                if seen_groups.contains(n) {
                    continue;
                }
                seen_groups.push(n.clone());
                let g = gs
                    .iter()
                    .find(|g| g.name == *n)
                    .expect("group built from the same outputs");
                let nested_ty = format_ident!("{}{}", base, pascal(n));
                let f = ident(n);
                let optional = group_optional(g);
                let (field_ty, decode) = nested_field(g, &nested_ty, optional)?;
                let d = if g.to_many {
                    format!(" `{n}.*` — a to-many nested group, folded from the result rows.")
                } else if optional {
                    format!(
                        " `{n}__*` — a to-one nested group, `None` when the outer join found no row."
                    )
                } else {
                    format!(" `{n}__*` — a to-one nested group.")
                };
                fields.push(quote! {
                    #[doc = #d]
                    pub #f: #field_ty,
                });
                decodes.push(quote!(#f: #decode,));
            }
        }
    }

    let doc_text = format!(" One row of `{name}`.");
    Ok(quote! {
        #(#nested_defs)*

        #[doc = #doc_text]
        #[derive(Debug, Clone, PartialEq)]
        pub struct #row_ty {
            #(#fields)*
        }

        impl keelson_exec::FromRow for #row_ty {
            fn from_row(
                row: &mut keelson_exec::Row,
            ) -> Result<Self, keelson_exec::ExecError> {
                Ok(#row_ty { #(#decodes)* })
            }
        }
    })
}

/// The nested field's type and the expression that decodes it.
///
/// A group the join can make absent is decoded through its own `NOT NULL`
/// columns: if any of them came back NULL there was no joined row, so the
/// whole side is `None`. A group with no `NOT NULL` column of its own has
/// nothing to witness absence with, and is taken to be present unless every
/// column is NULL — recorded here because it is the one place the shape is a
/// judgement rather than a deduction.
fn nested_field(
    g: &Group<'_>,
    nested_ty: &Ident,
    optional: bool,
) -> Result<(TokenStream, TokenStream)> {
    if !optional && !g.to_many {
        // Reached by an inner join: every column decodes at its own
        // nullability, straight out of the row.
        let plain = g.columns.iter().map(|c| {
            let f = ident(&c.field);
            let key = Literal::string(&c.name);
            quote!(#f: row.take(#key)?,)
        });
        return Ok((quote!(#nested_ty), quote!(#nested_ty { #(#plain)* })));
    }

    let mut lets = Vec::new();
    let mut witnesses = Vec::new();
    let mut inits = Vec::new();
    let mut any_some = Vec::new();
    for c in &g.columns {
        let f = ident(&c.field);
        let key = Literal::string(&c.name);
        let inner = ty(&c.rust_type, "nested column")?;
        lets.push(quote!(let #f: Option<#inner> = row.take(#key)?;));
        any_some.push(quote!(#f.is_some()));
        if c.inner_nullable {
            inits.push(quote!(#f,));
        } else {
            witnesses.push(f.clone());
            inits.push(quote!(#f,));
        }
    }

    let build = if witnesses.is_empty() {
        quote! {
            if #(#any_some)||* {
                Some(#nested_ty { #(#inits)* })
            } else {
                None
            }
        }
    } else {
        quote! {
            match (#(#witnesses,)*) {
                (#(Some(#witnesses),)*) => Some(#nested_ty { #(#inits)* }),
                _ => None,
            }
        }
    };
    let body = quote!({ #(#lets)* #build });
    if g.to_many {
        Ok((
            quote!(Vec<#nested_ty>),
            quote!(#body.into_iter().collect::<Vec<_>>()),
        ))
    } else {
        Ok((quote!(Option<#nested_ty>), body))
    }
}

fn rule_doc(c: &OutputColumn, nullable: bool) -> String {
    let null = if nullable { "nullable" } else { "never NULL" };
    format!("{null} by rule {}", c.rule)
}

// --- the query face -------------------------------------------------------

fn query_struct(
    a: &Analysis,
    dial: &Dial,
    params_ty: &Ident,
    query_ty: &Ident,
    query_fn: &Ident,
    user_doc: &TokenStream,
) -> TokenStream {
    let name = &a.spec.name;
    let dialect = &dial.dialect;
    let parts = a.statement_parts();
    let has_args = a.params.is_empty();
    let body = write_parts(&parts, |i| {
        let i = Literal::usize_unsuffixed(i);
        quote!(args[#i].clone())
    });
    let args_let = if has_args {
        quote!()
    } else {
        quote!(let args = self.params.args();)
    };
    let doc_text = format!(
        " `{name}` as a query object: the file's own SQL, run as written.\n\n\
         Placeholders are re-bound through the writer, so the statement composes \
         as a sub-select without re-numbering by hand."
    );
    let fn_doc = format!(" Build `{name}` without running it.");
    let query_type = quote!(keelson_core::QueryType::Select);

    quote! {
        #[doc = #doc_text]
        #[derive(Debug, Clone)]
        pub struct #query_ty {
            params: #params_ty,
        }

        impl #query_ty {
            /// The parameters this query was built with.
            pub fn params(&self) -> &#params_ty {
                &self.params
            }
        }

        impl keelson_core::Expression for #query_ty {
            fn write_sql(&self, w: &mut keelson_core::SqlWriter<'_>) {
                #args_let
                #body
            }
        }

        impl keelson_core::Query for #query_ty {
            fn query_type(&self) -> keelson_core::QueryType {
                #query_type
            }

            fn dialect(&self) -> &dyn keelson_core::Dialect {
                &#dialect
            }
        }

        impl<H, L, M> keelson_core::QueryExtensions<H, L, M> for #query_ty {}

        #user_doc
        #[doc = ""]
        #[doc = #fn_doc]
        pub fn #query_fn(params: impl Into<#params_ty>) -> #query_ty {
            #query_ty { params: params.into() }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn verb_fn(
    a: &Analysis,
    params_ty: &Ident,
    row_ty: &Ident,
    query_ty: &Ident,
    fn_name: &Ident,
    query_fn: &Ident,
    user_doc: &TokenStream,
) -> Result<TokenStream> {
    let name = &a.spec.name;
    let has_to_many = groups(&a.outputs).iter().any(|g| g.to_many);
    if has_to_many && a.spec.cardinality != Cardinality::Many {
        return Err(GenError::Config(format!(
            "query `{name}`: a to-many nested group needs `:many` — one row cannot carry a \
             folded collection"
        )));
    }

    let (ret, call) = match a.spec.cardinality {
        Cardinality::One => (quote!(#row_ty), quote!(fetch_one::<#row_ty>(db))),
        Cardinality::Optional => (
            quote!(Option<#row_ty>),
            quote!(fetch_optional::<#row_ty>(db)),
        ),
        Cardinality::Many => (quote!(Vec<#row_ty>), quote!(fetch_all::<#row_ty>(db))),
        Cardinality::Exec => (quote!(keelson_exec::ExecResult), quote!(execute(db))),
    };

    let fold = if has_to_many {
        let merge_fn = format_ident!("fold_{}", name);
        let mut compared: Vec<String> = Vec::new();
        let mut flat: Vec<TokenStream> = Vec::new();
        for c in &a.outputs {
            let field = match &c.nesting {
                Nesting::ToMany(_) => continue,
                Nesting::ToOne(n) => n.clone(),
                Nesting::Flat => c.field.clone(),
            };
            if compared.contains(&field) {
                continue;
            }
            compared.push(field.clone());
            let f = ident(&field);
            flat.push(quote!(kept.#f == row.#f));
        }
        let mut seen: Vec<String> = Vec::new();
        let mut extends = Vec::new();
        for c in &a.outputs {
            if let Nesting::ToMany(n) = &c.nesting {
                if seen.contains(n) {
                    continue;
                }
                seen.push(n.clone());
                let f = ident(n);
                extends.push(quote!(kept.#f.append(&mut row.#f);));
            }
        }
        let same = if flat.is_empty() {
            quote!(true)
        } else {
            quote!(#(#flat)&&*)
        };
        let d = format!(
            " Fold `{name}`'s flat result rows into their to-many groups.\n\n\
             Rows agreeing on every non-nested field are one row; the comparison is \
             linear in the number of distinct parents, which is what keeps the \
             generated code readable."
        );
        Some((
            merge_fn.clone(),
            quote! {
                #[doc = #d]
                fn #merge_fn(rows: Vec<#row_ty>) -> Vec<#row_ty> {
                    let mut out: Vec<#row_ty> = Vec::with_capacity(rows.len());
                    for mut row in rows {
                        if let Some(kept) = out.iter_mut().find(|kept| #same) {
                            #(#extends)*
                            continue;
                        }
                        out.push(row);
                    }
                    out
                }
            },
        ))
    } else {
        None
    };

    let (fold_call, fold_item) = match &fold {
        Some((f, item)) => (quote!(let rows = #f(rows);), item.clone()),
        None => (quote!(), quote!()),
    };

    let body = if has_to_many {
        quote! {
            let rows = #query_fn(params).#call.await?;
            #fold_call
            Ok(rows)
        }
    } else {
        quote!(#query_fn(params).#call.await)
    };

    let doc_text = match a.spec.cardinality {
        Cardinality::One => format!(" Run `{name}` and return its single row."),
        Cardinality::Optional => format!(" Run `{name}` and return its row, if there is one."),
        Cardinality::Many => format!(" Run `{name}` and return every row."),
        Cardinality::Exec => format!(" Run `{name}` for its side effect."),
    };
    let _ = query_ty;

    Ok(quote! {
        #fold_item

        #user_doc
        #[doc = ""]
        #[doc = #doc_text]
        pub async fn #fn_name(
            db: &dyn keelson_exec::Executor,
            params: impl Into<#params_ty>,
        ) -> Result<#ret, keelson_exec::ExecError> {
            use keelson_exec::Execute as _;
            #body
        }
    })
}

// --- the mod face ---------------------------------------------------------

fn mod_face(
    a: &Analysis,
    dial: &Dial,
    params_ty: &Ident,
    mod_fn_name: &Ident,
    name: &str,
) -> Result<TokenStream> {
    if let Some(why) = &a.clauses.unsupported {
        let d = format!(" `{name}` has no mod face: {why}.");
        return Ok(quote! {
            #[doc = #d]
            #[doc = ""]
            #[doc = " The query face above still runs it. Nesting it as a sub-select would"]
            #[doc = " not be the same statement, so the generator refuses rather than"]
            #[doc = " pretending."]
            const _: () = ();
        });
    }
    let krate = &dial.krate;
    let mut stmts = Vec::new();

    for (clause, span) in a.clauses.present() {
        let parts = a.parts(span);
        // Each fragment owns clones of the arguments that fall inside it, so
        // the closure stays `Fn` and the host may render it more than once.
        let binds: Vec<TokenStream> = parts
            .iter()
            .filter_map(|p| match p {
                Part::Arg(i) => {
                    let v = format_ident!("a{}", i);
                    let i = Literal::usize_unsuffixed(*i);
                    Some(quote!(let #v = args[#i].clone();))
                }
                Part::Sql(_) => None,
            })
            .collect();
        let inner = write_parts(&parts, |i| {
            let v = format_ident!("a{}", i);
            quote!(#v.clone())
        });
        // A condition is parenthesised on the way in. `WHERE`/`HAVING`
        // accumulate with `AND`, so a fragment whose top level is an `OR`
        // would otherwise re-bind against the host's own condition — the one
        // way slicing raw text could change what the author wrote.
        let body = if matches!(clause, "where" | "having") {
            quote! {
                w.push_str("(");
                #inner
                w.push_str(")");
            }
        } else {
            inner
        };
        let frag = quote! {
            keelson_core::dyn_expr(keelson_core::expr_fn(
                move |w: &mut keelson_core::SqlWriter<'_>| { #body },
            ))
        };
        let apply = match clause {
            "from" => quote! {
                if q.from.expression.is_none() {
                    q.from.set_table(#frag);
                }
            },
            "where" => quote!(q.where_.append_where(#frag);),
            "group_by" => quote!(q.group_by.append_group(#frag);),
            "having" => quote!(q.having.append_having(#frag);),
            "order_by" => quote!(q.order_by.append_order(#frag);),
            "limit" => quote! {
                if q.limit.is_empty() {
                    q.limit.set_limit(#frag);
                }
            },
            "offset" => quote! {
                if q.offset.is_empty() {
                    q.offset.set_offset(#frag);
                }
            },
            other => {
                return Err(GenError::Unsupported(format!(
                    "query `{name}`: no host clause for `{other}`"
                )));
            }
        };
        stmts.push(quote! {
            {
                #(#binds)*
                #apply
            }
        });
    }

    // A parameter that appears only in the select list is not carried by any
    // clause the mod contributes, so the argument vector can be genuinely
    // unused — bind it only when a fragment reaches for it.
    let uses_args = a
        .clauses
        .present()
        .iter()
        .any(|(_, span)| a.parts(*span).iter().any(|p| matches!(p, Part::Arg(_))));
    let args_let = if uses_args {
        quote!(let args = params.into().args();)
    } else {
        quote!(let _ = params;)
    };
    let doc_text = format!(
        " `{name}` as a mod: its clauses merged into the host statement, flat.\n\n\
         The `WHERE` is `AND`ed onto the host's; the `FROM` (joins included) is \
         contributed only when the host has none of its own, so a model query on \
         the same table keeps its own. Nothing nests as a sub-select — that \
         flatness is the point.\n\n\
         The select list is deliberately **not** contributed: the host statement \
         owns its projection, which is what lets a typed model query and this \
         mod sit in the same tuple."
    );
    let _ = krate;
    let select_query = match dial.placeholder {
        "$n" => quote!(keelson_psql::SelectQuery),
        _ => quote!(keelson_sqlite::SelectQuery),
    };

    Ok(quote! {
        #[doc = #doc_text]
        pub fn #mod_fn_name(
            params: impl Into<#params_ty>,
        ) -> impl keelson_core::Mod<#select_query> {
            #args_let
            keelson_core::mod_fn(move |q: &mut #select_query| {
                #(#stmts)*
            })
        }
    })
}
