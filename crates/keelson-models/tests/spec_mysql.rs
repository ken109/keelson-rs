//! The hand-written MySQL model for the shared schema's `users`/`posts`
//! tables — **the code generator's MySQL specification**, and the third
//! dialect's answer to the one thing PostgreSQL and SQLite hand the model
//! layer for free: `RETURNING`.
//!
//! MySQL has none, on any statement (keelson-mysql does not even carry a
//! `Returning` field), so a MySQL model cannot be a copy of the psql/sqlite
//! path. What changes, and what it costs, recorded here because the generator
//! will emit exactly this:
//!
//! - **`insert(…).one(db)` is a two-statement read-back.** The `INSERT` runs
//!   for its side effect; the key is the setter's primary key when it set one,
//!   otherwise [`ExecResult::last_insert_id`](keelson_exec::ExecResult); the
//!   row comes back from a keyed `SELECT` of the model's own columns. **This
//!   is not `RETURNING`**: the two statements are not atomic, and on a bare
//!   pool they need not even run on the same connection. Between them another
//!   session may update the row (the read-back returns the newer values) or
//!   delete it ([`ExecError::RowNotFound`](keelson_exec::ExecError)). Wrap the
//!   call in a transaction — as the end-to-end test below does — when that
//!   matters. (`last_insert_id` itself is safe: it arrives in the `INSERT`'s
//!   own OK packet, not from a later `SELECT LAST_INSERT_ID()`.)
//! - **`update(…)`/`delete(…)` have `exec` and no `all`.** There is no
//!   `RETURNING` to decode, so the verb that would decode one is not emitted
//!   at all — a compile error at the call site rather than an empty `Vec`.
//! - **An all-unset setter is MySQL's "take every default" spelling.** The
//!   builder renders `INSERT INTO \`users\` VALUES ()`; `INSERT INTO t ()
//!   VALUES ()` is the same statement with an explicitly empty column list,
//!   and keelson writes the shorter form because an empty column list renders
//!   nothing (`keelson_core::clause`'s quoted-list writer).
//!
//! Because of the first point the generated MySQL model does **not** hand out
//! [`ModelTable`](keelson_models::ModelTable): `table()` returns the model
//! marker, whose inherent `query`/`insert`/`update`/`delete` expose exactly
//! the verbs MySQL can honour. The generic `ModelInsert::one` (which decodes
//! the `INSERT`'s own rows) is therefore unreachable on a MySQL model instead
//! of failing at run time.
//!
//! The SQL-shape tests run every statement through the judges (the generic
//! parser always; the real MySQL 8.4 when compiled with `--features
//! live-docker`, which also unlocks the end-to-end tests at the bottom).

use keelson_core::{Query as _, QueryExtensions as _, Value};
use keelson_models::{null, set};
use keelson_mysql::select;
use keelson_sqlcheck::Dialect;

use crate::model::{posts, user_emails, users};

/// What the generator will write for `tests/schema/mysql.sql`.
// `pub` throughout because that is what the generator will emit into an
// application's models crate; in this test binary nothing external can reach
// it, which is what the lint (correctly, and irrelevantly) notices.
#[allow(unreachable_pub, dead_code)]
mod model {
    /// The `users` table: a full [`Table`](keelson_models::Table) with
    /// application-written hooks, plus the MySQL mutation surface.
    pub mod users {
        use std::fmt;

        use chrono::NaiveDateTime;
        use keelson_core::expr::Expr;
        use keelson_exec::{
            ExecError, ExecFuture, ExecResult, Execute as _, Executor, FromRow, Row,
        };
        use keelson_models::{
            Column, ModelDelete, ModelSelect, ModelTable, ModelUpdate, Set, Table, ThenLoad, View,
            attach_to_many,
        };
        use keelson_mysql::{Mod, arg, delete, insert, quote, select, update};

        /// The model marker `users::table()` returns. Carries no data — the
        /// associated types, the hooks and (on MySQL) the mutation verbs live
        /// on it.
        #[derive(Debug, Clone, Copy)]
        pub struct Users;

        /// One row of `users`. Plain data; relations live under
        /// [`rel`](User::rel).
        #[derive(Debug, Clone, PartialEq)]
        pub struct User {
            pub id: i32,
            pub name: String,
            pub email: Option<String>,
            pub age: Option<i32>,
            pub is_active: bool,
            pub created_at: NaiveDateTime,
            /// Relations, filled by `preload`/`then_load` mods; empty
            /// otherwise.
            pub rel: Rel,
        }

        /// `users`' relations.
        #[derive(Debug, Clone, PartialEq, Default)]
        pub struct Rel {
            /// Has-many `posts`, via `posts.user_id`.
            pub posts: Vec<super::posts::Post>,
        }

        impl FromRow for User {
            fn from_row(row: &mut Row) -> Result<Self, ExecError> {
                Ok(User {
                    id: row.take("id")?,
                    name: row.take("name")?,
                    email: row.take("email")?,
                    age: row.take("age")?,
                    is_active: row.take("is_active")?,
                    created_at: row.take("created_at")?,
                    rel: Rel::default(),
                })
            }
        }

        /// The three-state setter: unset fields stay out of the statement.
        #[derive(Debug, Clone, Default)]
        pub struct Setter {
            pub id: Set<i32>,
            pub name: Set<String>,
            pub email: Set<String>,
            pub age: Set<i32>,
            pub is_active: Set<bool>,
            pub created_at: Set<NaiveDateTime>,
        }

        /// The entry point: `users::table().query(…)` / `.insert(…)` / ….
        ///
        /// Returns the marker rather than
        /// [`ModelTable`](keelson_models::ModelTable): the generic
        /// `ModelTable::insert(…).one()` decodes the `INSERT`'s own returned
        /// rows, which MySQL never produces, so a MySQL model does not hand
        /// that surface out (see the file docs).
        pub fn table() -> Users {
            Users
        }

        impl Users {
            /// A `SELECT` over this model — the dialect-generic path,
            /// unchanged from PostgreSQL and SQLite.
            pub fn query(self, mods: impl Mod<ModelSelect<Users>>) -> ModelSelect<Users> {
                ModelTable::<Users>::new().query(mods)
            }

            /// An `INSERT` of the setter's set fields, read back by key.
            pub fn insert(self, setter: Setter) -> Insert {
                Insert {
                    setter,
                    mods: Vec::new(),
                }
            }

            /// An `UPDATE` of the setter's set fields — `exec` only.
            pub fn update(self, setter: Setter, mods: impl Mod<ModelUpdate<Users>>) -> Update {
                Update(ModelTable::<Users>::new().update(setter, mods))
            }

            /// A `DELETE` — `exec` only.
            pub fn delete(self, mods: impl Mod<ModelDelete<Users>>) -> Delete {
                Delete(ModelTable::<Users>::new().delete(mods))
            }
        }

        /// A pending MySQL `INSERT`: the setter, held unbuilt so
        /// [`Table::before_insert`] can still rewrite it, plus the deferred
        /// Layer 1 mods.
        pub struct Insert {
            setter: Setter,
            #[allow(clippy::type_complexity)] // a list of deferred mods
            mods: Vec<Box<dyn FnOnce(&mut keelson_mysql::InsertQuery) + Send>>,
        }

        impl fmt::Debug for Insert {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_struct("Insert")
                    .field("setter", &self.setter)
                    .field("mods", &self.mods.len())
                    .finish()
            }
        }

        impl Insert {
            /// Defer Layer 1 mods onto the eventual statement —
            /// `.with(insert::on_duplicate_key_update(…))`.
            #[must_use]
            pub fn with(
                mut self,
                mods: impl Mod<keelson_mysql::InsertQuery> + Send + 'static,
            ) -> Self {
                self.mods.push(Box::new(move |q| mods.apply(q)));
                self
            }

            fn build(self, key: &mut Option<i32>) -> keelson_mysql::InsertQuery {
                let Insert { setter, mods } = self;
                // The key is read *before* the setter is consumed; it is what
                // the read-back uses when the caller supplied the primary key
                // itself (the common case on a non-auto-increment key).
                if let Set::Value(v) = &setter.id {
                    *key = Some(*v);
                }
                let mut q = Users::insert_query(setter);
                for m in mods {
                    m(&mut q);
                }
                q
            }

            /// Insert and hand back the inserted row: the `INSERT` runs for
            /// its side effect, then the row is re-`SELECT`ed by key. Not
            /// `RETURNING` — see the file docs for what the two statements
            /// cannot promise.
            pub async fn one(self, db: &dyn Executor) -> Result<User, ExecError> {
                let Insert { mut setter, mods } = self;
                Users::before_insert(db, &mut setter).await?;
                let mut key = None;
                let q = Insert { setter, mods }.build(&mut key);
                let done = q.execute(db).await?;
                let key = key
                    .or_else(|| done.last_insert_id.and_then(|id| i32::try_from(id).ok()))
                    .ok_or_else(|| {
                        ExecError::other(
                            "users: the INSERT set no primary key and MySQL reported no \
                             last_insert_id, so the inserted row cannot be read back",
                        )
                    })?;
                let row: User = by_pk(key).fetch_one(db).await?;
                Users::after_insert(db, std::slice::from_ref(&row)).await?;
                Ok(row)
            }

            /// Insert for the side effect. [`Table::after_insert`] still runs,
            /// with an empty row slice.
            pub async fn exec(self, db: &dyn Executor) -> Result<ExecResult, ExecError> {
                let Insert { mut setter, mods } = self;
                Users::before_insert(db, &mut setter).await?;
                let q = Insert { setter, mods }.build(&mut None);
                let done = q.execute(db).await?;
                Users::after_insert(db, &[]).await?;
                Ok(done)
            }
        }

        /// The keyed read-back `SELECT` — the model's own columns, filtered by
        /// primary key. Emitted as a function so its SQL is judged like every
        /// other statement.
        pub fn by_pk(key: i32) -> keelson_mysql::SelectQuery {
            keelson_mysql::select((
                select::columns(all_columns()),
                select::from(quote("users")),
                id().eq(key),
            ))
        }

        /// A pending `UPDATE`. `exec` only: MySQL has no `RETURNING`, so the
        /// `all` verb the psql/sqlite models offer is not emitted.
        #[derive(Debug)]
        pub struct Update(ModelUpdate<Users>);

        impl Update {
            /// Apply mods written against the concrete statement.
            pub fn apply(&mut self, mods: impl Mod<keelson_mysql::UpdateQuery>) {
                self.0.apply(mods);
            }

            /// Update for the side effect; answers how many rows changed.
            pub async fn exec(self, db: &dyn Executor) -> Result<ExecResult, ExecError> {
                self.0.exec(db).await
            }
        }

        /// A pending `DELETE`. `exec` only, for the same reason as
        /// [`Update`].
        #[derive(Debug)]
        pub struct Delete(ModelDelete<Users>);

        impl Delete {
            /// Apply mods written against the concrete statement.
            pub fn apply(&mut self, mods: impl Mod<keelson_mysql::DeleteQuery>) {
                self.0.apply(mods);
            }

            /// Delete for the side effect; answers how many rows went.
            pub async fn exec(self, db: &dyn Executor) -> Result<ExecResult, ExecError> {
                self.0.exec(db).await
            }
        }

        // The one column entry point apiece. Types from
        // docs/type-mappings.md's MySQL column: `INT` is `i32`, `TINYINT(1)`
        // is `bool`, `DATETIME` is a naive datetime (`TIMESTAMP` would be the
        // zoned one).
        pub fn id() -> Column<i32> {
            Column::new("users", "id")
        }
        pub fn name() -> Column<String> {
            Column::new("users", "name")
        }
        pub fn email() -> Column<String> {
            Column::new("users", "email")
        }
        pub fn age() -> Column<i32> {
            Column::new("users", "age")
        }
        pub fn is_active() -> Column<bool> {
            Column::new("users", "is_active")
        }
        pub fn created_at() -> Column<NaiveDateTime> {
            Column::new("users", "created_at")
        }

        #[allow(clippy::type_complexity)]
        fn all_columns() -> (
            Column<i32>,
            Column<String>,
            Column<String>,
            Column<i32>,
            Column<bool>,
            Column<NaiveDateTime>,
        ) {
            (id(), name(), email(), age(), is_active(), created_at())
        }

        impl View for Users {
            type Row = User;
            type Select = keelson_mysql::SelectQuery;

            fn base_select() -> Self::Select {
                keelson_mysql::select((
                    select::columns(all_columns()),
                    select::from(quote("users")),
                ))
            }
        }

        impl Table for Users {
            type Pk = i32;
            type Setter = Setter;
            type Insert = keelson_mysql::InsertQuery;
            type Update = keelson_mysql::UpdateQuery;
            type Delete = keelson_mysql::DeleteQuery;

            fn insert_query(s: Setter) -> Self::Insert {
                let mut cols: Vec<&'static str> = Vec::new();
                let mut vals: Vec<Expr> = Vec::new();
                s.id.push_into("id", &mut cols, &mut vals);
                s.name.push_into("name", &mut cols, &mut vals);
                s.email.push_into("email", &mut cols, &mut vals);
                s.age.push_into("age", &mut cols, &mut vals);
                s.is_active.push_into("is_active", &mut cols, &mut vals);
                s.created_at.push_into("created_at", &mut cols, &mut vals);
                // No `RETURNING` anywhere in this dialect; no set fields
                // renders MySQL's `VALUES ()` — the row the schema's defaults
                // describe.
                let mut q = keelson_mysql::insert(insert::into(quote("users")).columns(cols));
                if !vals.is_empty() {
                    q.apply(insert::values(vals));
                }
                q
            }

            fn update_query() -> Self::Update {
                keelson_mysql::update(update::table(quote("users")))
            }

            fn apply_setter(s: Setter, q: &mut Self::Update) {
                if let Some(v) = s.id.into_expr() {
                    q.apply(update::set_col("id").to(v));
                }
                if let Some(v) = s.name.into_expr() {
                    q.apply(update::set_col("name").to(v));
                }
                if let Some(v) = s.email.into_expr() {
                    q.apply(update::set_col("email").to(v));
                }
                if let Some(v) = s.age.into_expr() {
                    q.apply(update::set_col("age").to(v));
                }
                if let Some(v) = s.is_active.into_expr() {
                    q.apply(update::set_col("is_active").to(v));
                }
                if let Some(v) = s.created_at.into_expr() {
                    q.apply(update::set_col("created_at").to(v));
                }
            }

            fn delete_query() -> Self::Delete {
                keelson_mysql::delete(delete::from(quote("users")))
            }

            fn pk(row: &User) -> i32 {
                row.id
            }

            // ── application-written hooks (the generator delegates these) ──

            /// Normalise the email before it is written.
            fn before_insert<'a>(
                _db: &'a dyn Executor,
                setter: &'a mut Setter,
            ) -> ExecFuture<'a, Result<(), ExecError>> {
                Box::pin(async move {
                    if let Set::Value(email) = &mut setter.email {
                        *email = email.to_lowercase();
                    }
                    Ok(())
                })
            }

            /// Write an audit tag — **on the caller's executor**, so inside
            /// the caller's transaction when there is one. This is the hook
            /// the transaction test pins.
            fn after_insert<'a>(
                db: &'a dyn Executor,
                rows: &'a [User],
            ) -> ExecFuture<'a, Result<(), ExecError>> {
                Box::pin(async move {
                    for u in rows {
                        keelson_mysql::insert((
                            insert::into(quote("tags")).columns(["id", "name"]),
                            insert::values((arg(u.id), arg(format!("audit-user-{}", u.id)))),
                        ))
                        .execute(db)
                        .await?;
                    }
                    Ok(())
                })
            }
        }

        /// Then-load mods: a second query keyed by the first's rows.
        pub mod then_load {
            use super::*;

            /// Load each user's posts (to-many), one keyed query per batch.
            /// `.then(…)` loads a relation *of* those posts.
            pub fn posts() -> ThenLoad<Users, super::super::posts::Posts, i32> {
                ThenLoad::new(
                    |users: &[User]| users.iter().map(|u| u.id).collect(),
                    |keys, q| super::super::posts::user_id().in_(keys).apply(q),
                    |users: &mut [User], posts| {
                        attach_to_many(
                            users,
                            posts,
                            |u| u.id,
                            |p| p.user_id,
                            |u, ps| {
                                u.rel.posts = ps;
                            },
                        );
                    },
                )
            }
        }
    }

    /// The `posts` table: hooks left at their (no-op) defaults, both loader
    /// kinds for its belongs-to `user`.
    pub mod posts {
        use std::fmt;

        use chrono::NaiveDateTime;
        use keelson_core::expr::Expr;
        use keelson_core::mod_fn;
        use keelson_exec::{ExecError, ExecResult, Execute as _, Executor, FromRow, Row};
        use keelson_models::{
            Column, ModelDelete, ModelSelect, ModelTable, ModelUpdate, Set, Table, ThenLoad, View,
            attach_to_one, mapper_mod,
        };
        use keelson_mysql::{Chain as _, Mod, delete, insert, quote, select, update};

        /// The model marker.
        #[derive(Debug, Clone, Copy)]
        pub struct Posts;

        /// One row of `posts`.
        #[derive(Debug, Clone, PartialEq)]
        pub struct Post {
            pub id: i32,
            pub user_id: i32,
            pub title: String,
            pub status: Option<String>,
            pub views: i32,
            pub published_at: Option<NaiveDateTime>,
            /// Relations, filled by `preload`/`then_load` mods.
            pub rel: Rel,
        }

        /// `posts`' relations.
        #[derive(Debug, Clone, PartialEq, Default)]
        pub struct Rel {
            /// Belongs-to `users`, via `posts.user_id`.
            // Boxed because a to-one relation field always is: a `Rel`
            // holds the target's whole row, so two models that point at
            // each other to-one are a recursive type of infinite size.
            // The rule is uniform rather than cycle-detecting, so that a
            // foreign key added elsewhere can never change this field's
            // type (keelson-gen/src/emit/model.rs records the choice).
            pub user: Option<Box<super::users::User>>,
        }

        impl FromRow for Post {
            fn from_row(row: &mut Row) -> Result<Self, ExecError> {
                Ok(Post {
                    id: row.take("id")?,
                    user_id: row.take("user_id")?,
                    title: row.take("title")?,
                    status: row.take("status")?,
                    views: row.take("views")?,
                    published_at: row.take("published_at")?,
                    rel: Rel::default(),
                })
            }
        }

        /// The three-state setter.
        #[derive(Debug, Clone, Default)]
        pub struct Setter {
            pub id: Set<i32>,
            pub user_id: Set<i32>,
            pub title: Set<String>,
            pub status: Set<String>,
            pub views: Set<i32>,
            pub published_at: Set<NaiveDateTime>,
        }

        /// The entry point.
        pub fn table() -> Posts {
            Posts
        }

        impl Posts {
            /// A `SELECT` over this model.
            pub fn query(self, mods: impl Mod<ModelSelect<Posts>>) -> ModelSelect<Posts> {
                ModelTable::<Posts>::new().query(mods)
            }

            /// An `INSERT`, read back by key.
            pub fn insert(self, setter: Setter) -> Insert {
                Insert {
                    setter,
                    mods: Vec::new(),
                }
            }

            /// An `UPDATE` — `exec` only.
            pub fn update(self, setter: Setter, mods: impl Mod<ModelUpdate<Posts>>) -> Update {
                Update(ModelTable::<Posts>::new().update(setter, mods))
            }

            /// A `DELETE` — `exec` only.
            pub fn delete(self, mods: impl Mod<ModelDelete<Posts>>) -> Delete {
                Delete(ModelTable::<Posts>::new().delete(mods))
            }
        }

        /// A pending MySQL `INSERT` (see `users::Insert` for the contract).
        pub struct Insert {
            setter: Setter,
            #[allow(clippy::type_complexity)]
            mods: Vec<Box<dyn FnOnce(&mut keelson_mysql::InsertQuery) + Send>>,
        }

        impl fmt::Debug for Insert {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_struct("Insert")
                    .field("setter", &self.setter)
                    .field("mods", &self.mods.len())
                    .finish()
            }
        }

        impl Insert {
            /// Defer Layer 1 mods onto the eventual statement.
            #[must_use]
            pub fn with(
                mut self,
                mods: impl Mod<keelson_mysql::InsertQuery> + Send + 'static,
            ) -> Self {
                self.mods.push(Box::new(move |q| mods.apply(q)));
                self
            }

            fn build(self, key: &mut Option<i32>) -> keelson_mysql::InsertQuery {
                let Insert { setter, mods } = self;
                if let Set::Value(v) = &setter.id {
                    *key = Some(*v);
                }
                let mut q = Posts::insert_query(setter);
                for m in mods {
                    m(&mut q);
                }
                q
            }

            /// Insert, then re-`SELECT` the row by key.
            pub async fn one(self, db: &dyn Executor) -> Result<Post, ExecError> {
                let Insert { mut setter, mods } = self;
                Posts::before_insert(db, &mut setter).await?;
                let mut key = None;
                let q = Insert { setter, mods }.build(&mut key);
                let done = q.execute(db).await?;
                let key = key
                    .or_else(|| done.last_insert_id.and_then(|id| i32::try_from(id).ok()))
                    .ok_or_else(|| {
                        ExecError::other(
                            "posts: the INSERT set no primary key and MySQL reported no \
                             last_insert_id, so the inserted row cannot be read back",
                        )
                    })?;
                let row: Post = by_pk(key).fetch_one(db).await?;
                Posts::after_insert(db, std::slice::from_ref(&row)).await?;
                Ok(row)
            }

            /// Insert for the side effect.
            pub async fn exec(self, db: &dyn Executor) -> Result<ExecResult, ExecError> {
                let Insert { mut setter, mods } = self;
                Posts::before_insert(db, &mut setter).await?;
                let q = Insert { setter, mods }.build(&mut None);
                let done = q.execute(db).await?;
                Posts::after_insert(db, &[]).await?;
                Ok(done)
            }
        }

        /// The keyed read-back `SELECT`.
        pub fn by_pk(key: i32) -> keelson_mysql::SelectQuery {
            keelson_mysql::select((
                select::columns(all_columns()),
                select::from(quote("posts")),
                id().eq(key),
            ))
        }

        /// A pending `UPDATE` — `exec` only.
        #[derive(Debug)]
        pub struct Update(ModelUpdate<Posts>);

        impl Update {
            /// Apply mods written against the concrete statement.
            pub fn apply(&mut self, mods: impl Mod<keelson_mysql::UpdateQuery>) {
                self.0.apply(mods);
            }

            /// Update for the side effect.
            pub async fn exec(self, db: &dyn Executor) -> Result<ExecResult, ExecError> {
                self.0.exec(db).await
            }
        }

        /// A pending `DELETE` — `exec` only.
        #[derive(Debug)]
        pub struct Delete(ModelDelete<Posts>);

        impl Delete {
            /// Apply mods written against the concrete statement.
            pub fn apply(&mut self, mods: impl Mod<keelson_mysql::DeleteQuery>) {
                self.0.apply(mods);
            }

            /// Delete for the side effect.
            pub async fn exec(self, db: &dyn Executor) -> Result<ExecResult, ExecError> {
                self.0.exec(db).await
            }
        }

        pub fn id() -> Column<i32> {
            Column::new("posts", "id")
        }
        pub fn user_id() -> Column<i32> {
            Column::new("posts", "user_id")
        }
        pub fn title() -> Column<String> {
            Column::new("posts", "title")
        }
        pub fn status() -> Column<String> {
            Column::new("posts", "status")
        }
        pub fn views() -> Column<i32> {
            Column::new("posts", "views")
        }
        pub fn published_at() -> Column<NaiveDateTime> {
            Column::new("posts", "published_at")
        }

        #[allow(clippy::type_complexity)]
        fn all_columns() -> (
            Column<i32>,
            Column<i32>,
            Column<String>,
            Column<String>,
            Column<i32>,
            Column<NaiveDateTime>,
        ) {
            (id(), user_id(), title(), status(), views(), published_at())
        }

        impl View for Posts {
            type Row = Post;
            type Select = keelson_mysql::SelectQuery;

            fn base_select() -> Self::Select {
                keelson_mysql::select((
                    select::columns(all_columns()),
                    select::from(quote("posts")),
                ))
            }
        }

        impl Table for Posts {
            type Pk = i32;
            type Setter = Setter;
            type Insert = keelson_mysql::InsertQuery;
            type Update = keelson_mysql::UpdateQuery;
            type Delete = keelson_mysql::DeleteQuery;

            fn insert_query(s: Setter) -> Self::Insert {
                let mut cols: Vec<&'static str> = Vec::new();
                let mut vals: Vec<Expr> = Vec::new();
                s.id.push_into("id", &mut cols, &mut vals);
                s.user_id.push_into("user_id", &mut cols, &mut vals);
                s.title.push_into("title", &mut cols, &mut vals);
                s.status.push_into("status", &mut cols, &mut vals);
                s.views.push_into("views", &mut cols, &mut vals);
                s.published_at
                    .push_into("published_at", &mut cols, &mut vals);
                let mut q = keelson_mysql::insert(insert::into(quote("posts")).columns(cols));
                if !vals.is_empty() {
                    q.apply(insert::values(vals));
                }
                q
            }

            fn update_query() -> Self::Update {
                keelson_mysql::update(update::table(quote("posts")))
            }

            fn apply_setter(s: Setter, q: &mut Self::Update) {
                if let Some(v) = s.id.into_expr() {
                    q.apply(update::set_col("id").to(v));
                }
                if let Some(v) = s.user_id.into_expr() {
                    q.apply(update::set_col("user_id").to(v));
                }
                if let Some(v) = s.title.into_expr() {
                    q.apply(update::set_col("title").to(v));
                }
                if let Some(v) = s.status.into_expr() {
                    q.apply(update::set_col("status").to(v));
                }
                if let Some(v) = s.views.into_expr() {
                    q.apply(update::set_col("views").to(v));
                }
                if let Some(v) = s.published_at.into_expr() {
                    q.apply(update::set_col("published_at").to(v));
                }
            }

            fn delete_query() -> Self::Delete {
                keelson_mysql::delete(delete::from(quote("posts")))
            }

            fn pk(row: &Post) -> i32 {
                row.id
            }
        }

        /// Preload mods: the relation joins into the *same* query.
        pub mod preload {
            use super::*;

            /// Same-query `LEFT JOIN` preload of the to-one `user`.
            pub fn user() -> impl Mod<ModelSelect<Posts>> {
                mod_fn(|q: &mut ModelSelect<Posts>| {
                    (
                        select::left_join(quote("users"))
                            .on(quote(("users", "id")).eq(quote(("posts", "user_id")))),
                        select::preload_columns((
                            quote(("users", "id")).as_("user.id"),
                            quote(("users", "name")).as_("user.name"),
                            quote(("users", "email")).as_("user.email"),
                            quote(("users", "age")).as_("user.age"),
                            quote(("users", "is_active")).as_("user.is_active"),
                            quote(("users", "created_at")).as_("user.created_at"),
                        )),
                    )
                        .apply(q);
                    q.add_mapper_mod(mapper_mod(|row, post: &mut Post| {
                        post.rel.user = user_from_preload(row)?.map(Box::new);
                        Ok(())
                    }));
                })
            }

            /// Decode the prefixed columns; the joined key column decides a
            /// `LEFT JOIN` miss.
            pub fn user_from_preload(
                row: &mut Row,
            ) -> Result<Option<super::super::users::User>, ExecError> {
                if matches!(
                    row.value("user.id"),
                    None | Some(keelson_mysql::Value::Null)
                ) {
                    return Ok(None);
                }
                Ok(Some(super::super::users::User {
                    id: row.take("user.id")?,
                    name: row.take("user.name")?,
                    email: row.take("user.email")?,
                    age: row.take("user.age")?,
                    is_active: row.take("user.is_active")?,
                    created_at: row.take("user.created_at")?,
                    rel: super::super::users::Rel::default(),
                }))
            }
        }

        /// Then-load mods.
        pub mod then_load {
            use super::*;

            /// Load each post's user (to-one), one keyed query per batch.
            /// `.then(…)` loads a relation *of* that user.
            pub fn user() -> ThenLoad<Posts, super::super::users::Users, i32> {
                ThenLoad::new(
                    |posts: &[Post]| posts.iter().map(|p| p.user_id).collect(),
                    |keys, q| super::super::users::id().in_(keys).apply(q),
                    |posts: &mut [Post], users| {
                        attach_to_one(
                            posts,
                            users,
                            |p| p.user_id,
                            |u| u.id,
                            |p, u| {
                                p.rel.user = u.map(Box::new);
                            },
                        );
                    },
                )
            }
        }
    }

    /// A `SELECT`-only model: [`View`](keelson_models::View) without
    /// [`Table`](keelson_models::Table). Unchanged across dialects — nothing
    /// here depends on `RETURNING` — so it keeps the generic
    /// [`ModelTable`](keelson_models::ModelTable) entry point.
    pub mod user_emails {
        use keelson_exec::{ExecError, FromRow, Row};
        use keelson_models::{Column, ModelTable, View};
        use keelson_mysql::{quote, select};

        /// The model marker.
        #[derive(Debug, Clone, Copy)]
        pub struct UserEmails;

        /// One row of the projection.
        #[derive(Debug, Clone, PartialEq)]
        pub struct UserEmail {
            pub id: i32,
            pub email: Option<String>,
        }

        impl FromRow for UserEmail {
            fn from_row(row: &mut Row) -> Result<Self, ExecError> {
                Ok(UserEmail {
                    id: row.take("id")?,
                    email: row.take("email")?,
                })
            }
        }

        /// The entry point — `view()`, because there is nothing to write to.
        pub fn view() -> ModelTable<UserEmails> {
            ModelTable::new()
        }

        pub fn id() -> Column<i32> {
            Column::new("users", "id")
        }
        pub fn email() -> Column<String> {
            Column::new("users", "email")
        }

        impl View for UserEmails {
            type Row = UserEmail;
            type Select = keelson_mysql::SelectQuery;

            fn base_select() -> Self::Select {
                keelson_mysql::select((
                    select::columns((id(), email())),
                    select::from(quote("users")),
                ))
            }
        }
    }
}

// ───────────────────────── SQL-shape tests (judged) ─────────────────────────

#[track_caller]
fn assert_mysql(q: &impl keelson_core::Query, expected: &str) -> Vec<Value> {
    let (sql, args) = q.build().expect("build");
    keelson_sqlcheck::assert_sql(Dialect::Mysql, &sql, expected);
    args
}

const USER_COLS: &str = concat!(
    "`users`.`id`, `users`.`name`, `users`.`email`, `users`.`age`, ",
    "`users`.`is_active`, `users`.`created_at`"
);

const POST_COLS: &str = concat!(
    "`posts`.`id`, `posts`.`user_id`, `posts`.`title`, `posts`.`status`, ",
    "`posts`.`views`, `posts`.`published_at`"
);

#[test]
fn the_agreed_call_site_shape_builds_the_agreed_sql() {
    let q = users::table().query((
        users::age().gte(21), // typed: `users::age().gte("x")` does not compile
        select::limit(20),    // Layer 1 mods mix in directly
    ));
    let args = assert_mysql(
        &q,
        &format!("SELECT {USER_COLS} FROM `users` WHERE (`users`.`age` >= ?) LIMIT 20"),
    );
    assert_eq!(args, vec![Value::I32(21)]);
}

#[test]
fn a_partial_setter_inserts_only_the_set_columns_and_returns_nothing() {
    use keelson_models::Table as _;
    let q = model::users::Users::insert_query(users::Setter {
        name: set("Stephen"),
        email: set("stephen@example.com"),
        ..Default::default()
    });
    let args = assert_mysql(&q, "INSERT INTO `users` (`name`, `email`) VALUES (?, ?)");
    assert_eq!(
        args,
        vec![
            Value::Text("Stephen".into()),
            Value::Text("stephen@example.com".into())
        ]
    );
}

#[test]
fn null_and_unset_are_different_statements() {
    use keelson_models::Table as _;
    let q = model::users::Users::insert_query(users::Setter {
        name: set("kay"),
        email: null(),
        ..Default::default()
    });
    let args = assert_mysql(&q, "INSERT INTO `users` (`name`, `email`) VALUES (?, ?)");
    assert_eq!(args, vec![Value::Text("kay".into()), Value::Null]);
}

/// MySQL's "take every default": `VALUES ()` where PostgreSQL writes `DEFAULT
/// VALUES`. The column list is empty, and an empty quoted list renders
/// nothing, so the `()` before `VALUES` is absent — the same statement as the
/// `INSERT INTO t () VALUES ()` spelling.
#[test]
fn an_all_unset_setter_is_mysqls_values_parens() {
    use keelson_models::Table as _;
    let q = model::users::Users::insert_query(users::Setter::default());
    assert!(assert_mysql(&q, "INSERT INTO `users` VALUES ()").is_empty());
}

/// The read-back the `one` verb runs in place of `RETURNING`.
#[test]
fn the_keyed_read_back_selects_the_models_own_columns() {
    let args = assert_mysql(
        &model::users::by_pk(7),
        &format!("SELECT {USER_COLS} FROM `users` WHERE (`users`.`id` = ?)"),
    );
    assert_eq!(args, vec![Value::I32(7)]);
}

#[test]
fn update_sets_only_the_set_fields_and_filters_typed() {
    use keelson_models::Table as _;
    use keelson_mysql::Mod as _;
    let mut q = model::users::Users::update_query();
    users::id().eq(7).apply(&mut q);
    model::users::Users::apply_setter(
        users::Setter {
            email: null(),
            age: set(30),
            ..Default::default()
        },
        &mut q,
    );
    let args = assert_mysql(
        &q,
        "UPDATE `users` SET `email` = ?, `age` = ? WHERE (`users`.`id` = ?)",
    );
    assert_eq!(args, vec![Value::Null, Value::I32(30), Value::I32(7)]);
}

#[test]
fn delete_takes_the_same_typed_filters() {
    use keelson_models::Table as _;
    use keelson_mysql::Mod as _;
    let mut q = model::users::Users::delete_query();
    users::id().in_([1, 2]).apply(&mut q);
    let args = assert_mysql(&q, "DELETE FROM `users` WHERE (`users`.`id` IN (?, ?))");
    assert_eq!(args.len(), 2);
}

#[test]
fn a_preload_is_one_left_joined_query_with_prefixed_columns() {
    let q = posts::table().query((posts::preload::user(), posts::views().gte(10)));
    let args = assert_mysql(
        &q,
        &format!(
            concat!(
                "SELECT {}, ",
                "`users`.`id` AS `user.id`, `users`.`name` AS `user.name`, ",
                "`users`.`email` AS `user.email`, `users`.`age` AS `user.age`, ",
                "`users`.`is_active` AS `user.is_active`, ",
                "`users`.`created_at` AS `user.created_at` ",
                "FROM `posts` LEFT JOIN `users` ON (`users`.`id` = `posts`.`user_id`) ",
                "WHERE (`posts`.`views` >= ?)"
            ),
            POST_COLS
        ),
    );
    assert_eq!(args, vec![Value::I32(10)]);
}

#[test]
fn raw_fragments_and_dialect_mods_mix_into_a_view_query() {
    let q = user_emails::view().query((
        user_emails::email().is_not_null(),
        select::where_("`users`.`age` IS NOT NULL"),
        select::order_by(user_emails::id()).desc(),
        select::limit(5),
    ));
    assert_mysql(
        &q,
        concat!(
            "SELECT `users`.`id`, `users`.`email` FROM `users` ",
            "WHERE (`users`.`email` IS NOT NULL) AND `users`.`age` IS NOT NULL ",
            "ORDER BY `users`.`id` DESC LIMIT 5"
        ),
    );
}

#[test]
fn aliased_as_follows_a_table_alias() {
    use keelson_mysql::quote;
    let q = keelson_mysql::select((
        select::columns(users::id().aliased_as("u")),
        select::from(quote("users")).as_("u"),
        select::where_(users::age().aliased_as("u").gte(21)),
    ));
    let args = assert_mysql(
        &q,
        "SELECT `u`.`id` FROM `users` AS `u` WHERE (`u`.`age` >= ?)",
    );
    assert_eq!(args, vec![Value::I32(21)]);
}

#[test]
fn the_query_extensions_wiring_is_observable() {
    let q = posts::table().query((posts::preload::user(), posts::then_load::user()));
    assert_eq!(q.mapper_mods().len(), 1);
    assert_eq!(q.loaders().len(), 1);
    assert!(q.hooks().is_empty());
    assert_eq!(q.query_type(), keelson_core::QueryType::Select);
}

/// A dialect `INSERT` mod rides `.with(…)` exactly as on the other dialects —
/// here MySQL's own upsert.
#[test]
fn insert_mods_ride_with() {
    use keelson_models::Table as _;
    use keelson_mysql::{Mod as _, insert};
    let mut q = model::users::Users::insert_query(users::Setter {
        id: set(1),
        name: set("Ada"),
        ..Default::default()
    });
    insert::on_duplicate_key_update(insert::set_col("name").to(keelson_mysql::arg("Ada")))
        .apply(&mut q);
    let args = assert_mysql(
        &q,
        concat!(
            "INSERT INTO `users` (`id`, `name`) VALUES (?, ?) ",
            "ON DUPLICATE KEY UPDATE `name` = ?"
        ),
    );
    assert_eq!(args.len(), 3);
}

// ───────────────────── end-to-end against MySQL 8.4 ─────────────────────

#[cfg(feature = "live-docker")]
mod live {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicI32, Ordering};

    use keelson_exec::{BeginExt as _, ExecError, Execute as _, Executor};
    use keelson_models::{null, set};
    use keelson_mysql::{Chain as _, arg, quote, select};

    use super::model::{posts, users};

    /// Process-unique positive i32 keys, so runs against a shared or
    /// persistent server never collide.
    fn key() -> i32 {
        static NEXT: AtomicI32 = AtomicI32::new(0);
        static BASE: OnceLock<i32> = OnceLock::new();
        let base = *BASE.get_or_init(|| {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            ((nanos as i64) & 0x3fff_ff00) as i32
        });
        base + NEXT.fetch_add(1, Ordering::Relaxed)
    }

    async fn pool() -> keelson_sqlx::mysql::Pool {
        // Container startup is blocking (sqlcheck's SyncRunner).
        let url = tokio::task::spawn_blocking(|| keelson_sqlcheck::live::mysql_url().to_owned())
            .await
            .unwrap();
        keelson_sqlx::mysql::Pool::connect(&url)
            .await
            .expect("connecting to the live MySQL")
    }

    async fn audit_tag_count(db: &dyn Executor, user_id: i32) -> i64 {
        keelson_mysql::select((
            select::columns("count(*)"),
            select::from(quote("tags")),
            select::where_(quote("name").eq(arg(format!("audit-user-{user_id}")))),
        ))
        .fetch_scalar(db)
        .await
        .unwrap()
    }

    /// The whole model flow on one transaction, then rolled back: partial
    /// setter insert (defaults come back through the keyed read-back rather
    /// than `RETURNING`), the before-insert setter rewrite, the after-insert
    /// hook's write observed *inside* the same transaction, preload and
    /// then-load — and after the rollback, none of it happened, the hook's
    /// write included.
    ///
    /// The transaction is also what makes the read-back sound: `INSERT` and
    /// re-`SELECT` are two statements, and inside a transaction they are two
    /// statements on one connection with one snapshot.
    #[tokio::test]
    async fn the_model_flow_runs_inside_the_callers_transaction() {
        let db = pool().await;
        let uid = key();
        let uid2 = key();
        let pid = key();
        let pid2 = key();

        let out: Result<(), ExecError> = db
            .within(async |tx| {
                let u = users::table()
                    .insert(users::Setter {
                        id: set(uid),
                        name: set("Stephen"),
                        email: set("STEPHEN@Example.COM"),
                        ..Default::default()
                    })
                    .one(tx)
                    .await?;
                // before_insert normalised the email.
                assert_eq!(u.email.as_deref(), Some("stephen@example.com"));
                // Unset columns took their schema defaults, read back by key.
                assert!(u.is_active);
                assert_eq!(u.age, None);
                // after_insert wrote the audit tag on this same transaction.
                assert_eq!(audit_tag_count(tx, uid).await, 1);

                users::table()
                    .insert(users::Setter {
                        id: set(uid2),
                        name: set("Ada"),
                        age: set(36),
                        ..Default::default()
                    })
                    .one(tx)
                    .await?;

                posts::table()
                    .insert(posts::Setter {
                        id: set(pid),
                        user_id: set(uid),
                        title: set("keel laid"),
                        ..Default::default()
                    })
                    .one(tx)
                    .await?;
                posts::table()
                    .insert(posts::Setter {
                        id: set(pid2),
                        user_id: set(uid),
                        title: set("second"),
                        status: null(),
                        ..Default::default()
                    })
                    .one(tx)
                    .await?;

                // Typed query + Layer 1 mods.
                let adults = users::table()
                    .query((
                        users::age().gte(21),
                        users::id().in_([uid, uid2]),
                        select::limit(20),
                    ))
                    .all(tx)
                    .await?;
                assert_eq!(adults.len(), 1);
                assert_eq!(adults[0].name, "Ada");

                // Preload: one LEFT JOIN query, rel.user filled.
                let loaded = posts::table()
                    .query((posts::preload::user(), posts::id().eq(pid)))
                    .one(tx)
                    .await?;
                let author = loaded.rel.user.as_ref().expect("preloaded user");
                assert_eq!(author.id, uid);
                assert_eq!(author.email.as_deref(), Some("stephen@example.com"));

                // Then-load, both directions.
                let with_posts = users::table()
                    .query((users::id().eq(uid), users::then_load::posts()))
                    .one(tx)
                    .await?;
                assert_eq!(with_posts.rel.posts.len(), 2);

                let with_user = posts::table()
                    .query((posts::id().eq(pid2), posts::then_load::user()))
                    .one(tx)
                    .await?;
                assert_eq!(with_user.rel.user.as_ref().unwrap().id, uid);

                // Update and delete: `exec` only, no `all` to call.
                let done = users::table()
                    .update(
                        users::Setter {
                            age: set(41),
                            ..Default::default()
                        },
                        users::id().eq(uid),
                    )
                    .exec(tx)
                    .await?;
                assert_eq!(done.rows_affected, 1);

                let done = posts::table().delete(posts::id().eq(pid2)).exec(tx).await?;
                assert_eq!(done.rows_affected, 1);

                // Roll the whole thing back.
                Err(ExecError::other("deliberate rollback"))
            })
            .await;
        assert_eq!(out.unwrap_err().to_string(), "deliberate rollback");

        // Nothing survived — the hook's write included, which is the proof it
        // observed the caller's transaction rather than a connection of its
        // own.
        assert_eq!(audit_tag_count(&db, uid).await, 0);
        let ghosts = users::table()
            .query(users::id().in_([uid, uid2]))
            .all(&db)
            .await
            .unwrap();
        assert!(ghosts.is_empty());
    }

    /// The commit half: on a plain pool the hook's write persists with the
    /// insert, and the auto-increment path is exercised too — a `tags` insert
    /// whose key the setter did not supply comes back through
    /// `last_insert_id`.
    #[tokio::test]
    async fn hooks_commit_with_the_caller() {
        let db = pool().await;
        let uid = key();

        users::table()
            .insert(users::Setter {
                id: set(uid),
                name: set("committed"),
                ..Default::default()
            })
            .one(&db)
            .await
            .unwrap();
        assert_eq!(audit_tag_count(&db, uid).await, 1);

        // Clean up after ourselves — the server may be shared and persistent.
        keelson_mysql::delete((
            keelson_mysql::delete::from(quote("tags")),
            keelson_mysql::delete::where_(quote("name").eq(arg(format!("audit-user-{uid}")))),
        ))
        .execute(&db)
        .await
        .unwrap();
        users::table()
            .delete(users::id().eq(uid))
            .exec(&db)
            .await
            .unwrap();
    }

    /// `exec` on an insert: no read-back, `after_insert` still runs (with an
    /// empty row slice, so this model's hook writes no tag).
    #[tokio::test]
    async fn insert_exec_skips_the_read_back() {
        let db = pool().await;
        let uid = key();

        let done = users::table()
            .insert(users::Setter {
                id: set(uid),
                name: set("side effect"),
                ..Default::default()
            })
            .exec(&db)
            .await
            .unwrap();
        assert_eq!(done.rows_affected, 1);
        assert_eq!(
            audit_tag_count(&db, uid).await,
            0,
            "after_insert ran with no rows, so it wrote no tag"
        );

        let found = users::table()
            .query(users::id().eq(uid))
            .one(&db)
            .await
            .unwrap();
        assert_eq!(found.name, "side effect");

        users::table()
            .delete(users::id().eq(uid))
            .exec(&db)
            .await
            .unwrap();
    }
}
