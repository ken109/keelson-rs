//! The typed model layer — the runtime the code generator will emit against.
//!
//! Nothing is generated yet: this crate is the machinery ([`View`]/[`Table`],
//! the [`Set`] three-state setter, hooks, preload/then-load plumbing), and the
//! hand-written users/posts model in `tests/` is the generator's
//! specification — what it will write, written once by hand and tested end to
//! end. The call-site shape being served:
//!
//! ```ignore
//! use models::users;
//!
//! let adults = users::table().query((
//!     users::age().gte(21),   // typed: passing &str is a compile error
//!     select::limit(20),      // Layer 1 mods mix in directly
//! )).all(&db).await?;
//!
//! let u = users::table().insert(users::Setter {
//!     name: set("Stephen"),
//!     ..Default::default()
//! }).one(&db).await?;
//! ```
//!
//! # Where this sits
//!
//! Layer 3 of keelson: the runtime a generated model is written against. It
//! stands on Layer 1 (the dialect crates, through [keelson-core](https://docs.rs/keelson-core)'s
//! clause traits) and Layer 2 ([keelson-exec](https://docs.rs/keelson-exec), through
//! `&dyn Executor`), and names no dialect and no driver itself. The models that
//! use it are written by Layer 4, [keelson-gen](https://docs.rs/keelson-gen) — you can write
//! one by hand, and the `tests/` in this crate are exactly that. Test data for
//! those models is [keelson-factory](https://docs.rs/keelson-factory). The whole map is the
//! [keelson](https://docs.rs/keelson) facade crate.
//!
//! # The decisions, recorded
//!
//! **One column entry point.** bob splits a column across four generated
//! surfaces (`ColumnNames`/`Columns`/`SelectWhere`/`Preload`); here
//! `users::age()` is one [`Column<i32>`] that is the expression, the typed
//! filter origin and the alias carrier at once. The column's Rust type comes
//! from `docs/type-mappings.md`; comparisons take `impl Into<T>`, so
//! `age().gte(21)` compiles and `age().gte("x")` does not (pinned by a
//! `compile_fail` doctest on [`Column::eq`]).
//!
//! **Setter three states by type.** [`Set<T>`] is `Unset | Null | Value(T)`
//! with `Default = Unset`, built by [`set`]/[`null`]; an unset field does not
//! appear in the statement at all. `Null` stays representable on `NOT NULL`
//! columns — the constraint is the engine's to enforce, as it is for raw SQL.
//!
//! **Hooks are trait default methods** on the model marker — static dispatch,
//! no downcasting, the deliberate departure from bob's runtime
//! type-assertion opt-in. before/after insert/update/delete, after select
//! (there is no before-select: a query mod at the same call site already *is*
//! that hook). Every hook receives `&dyn Executor` — the caller's own
//! executor — so hooks run inside the caller's transaction and cannot end it.
//! The before-mutation hooks receive the `Setter` mutably.
//!
//! **`QueryExtensions`, wired shut.** Core fixed the mechanism with
//! type-parameter payloads; keelson-exec pinned `Hook` = `ExecHook`; the
//! remaining two are pinned here, where the row-mapper lives:
//! `MapperMod` = [`MapperMod<T>`] (same-query preloads decode prefixed columns
//! into the already-mapped struct) and `Loader` = [`Loader<T>`] (typed over
//! the model, not `ExecLoader`'s `&[Row]`, because a then-loader's job is to
//! mutate decoded structs). [`ModelSelect`] implements
//! `QueryExtensions<ExecHook, Loader<_>, MapperMod<_>>` and its verbs consume
//! the extensions through that trait.
//!
//! **Loaders.** *Preload* is a same-query `LEFT JOIN` for to-one relations:
//! the generated mod joins, appends prefixed columns through the dialect's
//! `preload_columns` (kept apart from the caller's projection by
//! `SelectList`, which was designed for this), and registers a mapper mod
//! that reads `"user.id"`-style columns back — `Row`'s by-name access is what
//! makes the prefix trick work. *Then-load* is a second query keyed by the
//! first's keys, to-one and to-many, attached by
//! [`attach_to_one`]/[`attach_to_many`].
//!
//! **Nested loads are chained values, not paths in a string.** A then-load is
//! a [`ThenLoad`], and another one hangs off it:
//! `posts::then_load::user().then(users::then_load::posts())` is
//! posts → author → the author's posts in three queries, checked by the
//! compiler (the inner level must load onto *this* level's child model).
//! One batched `IN` query per level, [`KEY_BATCH`] keys at a time, over the
//! deduplicated child set — the design and its alternatives are recorded in
//! `load.rs`.
//!
//! **Relation field naming: `rel`, not bob's `r`.** The row struct carries
//! `post.rel.user` / `user.rel.posts`. `r` is a Go-ism (single-letter
//! receivers are idiomatic there; in Rust a one-letter public field reads as
//! an accident), `rel` is greppable, self-describing, and still two
//! characters shorter than `related`. The generated mod modules follow the
//! design vocabulary: `posts::preload::user()`, `posts::then_load::user()`.
//!
//! **Relation field shape: `Option<Box<Row>>` to-one, `Vec<Row>` to-many.**
//! A `Rel` field holds the target's whole row, so two models whose to-one
//! relations point at each other would be a recursive type of infinite size
//! — `post.rel.user: Option<User>` next to `user.rel.featured_post:
//! Option<Post>` does not compile. Every to-one field is therefore boxed,
//! uniformly, rather than only the ones a schema's cycles happen to need:
//! a field's type must not depend on the rest of the schema graph, or
//! adding an unrelated foreign key would silently change it. A to-many
//! field is a `Vec` and already carries its own indirection. Reading is
//! unaffected (`post.rel.user.as_ref().unwrap().name` derefs through the
//! `Box`); building one by hand is `Some(Box::new(row))`. The rejected
//! alternative and the whole argument are recorded in
//! `keelson-gen/src/emit/model.rs`.
//!
//! **View vs Table.** [`View`] is `SELECT`-only and needs no primary key;
//! [`Table`] adds insert/update/delete and requires one. The mutations are
//! bounded on `Table`, so calling `insert` on a view model is a compile
//! error, not a runtime one.
//!
//! **Layer 1 interop is structural, not special-cased.** The wrappers
//! ([`ModelSelect`], [`ModelUpdate`], [`ModelDelete`]) implement every
//! `Has*` clause trait their statement implements, so the dialect's shared
//! mods — and any raw `&str` fragment those mods accept — apply to the
//! wrapper directly, in the same tuple as typed filters. Statement-specific
//! mods (psql's `select::distinct()`) go through each wrapper's `apply`;
//! `INSERT` mods ride [`ModelInsert::with`], deferred because the statement
//! is only built after `before_insert` has seen the setter.
//!
//! **Verbs.** `all`/`one`/`optional` on select (`one` means one:
//! `RowNotFound`/`TooManyRows`, matching the execution layer); `one`/`exec`
//! on insert; `exec`/`all` on update and delete, where `all` decodes whatever
//! `RETURNING` the statement carries. The statement itself always goes
//! through keelson-exec's traced verb funnel (`fetch_rows`/`execute`), so
//! model queries appear in telemetry like every other query.
//!
//! # Per-dialect notes
//!
//! The machinery is dialect-generic — it names only keelson-core's `Has*`
//! traits and keelson-exec's `Executor`. What diverges lives in the generated
//! (here: hand-written) model:
//!
//! - **PostgreSQL** (the demonstration dialect): `RETURNING` carries
//!   `insert(...).one()` and `update/delete(...).all()`; an all-unset setter
//!   renders `INSERT INTO t DEFAULT VALUES`.
//! - **SQLite**: identical shapes (SQLite has `RETURNING` since 3.35 and
//!   `DEFAULT VALUES`); `timestamptz` columns are `TEXT`, and a column whose
//!   *default* writes the naive `CURRENT_TIMESTAMP` form is honestly typed
//!   `NaiveDateTime` by the schema-reading generator.
//! - **MySQL**: no `RETURNING` anywhere. A generated MySQL model backs
//!   `insert(...).one()` with `ExecResult::last_insert_id` plus a keyed
//!   re-`SELECT`, and offers no `update/delete(...).all()`; an all-unset
//!   setter is spelled `INSERT INTO t () VALUES ()`. The `Table` trait's
//!   `*_query` seam is per-model precisely so these differences stay inside
//!   the generator's output.

#![warn(missing_docs)]

mod column;
mod delegate;
mod load;
mod model;
mod mutate;
mod select;
mod set;
mod table;

pub use column::{Column, Filter};
pub use load::{Attach, IntoLoader, KEY_BATCH, Relation, ThenLoad, attach_to_many, attach_to_one};
pub use model::{Table, View};
pub use mutate::{ModelDelete, ModelInsert, ModelUpdate};
pub use select::{Loader, MapperMod, ModelSelect, hook, loader, mapper_mod};
pub use set::{Set, null, set};
pub use table::ModelTable;
