//! Factory emission: the resolved models → one `factories.rs` holding a
//! keelson-factory template module per writable table.
//!
//! What it writes is what `keelson-factory/tests/spec_psql.rs` /
//! `spec_sqlite.rs` fix: a `<Row>Template` with one
//! [`Source`](keelson_factory::Source) field per data column, a
//! [`Parent`](keelson_factory::Parent) /
//! [`OptionalParent`](keelson_factory::OptionalParent) field per foreign key,
//! a `Vec<child template>` per back-reference; one mod per column (`id(v)`,
//! `title(v)`, `email_null()`, `random_id()`), the parent triple
//! (`user(&row)` / `user_id(k)` / `for_user(tpl)`) and the child mod
//! (`with_new_post(tpl)`); then `build`/`create_with`/`create`/`create_many`.
//!
//! # The decisions this file makes for the generator
//!
//! **One file, not one per table.** Every generated file lands in the same
//! output directory, and the call-site shape the spec fixes is
//! `fac::users::factory(…)` — a *module tree*. `factories.rs` with one `pub
//! mod` per table gives exactly that shape with one added file and one added
//! `pub mod factories;` line in `mod.rs`; a flat `users_factory.rs` per table
//! would not.
//!
//! **The per-column default rule**, in order, first hit wins:
//!
//! 1. a foreign-key column is not a `Source` at all — it is the parent field;
//! 2. a **unique** column (a single-column `UNIQUE` key, or a single-column
//!    primary key) takes a [`Sequence`](keelson_factory::Sequence) value, so
//!    `create_many(&db, 100)` cannot collide — as text, `"user-<n>"`. This
//!    holds for an `AUTO_INCREMENT`/rowid key too, deliberately: `build()`
//!    must produce a complete setter **without a database**, and a key the
//!    engine assigns does not exist until there is one. A caller who wants
//!    the engine's key sets the column's source to `Source::Omit`;
//! 3. a column with a **database default** (or a non-key auto-increment) is
//!    omitted, so the default is what the row gets — this is what keeps
//!    `is_active`/`created_at` out of the statement, as the spec's `build`
//!    assertions pin;
//! 4. otherwise a random value drawn from the run's
//!    [`Faker`](keelson_factory::Faker) — integers, floats, booleans and
//!    strings;
//! 5. and for a type the faker has no honest generator for (temporals,
//!    decimals, uuids, json, bytes) the column is **omitted**. Recorded
//!    limitation: a `NOT NULL` column of such a type with no database default
//!    cannot be created without a mod supplying the value.
//!
//! **Parent templates are boxed.** `Parent<Box<UserTemplate>, i32>`, not
//! `Parent<UserTemplate, i32>`: a self-referencing foreign key
//! (`employees.manager_id`) or a pair of mutually-referencing tables would
//! otherwise be an infinitely-sized type. The indirection is invisible at
//! every call site — `for_user(users::factory(…))` still takes the template
//! by value. (A *non-null* self-referencing key still cannot be
//! auto-created: `Parent::Auto` would recurse forever, which is the schema
//! saying no row can exist without another. Pass an existing key.)
//!
//! **Views are not in the factory's world at all.** A factory creates rows,
//! and a view has none of its own: its rows appear when the rows underneath
//! them do. So a view gets no template (even a writable one — `resolve`
//! refuses that combination outright rather than emit a template with no
//! unique constraints or auto-increment columns to draw distinct values
//! from), a foreign key *pointing at* a view stays a plain value column
//! rather than a `Parent` field, and a back-reference *from* a view gets no
//! `with_new_…` mod. Relations to views are a read-side feature; see
//! `docs/views.md`.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::config::Config;
use crate::error::{GenError, Result};
use crate::names::ident;
use crate::resolve::{BelongsTo, Model, ModelColumn};
use crate::schema::TableKind;

/// Copy types, so a key access needs no `.clone()` (the model emitter's list,
/// kept in step).
fn is_copy(rust_type: &str) -> bool {
    matches!(
        rust_type,
        "i8" | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "bool"
            | "f32"
            | "f64"
            | "char"
            | "uuid::Uuid"
            | "chrono::NaiveDate"
            | "chrono::NaiveTime"
            | "chrono::NaiveDateTime"
            | "chrono::DateTime<chrono::Utc>"
            | "rust_decimal::Decimal"
    )
}

fn parse_type(rust_type: &str, what: &str) -> Result<syn::Type> {
    syn::parse_str(rust_type)
        .map_err(|e| GenError::Config(format!("{what}: `{rust_type}` is not a Rust type: {e}")))
}

/// `row.field` / `row.field.clone()`, as the type demands.
fn key_access(recv: TokenStream, c: &ModelColumn) -> TokenStream {
    let f = ident(&c.field);
    if is_copy(&c.rust_type) {
        quote!(#recv.#f)
    } else {
        quote!(#recv.#f.clone())
    }
}

/// Render `factories.rs`: one module per writable model, in table order.
///
/// No [`Dial`](super::Dial) in the signature, and that is the point: a
/// factory writes through the *model's* insert path (so hooks fire), never
/// through a dialect statement, so the emitted code is identical on all
/// three dialects.
pub(crate) fn factories_file(models: &[Model], config: &Config) -> Result<TokenStream> {
    let mut mods = Vec::new();
    // Writable base tables only. A writable *view* is refused earlier, in
    // `resolve`, rather than skipped here: a view reports neither
    // auto-increment columns nor unique constraints, so its factory would
    // look right and collide on the second row.
    for m in models
        .iter()
        .filter(|m| m.writable && m.kind == TableKind::Table)
    {
        mods.push(factory_mod(m, models, config)?);
    }
    if mods.is_empty() {
        return Err(GenError::Unsupported(
            "[output] factories is on but no table has a primary key, so there is nothing \
             to build a factory on"
                .to_owned(),
        ));
    }
    Ok(quote! { #(#mods)* })
}

/// What a column's `Source::Auto` resolves to.
enum Auto {
    /// A sequence value — unique columns.
    SeqI32,
    /// The same counter, widened.
    SeqI64,
    /// A sequence-suffixed string — unique text columns.
    SeqText,
    /// A faker-drawn value.
    Random(TokenStream),
    /// Left out of the statement: the database default (or NULL) applies.
    Unset,
}

impl Auto {
    /// The closure body `Source::resolve` is called with.
    fn closure(&self, singular: &str) -> TokenStream {
        match self {
            Auto::SeqI32 => quote!(|_| keelson_models::Set::Value(SEQ.next_i32())),
            Auto::SeqI64 => quote!(|_| keelson_models::Set::Value(SEQ.next_i64())),
            Auto::SeqText => {
                let fmt = format!("{singular}-{{}}");
                quote!(|_| keelson_models::Set::Value(format!(#fmt, SEQ.next_i32())))
            }
            Auto::Random(v) => quote!(|f| keelson_models::Set::Value(#v)),
            Auto::Unset => quote!(|_| keelson_models::Set::Unset),
        }
    }

    fn uses_sequence(&self) -> bool {
        matches!(self, Auto::SeqI32 | Auto::SeqI64 | Auto::SeqText)
    }
}

/// The per-column default rule (the numbered list in the module docs).
fn auto_rule(c: &ModelColumn, singular: &str) -> Auto {
    // Uniqueness first, auto-increment included: the factory supplies its own
    // keys because `build()` must produce a complete setter *without a
    // database*, and an engine-assigned key does not exist until there is
    // one. (`Source::Omit` — `id_omit()` is not emitted, but
    // `Source::Omit` is reachable by hand — leaves the column to the engine
    // for callers who prefer that.)
    if c.unique {
        return match c.rust_type.as_str() {
            "i32" | "u32" => Auto::SeqI32,
            "i64" | "u64" => Auto::SeqI64,
            "String" => Auto::SeqText,
            // No honest sequence for the type: leave it to the caller rather
            // than manufacture collisions.
            _ => Auto::Unset,
        };
    }
    if c.autoincrement || c.default.is_some() {
        return Auto::Unset;
    }
    match random_value(&c.rust_type, singular) {
        Some(v) => Auto::Random(v),
        None => Auto::Unset,
    }
}

/// A faker expression for the types the faker can generate honestly.
fn random_value(rust_type: &str, singular: &str) -> Option<TokenStream> {
    let fmt = format!("{singular}-{{}}");
    Some(match rust_type {
        "String" => quote!(format!(#fmt, f.alnum(8))),
        "bool" => quote!(f.i64_in(0, 1) == 1),
        "i32" => quote!(f.i32_in(1, 1000)),
        "i64" => quote!(f.i64_in(1, 1000)),
        "i8" | "u8" => {
            let t = format_ident!("{rust_type}");
            quote!(f.i64_in(1, 100) as #t)
        }
        "i16" | "u16" | "u32" | "u64" | "usize" | "isize" => {
            let t = format_ident!("{rust_type}");
            quote!(f.i64_in(1, 1000) as #t)
        }
        "f32" => quote!(f.i64_in(0, 10_000) as f32 / 100.0),
        "f64" => quote!(f.i64_in(0, 10_000) as f64 / 100.0),
        _ => return None,
    })
}

/// One column's role in the template.
enum Role<'a> {
    /// A plain value source.
    Source,
    /// The foreign key of this to-one relation.
    Parent(&'a BelongsTo),
}

/// A relation only becomes a `Parent` field when the factory could actually
/// create the row it points at. A relation whose target is `SELECT`-only —
/// any relation to a view — has no template to create, so the foreign-key
/// column stays a plain value `Source`: the view's row appears when the row
/// underneath it does, which is not this template's business.
fn roles<'a>(m: &'a Model, all: &[Model]) -> Vec<Role<'a>> {
    (0..m.columns.len())
        .map(|i| match m.belongs_to.iter().find(|b| b.fk_column == i) {
            Some(b) if all.iter().any(|t| t.table == b.target && t.writable) => Role::Parent(b),
            _ => Role::Source,
        })
        .collect()
}

fn model_of<'a>(all: &'a [Model], table: &str) -> Result<&'a Model> {
    all.iter()
        .find(|m| m.table == table)
        .ok_or_else(|| GenError::Config(format!("factory: model `{table}` is not generated")))
}

fn template_name(m: &Model) -> proc_macro2::Ident {
    format_ident!("{}Template", m.row)
}

fn factory_mod(m: &Model, all: &[Model], config: &Config) -> Result<TokenStream> {
    let module = ident(&m.table);
    let table = &m.table;
    let tpl = template_name(m);
    let model_mod = ident(&m.table);
    let row = ident(&m.row);
    let singular = crate::names::singular(&m.table, &config.inflections);
    let roles = roles(m, all);

    // Every generated fn name, checked for collisions once at the end.
    let mut fn_names: Vec<String> = vec!["factory".to_owned()];

    let mut fields = Vec::new();
    let mut mods = Vec::new();
    let mut setter_inits = Vec::new();
    let mut parent_steps = Vec::new();
    let mut child_steps = Vec::new();
    let mut uses_sequence = false;

    // ── one field, one setter init and the mods per column ──
    for (i, c) in m.columns.iter().enumerate() {
        let f = ident(&c.field);
        match roles[i] {
            Role::Source => {
                let ty = parse_type(&c.rust_type, &format!("column {table}.{}", c.db_name))?;
                let doc = format!(" `{}`.", c.db_name);
                fields.push(quote! {
                    #[doc = #doc]
                    pub #f: keelson_factory::Source<#ty>
                });

                let auto = auto_rule(c, &singular);
                uses_sequence |= auto.uses_sequence();
                let closure = auto.closure(&singular);
                setter_inits.push(quote!(#f: self.#f.resolve(f, #closure)));

                // The value mod. Strings take `impl Into<String>` so a
                // `&str` literal works at the call site.
                let doc = format!(" Set `{}`.", c.db_name);
                if c.rust_type == "String" {
                    mods.push(quote! {
                        #[doc = #doc]
                        pub fn #f(v: impl Into<String>) -> impl keelson_core::Mod<#tpl> {
                            let v = v.into();
                            keelson_core::mod_fn(move |t: &mut #tpl| {
                                t.#f = keelson_factory::Source::Value(v)
                            })
                        }
                    });
                } else {
                    mods.push(quote! {
                        #[doc = #doc]
                        pub fn #f(v: #ty) -> impl keelson_core::Mod<#tpl> {
                            keelson_core::mod_fn(move |t: &mut #tpl| {
                                t.#f = keelson_factory::Source::Value(v)
                            })
                        }
                    });
                }
                fn_names.push(c.field.clone());

                if c.nullable {
                    let null_fn = format_ident!("{}_null", c.field);
                    let doc = format!(" Set `{}` to SQL NULL.", c.db_name);
                    mods.push(quote! {
                        #[doc = #doc]
                        pub fn #null_fn() -> impl keelson_core::Mod<#tpl> {
                            keelson_core::mod_fn(|t: &mut #tpl| {
                                t.#f = keelson_factory::Source::Null
                            })
                        }
                    });
                    fn_names.push(format!("{}_null", c.field));
                }

                // A sequence-backed column also gets the random alternative:
                // still inside the faker's seed, unlike the sequence.
                if auto.uses_sequence()
                    && let Some(range) = random_key(&c.rust_type)
                {
                    let random_fn = format_ident!("random_{}", c.field);
                    let doc = format!(
                        " Draw `{}` at random instead of from the sequence — \
                         random values are inside the faker's seed, sequence \
                         values deliberately are not.",
                        c.db_name
                    );
                    mods.push(quote! {
                        #[doc = #doc]
                        pub fn #random_fn() -> impl keelson_core::Mod<#tpl> {
                            keelson_core::mod_fn(|t: &mut #tpl| {
                                t.#f = keelson_factory::Source::from_fn(|f| #range)
                            })
                        }
                    });
                    fn_names.push(format!("random_{}", c.field));
                }
            }
            Role::Parent(b) => {
                let parent = model_of(all, &b.target)?;
                let parent_mod = ident(&parent.table);
                let parent_tpl = template_name(parent);
                let parent_row = ident(&parent.row);
                let (_, ref_col) = parent.column(&b.ref_column).ok_or_else(|| {
                    GenError::Config(format!(
                        "factory: relation {table}.{} references missing column {}.{}",
                        c.db_name, b.target, b.ref_column
                    ))
                })?;
                let pk_ty = parse_type(&ref_col.rust_type, "parent key")?;
                let rel = ident(&b.name);
                let ref_f = ident(&ref_col.field);

                let (state, doc) = if c.nullable {
                    (
                        quote!(keelson_factory::OptionalParent),
                        format!(
                            " Nullable FK `{}` → `{}`: absent (NULL) unless a mod opts in.",
                            c.db_name, b.target
                        ),
                    )
                } else {
                    (
                        quote!(keelson_factory::Parent),
                        format!(
                            " FK `{}` → `{}`: auto-created from `{}`'s own template \
                             unless a mod overrides it.",
                            c.db_name, b.target, b.target
                        ),
                    )
                };
                fields.push(quote! {
                    #[doc = #doc]
                    pub #rel: #state<Box<super::#parent_mod::#parent_tpl>, #pk_ty>
                });

                let existing = if is_copy(&ref_col.rust_type) {
                    quote!(keelson_models::Set::Value(*pk))
                } else {
                    quote!(keelson_models::Set::Value(pk.clone()))
                };
                setter_inits.push(if c.nullable {
                    quote! {
                        #f: match &self.#rel {
                            keelson_factory::OptionalParent::Existing(pk) => #existing,
                            keelson_factory::OptionalParent::Absent
                            | keelson_factory::OptionalParent::Template(_) => {
                                keelson_models::Set::Unset
                            }
                        }
                    }
                } else {
                    quote! {
                        #f: match &self.#rel {
                            keelson_factory::Parent::Existing(pk) => #existing,
                            keelson_factory::Parent::Auto
                            | keelson_factory::Parent::Template(_) => keelson_models::Set::Unset,
                        }
                    }
                });

                parent_steps.push(if c.nullable {
                    quote! {
                        if let keelson_factory::OptionalParent::Template(t) = &self.#rel {
                            s.#f = keelson_models::Set::Value(
                                t.create_with(db, &mut *f).await?.#ref_f,
                            );
                        }
                    }
                } else {
                    quote! {
                        match &self.#rel {
                            keelson_factory::Parent::Existing(_) => {}
                            keelson_factory::Parent::Template(t) => {
                                s.#f = keelson_models::Set::Value(
                                    t.create_with(db, &mut *f).await?.#ref_f,
                                );
                            }
                            keelson_factory::Parent::Auto => {
                                let t = super::#parent_mod::#parent_tpl::default();
                                s.#f = keelson_models::Set::Value(
                                    t.create_with(db, &mut *f).await?.#ref_f,
                                );
                            }
                        }
                    }
                });

                // The parent triple. The key form is named after the FK
                // column; when that name is already the relation's, it takes
                // a `_key` suffix so the two cannot collide.
                let key_name = if c.field == b.name {
                    format!("{}_key", c.field)
                } else {
                    c.field.clone()
                };
                let key_fn = ident(&key_name);
                let for_fn = format_ident!("for_{}", b.name);
                let existing_state = if c.nullable {
                    quote!(keelson_factory::OptionalParent::Existing)
                } else {
                    quote!(keelson_factory::Parent::Existing)
                };
                let template_state = if c.nullable {
                    quote!(keelson_factory::OptionalParent::Template)
                } else {
                    quote!(keelson_factory::Parent::Template)
                };
                let take_key = key_access(quote!(row), ref_col);
                let row_doc = format!(" Use this existing `{}` row as the parent.", b.target);
                let key_doc = format!(" Use this existing `{}` key as the parent.", b.target);
                let tpl_doc = format!(" Create the `{}` parent from this template.", b.target);
                mods.push(quote! {
                    #[doc = #row_doc]
                    pub fn #rel(
                        row: &super::super::#parent_mod::#parent_row,
                    ) -> impl keelson_core::Mod<#tpl> {
                        let pk = #take_key;
                        keelson_core::mod_fn(move |t: &mut #tpl| t.#rel = #existing_state(pk))
                    }

                    #[doc = #key_doc]
                    pub fn #key_fn(pk: #pk_ty) -> impl keelson_core::Mod<#tpl> {
                        keelson_core::mod_fn(move |t: &mut #tpl| t.#rel = #existing_state(pk))
                    }

                    #[doc = #tpl_doc]
                    pub fn #for_fn(
                        tpl: super::#parent_mod::#parent_tpl,
                    ) -> impl keelson_core::Mod<#tpl> {
                        keelson_core::mod_fn(move |t: &mut #tpl| {
                            t.#rel = #template_state(Box::new(tpl))
                        })
                    }
                });
                fn_names.push(b.name.clone());
                fn_names.push(key_name);
                fn_names.push(format!("for_{}", b.name));
            }
        }
    }

    // ── has-many children ──
    for h in &m.has_many {
        let child = model_of(all, &h.child)?;
        // Same rule as `roles`: a `SELECT`-only child — any view — has no
        // template to create, so no `with_new_…` mod is offered for it.
        if !child.writable {
            continue;
        }
        let child_mod = ident(&child.table);
        let child_tpl = template_name(child);
        let rel = ident(&h.name);
        let (_, parent_key) = m.column(&h.parent_key_column).ok_or_else(|| {
            GenError::Config(format!(
                "factory: back-reference key {table}.{} is not generated",
                h.parent_key_column
            ))
        })?;
        // The child's own parent field for this key: same relation the child
        // model resolved for the foreign key.
        let (child_fk_idx, child_fk) = child.column(&h.child_fk_column).ok_or_else(|| {
            GenError::Config(format!(
                "factory: back-reference key {}.{} is not generated",
                h.child, h.child_fk_column
            ))
        })?;
        let child_rel = child
            .belongs_to
            .iter()
            .find(|b| b.fk_column == child_fk_idx)
            .ok_or_else(|| {
                GenError::Config(format!(
                    "factory: `{}.{}` has no to-one relation to bind a new child to",
                    h.child, h.child_fk_column
                ))
            })?;
        let child_rel_f = ident(&child_rel.name);

        let doc = format!(
            " Has-many `{}` children, created after this row exists, each with \
             `{}.{}` forced to it.",
            h.child, h.child, h.child_fk_column
        );
        fields.push(quote! {
            #[doc = #doc]
            pub #rel: Vec<super::#child_mod::#child_tpl>
        });

        let existing_state = if child_fk.nullable {
            quote!(keelson_factory::OptionalParent::Existing)
        } else {
            quote!(keelson_factory::Parent::Existing)
        };
        let key = key_access(quote!(row), parent_key);
        child_steps.push(quote! {
            for c in &self.#rel {
                let mut child = c.clone();
                child.#child_rel_f = #existing_state(#key);
                child.create_with(db, &mut *f).await?;
            }
        });

        let with_fn = format_ident!(
            "with_new_{}",
            crate::names::singular(&h.child, &config.inflections)
        );
        let with_name = format!(
            "with_new_{}",
            crate::names::singular(&h.child, &config.inflections)
        );
        let doc = format!(" Queue a new `{}` child for this row.", h.child);
        mods.push(quote! {
            #[doc = #doc]
            pub fn #with_fn(tpl: super::#child_mod::#child_tpl) -> impl keelson_core::Mod<#tpl> {
                keelson_core::mod_fn(move |t: &mut #tpl| t.#rel.push(tpl))
            }
        });
        fn_names.push(with_name);
    }

    // Two mods of the same name would be a confusing compile error inside a
    // generated file; name the clash here instead.
    let mut sorted = fn_names.clone();
    sorted.sort();
    sorted.dedup();
    if sorted.len() != fn_names.len() {
        let mut seen = std::collections::BTreeSet::new();
        let dup = fn_names
            .iter()
            .find(|n| !seen.insert((*n).clone()))
            .expect("length mismatch means a duplicate");
        return Err(GenError::Config(format!(
            "factory `{table}`: two generated mods would both be named `{dup}`; \
             rename the column or the relation in [aliases.{table}]"
        )));
    }

    // A table whose every column is a foreign key (a pure join table) draws
    // nothing from the faker, and an unused parameter is a warning in the
    // user's crate.
    let faker_param = if m
        .columns
        .iter()
        .enumerate()
        .any(|(i, _)| matches!(roles[i], Role::Source))
    {
        quote!(f)
    } else {
        quote!(_f)
    };

    let seq_item = if uses_sequence {
        quote! {
            #[doc = " The model's uniqueness source: one process-wide sequence."]
            static SEQ: keelson_factory::Sequence = keelson_factory::Sequence::new();
        }
    } else {
        quote!()
    };

    // `let mut s` only when something still writes to it.
    let build_call = if parent_steps.is_empty() {
        quote!(let s = self.build(f);)
    } else {
        quote!(let mut s = self.build(f);)
    };
    let insert_call = if child_steps.is_empty() {
        quote! {
            super::super::#model_mod::table().insert(s).one(db).await
        }
    } else {
        quote! {
            let row = super::super::#model_mod::table().insert(s).one(db).await?;
            #(#child_steps)*
            Ok(row)
        }
    };

    let mod_doc = format!(" The `{table}` factory.");
    let tpl_doc = format!(" The `{table}` template: one value source per column.");
    let row_path = quote!(super::super::#model_mod::#row);

    Ok(quote! {
        #[doc = #mod_doc]
        pub mod #module {
            #seq_item

            #[doc = #tpl_doc]
            #[derive(Debug, Clone, Default)]
            pub struct #tpl {
                #(#fields,)*
            }

            #[doc = " The entry point: `factory((id(10), …))`."]
            pub fn factory(mods: impl keelson_core::Mod<#tpl>) -> #tpl {
                let mut t = #tpl::default();
                mods.apply(&mut t);
                t
            }

            #(#mods)*

            impl #tpl {
                #[doc = " The no-database strategy: the setter `create` would insert. \
                          No executor in the signature — that absence is the guarantee."]
                pub fn build(
                    &self,
                    #faker_param: &mut keelson_factory::Faker,
                ) -> super::super::#model_mod::Setter {
                    super::super::#model_mod::Setter {
                        #(#setter_inits,)*
                    }
                }

                #[doc = " Create the parent chain (unless overridden), the row itself \
                          through the model's own insert path — so hooks fire — and then \
                          the queued children. Boxed rather than an `async fn` because \
                          factory graphs recurse."]
                pub fn create_with<'a>(
                    &'a self,
                    db: &'a dyn keelson_exec::Executor,
                    f: &'a mut keelson_factory::Faker,
                ) -> keelson_exec::ExecFuture<'a, Result<#row_path, keelson_exec::ExecError>>
                {
                    Box::pin(async move {
                        #build_call
                        #(#parent_steps)*
                        #insert_call
                    })
                }

                #[doc = " Create one row (and whatever parents and children the template \
                          implies) with a fresh entropy-seeded faker."]
                pub async fn create(
                    &self,
                    db: &dyn keelson_exec::Executor,
                ) -> Result<#row_path, keelson_exec::ExecError> {
                    let mut f = keelson_factory::Faker::from_entropy();
                    self.create_with(db, &mut f).await
                }

                #[doc = " Create `n` rows; sequences keep the unique columns apart."]
                pub async fn create_many(
                    &self,
                    db: &dyn keelson_exec::Executor,
                    n: usize,
                ) -> Result<Vec<#row_path>, keelson_exec::ExecError> {
                    let mut f = keelson_factory::Faker::from_entropy();
                    let mut out = Vec::with_capacity(n);
                    for _ in 0..n {
                        out.push(self.create_with(db, &mut f).await?);
                    }
                    Ok(out)
                }
            }
        }
    })
}

/// The random draw a `random_<column>()` mod uses, for the key types a
/// sequence would otherwise fill. Deliberately narrower than the sequence
/// list: a `u32` key drawn from an `i32` range would need a cast the call
/// site never asked for.
fn random_key(rust_type: &str) -> Option<TokenStream> {
    Some(match rust_type {
        "i32" => quote!(f.i32_in(1, i32::MAX / 2)),
        "i64" => quote!(f.i64_in(1, i64::MAX / 2)),
        "String" => quote!(f.alnum(12)),
        _ => return None,
    })
}
