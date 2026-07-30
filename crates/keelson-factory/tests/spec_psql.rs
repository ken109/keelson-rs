//! The hand-written PostgreSQL factory — the same generator specification as
//! `spec_sqlite.rs`, emitted for the psql dialect. The no-database tests
//! (build shapes, seeded reproduction, judged SQL) run in every plain
//! `cargo test`; the end-to-end tests at the bottom run against the shared
//! PostgreSQL 17 container under `--features live-docker`, inside a rolled-
//! back transaction so a shared or persistent server is left untouched.
//!
//! What honestly differs from the SQLite twin, per `docs/type-mappings.md`
//! and the schema: `integer` columns are `i32`, `timestamptz` columns are
//! `DateTime<Utc>`, and — because the schema's `id integer PRIMARY KEY` has
//! no default on PostgreSQL — the sequence-based id default is not merely
//! convenient here but required for an insert to succeed at all.

use keelson_core::Query as _;
use keelson_factory::Faker;
use keelson_models::Set;
use keelson_sqlcheck::Dialect;

use crate::fac::{posts, users};

/// The trimmed Layer 2 model (hooks kept; loaders dropped — see
/// `spec_sqlite.rs` for the rationale; the full model shape lives in
/// `keelson-models/tests/spec_psql.rs`).
#[allow(unreachable_pub, dead_code)]
mod model {
    /// The `users` table, hooks included.
    pub mod users {
        use chrono::{DateTime, Utc};
        use keelson_core::expr::Expr;
        use keelson_exec::{ExecError, ExecFuture, Execute as _, Executor, FromRow, Row};
        use keelson_models::{Column, ModelTable, Set, Table, View};
        use keelson_psql::{arg, delete, insert, quote, select, update};

        /// The model marker.
        #[derive(Debug, Clone, Copy)]
        pub struct Users;

        /// One row of `users`.
        #[derive(Debug, Clone, PartialEq)]
        pub struct User {
            pub id: i32,
            pub name: String,
            pub email: Option<String>,
            pub age: Option<i32>,
            pub is_active: bool,
            pub created_at: DateTime<Utc>,
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
            pub id: Set<i32>,
            pub name: Set<String>,
            pub email: Set<String>,
            pub age: Set<i32>,
            pub is_active: Set<bool>,
            pub created_at: Set<DateTime<Utc>>,
        }

        /// The entry point.
        pub fn table() -> ModelTable<Users> {
            ModelTable::new()
        }

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

        #[allow(clippy::type_complexity)]
        fn all_columns() -> (
            Column<i32>,
            Column<String>,
            Column<String>,
            Column<i32>,
            Column<bool>,
            Column<DateTime<Utc>>,
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
            type Select = keelson_psql::SelectQuery;

            fn base_select() -> Self::Select {
                keelson_psql::select((select::columns(all_columns()), select::from(quote("users"))))
            }
        }

        impl Table for Users {
            type Pk = i32;
            type Setter = Setter;
            type Insert = keelson_psql::InsertQuery;
            type Update = keelson_psql::UpdateQuery;
            type Delete = keelson_psql::DeleteQuery;

            fn insert_query(s: Setter) -> Self::Insert {
                let mut cols: Vec<&'static str> = Vec::new();
                let mut vals: Vec<Expr> = Vec::new();
                s.id.push_into("id", &mut cols, &mut vals);
                s.name.push_into("name", &mut cols, &mut vals);
                s.email.push_into("email", &mut cols, &mut vals);
                s.age.push_into("age", &mut cols, &mut vals);
                s.is_active.push_into("is_active", &mut cols, &mut vals);
                s.created_at.push_into("created_at", &mut cols, &mut vals);
                let mut q = keelson_psql::insert((
                    insert::into(quote("users")).columns(cols),
                    insert::returning(all_columns()),
                ));
                if !vals.is_empty() {
                    q.apply(insert::values(vals));
                }
                q
            }

            fn update_query() -> Self::Update {
                keelson_psql::update(update::table(quote("users")))
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
                keelson_psql::delete(delete::from(quote("users")))
            }

            fn pk(row: &User) -> i32 {
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

            /// Write an audit tag on the caller's executor.
            fn after_insert<'a>(
                db: &'a dyn Executor,
                rows: &'a [User],
            ) -> ExecFuture<'a, Result<(), ExecError>> {
                Box::pin(async move {
                    for u in rows {
                        keelson_psql::insert((
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
        use chrono::{DateTime, Utc};
        use keelson_core::expr::Expr;
        use keelson_exec::{ExecError, FromRow, Row};
        use keelson_models::{Column, ModelTable, Set, Table, View};
        use keelson_psql::{delete, insert, quote, select, update};

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
            pub published_at: Option<DateTime<Utc>>,
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
            pub id: Set<i32>,
            pub user_id: Set<i32>,
            pub title: Set<String>,
            pub status: Set<String>,
            pub views: Set<i32>,
            pub published_at: Set<DateTime<Utc>>,
        }

        /// The entry point.
        pub fn table() -> ModelTable<Posts> {
            ModelTable::new()
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

        #[allow(clippy::type_complexity)]
        fn all_columns() -> (
            Column<i32>,
            Column<i32>,
            Column<String>,
            Column<String>,
            Column<i32>,
            Column<DateTime<Utc>>,
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
            type Select = keelson_psql::SelectQuery;

            fn base_select() -> Self::Select {
                keelson_psql::select((select::columns(all_columns()), select::from(quote("posts"))))
            }
        }

        impl Table for Posts {
            type Pk = i32;
            type Setter = Setter;
            type Insert = keelson_psql::InsertQuery;
            type Update = keelson_psql::UpdateQuery;
            type Delete = keelson_psql::DeleteQuery;

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
                let mut q = keelson_psql::insert((
                    insert::into(quote("posts")).columns(cols),
                    insert::returning(all_columns()),
                ));
                if !vals.is_empty() {
                    q.apply(insert::values(vals));
                }
                q
            }

            fn update_query() -> Self::Update {
                keelson_psql::update(update::table(quote("posts")))
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
                keelson_psql::delete(delete::from(quote("posts")))
            }

            fn pk(row: &Post) -> i32 {
                row.id
            }
        }
    }

    /// The `comments` table — the chain-and-optional pair.
    pub mod comments {
        use chrono::{DateTime, Utc};
        use keelson_core::expr::Expr;
        use keelson_exec::{ExecError, FromRow, Row};
        use keelson_models::{Column, ModelTable, Set, Table, View};
        use keelson_psql::{delete, insert, quote, select, update};

        /// The model marker.
        #[derive(Debug, Clone, Copy)]
        pub struct Comments;

        /// One row of `comments`.
        #[derive(Debug, Clone, PartialEq)]
        pub struct Comment {
            pub id: i32,
            pub post_id: i32,
            pub user_id: Option<i32>,
            pub body: String,
            pub created_at: DateTime<Utc>,
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
            pub id: Set<i32>,
            pub post_id: Set<i32>,
            pub user_id: Set<i32>,
            pub body: Set<String>,
            pub created_at: Set<DateTime<Utc>>,
        }

        /// The entry point.
        pub fn table() -> ModelTable<Comments> {
            ModelTable::new()
        }

        pub fn id() -> Column<i32> {
            Column::new("comments", "id")
        }
        pub fn post_id() -> Column<i32> {
            Column::new("comments", "post_id")
        }
        pub fn user_id() -> Column<i32> {
            Column::new("comments", "user_id")
        }
        pub fn body() -> Column<String> {
            Column::new("comments", "body")
        }

        #[allow(clippy::type_complexity)]
        fn all_columns() -> (
            Column<i32>,
            Column<i32>,
            Column<i32>,
            Column<String>,
            Column<DateTime<Utc>>,
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
            type Select = keelson_psql::SelectQuery;

            fn base_select() -> Self::Select {
                keelson_psql::select((
                    select::columns(all_columns()),
                    select::from(quote("comments")),
                ))
            }
        }

        impl Table for Comments {
            type Pk = i32;
            type Setter = Setter;
            type Insert = keelson_psql::InsertQuery;
            type Update = keelson_psql::UpdateQuery;
            type Delete = keelson_psql::DeleteQuery;

            fn insert_query(s: Setter) -> Self::Insert {
                let mut cols: Vec<&'static str> = Vec::new();
                let mut vals: Vec<Expr> = Vec::new();
                s.id.push_into("id", &mut cols, &mut vals);
                s.post_id.push_into("post_id", &mut cols, &mut vals);
                s.user_id.push_into("user_id", &mut cols, &mut vals);
                s.body.push_into("body", &mut cols, &mut vals);
                s.created_at.push_into("created_at", &mut cols, &mut vals);
                let mut q = keelson_psql::insert((
                    insert::into(quote("comments")).columns(cols),
                    insert::returning(all_columns()),
                ));
                if !vals.is_empty() {
                    q.apply(insert::values(vals));
                }
                q
            }

            fn update_query() -> Self::Update {
                keelson_psql::update(update::table(quote("comments")))
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
                keelson_psql::delete(delete::from(quote("comments")))
            }

            fn pk(row: &Comment) -> i32 {
                row.id
            }
        }
    }
}

/// What the factory generator will write for the psql rendition — identical
/// shapes to the SQLite twin, `i32`/`DateTime<Utc>`-typed per
/// `docs/type-mappings.md`.
#[allow(unreachable_pub, dead_code)]
mod fac {
    /// The `users` factory.
    pub mod users {
        use chrono::{DateTime, Utc};
        use keelson_core::{Mod, mod_fn};
        use keelson_exec::{ExecError, ExecFuture, Executor};
        use keelson_factory::{Faker, Parent, Sequence, Source};
        use keelson_models::Set;

        use super::super::model::users as m;

        static SEQ: Sequence = Sequence::new();

        /// The `users` template.
        #[derive(Debug, Clone, Default)]
        pub struct UserTemplate {
            pub id: Source<i32>,
            pub name: Source<String>,
            pub email: Source<String>,
            pub age: Source<i32>,
            pub is_active: Source<bool>,
            pub created_at: Source<DateTime<Utc>>,
            /// Has-many `posts` children.
            pub posts: Vec<super::posts::PostTemplate>,
        }

        /// The entry point.
        pub fn factory(mods: impl Mod<UserTemplate>) -> UserTemplate {
            let mut t = UserTemplate::default();
            mods.apply(&mut t);
            t
        }

        pub fn id(v: i32) -> impl Mod<UserTemplate> {
            mod_fn(move |t: &mut UserTemplate| t.id = Source::Value(v))
        }
        /// A random (not sequence) id — seed-covered via the run's [`Faker`].
        pub fn random_id() -> impl Mod<UserTemplate> {
            mod_fn(|t: &mut UserTemplate| t.id = Source::from_fn(|f| f.i32_in(1, i32::MAX / 2)))
        }
        pub fn name(v: impl Into<String>) -> impl Mod<UserTemplate> {
            let v = v.into();
            mod_fn(move |t: &mut UserTemplate| t.name = Source::Value(v))
        }
        pub fn email(v: impl Into<String>) -> impl Mod<UserTemplate> {
            let v = v.into();
            mod_fn(move |t: &mut UserTemplate| t.email = Source::Value(v))
        }
        pub fn email_null() -> impl Mod<UserTemplate> {
            mod_fn(|t: &mut UserTemplate| t.email = Source::Null)
        }
        pub fn age(v: i32) -> impl Mod<UserTemplate> {
            mod_fn(move |t: &mut UserTemplate| t.age = Source::Value(v))
        }
        pub fn age_null() -> impl Mod<UserTemplate> {
            mod_fn(|t: &mut UserTemplate| t.age = Source::Null)
        }
        pub fn is_active(v: bool) -> impl Mod<UserTemplate> {
            mod_fn(move |t: &mut UserTemplate| t.is_active = Source::Value(v))
        }
        pub fn created_at(v: DateTime<Utc>) -> impl Mod<UserTemplate> {
            mod_fn(move |t: &mut UserTemplate| t.created_at = Source::Value(v))
        }
        /// Queue a has-many child.
        pub fn with_new_post(tpl: super::posts::PostTemplate) -> impl Mod<UserTemplate> {
            mod_fn(move |t: &mut UserTemplate| t.posts.push(tpl))
        }

        impl UserTemplate {
            /// The no-database strategy. On psql the sequence id is not just
            /// unique but *required*: `id integer PRIMARY KEY` has no
            /// default.
            pub fn build(&self, f: &mut Faker) -> m::Setter {
                m::Setter {
                    id: self.id.resolve(f, |_| Set::Value(SEQ.next_i32())),
                    name: self
                        .name
                        .resolve(f, |f| Set::Value(format!("user-{}", f.alnum(8)))),
                    email: self
                        .email
                        .resolve(f, |f| Set::Value(format!("{}@example.test", f.alnum(10)))),
                    age: self.age.resolve(f, |f| Set::Value(f.i32_in(18, 90))),
                    is_active: self.is_active.resolve(f, |_| Set::Unset),
                    created_at: self.created_at.resolve(f, |_| Set::Unset),
                }
            }

            /// Insert through the setter path (hooks fire), then create the
            /// queued children. Boxed: factory graphs recurse.
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

            /// Create one row with a fresh entropy-seeded faker.
            pub async fn create(&self, db: &dyn Executor) -> Result<m::User, ExecError> {
                let mut f = Faker::from_entropy();
                self.create_with(db, &mut f).await
            }

            /// Create `n` rows; the sequence keeps ids apart.
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

    /// The `posts` factory.
    pub mod posts {
        use chrono::{DateTime, Utc};
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
            pub id: Source<i32>,
            /// The non-null FK.
            pub user: Parent<super::users::UserTemplate, i32>,
            pub title: Source<String>,
            pub status: Source<String>,
            pub views: Source<i32>,
            pub published_at: Source<DateTime<Utc>>,
        }

        /// The entry point.
        pub fn factory(mods: impl Mod<PostTemplate>) -> PostTemplate {
            let mut t = PostTemplate::default();
            mods.apply(&mut t);
            t
        }

        pub fn id(v: i32) -> impl Mod<PostTemplate> {
            mod_fn(move |t: &mut PostTemplate| t.id = Source::Value(v))
        }
        pub fn user(u: &um::User) -> impl Mod<PostTemplate> {
            let pk = u.id;
            mod_fn(move |t: &mut PostTemplate| t.user = Parent::Existing(pk))
        }
        pub fn user_id(pk: i32) -> impl Mod<PostTemplate> {
            mod_fn(move |t: &mut PostTemplate| t.user = Parent::Existing(pk))
        }
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
        pub fn views(v: i32) -> impl Mod<PostTemplate> {
            mod_fn(move |t: &mut PostTemplate| t.views = Source::Value(v))
        }
        pub fn published_at(v: DateTime<Utc>) -> impl Mod<PostTemplate> {
            mod_fn(move |t: &mut PostTemplate| t.published_at = Source::Value(v))
        }
        pub fn published_at_null() -> impl Mod<PostTemplate> {
            mod_fn(|t: &mut PostTemplate| t.published_at = Source::Null)
        }

        impl PostTemplate {
            /// The setter, database-free; the FK fills only from `Existing`.
            pub fn build(&self, f: &mut Faker) -> m::Setter {
                m::Setter {
                    id: self.id.resolve(f, |_| Set::Value(SEQ.next_i32())),
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
            /// row.
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

    /// The `comments` factory.
    pub mod comments {
        use chrono::{DateTime, Utc};
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
            pub id: Source<i32>,
            /// Non-null FK: auto-creates the post → user chain.
            pub post: Parent<super::posts::PostTemplate, i32>,
            /// Nullable FK: NULL unless opted in.
            pub user: OptionalParent<super::users::UserTemplate, i32>,
            pub body: Source<String>,
            pub created_at: Source<DateTime<Utc>>,
        }

        /// The entry point.
        pub fn factory(mods: impl Mod<CommentTemplate>) -> CommentTemplate {
            let mut t = CommentTemplate::default();
            mods.apply(&mut t);
            t
        }

        pub fn id(v: i32) -> impl Mod<CommentTemplate> {
            mod_fn(move |t: &mut CommentTemplate| t.id = Source::Value(v))
        }
        pub fn post(p: &pm::Post) -> impl Mod<CommentTemplate> {
            let pk = p.id;
            mod_fn(move |t: &mut CommentTemplate| t.post = Parent::Existing(pk))
        }
        pub fn post_id(pk: i32) -> impl Mod<CommentTemplate> {
            mod_fn(move |t: &mut CommentTemplate| t.post = Parent::Existing(pk))
        }
        pub fn for_post(tpl: super::posts::PostTemplate) -> impl Mod<CommentTemplate> {
            mod_fn(move |t: &mut CommentTemplate| t.post = Parent::Template(tpl))
        }
        pub fn user(u: &um::User) -> impl Mod<CommentTemplate> {
            let pk = u.id;
            mod_fn(move |t: &mut CommentTemplate| t.user = OptionalParent::Existing(pk))
        }
        pub fn user_id(pk: i32) -> impl Mod<CommentTemplate> {
            mod_fn(move |t: &mut CommentTemplate| t.user = OptionalParent::Existing(pk))
        }
        pub fn for_user(tpl: super::users::UserTemplate) -> impl Mod<CommentTemplate> {
            mod_fn(move |t: &mut CommentTemplate| t.user = OptionalParent::Template(tpl))
        }
        pub fn body(v: impl Into<String>) -> impl Mod<CommentTemplate> {
            let v = v.into();
            mod_fn(move |t: &mut CommentTemplate| t.body = Source::Value(v))
        }
        pub fn created_at(v: DateTime<Utc>) -> impl Mod<CommentTemplate> {
            mod_fn(move |t: &mut CommentTemplate| t.created_at = Source::Value(v))
        }

        impl CommentTemplate {
            /// The setter, database-free.
            pub fn build(&self, f: &mut Faker) -> m::Setter {
                m::Setter {
                    id: self.id.resolve(f, |_| Set::Value(SEQ.next_i32())),
                    post_id: match &self.post {
                        Parent::Existing(pk) => Set::Value(*pk),
                        Parent::Auto | Parent::Template(_) => Set::Unset,
                    },
                    user_id: match &self.user {
                        OptionalParent::Existing(pk) => Set::Value(*pk),
                        OptionalParent::Absent | OptionalParent::Template(_) => Set::Unset,
                    },
                    body: self
                        .body
                        .resolve(f, |f| Set::Value(format!("comment-{}", f.alnum(12)))),
                    created_at: self.created_at.resolve(f, |_| Set::Unset),
                }
            }

            /// Create the required chain, the optional parent if opted in,
            /// then the comment.
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

            /// Create `n` chains.
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

// ─────────────────── no-database tests (judged SQL included) ───────────────────

#[test]
fn seeded_builds_reproduce_random_sources_in_i32_shapes() {
    let a = users::factory(()).build(&mut Faker::seeded(7));
    let b = users::factory(()).build(&mut Faker::seeded(7));
    assert_eq!(a.name, b.name);
    assert_eq!(a.email, b.email);
    assert_eq!(a.age, b.age);
    assert_ne!(a.id, b.id, "sequence ids stay outside the seed");
    match a.id {
        Set::Value(id) => assert!(id >= 0, "psql integer ids stay in i32 range"),
        other => panic!("expected a sequence id, got {other:?}"),
    }
}

#[test]
fn the_built_insert_is_judged_sql() {
    use keelson_models::Table as _;

    let s = users::factory((users::id(1), users::name("Ada"), users::email_null()))
        .build(&mut Faker::seeded(0));
    let q = model::users::Users::insert_query(model::users::Setter {
        age: Set::Unset, // keep the judged shape to the pinned columns
        ..s
    });
    let (sql, args) = q.build().expect("build");
    keelson_sqlcheck::assert_sql(
        Dialect::Psql,
        &sql,
        concat!(
            r#"INSERT INTO "users" ("id", "name", "email") VALUES ($1, $2, $3) "#,
            r#"RETURNING "users"."id", "users"."name", "users"."email", "users"."age", "#,
            r#""users"."is_active", "users"."created_at""#
        ),
    );
    assert_eq!(args.len(), 3);
}

#[test]
fn a_chained_build_leaves_the_required_fk_to_create() {
    let s = posts::factory(()).build(&mut Faker::seeded(3));
    assert!(s.user_id.is_unset(), "no database, no parent key");
    let s = posts::factory(posts::user_id(9)).build(&mut Faker::seeded(3));
    assert_eq!(s.user_id, Set::Value(9));
}

// ─────────────────── end-to-end against PostgreSQL 17 ───────────────────

#[cfg(feature = "live-docker")]
mod live {
    use keelson_exec::{BeginExt as _, ExecError, Execute as _, Executor};
    use keelson_factory::Faker;
    use keelson_psql::{Chain as _, arg, quote, select};

    use super::fac::{comments, posts, users};
    use super::model;

    async fn pool() -> keelson_sqlx::psql::Pool {
        // Container startup is blocking (sqlcheck's SyncRunner).
        let url = tokio::task::spawn_blocking(|| keelson_sqlcheck::live::psql_url().to_owned())
            .await
            .unwrap();
        keelson_sqlx::psql::Pool::connect(&url)
            .await
            .expect("connecting to the live PostgreSQL")
    }

    async fn audit_tag_count(db: &dyn Executor, user_id: i32) -> i64 {
        keelson_psql::select((
            select::columns("count(*)"),
            select::from(quote("tags")),
            select::where_(quote("name").eq(arg(format!("audit-user-{user_id}")))),
        ))
        .fetch_scalar(db)
        .await
        .unwrap()
    }

    /// The whole factory contract on one transaction, then rolled back so a
    /// shared or persistent server is left untouched: hooks fire through the
    /// factory's create, a comment chains post and user into existence,
    /// create_many holds uniqueness at n=100, and overrides stop the chain.
    #[tokio::test]
    async fn the_factory_contract_runs_inside_a_rolled_back_transaction() {
        let db = pool().await;

        let out: Result<(), ExecError> = db
            .within(async |tx| {
                // Hooks fire: before_insert normalises, after_insert writes
                // the audit tag on this same transaction.
                let mut f = Faker::from_entropy();
                let u = users::factory(users::email("STEPHEN@Example.COM"))
                    .create_with(tx, &mut f)
                    .await?;
                assert_eq!(u.email.as_deref(), Some("stephen@example.com"));
                assert_eq!(audit_tag_count(tx, u.id).await, 1);

                // The chain: one create makes comment → post → user exist.
                let c = comments::factory(()).create_with(tx, &mut f).await?;
                let p = model::posts::table()
                    .query(model::posts::id().eq(c.post_id))
                    .one(tx)
                    .await?;
                let owner = model::users::table()
                    .query(model::users::id().eq(p.user_id))
                    .one(tx)
                    .await?;
                assert!(owner.is_active, "schema defaults came back via RETURNING");
                assert_eq!(c.user_id, None, "nullable FK stayed NULL");

                // Uniqueness at n=100, on the engine that enforces the PK.
                let us = users::factory(()).create_many(tx, 100).await?;
                let mut ids: Vec<i32> = us.iter().map(|u| u.id).collect();
                ids.sort_unstable();
                ids.dedup();
                assert_eq!(ids.len(), 100);

                // Overrides: an existing parent stops the chain.
                let p2 = posts::factory(posts::user(&u))
                    .create_with(tx, &mut f)
                    .await?;
                assert_eq!(p2.user_id, u.id);
                let c2 = comments::factory((comments::post(&p2), comments::user(&u)))
                    .create_with(tx, &mut f)
                    .await?;
                assert_eq!(c2.post_id, p2.id);
                assert_eq!(c2.user_id, Some(u.id));

                // Roll the whole thing back.
                Err(ExecError::other("deliberate rollback"))
            })
            .await;
        assert_eq!(out.unwrap_err().to_string(), "deliberate rollback");
    }

    /// The commit half, kept small and cleaned up: a factory create persists,
    /// its hook's write included.
    #[tokio::test]
    async fn a_committed_factory_create_persists_with_its_hook_write() {
        let db = pool().await;

        let u = users::factory(()).create(&db).await.unwrap();
        assert_eq!(audit_tag_count(&db, u.id).await, 1);

        // Clean up after ourselves — the server may be shared and persistent.
        keelson_psql::delete((
            keelson_psql::delete::from(quote("tags")),
            keelson_psql::delete::where_(quote("name").eq(arg(format!("audit-user-{}", u.id)))),
        ))
        .execute(&db)
        .await
        .unwrap();
        model::users::table()
            .delete(model::users::id().eq(u.id))
            .exec(&db)
            .await
            .unwrap();
    }
}
