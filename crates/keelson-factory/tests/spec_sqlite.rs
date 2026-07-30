//! The hand-written SQLite factory for the shared schema's
//! `users`/`posts`/`comments` tables — **the factory generator's
//! specification**, and the **always-on end-to-end lane**: every test here
//! runs against real SQLite (in-process, via keelson-sqlx's `sqlite` feature)
//! in a plain `cargo test`.
//!
//! Everything in `mod fac` below is what the generator will emit for this
//! schema, written once by hand so it can be executed before a generator
//! exists — the same spec-first pattern Layer 2 used, and `mod model` is a
//! trimmed rendition of Layer 2's own spec model (hooks kept, because the
//! factory contract includes firing them; loaders dropped, because the
//! factory never touches them — the full model shape lives in
//! `keelson-models/tests/spec_sqlite.rs`).
//!
//! The shapes the generator commits to, visible below:
//!
//! - one template struct per table, one [`Source`] field per data column,
//!   one [`Parent`]/[`OptionalParent`] field per FK, one `Vec<child
//!   template>` per has-many the spec demonstrates;
//! - one mod per column (`id(v)`, `title(v)`, …, plus `*_null()` for
//!   nullable columns), parent mods in the `post(&row)` / `post_id(k)` /
//!   `for_post(template)` triple, and child mods (`with_new_post(…)`) —
//!   all values, keelson's house style;
//! - `build(&mut Faker)` (no database — no executor in the signature),
//!   `create`/`create_with`/`create_many` inserting through Layer 2's
//!   setter path, so model hooks fire;
//! - `create_with` returns a boxed [`ExecFuture`] rather than being an
//!   `async fn`, because factory graphs recurse (user → posts → user) and
//!   mutually recursive `async fn`s do not compile — the same named-fn-plus-
//!   `Box::pin` shape the model hooks already use.

use keelson_exec::{Executor, Statement};
use keelson_factory::Faker;
use keelson_models::Set;
use keelson_sqlite::{quote, select};
use keelson_sqlx::sqlite::Pool;

use crate::fac::{comments, posts, users};

/// The trimmed Layer 2 model this factory writes through (see the module
/// docs). `users` keeps the spec hooks: `before_insert` lowercases the email,
/// `after_insert` writes an `audit-user-{id}` tag on the caller's executor.
// `pub` throughout because that is what the generator emits into an
// application's crates; this test binary has no external readers.
#[allow(unreachable_pub, dead_code)]
mod model {
    /// The `users` table, hooks included.
    pub mod users {
        use chrono::NaiveDateTime;
        use keelson_core::expr::Expr;
        use keelson_exec::{ExecError, ExecFuture, Execute as _, Executor, FromRow, Row};
        use keelson_models::{Column, ModelTable, Set, Table, View};
        use keelson_sqlite::{arg, delete, insert, quote, select, update};

        /// The model marker.
        #[derive(Debug, Clone, Copy)]
        pub struct Users;

        /// One row of `users`.
        #[derive(Debug, Clone, PartialEq)]
        pub struct User {
            pub id: i64,
            pub name: String,
            pub email: Option<String>,
            pub age: Option<i64>,
            pub is_active: bool,
            pub created_at: NaiveDateTime,
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
                })
            }
        }

        /// The three-state setter.
        #[derive(Debug, Clone, Default)]
        pub struct Setter {
            pub id: Set<i64>,
            pub name: Set<String>,
            pub email: Set<String>,
            pub age: Set<i64>,
            pub is_active: Set<bool>,
            pub created_at: Set<NaiveDateTime>,
        }

        /// The entry point.
        pub fn table() -> ModelTable<Users> {
            ModelTable::new()
        }

        pub fn id() -> Column<i64> {
            Column::new("users", "id")
        }
        pub fn name() -> Column<String> {
            Column::new("users", "name")
        }
        pub fn email() -> Column<String> {
            Column::new("users", "email")
        }
        pub fn age() -> Column<i64> {
            Column::new("users", "age")
        }

        #[allow(clippy::type_complexity)]
        fn all_columns() -> (
            Column<i64>,
            Column<String>,
            Column<String>,
            Column<i64>,
            Column<bool>,
            Column<NaiveDateTime>,
        ) {
            (
                id(),
                name(),
                email(),
                age(),
                Column::new("users", "is_active"),
                Column::new("users", "created_at"),
            )
        }

        impl View for Users {
            type Row = User;
            type Select = keelson_sqlite::SelectQuery;

            fn base_select() -> Self::Select {
                keelson_sqlite::select((
                    select::columns(all_columns()),
                    select::from(quote("users")),
                ))
            }
        }

        impl Table for Users {
            type Pk = i64;
            type Setter = Setter;
            type Insert = keelson_sqlite::InsertQuery;
            type Update = keelson_sqlite::UpdateQuery;
            type Delete = keelson_sqlite::DeleteQuery;

            fn insert_query(s: Setter) -> Self::Insert {
                let mut cols: Vec<&'static str> = Vec::new();
                let mut vals: Vec<Expr> = Vec::new();
                s.id.push_into("id", &mut cols, &mut vals);
                s.name.push_into("name", &mut cols, &mut vals);
                s.email.push_into("email", &mut cols, &mut vals);
                s.age.push_into("age", &mut cols, &mut vals);
                s.is_active.push_into("is_active", &mut cols, &mut vals);
                s.created_at.push_into("created_at", &mut cols, &mut vals);
                let mut q = keelson_sqlite::insert((
                    insert::into(quote("users")).columns(cols),
                    insert::returning(all_columns()),
                ));
                if !vals.is_empty() {
                    q.apply(insert::values(vals));
                }
                q
            }

            fn update_query() -> Self::Update {
                keelson_sqlite::update(update::table(quote("users")))
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
                keelson_sqlite::delete(delete::from(quote("users")))
            }

            fn pk(row: &User) -> i64 {
                row.id
            }

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

            /// Write an audit tag on the caller's executor — what the
            /// factories-fire-hooks tests pin.
            fn after_insert<'a>(
                db: &'a dyn Executor,
                rows: &'a [User],
            ) -> ExecFuture<'a, Result<(), ExecError>> {
                Box::pin(async move {
                    for u in rows {
                        keelson_sqlite::insert((
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
    }

    /// The `posts` table: default (no-op) hooks.
    pub mod posts {
        use chrono::NaiveDateTime;
        use keelson_core::expr::Expr;
        use keelson_exec::{ExecError, FromRow, Row};
        use keelson_models::{Column, ModelTable, Set, Table, View};
        use keelson_sqlite::{delete, insert, quote, select, update};

        /// The model marker.
        #[derive(Debug, Clone, Copy)]
        pub struct Posts;

        /// One row of `posts`.
        #[derive(Debug, Clone, PartialEq)]
        pub struct Post {
            pub id: i64,
            pub user_id: i64,
            pub title: String,
            pub status: Option<String>,
            pub views: i64,
            pub published_at: Option<NaiveDateTime>,
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
                })
            }
        }

        /// The three-state setter.
        #[derive(Debug, Clone, Default)]
        pub struct Setter {
            pub id: Set<i64>,
            pub user_id: Set<i64>,
            pub title: Set<String>,
            pub status: Set<String>,
            pub views: Set<i64>,
            pub published_at: Set<NaiveDateTime>,
        }

        /// The entry point.
        pub fn table() -> ModelTable<Posts> {
            ModelTable::new()
        }

        pub fn id() -> Column<i64> {
            Column::new("posts", "id")
        }
        pub fn user_id() -> Column<i64> {
            Column::new("posts", "user_id")
        }
        pub fn title() -> Column<String> {
            Column::new("posts", "title")
        }

        #[allow(clippy::type_complexity)]
        fn all_columns() -> (
            Column<i64>,
            Column<i64>,
            Column<String>,
            Column<String>,
            Column<i64>,
            Column<NaiveDateTime>,
        ) {
            (
                id(),
                user_id(),
                title(),
                Column::new("posts", "status"),
                Column::new("posts", "views"),
                Column::new("posts", "published_at"),
            )
        }

        impl View for Posts {
            type Row = Post;
            type Select = keelson_sqlite::SelectQuery;

            fn base_select() -> Self::Select {
                keelson_sqlite::select((
                    select::columns(all_columns()),
                    select::from(quote("posts")),
                ))
            }
        }

        impl Table for Posts {
            type Pk = i64;
            type Setter = Setter;
            type Insert = keelson_sqlite::InsertQuery;
            type Update = keelson_sqlite::UpdateQuery;
            type Delete = keelson_sqlite::DeleteQuery;

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
                let mut q = keelson_sqlite::insert((
                    insert::into(quote("posts")).columns(cols),
                    insert::returning(all_columns()),
                ));
                if !vals.is_empty() {
                    q.apply(insert::values(vals));
                }
                q
            }

            fn update_query() -> Self::Update {
                keelson_sqlite::update(update::table(quote("posts")))
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
                keelson_sqlite::delete(delete::from(quote("posts")))
            }

            fn pk(row: &Post) -> i64 {
                row.id
            }
        }
    }

    /// The `comments` table: a non-null FK to `posts`, a nullable FK to
    /// `users` — the factory's chain-and-optional demonstration pair.
    pub mod comments {
        use chrono::NaiveDateTime;
        use keelson_core::expr::Expr;
        use keelson_exec::{ExecError, FromRow, Row};
        use keelson_models::{Column, ModelTable, Set, Table, View};
        use keelson_sqlite::{delete, insert, quote, select, update};

        /// The model marker.
        #[derive(Debug, Clone, Copy)]
        pub struct Comments;

        /// One row of `comments`.
        #[derive(Debug, Clone, PartialEq)]
        pub struct Comment {
            pub id: i64,
            pub post_id: i64,
            pub user_id: Option<i64>,
            pub body: String,
            pub created_at: NaiveDateTime,
        }

        impl FromRow for Comment {
            fn from_row(row: &mut Row) -> Result<Self, ExecError> {
                Ok(Comment {
                    id: row.take("id")?,
                    post_id: row.take("post_id")?,
                    user_id: row.take("user_id")?,
                    body: row.take("body")?,
                    created_at: row.take("created_at")?,
                })
            }
        }

        /// The three-state setter.
        #[derive(Debug, Clone, Default)]
        pub struct Setter {
            pub id: Set<i64>,
            pub post_id: Set<i64>,
            pub user_id: Set<i64>,
            pub body: Set<String>,
            pub created_at: Set<NaiveDateTime>,
        }

        /// The entry point.
        pub fn table() -> ModelTable<Comments> {
            ModelTable::new()
        }

        pub fn id() -> Column<i64> {
            Column::new("comments", "id")
        }
        pub fn post_id() -> Column<i64> {
            Column::new("comments", "post_id")
        }
        pub fn user_id() -> Column<i64> {
            Column::new("comments", "user_id")
        }
        pub fn body() -> Column<String> {
            Column::new("comments", "body")
        }

        #[allow(clippy::type_complexity)]
        fn all_columns() -> (
            Column<i64>,
            Column<i64>,
            Column<i64>,
            Column<String>,
            Column<NaiveDateTime>,
        ) {
            (
                id(),
                post_id(),
                user_id(),
                body(),
                Column::new("comments", "created_at"),
            )
        }

        impl View for Comments {
            type Row = Comment;
            type Select = keelson_sqlite::SelectQuery;

            fn base_select() -> Self::Select {
                keelson_sqlite::select((
                    select::columns(all_columns()),
                    select::from(quote("comments")),
                ))
            }
        }

        impl Table for Comments {
            type Pk = i64;
            type Setter = Setter;
            type Insert = keelson_sqlite::InsertQuery;
            type Update = keelson_sqlite::UpdateQuery;
            type Delete = keelson_sqlite::DeleteQuery;

            fn insert_query(s: Setter) -> Self::Insert {
                let mut cols: Vec<&'static str> = Vec::new();
                let mut vals: Vec<Expr> = Vec::new();
                s.id.push_into("id", &mut cols, &mut vals);
                s.post_id.push_into("post_id", &mut cols, &mut vals);
                s.user_id.push_into("user_id", &mut cols, &mut vals);
                s.body.push_into("body", &mut cols, &mut vals);
                s.created_at.push_into("created_at", &mut cols, &mut vals);
                let mut q = keelson_sqlite::insert((
                    insert::into(quote("comments")).columns(cols),
                    insert::returning(all_columns()),
                ));
                if !vals.is_empty() {
                    q.apply(insert::values(vals));
                }
                q
            }

            fn update_query() -> Self::Update {
                keelson_sqlite::update(update::table(quote("comments")))
            }

            fn apply_setter(s: Setter, q: &mut Self::Update) {
                if let Some(v) = s.id.into_expr() {
                    q.apply(update::set_col("id").to(v));
                }
                if let Some(v) = s.post_id.into_expr() {
                    q.apply(update::set_col("post_id").to(v));
                }
                if let Some(v) = s.user_id.into_expr() {
                    q.apply(update::set_col("user_id").to(v));
                }
                if let Some(v) = s.body.into_expr() {
                    q.apply(update::set_col("body").to(v));
                }
                if let Some(v) = s.created_at.into_expr() {
                    q.apply(update::set_col("created_at").to(v));
                }
            }

            fn delete_query() -> Self::Delete {
                keelson_sqlite::delete(delete::from(quote("comments")))
            }

            fn pk(row: &Comment) -> i64 {
                row.id
            }
        }
    }
}

/// What the factory generator will write for the SQLite rendition of the
/// schema — the specification under test.
#[allow(unreachable_pub, dead_code)]
mod fac {
    /// The `users` factory.
    pub mod users {
        use chrono::NaiveDateTime;
        use keelson_core::{Mod, mod_fn};
        use keelson_exec::{ExecError, ExecFuture, Executor};
        use keelson_factory::{Faker, Parent, Sequence, Source};
        use keelson_models::Set;

        use super::super::model::users as m;

        /// One process-wide sequence per model — the uniqueness source for
        /// the primary key.
        static SEQ: Sequence = Sequence::new();

        /// The `users` template: one value source per column, plus the
        /// has-many children the spec demonstrates.
        #[derive(Debug, Clone, Default)]
        pub struct UserTemplate {
            pub id: Source<i64>,
            pub name: Source<String>,
            pub email: Source<String>,
            pub age: Source<i64>,
            pub is_active: Source<bool>,
            pub created_at: Source<NaiveDateTime>,
            /// Has-many `posts` children, created after the user row exists,
            /// each with its parent forced to the created user.
            pub posts: Vec<super::posts::PostTemplate>,
        }

        /// The entry point: `users::factory((users::id(10), …))`.
        pub fn factory(mods: impl Mod<UserTemplate>) -> UserTemplate {
            let mut t = UserTemplate::default();
            mods.apply(&mut t);
            t
        }

        // One mod per column — mods are values, the house style.
        pub fn id(v: i64) -> impl Mod<UserTemplate> {
            mod_fn(move |t: &mut UserTemplate| t.id = Source::Value(v))
        }
        /// A random (not sequence) id — still seed-covered, drawn from the
        /// run's [`Faker`].
        pub fn random_id() -> impl Mod<UserTemplate> {
            mod_fn(|t: &mut UserTemplate| t.id = Source::from_fn(|f| f.i64_in(1, i64::MAX / 2)))
        }
        pub fn name(v: impl Into<String>) -> impl Mod<UserTemplate> {
            let v = v.into();
            mod_fn(move |t: &mut UserTemplate| t.name = Source::Value(v))
        }
        pub fn email(v: impl Into<String>) -> impl Mod<UserTemplate> {
            let v = v.into();
            mod_fn(move |t: &mut UserTemplate| t.email = Source::Value(v))
        }
        /// Nullable column: the generator also emits the `NULL` mod.
        pub fn email_null() -> impl Mod<UserTemplate> {
            mod_fn(|t: &mut UserTemplate| t.email = Source::Null)
        }
        pub fn age(v: i64) -> impl Mod<UserTemplate> {
            mod_fn(move |t: &mut UserTemplate| t.age = Source::Value(v))
        }
        pub fn age_null() -> impl Mod<UserTemplate> {
            mod_fn(|t: &mut UserTemplate| t.age = Source::Null)
        }
        pub fn is_active(v: bool) -> impl Mod<UserTemplate> {
            mod_fn(move |t: &mut UserTemplate| t.is_active = Source::Value(v))
        }
        pub fn created_at(v: NaiveDateTime) -> impl Mod<UserTemplate> {
            mod_fn(move |t: &mut UserTemplate| t.created_at = Source::Value(v))
        }
        /// Queue a has-many child: created after the user, `user_id` forced
        /// to the created row.
        pub fn with_new_post(tpl: super::posts::PostTemplate) -> impl Mod<UserTemplate> {
            mod_fn(move |t: &mut UserTemplate| t.posts.push(tpl))
        }

        impl UserTemplate {
            /// The no-database strategy: the setter `create` would insert.
            /// No executor in the signature — that absence is the
            /// "build touches no DB" guarantee.
            pub fn build(&self, f: &mut Faker) -> m::Setter {
                m::Setter {
                    id: self.id.resolve(f, |_| Set::Value(SEQ.next_i64())),
                    name: self
                        .name
                        .resolve(f, |f| Set::Value(format!("user-{}", f.alnum(8)))),
                    email: self
                        .email
                        .resolve(f, |f| Set::Value(format!("{}@example.test", f.alnum(10)))),
                    age: self.age.resolve(f, |f| Set::Value(f.i64_in(18, 90))),
                    // Schema-defaulted columns stay out of the statement.
                    is_active: self.is_active.resolve(f, |_| Set::Unset),
                    created_at: self.created_at.resolve(f, |_| Set::Unset),
                }
            }

            /// Insert through the Layer 2 setter path — hooks fire — then
            /// create the queued children. Boxed, not `async fn`, because
            /// factory graphs recurse (user → posts → user).
            pub fn create_with<'a>(
                &'a self,
                db: &'a dyn Executor,
                f: &'a mut Faker,
            ) -> ExecFuture<'a, Result<m::User, ExecError>> {
                Box::pin(async move {
                    let u = m::table().insert(self.build(f)).one(db).await?;
                    for p in &self.posts {
                        let mut child = p.clone();
                        child.user = Parent::Existing(u.id);
                        child.create_with(db, &mut *f).await?;
                    }
                    Ok(u)
                })
            }

            /// Create one row (and whatever parents/children the template
            /// implies) with a fresh entropy-seeded faker.
            pub async fn create(&self, db: &dyn Executor) -> Result<m::User, ExecError> {
                let mut f = Faker::from_entropy();
                self.create_with(db, &mut f).await
            }

            /// Create `n` rows; sequences keep the unique columns apart.
            pub async fn create_many(
                &self,
                db: &dyn Executor,
                n: usize,
            ) -> Result<Vec<m::User>, ExecError> {
                let mut f = Faker::from_entropy();
                let mut out = Vec::with_capacity(n);
                for _ in 0..n {
                    out.push(self.create_with(db, &mut f).await?);
                }
                Ok(out)
            }
        }
    }

    /// The `posts` factory: one required parent (`users`).
    pub mod posts {
        use chrono::NaiveDateTime;
        use keelson_core::{Mod, mod_fn};
        use keelson_exec::{ExecError, ExecFuture, Executor};
        use keelson_factory::{Faker, Parent, Sequence, Source};
        use keelson_models::Set;

        use super::super::model::posts as m;
        use super::super::model::users as um;

        static SEQ: Sequence = Sequence::new();

        /// The `posts` template.
        #[derive(Debug, Clone, Default)]
        pub struct PostTemplate {
            pub id: Source<i64>,
            /// The non-null FK: `Auto` (the default) creates a user from the
            /// user factory's own defaults at create time.
            pub user: Parent<super::users::UserTemplate, i64>,
            pub title: Source<String>,
            pub status: Source<String>,
            pub views: Source<i64>,
            pub published_at: Source<NaiveDateTime>,
        }

        /// The entry point.
        pub fn factory(mods: impl Mod<PostTemplate>) -> PostTemplate {
            let mut t = PostTemplate::default();
            mods.apply(&mut t);
            t
        }

        pub fn id(v: i64) -> impl Mod<PostTemplate> {
            mod_fn(move |t: &mut PostTemplate| t.id = Source::Value(v))
        }
        /// The parent-override triple, existing-row form: no user is created.
        pub fn user(u: &um::User) -> impl Mod<PostTemplate> {
            let pk = u.id;
            mod_fn(move |t: &mut PostTemplate| t.user = Parent::Existing(pk))
        }
        /// Existing-row form when only the key is at hand.
        pub fn user_id(pk: i64) -> impl Mod<PostTemplate> {
            mod_fn(move |t: &mut PostTemplate| t.user = Parent::Existing(pk))
        }
        /// Shaped-parent form: create the user from this template.
        pub fn for_user(tpl: super::users::UserTemplate) -> impl Mod<PostTemplate> {
            mod_fn(move |t: &mut PostTemplate| t.user = Parent::Template(tpl))
        }
        pub fn title(v: impl Into<String>) -> impl Mod<PostTemplate> {
            let v = v.into();
            mod_fn(move |t: &mut PostTemplate| t.title = Source::Value(v))
        }
        pub fn status(v: impl Into<String>) -> impl Mod<PostTemplate> {
            let v = v.into();
            mod_fn(move |t: &mut PostTemplate| t.status = Source::Value(v))
        }
        pub fn status_null() -> impl Mod<PostTemplate> {
            mod_fn(|t: &mut PostTemplate| t.status = Source::Null)
        }
        pub fn views(v: i64) -> impl Mod<PostTemplate> {
            mod_fn(move |t: &mut PostTemplate| t.views = Source::Value(v))
        }
        pub fn published_at(v: NaiveDateTime) -> impl Mod<PostTemplate> {
            mod_fn(move |t: &mut PostTemplate| t.published_at = Source::Value(v))
        }
        pub fn published_at_null() -> impl Mod<PostTemplate> {
            mod_fn(|t: &mut PostTemplate| t.published_at = Source::Null)
        }

        impl PostTemplate {
            /// The setter, database-free. The FK column is filled only when
            /// the parent is `Existing`; `Auto`/`Template` need a database to
            /// produce a key, so `build` leaves it unset (crate docs).
            pub fn build(&self, f: &mut Faker) -> m::Setter {
                m::Setter {
                    id: self.id.resolve(f, |_| Set::Value(SEQ.next_i64())),
                    user_id: match &self.user {
                        Parent::Existing(pk) => Set::Value(*pk),
                        Parent::Auto | Parent::Template(_) => Set::Unset,
                    },
                    title: self
                        .title
                        .resolve(f, |f| Set::Value(format!("post-{}", f.alnum(8)))),
                    status: self.status.resolve(f, |_| Set::Unset),
                    views: self.views.resolve(f, |_| Set::Unset),
                    published_at: self.published_at.resolve(f, |_| Set::Unset),
                }
            }

            /// Create the parent chain first (unless overridden), then the
            /// row itself — hooks fire on every link.
            pub fn create_with<'a>(
                &'a self,
                db: &'a dyn Executor,
                f: &'a mut Faker,
            ) -> ExecFuture<'a, Result<m::Post, ExecError>> {
                Box::pin(async move {
                    let mut s = self.build(f);
                    match &self.user {
                        Parent::Existing(_) => {}
                        Parent::Template(t) => {
                            s.user_id = Set::Value(t.create_with(db, &mut *f).await?.id);
                        }
                        Parent::Auto => {
                            let t = super::users::UserTemplate::default();
                            s.user_id = Set::Value(t.create_with(db, &mut *f).await?.id);
                        }
                    }
                    m::table().insert(s).one(db).await
                })
            }

            /// Create one row with a fresh entropy-seeded faker.
            pub async fn create(&self, db: &dyn Executor) -> Result<m::Post, ExecError> {
                let mut f = Faker::from_entropy();
                self.create_with(db, &mut f).await
            }

            /// Create `n` rows, each with its own parent chain.
            pub async fn create_many(
                &self,
                db: &dyn Executor,
                n: usize,
            ) -> Result<Vec<m::Post>, ExecError> {
                let mut f = Faker::from_entropy();
                let mut out = Vec::with_capacity(n);
                for _ in 0..n {
                    out.push(self.create_with(db, &mut f).await?);
                }
                Ok(out)
            }
        }
    }

    /// The `comments` factory: the chain demonstration — a required parent
    /// (`posts`, itself chaining `users`) and an optional one (`users`).
    pub mod comments {
        use chrono::NaiveDateTime;
        use keelson_core::{Mod, mod_fn};
        use keelson_exec::{ExecError, ExecFuture, Executor};
        use keelson_factory::{Faker, OptionalParent, Parent, Sequence, Source};
        use keelson_models::Set;

        use super::super::model::comments as m;
        use super::super::model::posts as pm;
        use super::super::model::users as um;

        static SEQ: Sequence = Sequence::new();

        /// The `comments` template.
        #[derive(Debug, Clone, Default)]
        pub struct CommentTemplate {
            pub id: Source<i64>,
            /// Non-null FK: auto-creates the post (which auto-creates its
            /// user) unless overridden.
            pub post: Parent<super::posts::PostTemplate, i64>,
            /// Nullable FK: stays NULL unless a mod opts in.
            pub user: OptionalParent<super::users::UserTemplate, i64>,
            pub body: Source<String>,
            pub created_at: Source<NaiveDateTime>,
        }

        /// The entry point.
        pub fn factory(mods: impl Mod<CommentTemplate>) -> CommentTemplate {
            let mut t = CommentTemplate::default();
            mods.apply(&mut t);
            t
        }

        pub fn id(v: i64) -> impl Mod<CommentTemplate> {
            mod_fn(move |t: &mut CommentTemplate| t.id = Source::Value(v))
        }
        pub fn post(p: &pm::Post) -> impl Mod<CommentTemplate> {
            let pk = p.id;
            mod_fn(move |t: &mut CommentTemplate| t.post = Parent::Existing(pk))
        }
        pub fn post_id(pk: i64) -> impl Mod<CommentTemplate> {
            mod_fn(move |t: &mut CommentTemplate| t.post = Parent::Existing(pk))
        }
        pub fn for_post(tpl: super::posts::PostTemplate) -> impl Mod<CommentTemplate> {
            mod_fn(move |t: &mut CommentTemplate| t.post = Parent::Template(tpl))
        }
        pub fn user(u: &um::User) -> impl Mod<CommentTemplate> {
            let pk = u.id;
            mod_fn(move |t: &mut CommentTemplate| t.user = OptionalParent::Existing(pk))
        }
        pub fn user_id(pk: i64) -> impl Mod<CommentTemplate> {
            mod_fn(move |t: &mut CommentTemplate| t.user = OptionalParent::Existing(pk))
        }
        pub fn for_user(tpl: super::users::UserTemplate) -> impl Mod<CommentTemplate> {
            mod_fn(move |t: &mut CommentTemplate| t.user = OptionalParent::Template(tpl))
        }
        pub fn body(v: impl Into<String>) -> impl Mod<CommentTemplate> {
            let v = v.into();
            mod_fn(move |t: &mut CommentTemplate| t.body = Source::Value(v))
        }
        pub fn created_at(v: NaiveDateTime) -> impl Mod<CommentTemplate> {
            mod_fn(move |t: &mut CommentTemplate| t.created_at = Source::Value(v))
        }

        impl CommentTemplate {
            /// The setter, database-free; FK rules as on `posts`.
            pub fn build(&self, f: &mut Faker) -> m::Setter {
                m::Setter {
                    id: self.id.resolve(f, |_| Set::Value(SEQ.next_i64())),
                    post_id: match &self.post {
                        Parent::Existing(pk) => Set::Value(*pk),
                        Parent::Auto | Parent::Template(_) => Set::Unset,
                    },
                    user_id: match &self.user {
                        OptionalParent::Existing(pk) => Set::Value(*pk),
                        // NULL by omission; the schema's nullable default.
                        OptionalParent::Absent | OptionalParent::Template(_) => Set::Unset,
                    },
                    body: self
                        .body
                        .resolve(f, |f| Set::Value(format!("comment-{}", f.alnum(12)))),
                    created_at: self.created_at.resolve(f, |_| Set::Unset),
                }
            }

            /// Create the required chain (and the optional parent if opted
            /// in), then the comment.
            pub fn create_with<'a>(
                &'a self,
                db: &'a dyn Executor,
                f: &'a mut Faker,
            ) -> ExecFuture<'a, Result<m::Comment, ExecError>> {
                Box::pin(async move {
                    let mut s = self.build(f);
                    match &self.post {
                        Parent::Existing(_) => {}
                        Parent::Template(t) => {
                            s.post_id = Set::Value(t.create_with(db, &mut *f).await?.id);
                        }
                        Parent::Auto => {
                            let t = super::posts::PostTemplate::default();
                            s.post_id = Set::Value(t.create_with(db, &mut *f).await?.id);
                        }
                    }
                    if let OptionalParent::Template(t) = &self.user {
                        s.user_id = Set::Value(t.create_with(db, &mut *f).await?.id);
                    }
                    m::table().insert(s).one(db).await
                })
            }

            /// Create one chain with a fresh entropy-seeded faker.
            pub async fn create(&self, db: &dyn Executor) -> Result<m::Comment, ExecError> {
                let mut f = Faker::from_entropy();
                self.create_with(db, &mut f).await
            }

            /// Create `n` chains — each comment gets its own post and user.
            pub async fn create_many(
                &self,
                db: &dyn Executor,
                n: usize,
            ) -> Result<Vec<m::Comment>, ExecError> {
                let mut f = Faker::from_entropy();
                let mut out = Vec::with_capacity(n);
                for _ in 0..n {
                    out.push(self.create_with(db, &mut f).await?);
                }
                Ok(out)
            }
        }
    }
}

// ────────────────────────── build(): no database ──────────────────────────

#[test]
fn build_produces_a_setter_with_no_executor_in_sight() {
    // The signature is the proof — `build(&mut Faker)` cannot touch a
    // database. The assertions pin what it fills in.
    let mut f = Faker::seeded(1);
    let s = users::factory(()).build(&mut f);
    assert!(matches!(s.id, Set::Value(_)), "sequence-based id");
    match &s.name {
        Set::Value(n) => assert!(n.starts_with("user-"), "random default name, got {n}"),
        other => panic!("expected a generated name, got {other:?}"),
    }
    assert!(matches!(s.email, Set::Value(_)));
    assert!(matches!(s.age, Set::Value(_)));
    // Schema-defaulted columns stay out of the statement entirely.
    assert!(s.is_active.is_unset());
    assert!(s.created_at.is_unset());

    // A required FK without an existing parent stays unset: build alone
    // cannot create the chain (crate docs) — that is create()'s job.
    let s = posts::factory(()).build(&mut f);
    assert!(s.user_id.is_unset());
    // With an existing key, build fills it.
    let s = posts::factory(posts::user_id(77)).build(&mut f);
    assert_eq!(s.user_id, Set::Value(77));
}

#[test]
fn seeded_fakers_reproduce_random_sources_while_sequences_stay_unique() {
    let a = users::factory(()).build(&mut Faker::seeded(7));
    let b = users::factory(()).build(&mut Faker::seeded(7));
    // Random-sourced columns reproduce under the seed…
    assert_eq!(a.name, b.name);
    assert_eq!(a.email, b.email);
    assert_eq!(a.age, b.age);
    // …while the sequence-backed unique column deliberately does not: it is
    // uniqueness machinery, outside the seed (crate docs).
    assert_ne!(a.id, b.id);

    // A Gen source (random_id) is inside the seed too.
    let a = users::factory(users::random_id()).build(&mut Faker::seeded(7));
    let b = users::factory(users::random_id()).build(&mut Faker::seeded(7));
    assert_eq!(a.id, b.id);
}

#[test]
fn mods_are_values_and_override_the_default_sources() {
    let s = users::factory((
        users::id(42),
        users::name("Ada"),
        users::email_null(),
        users::age(36),
    ))
    .build(&mut Faker::seeded(0));
    assert_eq!(s.id, Set::Value(42));
    assert_eq!(s.name, Set::Value("Ada".to_owned()));
    assert_eq!(s.email, Set::Null, "the generated NULL mod");
    assert_eq!(s.age, Set::Value(36));
}

#[test]
fn the_built_insert_is_judged_sql() {
    use keelson_core::Query as _;
    use keelson_models::Table as _;

    let s = users::factory((users::id(1), users::name("Ada"), users::email_null()))
        .build(&mut Faker::seeded(0));
    let q = model::users::Users::insert_query(model::users::Setter {
        age: Set::Unset, // pin: unseeded age came from the faker; drop it here
        ..s
    });
    let (sql, _args) = q.build().expect("build");
    keelson_sqlcheck::assert_sql(
        keelson_sqlcheck::Dialect::Sqlite,
        &sql,
        concat!(
            r#"INSERT INTO "users" ("id", "name", "email") VALUES (?1, ?2, ?3) "#,
            r#"RETURNING "users"."id", "users"."name", "users"."email", "users"."age", "#,
            r#""users"."is_active", "users"."created_at""#
        ),
    );
}

// ─────────────────────── end-to-end (real SQLite) ───────────────────────

async fn db() -> Pool {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "keelson-factory-{}-{}.db",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&path);
    let pool = Pool::connect(&format!("sqlite://{}", path.display()))
        .await
        .expect("opening the SQLite database");
    for ddl in [
        "CREATE TABLE users (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            email TEXT,
            age INTEGER,
            is_active BOOLEAN NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP)",
        "CREATE TABLE posts (
            id INTEGER PRIMARY KEY,
            user_id INTEGER NOT NULL REFERENCES users (id),
            title TEXT NOT NULL,
            status TEXT,
            views INTEGER NOT NULL DEFAULT 0,
            published_at TEXT)",
        "CREATE TABLE comments (
            id INTEGER PRIMARY KEY,
            post_id INTEGER NOT NULL REFERENCES posts (id),
            user_id INTEGER REFERENCES users (id),
            body TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP)",
        "CREATE TABLE tags (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE)",
    ] {
        pool.execute(Statement::new(ddl, vec![])).await.unwrap();
    }
    pool
}

async fn count(db: &dyn Executor, table: &'static str) -> i64 {
    use keelson_exec::Execute as _;
    keelson_sqlite::select((select::columns("count(*)"), select::from(quote(table))))
        .fetch_scalar(db)
        .await
        .unwrap()
}

#[tokio::test]
async fn build_alone_leaves_the_database_untouched() {
    let db = db().await;
    let _ = users::factory(()).build(&mut Faker::from_entropy());
    let _ = comments::factory(()).build(&mut Faker::from_entropy());
    for t in ["users", "posts", "comments", "tags"] {
        assert_eq!(count(&db, t).await, 0, "{t} rows appeared from build()");
    }
}

/// Factories fire model hooks — the decision the crate docs record, pinned:
/// `before_insert` rewrote the setter and `after_insert` wrote its audit tag,
/// exactly as a production write would.
#[tokio::test]
async fn create_goes_through_the_model_path_so_hooks_fire() {
    let db = db().await;
    let u = users::factory(users::email("ADA@Example.COM"))
        .create(&db)
        .await
        .unwrap();
    assert_eq!(
        u.email.as_deref(),
        Some("ada@example.com"),
        "before_insert normalised the factory's setter"
    );
    assert_eq!(count(&db, "users").await, 1);
    let audit: i64 = {
        use keelson_exec::Execute as _;
        use keelson_sqlite::{Chain as _, arg};
        keelson_sqlite::select((
            select::columns("count(*)"),
            select::from(quote("tags")),
            select::where_(quote("name").eq(arg(format!("audit-user-{}", u.id)))),
        ))
        .fetch_scalar(&db)
        .await
        .unwrap()
    };
    assert_eq!(audit, 1, "after_insert ran on the caller's executor");
}

/// The schema-aware win: a comment needs a post needs a user, and one create
/// makes the chain exist. The nullable FK stays NULL.
#[tokio::test]
async fn a_comment_auto_creates_its_post_and_user_chain() {
    let db = db().await;
    let c = comments::factory(()).create(&db).await.unwrap();

    let p = model::posts::table()
        .query(model::posts::id().eq(c.post_id))
        .one(&db)
        .await
        .unwrap();
    let owner = model::users::table()
        .query(model::users::id().eq(p.user_id))
        .one(&db)
        .await
        .unwrap();
    assert!(owner.is_active, "the chained user is a real defaulted row");
    assert_eq!(
        c.user_id, None,
        "the nullable FK stayed NULL — factories do not invent optional rows"
    );
    assert_eq!(count(&db, "users").await, 1);
    assert_eq!(count(&db, "posts").await, 1);
    assert_eq!(count(&db, "comments").await, 1);
}

/// FactoryBot's association semantics: each created row gets its own chain.
#[tokio::test]
async fn create_many_on_comments_builds_a_chain_per_row() {
    let db = db().await;
    let cs = comments::factory(()).create_many(&db, 10).await.unwrap();
    assert_eq!(cs.len(), 10);
    assert_eq!(count(&db, "comments").await, 10);
    assert_eq!(count(&db, "posts").await, 10, "one post per comment");
    assert_eq!(count(&db, "users").await, 10, "one user per post");
    // Every FK resolves, and to ten *distinct* posts.
    let mut post_ids: Vec<i64> = cs.iter().map(|c| c.post_id).collect();
    post_ids.sort_unstable();
    post_ids.dedup();
    assert_eq!(post_ids.len(), 10);
    let found = model::posts::table()
        .query(model::posts::id().in_(post_ids))
        .all(&db)
        .await
        .unwrap();
    assert_eq!(found.len(), 10);
}

/// Uniqueness at n=100: sequence-based unique columns cannot collide, and the
/// hook fired for every row.
#[tokio::test]
async fn create_many_at_one_hundred_holds_uniqueness_and_fires_every_hook() {
    let db = db().await;
    let us = users::factory(()).create_many(&db, 100).await.unwrap();
    assert_eq!(us.len(), 100);
    let mut ids: Vec<i64> = us.iter().map(|u| u.id).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 100, "sequence ids collided");
    assert_eq!(count(&db, "users").await, 100);
    assert_eq!(count(&db, "tags").await, 100, "one audit tag per hook run");
}

/// The override matrix: an existing row, and a shaped parent template.
#[tokio::test]
async fn existing_and_shaped_parents_override_auto_creation() {
    let db = db().await;

    // Existing: no new user is created.
    let u = users::factory(users::name("owner"))
        .create(&db)
        .await
        .unwrap();
    let p = posts::factory(posts::user(&u)).create(&db).await.unwrap();
    assert_eq!(p.user_id, u.id);
    assert_eq!(count(&db, "users").await, 1);

    // Shaped: for_post creates exactly the template's post.
    let c = comments::factory(comments::for_post(posts::factory((
        posts::title("shaped"),
        posts::user(&u),
    ))))
    .create(&db)
    .await
    .unwrap();
    let shaped = model::posts::table()
        .query(model::posts::id().eq(c.post_id))
        .one(&db)
        .await
        .unwrap();
    assert_eq!(shaped.title, "shaped");
    assert_eq!(shaped.user_id, u.id);
    assert_eq!(count(&db, "users").await, 1, "still no invented users");

    // Optional parent, opted in with an existing row.
    let c = comments::factory((comments::post(&p), comments::user(&u)))
        .create(&db)
        .await
        .unwrap();
    assert_eq!(c.post_id, p.id);
    assert_eq!(c.user_id, Some(u.id));
    // And with a shaped template: exactly one more user appears.
    let c = comments::factory((
        comments::post(&p),
        comments::for_user(users::factory(users::name("commenter"))),
    ))
    .create(&db)
    .await
    .unwrap();
    let commenter = model::users::table()
        .query(model::users::id().eq(c.user_id.unwrap()))
        .one(&db)
        .await
        .unwrap();
    assert_eq!(commenter.name, "commenter");
    assert_eq!(count(&db, "users").await, 2);
}

/// Has-many children: `with_new_post` creates the child after the user, with
/// the FK forced to the created row.
#[tokio::test]
async fn with_new_post_creates_children_bound_to_the_new_user() {
    let db = db().await;
    let u = users::factory((
        users::with_new_post(posts::factory(posts::title("first"))),
        users::with_new_post(posts::factory(posts::title("second"))),
    ))
    .create(&db)
    .await
    .unwrap();

    let ps = model::posts::table()
        .query(model::posts::user_id().eq(u.id))
        .all(&db)
        .await
        .unwrap();
    let mut titles: Vec<&str> = ps.iter().map(|p| p.title.as_str()).collect();
    titles.sort_unstable();
    assert_eq!(titles, vec!["first", "second"]);
    assert_eq!(count(&db, "users").await, 1, "children reuse their creator");
}

/// Seeded creates reproduce the random columns across two databases — the
/// determinism switch, end to end.
#[tokio::test]
async fn seeded_creates_reproduce_across_databases() {
    let db_a = db().await;
    let db_b = db().await;
    let mut fa = Faker::seeded(99);
    let mut fb = Faker::seeded(99);
    let a = users::factory(())
        .create_with(&db_a, &mut fa)
        .await
        .unwrap();
    let b = users::factory(())
        .create_with(&db_b, &mut fb)
        .await
        .unwrap();
    assert_eq!(a.name, b.name);
    assert_eq!(a.email, b.email);
    assert_eq!(a.age, b.age);
}
