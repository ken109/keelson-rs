//! One model → one file: the marker, row, `Rel`, `FromRow`, `Setter`,
//! column fns, `View`/`Table` impls, hook delegations and the
//! `preload`/`then_load` mod modules — the shapes the hand-written spec in
//! `keelson-models/tests/` fixes.
//!
//! # The decisions, recorded
//!
//! **Every to-one relation field is `Option<Box<Row>>`, uniformly** —
//! belongs-to, and the `cardinality = "one_to_one"` back-reference alike.
//! A `Rel` holds the target's whole row, so two models that reference each
//! other to-one hold each other by value: `ARow.rel.b: Option<BRow>` and
//! `BRow.rel.a: Option<ARow>` is a recursive type of infinite size, which
//! is a hard compile error in the *generated* code — nothing a user of this
//! generator can work around. Two base tables with mutual single-column
//! foreign keys are enough to produce it. `Box` is the indirection that
//! breaks the cycle; a to-many field is a `Vec` and already carries its
//! own.
//!
//! The alternative — walking the relation graph, finding its cycles, and
//! boxing only the edges that close one — was rejected. It makes a
//! generated field's type a function of the whole schema: adding an
//! unrelated foreign key somewhere else could flip an existing
//! `Option<Row>` to `Option<Box<Row>>` and break user code at a distance,
//! for a reason nothing at the call site explains. The uniform rule costs
//! one allocation per attached to-one row and fits in one sentence.
//!
//! Reading is unaffected — `Box` derefs — but *constructing* one is
//! `Some(Box::new(row))`. The boxing itself happens at exactly one kind of
//! place: wherever a to-one `rel` field is written (`= c.map(Box::new)`).
//! The `<name>_from_preload` decoder keeps returning the plain row, so the
//! `Box` is never something a caller has to unwrap to get at a decode
//! result.

use proc_macro2::TokenStream;
use quote::quote;

use crate::config::{Config, Hook};
use crate::error::{GenError, Result};
use crate::names::ident;
use crate::resolve::{Model, ModelColumn};
use crate::rust_types::{is_copy, key_access, needs_copy_allow, parse_type};

use super::Dial;

/// The row struct: one field per column, plus `rel` when the model carries
/// relations.
fn row_struct(
    m: &Model,
    row: &proc_macro2::Ident,
    serde_derive: &TokenStream,
    has_rel: bool,
) -> Result<TokenStream> {
    let table = &m.table;
    let mut row_fields = Vec::new();
    for c in &m.columns {
        let f = ident(&c.field);
        let t = parse_type(&c.rust_type, &format!("column {table}.{}", c.db_name))?;
        let t = if c.nullable {
            quote!(Option<#t>)
        } else {
            quote!(#t)
        };
        row_fields.push(quote!(pub #f: #t));
    }
    if has_rel {
        row_fields.push(quote! {
            #[doc = " Relations, filled by `preload`/`then_load` mods; empty otherwise."]
            pub rel: Rel
        });
    }
    let row_doc = format!(" One row of `{table}`.");
    let row_item = quote! {
        #[doc = #row_doc]
        #[derive(Debug, Clone, PartialEq #serde_derive)]
        pub struct #row {
            #(#row_fields,)*
        }
    };
    Ok(row_item)
}

/// One `fn` per column returning its typed [`Column`](keelson_models::Column),
/// and `all_columns()` — the select list every query starts from.
fn column_fns(m: &Model) -> Result<(Vec<TokenStream>, TokenStream)> {
    let table = &m.table;
    let mut col_fns = Vec::new();
    for c in &m.columns {
        let f = ident(&c.field);
        let t = parse_type(&c.rust_type, "column")?;
        let n = &c.db_name;
        col_fns.push(quote! {
            pub fn #f() -> keelson_models::Column<#t> {
                keelson_models::Column::new(#table, #n)
            }
        });
    }
    let all_columns_item = if m.columns.len() <= 16 {
        let types = m
            .columns
            .iter()
            .map(|c| {
                let t = parse_type(&c.rust_type, "column")?;
                Ok(quote!(keelson_models::Column<#t>))
            })
            .collect::<Result<Vec<_>>>()?;
        let calls = m.columns.iter().map(|c| {
            let f = ident(&c.field);
            quote!(#f())
        });
        quote! {
            #[allow(clippy::type_complexity)]
            fn all_columns() -> (#(#types,)*) {
                (#(#calls,)*)
            }
        }
    } else {
        // Past the 16-element tuple impls, the projection is a Vec<Expr> —
        // same SQL, the per-column types stay on the column fns. Statements
        // rather than a vec![] macro, because prettyplease flows macro
        // tokens instead of formatting them.
        let pushes = m.columns.iter().map(|c| {
            let f = ident(&c.field);
            quote!(all.push(#f().expr());)
        });
        quote! {
            fn all_columns() -> Vec<keelson_core::expr::Expr> {
                let mut all: Vec<keelson_core::expr::Expr> = Vec::new();
                #(#pushes)*
                all
            }
        }
    };
    Ok((col_fns, all_columns_item))
}

pub(crate) fn model_file(
    m: &Model,
    all: &[Model],
    config: &Config,
    dial: &Dial,
) -> Result<TokenStream> {
    let krate = &dial.krate;
    let table = &m.table;
    let marker = ident(&m.marker);
    let row = ident(&m.row);
    // Two independent questions, because a view answers them differently: is
    // the write surface generated (`writable`), and does the model carry
    // relations (`holds_relations` — a `SELECT`-only view does, as soon as
    // the configuration declares one, because loading needs a join column
    // rather than an identity).
    let is_table = m.writable;
    let has_rel = m.holds_relations();

    let hooks_path: syn::Path = syn::parse_str(&config.hooks.module).map_err(|e| {
        GenError::Config(format!(
            "[hooks] module `{}` is not a module path: {e}",
            config.hooks.module
        ))
    })?;
    let table_mod = ident(table);

    // ── the marker ──
    let entry = m.entry();
    let marker_doc = format!(" The model marker `{table}::{entry}()` hangs off.");
    let marker_item = quote! {
        #[doc = #marker_doc]
        #[derive(Debug, Clone, Copy)]
        pub struct #marker;
    };

    // ── the row struct ──
    let serde_derive = if config.output.serde {
        quote!(, serde::Serialize, serde::Deserialize)
    } else {
        quote!()
    };
    let row_item = row_struct(m, &row, &serde_derive, has_rel)?;
    // ── Rel ──
    let rel_item = if has_rel {
        let mut rel_fields = Vec::new();
        for b in &m.belongs_to {
            let f = ident(&b.name);
            let (tmod, trow) = target_names(all, &b.target)?;
            let fk = &m.columns[b.fk_column].db_name;
            let doc = format!(" Belongs-to `{}`, via `{table}.{fk}`.", b.target);
            // Boxed unconditionally — see the module docs.
            rel_fields.push(quote! {
                #[doc = #doc]
                pub #f: Option<Box<super::#tmod::#trow>>
            });
        }
        for h in &m.has_many {
            let f = ident(&h.name);
            let (cmod, crow) = target_names(all, &h.child)?;
            // `cardinality = "one_to_one"` makes the back-reference an
            // `Option` rather than a `Vec`; a foreign key always means many.
            // The to-one arm is boxed for the same reason every to-one field
            // is (see the module docs); a `Vec` carries its own indirection
            // and needs none.
            let (kind, ty) = if h.to_one {
                ("Has-one", quote!(Option<Box<super::#cmod::#crow>>))
            } else {
                ("Has-many", quote!(Vec<super::#cmod::#crow>))
            };
            let doc = format!(
                " {kind} `{}`, via `{}.{}`.",
                h.child, h.child, h.child_fk_column
            );
            rel_fields.push(quote! {
                #[doc = #doc]
                pub #f: #ty
            });
        }
        let rel_doc = if table.ends_with('s') {
            format!(" `{table}`' relations.")
        } else {
            format!(" `{table}`'s relations.")
        };
        quote! {
            #[doc = #rel_doc]
            #[derive(Debug, Clone, PartialEq, Default #serde_derive)]
            pub struct Rel {
                #(#rel_fields,)*
            }
        }
    } else {
        quote!()
    };

    // ── FromRow ──
    let takes = m.columns.iter().map(|c| {
        let f = ident(&c.field);
        let n = &c.db_name;
        quote!(#f: row.take(#n)?)
    });
    let rel_init = if has_rel {
        quote!(rel: Rel::default(),)
    } else {
        quote!()
    };
    let from_row_item = quote! {
        impl keelson_exec::FromRow for #row {
            fn from_row(row: &mut keelson_exec::Row) -> Result<Self, keelson_exec::ExecError> {
                Ok(#row {
                    #(#takes,)*
                    #rel_init
                })
            }
        }
    };

    // ── assert_bind for every configured type ──
    let mut binds = Vec::new();
    for c in m.columns.iter().filter(|c| c.overridden) {
        let t = parse_type(&c.rust_type, &format!("column {table}.{}", c.db_name))?;
        let doc = format!(
            " `{}` (db type `{}`) is overridden by configuration to `{}`; the replacement must bind.",
            c.db_name, c.db_type, c.rust_type
        );
        binds.push(quote! {
            #[doc = #doc]
            const _: () = keelson_exec::assert_bind::<#t>();
        });
    }

    // ── Setter ──
    let setter_item = if is_table {
        let fields = m
            .columns
            .iter()
            .map(|c| {
                let f = ident(&c.field);
                let t = parse_type(&c.rust_type, "setter")?;
                Ok(quote!(pub #f: keelson_models::Set<#t>))
            })
            .collect::<Result<Vec<_>>>()?;
        quote! {
            #[doc = " The three-state setter: unset fields stay out of the statement."]
            #[derive(Debug, Clone, Default)]
            pub struct Setter {
                #(#fields,)*
            }
        }
    } else {
        quote!()
    };

    // ── the entry point ──
    //
    // On a `RETURNING` dialect this is the generic `ModelTable`. On MySQL a
    // writable model hands out the *marker* instead, whose inherent verbs are
    // exactly the ones MySQL can honour — `ModelTable::insert(…).one()`
    // decodes the `INSERT`'s own returned rows, which MySQL never produces,
    // so it is made unreachable rather than left to fail at run time.
    let entry_fn = ident(entry);
    let mysql_surface = is_table && !dial.returning;
    let entry_doc = format!(
        " The entry point: `{table}::{entry}().query(…)`{}.",
        if is_table {
            " / `.insert(…)` / …"
        } else {
            " — a SELECT-only model"
        }
    );
    let entry_item = if mysql_surface {
        quote! {
            #[doc = #entry_doc]
            pub fn #entry_fn() -> #marker {
                #marker
            }
        }
    } else {
        quote! {
            #[doc = #entry_doc]
            pub fn #entry_fn() -> keelson_models::ModelTable<#marker> {
                keelson_models::ModelTable::new()
            }
        }
    };

    // ── column fns and all_columns ──
    let (col_fns, all_columns_item) = column_fns(m)?;
    // ── View impl ──
    let after_select_item = if m.hooks.contains(&Hook::AfterSelect) {
        let doc = hook_doc(&config.hooks.module, table, "after_select");
        quote! {
            #[doc = #doc]
            fn after_select<'a>(
                db: &'a dyn keelson_exec::Executor,
                rows: &'a mut Vec<#row>,
            ) -> keelson_exec::ExecFuture<'a, Result<(), keelson_exec::ExecError>> {
                #hooks_path::#table_mod::after_select(db, rows)
            }
        }
    } else {
        quote!()
    };
    let view_item = quote! {
        impl keelson_models::View for #marker {
            type Row = #row;
            type Select = #krate::SelectQuery;

            fn base_select() -> Self::Select {
                #krate::select((
                    #krate::select::columns(all_columns()),
                    #krate::select::from(#krate::quote(#table)),
                ))
            }

            #after_select_item
        }
    };

    // ── Table impl ──
    let table_item = if is_table {
        table_impl(m, config, dial, &marker, &row, &hooks_path, &table_mod)?
    } else {
        quote!()
    };

    // ── the no-RETURNING mutation surface (MySQL) ──
    let mysql_item = if mysql_surface {
        no_returning_surface(m, dial, &marker, &row)?
    } else {
        quote!()
    };

    // ── preload / then_load ──
    let preload_item = if !m.belongs_to.is_empty() {
        preload_mod(m, all, dial, &marker, &row)?
    } else {
        quote!()
    };
    let then_load_item = if !m.belongs_to.is_empty() || !m.has_many.is_empty() {
        then_load_mod(m, all, &marker, &row)?
    } else {
        quote!()
    };

    Ok(quote! {
        #marker_item
        #row_item
        #rel_item
        #from_row_item
        #(#binds)*
        #setter_item
        #entry_item
        #(#col_fns)*
        #all_columns_item
        #view_item
        #table_item
        #mysql_item
        #preload_item
        #then_load_item
    })
}

fn hook_doc(module: &str, table: &str, method: &str) -> String {
    format!(
        " Delegates to the application's `{module}::{table}::{method}` \
         (configured in `[tables.{table}] hooks`)."
    )
}

fn target_names(all: &[Model], table: &str) -> Result<(proc_macro2::Ident, proc_macro2::Ident)> {
    let t = all
        .iter()
        .find(|m| m.table == table)
        .ok_or_else(|| GenError::Config(format!("relation target `{table}` is not generated")))?;
    Ok((ident(&t.table), ident(&t.row)))
}

fn table_impl(
    m: &Model,
    config: &Config,
    dial: &Dial,
    marker: &proc_macro2::Ident,
    row: &proc_macro2::Ident,
    hooks_path: &syn::Path,
    table_mod: &proc_macro2::Ident,
) -> Result<TokenStream> {
    let krate = &dial.krate;
    let table = &m.table;

    // Pk: a composite key is a tuple.
    let pk_cols: Vec<&ModelColumn> = m.pk.iter().map(|i| &m.columns[*i]).collect();
    let pk_types = pk_cols
        .iter()
        .map(|c| parse_type(&c.rust_type, "primary key"))
        .collect::<Result<Vec<_>>>()?;
    let (pk_type, pk_expr) = if pk_cols.len() == 1 {
        let t = &pk_types[0];
        (quote!(#t), key_access(quote!(row), pk_cols[0]))
    } else {
        let parts = pk_cols.iter().map(|c| key_access(quote!(row), c));
        (quote!((#(#pk_types),*)), quote!((#(#parts),*)))
    };
    let pk_allow = if pk_cols.iter().any(|c| needs_copy_allow(&c.rust_type)) {
        quote!(#[allow(clippy::clone_on_copy)])
    } else {
        quote!()
    };

    let pushes = m.columns.iter().map(|c| {
        let f = ident(&c.field);
        let n = &c.db_name;
        quote!(s.#f.push_into(#n, &mut cols, &mut vals);)
    });
    let sets = m.columns.iter().map(|c| {
        let f = ident(&c.field);
        let n = &c.db_name;
        quote! {
            if let Some(v) = s.#f.into_expr() {
                q.apply(#krate::update::set_col(#n).to(v));
            }
        }
    });

    let mut hook_items = Vec::new();
    for h in m.hooks.iter().filter(|h| **h != Hook::AfterSelect) {
        let method = ident(h.method());
        let doc = hook_doc(&config.hooks.module, table, h.method());
        let item = match h {
            Hook::BeforeInsert | Hook::BeforeUpdate => quote! {
                #[doc = #doc]
                fn #method<'a>(
                    db: &'a dyn keelson_exec::Executor,
                    setter: &'a mut Setter,
                ) -> keelson_exec::ExecFuture<'a, Result<(), keelson_exec::ExecError>> {
                    #hooks_path::#table_mod::#method(db, setter)
                }
            },
            Hook::AfterInsert => quote! {
                #[doc = #doc]
                fn #method<'a>(
                    db: &'a dyn keelson_exec::Executor,
                    rows: &'a [#row],
                ) -> keelson_exec::ExecFuture<'a, Result<(), keelson_exec::ExecError>> {
                    #hooks_path::#table_mod::#method(db, rows)
                }
            },
            Hook::AfterUpdate | Hook::AfterDelete => quote! {
                #[doc = #doc]
                fn #method<'a>(
                    db: &'a dyn keelson_exec::Executor,
                    affected: u64,
                ) -> keelson_exec::ExecFuture<'a, Result<(), keelson_exec::ExecError>> {
                    #hooks_path::#table_mod::#method(db, affected)
                }
            },
            Hook::BeforeDelete => quote! {
                #[doc = #doc]
                fn #method(
                    db: &dyn keelson_exec::Executor,
                ) -> keelson_exec::ExecFuture<'_, Result<(), keelson_exec::ExecError>> {
                    #hooks_path::#table_mod::#method(db)
                }
            },
            Hook::AfterSelect => unreachable!("filtered above"),
        };
        hook_items.push(item);
    }

    // The `INSERT`'s row source is the same everywhere; what differs is
    // whether it can carry `RETURNING` (and so whether an all-unset setter is
    // `DEFAULT VALUES` or MySQL's `VALUES ()`, which the statement writes for
    // itself when no row source is present).
    let insert_stmt = if dial.returning {
        quote! {
            let mut q = #krate::insert((
                #krate::insert::into(#krate::quote(#table)).columns(cols),
                #krate::insert::returning(all_columns()),
            ));
        }
    } else {
        quote! {
            let mut q = #krate::insert(#krate::insert::into(#krate::quote(#table)).columns(cols));
        }
    };

    Ok(quote! {
        impl keelson_models::Table for #marker {
            type Pk = #pk_type;
            type Setter = Setter;
            type Insert = #krate::InsertQuery;
            type Update = #krate::UpdateQuery;
            type Delete = #krate::DeleteQuery;

            fn insert_query(s: Setter) -> Self::Insert {
                let mut cols: Vec<&'static str> = Vec::new();
                let mut vals: Vec<keelson_core::expr::Expr> = Vec::new();
                #(#pushes)*
                #insert_stmt
                if !vals.is_empty() {
                    q.apply(#krate::insert::values(vals));
                }
                q
            }

            fn update_query() -> Self::Update {
                #krate::update(#krate::update::table(#krate::quote(#table)))
            }

            fn apply_setter(s: Setter, q: &mut Self::Update) {
                #(#sets)*
            }

            fn delete_query() -> Self::Delete {
                #krate::delete(#krate::delete::from(#krate::quote(#table)))
            }

            #pk_allow
            fn pk(row: &#row) -> Self::Pk {
                #pk_expr
            }

            #(#hook_items)*
        }
    })
}

/// Rust integer types the read-back can take from
/// `ExecResult::last_insert_id` (an `i64`) — the only keys MySQL's
/// `AUTO_INCREMENT` can produce.
const INTEGER_KEYS: &[&str] = &[
    "i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64", "usize", "isize",
];

/// `by_pk(...)`: the keyed read-back that stands in for `RETURNING`.
///
/// A second statement, emitted as a function so its SQL is judged like any
/// other. Returns the item and the parameter names, which the insert path
/// then binds its captured key values to.
fn by_pk_item(
    m: &Model,
    dial: &Dial,
    pk_cols: &[&ModelColumn],
) -> Result<(TokenStream, Vec<proc_macro2::Ident>)> {
    let krate = &dial.krate;
    let table = &m.table;
    let mut by_pk_params = Vec::new();
    let mut by_pk_filters = Vec::new();
    let mut key_names = Vec::new();
    for (i, c) in pk_cols.iter().enumerate() {
        let name = quote::format_ident!("k{i}");
        let ty = parse_type(&c.rust_type, "primary key")?;
        let col_fn = ident(&c.field);
        by_pk_params.push(quote!(#name: #ty));
        by_pk_filters.push(quote!(#col_fn().eq(#name)));
        key_names.push(name);
    }
    let by_pk_doc = format!(
        " The keyed read-back: `{table}`'s own columns, filtered by primary \
         key. This is what stands in for `RETURNING` — a second statement, \
         emitted as a function so its SQL is judged like any other."
    );
    let by_pk_item = quote! {
        #[doc = #by_pk_doc]
        pub fn by_pk(#(#by_pk_params),*) -> #krate::SelectQuery {
            #krate::select((
                #krate::select::columns(all_columns()),
                #krate::select::from(#krate::quote(#table)),
                #(#by_pk_filters,)*
            ))
        }
    };
    Ok((by_pk_item, key_names))
}

/// Reading the primary key out of the setter before the insert consumes it,
/// and resolving what MySQL's `last_insert_id` can and cannot supply.
///
/// Returns the capture statements, the resolutions, and whether anything
/// reads the insert's result — a composite key has no `last_insert_id`
/// fallback, and an unused binding is a warning in the reader's crate.
fn key_capture(
    m: &Model,
    pk_cols: &[&ModelColumn],
    key_names: &[proc_macro2::Ident],
) -> Result<(Vec<TokenStream>, Vec<TokenStream>, bool)> {
    let table = &m.table;
    let mut captures = Vec::new();
    let mut resolves = Vec::new();
    let mut uses_last_insert_id = false;
    for (i, c) in pk_cols.iter().enumerate() {
        let name = &key_names[i];
        let f = ident(&c.field);
        let take = if is_copy(&c.rust_type) {
            quote!(Some(*v))
        } else {
            quote!(Some(v.clone()))
        };
        captures.push(quote! {
            let #name = match &setter.#f {
                keelson_models::Set::Value(v) => #take,
                _ => None,
            };
        });
        let ty = parse_type(&c.rust_type, "primary key")?;
        // The `last_insert_id` fallback exists only for a single integer key:
        // that is the only thing MySQL's AUTO_INCREMENT produces.
        let has_fallback = pk_cols.len() == 1 && INTEGER_KEYS.contains(&c.rust_type.as_str());
        uses_last_insert_id |= has_fallback;
        let fallback = if has_fallback {
            let msg = format!(
                "{table}: the INSERT set no `{}` and MySQL reported no last_insert_id, \
                 so the inserted row cannot be read back",
                c.db_name
            );
            quote! {
                let #name: #ty = #name
                    .or_else(|| done.last_insert_id.and_then(|id| <#ty as std::convert::TryFrom<i64>>::try_from(id).ok()))
                    .ok_or_else(|| keelson_exec::ExecError::other(#msg))?;
            }
        } else {
            let msg = format!(
                "{table}: `{}` must be set to insert with `one()` — MySQL reports \
                 last_insert_id only for a single AUTO_INCREMENT integer key",
                c.db_name
            );
            quote! {
                let #name: #ty = #name
                    .ok_or_else(|| keelson_exec::ExecError::other(#msg))?;
            }
        };
        resolves.push(fallback);
    }
    Ok((captures, resolves, uses_last_insert_id))
}

/// The mutation surface for a dialect without `RETURNING` (MySQL): the
/// `Insert` that inserts and then re-`SELECT`s by key, the keyed read-back
/// query itself, and `Update`/`Delete` wrappers that offer `exec` and no
/// `all`.
///
/// The honesty this shape buys, and its cost, are recorded in
/// `keelson-models/tests/spec_mysql.rs`: the read-back is a *second*
/// statement, so it is not atomic with the `INSERT` and on a bare pool need
/// not even share its connection — wrap the call in a transaction when that
/// matters. `last_insert_id` itself is safe: it arrives in the `INSERT`'s own
/// OK packet.
fn no_returning_surface(
    m: &Model,
    dial: &Dial,
    marker: &proc_macro2::Ident,
    row: &proc_macro2::Ident,
) -> Result<TokenStream> {
    let krate = &dial.krate;
    let table = &m.table;
    let pk_cols: Vec<&ModelColumn> = m.pk.iter().map(|i| &m.columns[*i]).collect();
    let (by_pk_item, key_names) = by_pk_item(m, dial, &pk_cols)?;
    let (captures, resolves, uses_last_insert_id) = key_capture(m, &pk_cols, &key_names)?;
    // A composite key has no `last_insert_id` fallback, so nothing reads the
    // insert's result — and an unused binding is a warning in the user's
    // crate.
    let done_binding = if uses_last_insert_id {
        quote!(done)
    } else {
        quote!(_done)
    };

    let insert_doc = format!(
        " A pending `INSERT` on `{table}`: the setter, held unbuilt so \
         `before_insert` can still rewrite it, plus the deferred Layer 1 mods."
    );
    let one_doc = " Insert, then read the row back by key. **Not** `RETURNING`: the two \
                   statements are not atomic — see the module's dialect notes.";

    Ok(quote! {
        impl #marker {
            #[doc = " A `SELECT` over this model — the dialect-generic path."]
            pub fn query(
                self,
                mods: impl keelson_core::Mod<keelson_models::ModelSelect<#marker>>,
            ) -> keelson_models::ModelSelect<#marker> {
                keelson_models::ModelTable::<#marker>::new().query(mods)
            }

            #[doc = " An `INSERT` of the setter's set fields, read back by key."]
            pub fn insert(self, setter: Setter) -> Insert {
                Insert { setter, mods: Vec::new() }
            }

            #[doc = " An `UPDATE` of the setter's set fields — `exec` only."]
            pub fn update(
                self,
                setter: Setter,
                mods: impl keelson_core::Mod<keelson_models::ModelUpdate<#marker>>,
            ) -> Update {
                Update(keelson_models::ModelTable::<#marker>::new().update(setter, mods))
            }

            #[doc = " A `DELETE` — `exec` only."]
            pub fn delete(
                self,
                mods: impl keelson_core::Mod<keelson_models::ModelDelete<#marker>>,
            ) -> Delete {
                Delete(keelson_models::ModelTable::<#marker>::new().delete(mods))
            }
        }

        #[doc = #insert_doc]
        pub struct Insert {
            setter: Setter,
            #[allow(clippy::type_complexity)]
            mods: Vec<Box<dyn FnOnce(&mut #krate::InsertQuery) + Send>>,
        }

        impl std::fmt::Debug for Insert {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct("Insert")
                    .field("setter", &self.setter)
                    .field("mods", &self.mods.len())
                    .finish()
            }
        }

        impl Insert {
            #[doc = " Defer Layer 1 mods onto the eventual statement."]
            #[must_use]
            pub fn with(
                mut self,
                mods: impl keelson_core::Mod<#krate::InsertQuery> + Send + 'static,
            ) -> Self {
                self.mods.push(Box::new(move |q| mods.apply(q)));
                self
            }

            #[doc = #one_doc]
            pub async fn one(
                self,
                db: &dyn keelson_exec::Executor,
            ) -> Result<#row, keelson_exec::ExecError> {
                use keelson_exec::Execute as _;
                let Insert { mut setter, mods } = self;
                <#marker as keelson_models::Table>::before_insert(db, &mut setter).await?;
                #(#captures)*
                let mut q = <#marker as keelson_models::Table>::insert_query(setter);
                for m in mods {
                    m(&mut q);
                }
                let #done_binding = q.execute(db).await?;
                #(#resolves)*
                let row: #row = by_pk(#(#key_names),*).fetch_one(db).await?;
                <#marker as keelson_models::Table>::after_insert(db, std::slice::from_ref(&row))
                    .await?;
                Ok(row)
            }

            #[doc = " Insert for the side effect. `after_insert` still runs, with an empty row slice."]
            pub async fn exec(
                self,
                db: &dyn keelson_exec::Executor,
            ) -> Result<keelson_exec::ExecResult, keelson_exec::ExecError> {
                use keelson_exec::Execute as _;
                let Insert { mut setter, mods } = self;
                <#marker as keelson_models::Table>::before_insert(db, &mut setter).await?;
                let mut q = <#marker as keelson_models::Table>::insert_query(setter);
                for m in mods {
                    m(&mut q);
                }
                let done = q.execute(db).await?;
                <#marker as keelson_models::Table>::after_insert(db, &[]).await?;
                Ok(done)
            }
        }

        #by_pk_item

        #[doc = " A pending `UPDATE`. `exec` only: with no `RETURNING` there is nothing for an `all` verb to decode."]
        #[derive(Debug)]
        pub struct Update(keelson_models::ModelUpdate<#marker>);

        impl Update {
            #[doc = " Apply mods written against the concrete statement."]
            pub fn apply(&mut self, mods: impl keelson_core::Mod<#krate::UpdateQuery>) {
                self.0.apply(mods);
            }

            #[doc = " Update for the side effect; answers how many rows changed."]
            pub async fn exec(
                self,
                db: &dyn keelson_exec::Executor,
            ) -> Result<keelson_exec::ExecResult, keelson_exec::ExecError> {
                self.0.exec(db).await
            }
        }

        #[doc = " A pending `DELETE`. `exec` only, for the same reason as `Update`."]
        #[derive(Debug)]
        pub struct Delete(keelson_models::ModelDelete<#marker>);

        impl Delete {
            #[doc = " Apply mods written against the concrete statement."]
            pub fn apply(&mut self, mods: impl keelson_core::Mod<#krate::DeleteQuery>) {
                self.0.apply(mods);
            }

            #[doc = " Delete for the side effect; answers how many rows went."]
            pub async fn exec(
                self,
                db: &dyn keelson_exec::Executor,
            ) -> Result<keelson_exec::ExecResult, keelson_exec::ExecError> {
                self.0.exec(db).await
            }
        }
    })
}

fn preload_mod(
    m: &Model,
    all: &[Model],
    dial: &Dial,
    marker: &proc_macro2::Ident,
    row: &proc_macro2::Ident,
) -> Result<TokenStream> {
    let krate = &dial.krate;
    let table = &m.table;
    let mut items = Vec::new();

    for b in &m.belongs_to {
        let target = all
            .iter()
            .find(|t| t.table == b.target)
            .ok_or_else(|| GenError::Config(format!("relation target `{}` missing", b.target)))?;
        let name = ident(&b.name);
        let from_fn = ident(&format!("{}_from_preload", b.name));
        let (tmod, trow) = (ident(&target.table), ident(&target.row));
        let target_table = &target.table;
        let fk_col = &m.columns[b.fk_column].db_name;
        let ref_col = &b.ref_column;
        let probe = format!("{}.{}", b.name, ref_col);

        let aliased = target.columns.iter().map(|c| {
            let n = &c.db_name;
            let prefixed = format!("{}.{}", b.name, c.db_name);
            quote!(#krate::quote((#target_table, #n)).as_(#prefixed))
        });
        let preload_cols = if target.columns.len() <= 16 {
            quote!(#krate::select::preload_columns((#(#aliased,)*)))
        } else {
            quote!(#krate::select::preload_columns(vec![#(#aliased,)*]))
        };
        let takes = target.columns.iter().map(|c| {
            let f = ident(&c.field);
            let prefixed = format!("{}.{}", b.name, c.db_name);
            quote!(#f: row.take(#prefixed)?)
        });
        let target_rel_init = if target.holds_relations() {
            quote!(rel: super::super::#tmod::Rel::default(),)
        } else {
            quote!()
        };

        let fn_doc = format!(
            " Same-query `LEFT JOIN` preload of the to-one `{}`.",
            b.name
        );
        let from_doc =
            " Decode the prefixed columns; the joined key column decides a `LEFT JOIN` miss.";
        items.push(quote! {
            #[doc = #fn_doc]
            pub fn #name() -> impl keelson_core::Mod<keelson_models::ModelSelect<super::#marker>> {
                keelson_core::mod_fn(|q: &mut keelson_models::ModelSelect<super::#marker>| {
                    use #krate::Chain as _;
                    use #krate::Mod as _;
                    (
                        #krate::select::left_join(#krate::quote(#target_table))
                            .on(#krate::quote((#target_table, #ref_col))
                                .eq(#krate::quote((#table, #fk_col)))),
                        #preload_cols,
                    )
                        .apply(q);
                    q.add_mapper_mod(keelson_models::mapper_mod(
                        |row, parent: &mut super::#row| {
                            parent.rel.#name = #from_fn(row)?.map(Box::new);
                            Ok(())
                        },
                    ));
                })
            }

            #[doc = #from_doc]
            pub fn #from_fn(
                row: &mut keelson_exec::Row,
            ) -> Result<Option<super::super::#tmod::#trow>, keelson_exec::ExecError> {
                if matches!(row.value(#probe), None | Some(#krate::Value::Null)) {
                    return Ok(None);
                }
                Ok(Some(super::super::#tmod::#trow {
                    #(#takes,)*
                    #target_rel_init
                }))
            }
        });
    }

    Ok(quote! {
        #[doc = " Preload mods: the relation joins into the *same* query — no \
                  second statement, and deliberately no `.then(…)`. A level \
                  below a join has no distinct child set to key on, so a \
                  nested path is spelled with `then_load` \
                  (see `keelson_models::ThenLoad`)."]
        pub mod preload {
            #(#items)*
        }
    })
}

fn then_load_mod(
    m: &Model,
    all: &[Model],
    marker: &proc_macro2::Ident,
    row: &proc_macro2::Ident,
) -> Result<TokenStream> {
    let mut items = Vec::new();

    // A belongs-to is the to-one direction of the same shape: this model's
    // foreign key is the parent key, and the target's referenced column is
    // what the keyed query filters on.
    for b in &m.belongs_to {
        let target = all
            .iter()
            .find(|t| t.table == b.target)
            .ok_or_else(|| GenError::Config(format!("relation target `{}` missing", b.target)))?;
        let fk = &m.columns[b.fk_column];
        let (_, ref_c) = target.column(&b.ref_column).ok_or_else(|| {
            GenError::Config(format!(
                "relation {}.{} references missing column {}.{}",
                m.table, fk.db_name, b.target, b.ref_column
            ))
        })?;
        items.push(then_load_item(
            marker, row, &b.name, target, fk, ref_c, false,
        )?);
    }

    for h in &m.has_many {
        let child = all
            .iter()
            .find(|t| t.table == h.child)
            .ok_or_else(|| GenError::Config(format!("relation child `{}` missing", h.child)))?;
        let (_, parent_c) = m.column(&h.parent_key_column).ok_or_else(|| {
            GenError::Config(format!(
                "back-reference key {}.{} is not generated",
                m.table, h.parent_key_column
            ))
        })?;
        let (_, child_fk) = child.column(&h.child_fk_column).ok_or_else(|| {
            GenError::Config(format!(
                "back-reference key {}.{} is not generated",
                h.child, h.child_fk_column
            ))
        })?;
        // `cardinality = "one_to_one"` attaches at most one child row.
        items.push(then_load_item(
            marker, row, &h.name, child, parent_c, child_fk, !h.to_one,
        )?);
    }

    Ok(quote! {
        #[doc = " Then-load mods: one keyed, batched query per level of a \
                  load path — `then_load::a().then(b::then_load::c())` is two \
                  levels, two queries, checked by the compiler."]
        pub mod then_load {
            #(#items)*
        }
    })
}

/// One then-load mod, in the one shape both relation directions have.
///
/// A belongs-to and a has-many differ in which model is on which end and
/// whether the attach takes one child or many. Everything else — collect the
/// parent keys, filter the other model by them, compare the two key columns
/// with the nullability bridged — is the same, and was written out twice
/// with the roles renamed.
///
/// `parent` is the column on *this* model whose values are the keys;
/// `child` is the column on `other` the keyed query filters, and the one the
/// attachment compares against.
#[allow(clippy::too_many_arguments)] // six of them are one relation, spelled out
fn then_load_item(
    marker: &proc_macro2::Ident,
    row: &proc_macro2::Ident,
    name: &str,
    other: &Model,
    parent: &ModelColumn,
    child: &ModelColumn,
    to_many: bool,
) -> Result<TokenStream> {
    let fn_name = ident(name);
    let omod = ident(&other.table);
    let omarker = ident(&other.marker);
    let orow = ident(&other.row);
    let child_fn = ident(&child.field);
    let kty = parse_type(&parent.rust_type, "relation key")?;
    let parent_f = ident(&parent.field);
    let child_f = ident(&child.field);

    // A key is `Option<K>` on both sides, so a nullable column is passed
    // through and a `NOT NULL` one is wrapped. That is the whole of the
    // nullability bridging: a `None` matches nothing, on either side.
    let key_of = |c: &ModelColumn, recv: TokenStream, f: &proc_macro2::Ident| {
        let read = if is_copy(&c.rust_type) {
            quote!(#recv.#f)
        } else {
            quote!(#recv.#f.clone())
        };
        if c.nullable {
            read
        } else {
            quote!(Some(#read))
        }
    };
    let parent_key = key_of(parent, quote!(r), &parent_f);
    let child_key = key_of(child, quote!(c), &child_f);
    let allow = if needs_copy_allow(&parent.rust_type) || needs_copy_allow(&child.rust_type) {
        quote!(#[allow(clippy::clone_on_copy)])
    } else {
        quote!()
    };

    let (arity, attach) = if to_many {
        (
            "to-many",
            quote! {
                keelson_models::Attach::Many(|r: &mut super::#row, cs| {
                    r.rel.#fn_name = cs;
                })
            },
        )
    } else {
        (
            "to-one",
            quote! {
                keelson_models::Attach::One(|r: &mut super::#row, c| {
                    r.rel.#fn_name = c.map(Box::new);
                })
            },
        )
    };

    let doc = format!(
        " Load each row's `{name}` ({arity}), one keyed query per batch of keys \
         — `.then(…)` hangs the next level of the path off this one."
    );
    Ok(quote! {
        #[doc = #doc]
        #allow
        pub fn #fn_name() -> keelson_models::ThenLoad<
            super::#marker,
            super::super::#omod::#omarker,
            #kty,
        > {
            keelson_models::ThenLoad::new(keelson_models::Relation {
                parent_key: |r: &super::#row| #parent_key,
                child_key: |c: &super::super::#omod::#orow| #child_key,
                filter: |keys, q| keelson_core::Mod::apply(
                    super::super::#omod::#child_fn().in_(keys),
                    q,
                ),
                attach: #attach,
            })
        }
    })
}
