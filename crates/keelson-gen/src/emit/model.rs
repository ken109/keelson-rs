//! One model → one file: the marker, row, `Rel`, `FromRow`, `Setter`,
//! column fns, `View`/`Table` impls, hook delegations and the
//! `preload`/`then_load` mod modules — the shapes the hand-written spec in
//! `keelson-models/tests/` fixes.

use proc_macro2::TokenStream;
use quote::quote;

use crate::config::{Config, Hook};
use crate::error::{GenError, Result};
use crate::names::ident;
use crate::resolve::{Model, ModelColumn};
use crate::schema::TableKind;

use super::Dial;

/// Rust types the generator knows are `Copy` (so a key access needs no
/// `.clone()`); everything else clones, and types outside
/// [`KNOWN_CLONE`] additionally get a `clone_on_copy` allow because the
/// generator cannot know whether an override type is `Copy`.
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

/// Rust types the generator knows are *not* `Copy` — cloning them is not a
/// `clone_on_copy` candidate, so no allow is needed.
const KNOWN_CLONE: &[&str] = &["String", "Vec<u8>", "serde_json::Value"];

fn is_copy(rust_type: &str) -> bool {
    KNOWN_COPY.contains(&rust_type)
}

fn needs_copy_allow(rust_type: &str) -> bool {
    !is_copy(rust_type) && !KNOWN_CLONE.contains(&rust_type)
}

fn parse_type(rust_type: &str, what: &str) -> Result<syn::Type> {
    syn::parse_str(rust_type)
        .map_err(|e| GenError::Config(format!("{what}: `{rust_type}` is not a Rust type: {e}")))
}

/// `row.field` / `row.field.clone()` as the column's type demands.
fn key_access(recv: TokenStream, c: &ModelColumn) -> TokenStream {
    let f = ident(&c.field);
    if is_copy(&c.rust_type) {
        quote!(#recv.#f)
    } else {
        quote!(#recv.#f.clone())
    }
}

/// The same access with an `Option` mismatch bridged: when only one side of
/// an attachment key is nullable, the non-null side wraps in `Some`.
fn key_access_leveled(recv: TokenStream, c: &ModelColumn, other_nullable: bool) -> TokenStream {
    let access = key_access(recv, c);
    if !c.nullable && other_nullable {
        quote!(Some(#access))
    } else {
        access
    }
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
    let is_table = m.kind == TableKind::Table;

    let hooks_path: syn::Path = syn::parse_str(&config.hooks.module).map_err(|e| {
        GenError::Config(format!(
            "[hooks] module `{}` is not a module path: {e}",
            config.hooks.module
        ))
    })?;
    let table_mod = ident(table);

    // ── the marker ──
    let entry = if is_table { "table" } else { "view" };
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
    if is_table {
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

    // ── Rel ──
    let rel_item = if is_table {
        let mut rel_fields = Vec::new();
        for b in &m.belongs_to {
            let f = ident(&b.name);
            let (tmod, trow) = target_names(all, &b.target)?;
            let fk = &m.columns[b.fk_column].db_name;
            let doc = format!(" Belongs-to `{}`, via `{table}.{fk}`.", b.target);
            rel_fields.push(quote! {
                #[doc = #doc]
                pub #f: Option<super::#tmod::#trow>
            });
        }
        for h in &m.has_many {
            let f = ident(&h.name);
            let (cmod, crow) = target_names(all, &h.child)?;
            let doc = format!(
                " Has-many `{}`, via `{}.{}`.",
                h.child, h.child, h.child_fk_column
            );
            rel_fields.push(quote! {
                #[doc = #doc]
                pub #f: Vec<super::#cmod::#crow>
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
    let rel_init = if is_table {
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
    let entry_fn = ident(entry);
    let entry_doc = format!(
        " The entry point: `{table}::{entry}().query(…)`{}.",
        if is_table {
            " / `.insert(…)` / …"
        } else {
            " — a SELECT-only model"
        }
    );
    let entry_item = quote! {
        #[doc = #entry_doc]
        pub fn #entry_fn() -> keelson_models::ModelTable<#marker> {
            keelson_models::ModelTable::new()
        }
    };

    // ── column fns and all_columns ──
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

    // ── preload / then_load ──
    let preload_item = if is_table && !m.belongs_to.is_empty() {
        preload_mod(m, all, dial, &marker, &row)?
    } else {
        quote!()
    };
    let then_load_item = if is_table && (!m.belongs_to.is_empty() || !m.has_many.is_empty()) {
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
                let mut q = #krate::insert((
                    #krate::insert::into(#krate::quote(#table)).columns(cols),
                    #krate::insert::returning(all_columns()),
                ));
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
        let target_rel_init = if target.kind == TableKind::Table {
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
                            parent.rel.#name = #from_fn(row)?;
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
        #[doc = " Preload mods: the relation joins into the *same* query."]
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
        let name = ident(&b.name);
        let load_fn = ident(&format!("load_{}", b.name));
        let tmod = ident(&target.table);
        let ref_fn = ident(&ref_c.field);
        let kty = parse_type(&fk.rust_type, "relation key")?;
        let fk_f = ident(&fk.field);

        let collect = match (fk.nullable, is_copy(&fk.rust_type)) {
            (false, true) => quote!(rows.iter().map(|r| r.#fk_f).collect()),
            (false, false) => quote!(rows.iter().map(|r| r.#fk_f.clone()).collect()),
            (true, true) => quote!(rows.iter().filter_map(|r| r.#fk_f).collect()),
            (true, false) => quote!(rows.iter().filter_map(|r| r.#fk_f.clone()).collect()),
        };
        let parent_key = key_access_leveled(quote!(r), fk, ref_c.nullable);
        let child_key = key_access_leveled(quote!(c), ref_c, fk.nullable);
        let allow = if needs_copy_allow(&fk.rust_type) || needs_copy_allow(&ref_c.rust_type) {
            quote!(#[allow(clippy::clone_on_copy)])
        } else {
            quote!()
        };

        let doc = format!(
            " Load each row's `{}` (to-one) with one keyed second query.",
            b.name
        );
        items.push(quote! {
            #[doc = #doc]
            pub fn #name() -> impl keelson_core::Mod<keelson_models::ModelSelect<super::#marker>> {
                keelson_core::mod_fn(|q: &mut keelson_models::ModelSelect<super::#marker>| {
                    q.add_loader(keelson_models::loader(|db, rows| {
                        Box::pin(#load_fn(db, rows))
                    }));
                })
            }

            #allow
            async fn #load_fn(
                db: &dyn keelson_exec::Executor,
                rows: &mut [super::#row],
            ) -> Result<(), keelson_exec::ExecError> {
                let mut keys: Vec<#kty> = #collect;
                keys.sort_unstable();
                keys.dedup();
                if keys.is_empty() {
                    return Ok(());
                }
                let related = super::super::#tmod::table()
                    .query(super::super::#tmod::#ref_fn().in_(keys))
                    .all(db)
                    .await?;
                keelson_models::attach_to_one(
                    rows,
                    related,
                    |r| #parent_key,
                    |c| #child_key,
                    |r, c| {
                        r.rel.#name = c;
                    },
                );
                Ok(())
            }
        });
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
        let name = ident(&h.name);
        let load_fn = ident(&format!("load_{}", h.name));
        let cmod = ident(&child.table);
        let fk_fn = ident(&child_fk.field);
        let kty = parse_type(&parent_c.rust_type, "relation key")?;
        let key_f = ident(&parent_c.field);

        let collect = match (parent_c.nullable, is_copy(&parent_c.rust_type)) {
            (false, true) => quote!(rows.iter().map(|r| r.#key_f).collect()),
            (false, false) => quote!(rows.iter().map(|r| r.#key_f.clone()).collect()),
            (true, true) => quote!(rows.iter().filter_map(|r| r.#key_f).collect()),
            (true, false) => quote!(rows.iter().filter_map(|r| r.#key_f.clone()).collect()),
        };
        let parent_key = key_access_leveled(quote!(r), parent_c, child_fk.nullable);
        let child_key = key_access_leveled(quote!(c), child_fk, parent_c.nullable);
        let allow =
            if needs_copy_allow(&parent_c.rust_type) || needs_copy_allow(&child_fk.rust_type) {
                quote!(#[allow(clippy::clone_on_copy)])
            } else {
                quote!()
            };

        let doc = format!(
            " Load each row's `{}` (to-many) with one keyed second query.",
            h.name
        );
        items.push(quote! {
            #[doc = #doc]
            pub fn #name() -> impl keelson_core::Mod<keelson_models::ModelSelect<super::#marker>> {
                keelson_core::mod_fn(|q: &mut keelson_models::ModelSelect<super::#marker>| {
                    q.add_loader(keelson_models::loader(|db, rows| {
                        Box::pin(#load_fn(db, rows))
                    }));
                })
            }

            #allow
            async fn #load_fn(
                db: &dyn keelson_exec::Executor,
                rows: &mut [super::#row],
            ) -> Result<(), keelson_exec::ExecError> {
                let mut keys: Vec<#kty> = #collect;
                keys.sort_unstable();
                keys.dedup();
                if keys.is_empty() {
                    return Ok(());
                }
                let related = super::super::#cmod::table()
                    .query(super::super::#cmod::#fk_fn().in_(keys))
                    .all(db)
                    .await?;
                keelson_models::attach_to_many(
                    rows,
                    related,
                    |r| #parent_key,
                    |c| #child_key,
                    |r, cs| {
                        r.rel.#name = cs;
                    },
                );
                Ok(())
            }
        });
    }

    Ok(quote! {
        #[doc = " Then-load mods: a second query keyed by the first's rows."]
        pub mod then_load {
            #(#items)*
        }
    })
}
