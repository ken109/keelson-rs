//! The hand-written PostgreSQL model for the shared schema's `users`/`posts`
//! tables — **the code generator's specification**. Every shape in
//! `mod model` below is what the generator will emit for this schema (plus
//! the application-written hook overrides, which the generator preserves);
//! it is written once by hand here so it can be judged and executed before a
//! generator exists.
//!
//! The SQL-shape tests run the model's statements through the same judges as
//! everything else (grammar always; the real PostgreSQL 17 when compiled with
//! `--features live-docker`, which also unlocks the end-to-end tests at the
//! bottom).

use keelson_core::{Query as _, QueryExtensions as _, Value};
use keelson_models::{null, set};
use keelson_psql::select;
use keelson_sqlcheck::Dialect;

use crate::model::{posts, user_emails, users};

/// What the generator will write for `tests/schema/psql.sql`.
// `pub` throughout because that is what the generator will emit into an
// application's models crate; in this test binary nothing external can reach
// it, which is what the lint (correctly, and irrelevantly) notices.
#[allow(unreachable_pub, dead_code)]
mod model {
    /// The `users` table: a full [`Table`](keelson_models::Table) with
    /// application-written hooks.
    pub mod users {
        use chrono::{DateTime, Utc};
        use keelson_core::expr::Expr;
        use keelson_exec::{ExecError, ExecFuture, Execute as _, Executor, FromRow, Row};
        use keelson_models::{Column, ModelTable, Set, Table, ThenLoad, View, attach_to_many};
        use keelson_psql::{Mod, arg, delete, insert, quote, select, update};

        /// The model marker `users::table()` hangs off. Carries no data — the
        /// associated types and hooks live on it.
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
            pub created_at: DateTime<Utc>,
            /// Relations, filled by `preload`/`then_load` mods; empty
            /// otherwise.
            pub rel: Rel,
        }

        /// `users`' relations. (`rel`, not bob's `r` — see the crate docs.)
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
            pub created_at: Set<DateTime<Utc>>,
        }

        /// The entry point: `users::table().query(…)` / `.insert(…)` / ….
        pub fn table() -> ModelTable<Users> {
            ModelTable::new()
        }

        // The one column entry point apiece: expression, filter origin and
        // alias carrier at once. Types from docs/type-mappings.md.
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
        pub fn created_at() -> Column<DateTime<Utc>> {
            Column::new("users", "created_at")
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
            (id(), name(), email(), age(), is_active(), created_at())
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
                // No set fields renders `DEFAULT VALUES` — the row the
                // schema's defaults describe.
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

            // ── application-written hooks (the generator preserves these) ──

            /// Normalise the email before it is written. Receives the setter
            /// mutably, so it can also tell unset from NULL.
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
            /// the transaction tests pin.
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

        /// Then-load mods: a second query keyed by the first's rows, plus
        /// whatever deeper levels the caller hangs off them.
        pub mod then_load {
            use super::*;

            /// Load each user's posts (to-many), one keyed query per batch of
            /// [`KEY_BATCH`](keelson_models::KEY_BATCH) keys. `.then(…)`
            /// loads a relation *of* those posts.
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
        use chrono::{DateTime, Utc};
        use keelson_core::expr::Expr;
        use keelson_core::mod_fn;
        use keelson_exec::{ExecError, FromRow, Row};
        use keelson_models::{
            Column, ModelSelect, ModelTable, Set, Table, ThenLoad, View, attach_to_one, mapper_mod,
        };
        use keelson_psql::{Chain as _, Mod, delete, insert, quote, select, update};

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
            /// Relations, filled by `preload`/`then_load` mods.
            pub rel: Rel,
        }

        /// `posts`' relations.
        #[derive(Debug, Clone, PartialEq, Default)]
        pub struct Rel {
            /// Belongs-to `users`, via `posts.user_id`.
            pub user: Option<super::users::User>,
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
        pub fn status() -> Column<String> {
            Column::new("posts", "status")
        }
        pub fn views() -> Column<i32> {
            Column::new("posts", "views")
        }
        pub fn published_at() -> Column<DateTime<Utc>> {
            Column::new("posts", "published_at")
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
            (id(), user_id(), title(), status(), views(), published_at())
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

        /// Preload mods: the relation joins into the *same* query.
        pub mod preload {
            use super::*;

            /// Same-query `LEFT JOIN` preload of the to-one `user`.
            ///
            /// Adds the join, appends `"user."`-prefixed columns through the
            /// dialect's `preload_columns` (kept apart from the caller's own
            /// projection), and registers the mapper mod that reads them back
            /// out of each row.
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
                        post.rel.user = user_from_preload(row)?;
                        Ok(())
                    }));
                })
            }

            /// Decode the prefixed columns; a `LEFT JOIN` that matched
            /// nothing hands back NULLs, and the primary-key column decides.
            pub fn user_from_preload(
                row: &mut Row,
            ) -> Result<Option<super::super::users::User>, ExecError> {
                if matches!(row.value("user.id"), None | Some(keelson_psql::Value::Null)) {
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
                                p.rel.user = u;
                            },
                        );
                    },
                )
            }
        }
    }

    /// A `SELECT`-only model: [`View`](keelson_models::View) without
    /// [`Table`](keelson_models::Table) — no primary key required, and
    /// `view().insert(…)` / `.update(…)` / `.delete(…)` do not compile.
    pub mod user_emails {
        use keelson_exec::{ExecError, FromRow, Row};
        use keelson_models::{Column, ModelTable, View};
        use keelson_psql::{quote, select};

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
            type Select = keelson_psql::SelectQuery;

            fn base_select() -> Self::Select {
                keelson_psql::select((
                    select::columns((id(), email())),
                    select::from(quote("users")),
                ))
            }
        }
    }
}

// ───────────────────────── SQL-shape tests (judged) ─────────────────────────

#[track_caller]
fn assert_psql(q: &impl keelson_core::Query, expected: &str) -> Vec<Value> {
    let (sql, args) = q.build().expect("build");
    keelson_sqlcheck::assert_sql(Dialect::Psql, &sql, expected);
    args
}

const USER_COLS: &str = concat!(
    r#""users"."id", "users"."name", "users"."email", "users"."age", "#,
    r#""users"."is_active", "users"."created_at""#
);

const POST_COLS: &str = concat!(
    r#""posts"."id", "posts"."user_id", "posts"."title", "posts"."status", "#,
    r#""posts"."views", "posts"."published_at""#
);

#[test]
fn the_agreed_call_site_shape_builds_the_agreed_sql() {
    // The design doc's exact call site: a typed filter and a Layer 1 mod in
    // one tuple.
    let q = users::table().query((
        users::age().gte(21), // typed: `users::age().gte("x")` does not compile
        select::limit(20),    // Layer 1 mods mix in directly
    ));
    let args = assert_psql(
        &q,
        &format!(r#"SELECT {USER_COLS} FROM "users" WHERE ("users"."age" >= $1) LIMIT 20"#),
    );
    assert_eq!(args, vec![Value::I32(21)]);
}

#[test]
fn a_partial_setter_inserts_only_the_set_columns() {
    use keelson_models::Table as _;
    let q = model::users::Users::insert_query(users::Setter {
        name: set("Stephen"),
        email: set("stephen@example.com"),
        ..Default::default()
    });
    let args = assert_psql(
        &q,
        &format!(r#"INSERT INTO "users" ("name", "email") VALUES ($1, $2) RETURNING {USER_COLS}"#),
    );
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
    // NULL appears, bound; unset does not appear at all.
    let q = model::users::Users::insert_query(users::Setter {
        name: set("kay"),
        email: null(),
        ..Default::default()
    });
    let args = assert_psql(
        &q,
        &format!(r#"INSERT INTO "users" ("name", "email") VALUES ($1, $2) RETURNING {USER_COLS}"#),
    );
    assert_eq!(args, vec![Value::Text("kay".into()), Value::Null]);
}

#[test]
fn an_all_unset_setter_is_default_values() {
    use keelson_models::Table as _;
    let q = model::users::Users::insert_query(users::Setter::default());
    assert_psql(
        &q,
        &format!(r#"INSERT INTO "users" DEFAULT VALUES RETURNING {USER_COLS}"#),
    );
}

#[test]
fn update_sets_only_the_set_fields_and_filters_typed() {
    use keelson_models::Table as _;
    use keelson_psql::Mod as _;
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
    let args = assert_psql(
        &q,
        r#"UPDATE "users" SET "email" = $1, "age" = $2 WHERE ("users"."id" = $3)"#,
    );
    assert_eq!(args, vec![Value::Null, Value::I32(30), Value::I32(7)]);
}

#[test]
fn delete_takes_the_same_typed_filters() {
    use keelson_models::Table as _;
    use keelson_psql::Mod as _;
    let mut q = model::users::Users::delete_query();
    users::id().in_([1, 2]).apply(&mut q);
    let args = assert_psql(
        &q,
        r#"DELETE FROM "users" WHERE ("users"."id" IN ($1, $2))"#,
    );
    assert_eq!(args.len(), 2);
}

#[test]
fn a_preload_is_one_left_joined_query_with_prefixed_columns() {
    let q = posts::table().query((posts::preload::user(), posts::views().gte(10)));
    let args = assert_psql(
        &q,
        &format!(
            concat!(
                r#"SELECT {}, "#,
                r#""users"."id" AS "user.id", "users"."name" AS "user.name", "#,
                r#""users"."email" AS "user.email", "users"."age" AS "user.age", "#,
                r#""users"."is_active" AS "user.is_active", "#,
                r#""users"."created_at" AS "user.created_at" "#,
                r#"FROM "posts" LEFT JOIN "users" ON ("users"."id" = "posts"."user_id") "#,
                r#"WHERE ("posts"."views" >= $1)"#
            ),
            POST_COLS
        ),
    );
    assert_eq!(args, vec![Value::I32(10)]);
}

#[test]
fn raw_fragments_and_dialect_mods_mix_into_a_view_query() {
    // Progressive enhancement on a SELECT-only model: a typed filter, a raw
    // &str WHERE fragment and dialect mods in one tuple.
    let q = user_emails::view().query((
        user_emails::email().is_not_null(),
        select::where_(r#""users"."age" IS NOT NULL"#),
        select::order_by(user_emails::id()).desc(),
        select::limit(5),
    ));
    assert_psql(
        &q,
        concat!(
            r#"SELECT "users"."id", "users"."email" FROM "users" "#,
            r#"WHERE ("users"."email" IS NOT NULL) AND "users"."age" IS NOT NULL "#,
            r#"ORDER BY "users"."id" DESC LIMIT 5"#
        ),
    );
}

#[test]
fn aliased_as_follows_a_table_alias() {
    use keelson_psql::quote;
    // When a query aliases the table, the column follows: the alias carrier
    // half of the one-column-entry-point decision.
    let q = keelson_psql::select((
        select::columns(users::id().aliased_as("u")),
        select::from(quote("users")).as_("u"),
        select::where_(users::age().aliased_as("u").gte(21)),
    ));
    let args = assert_psql(
        &q,
        r#"SELECT "u"."id" FROM "users" AS "u" WHERE ("u"."age" >= $1)"#,
    );
    assert_eq!(args, vec![Value::I32(21)]);
}

#[test]
fn the_query_extensions_wiring_is_observable() {
    // The QueryExtensions payloads core left as type parameters, pinned and
    // populated: a preload registers a mapper mod, a then-load registers a
    // loader, and the query still builds as a plain statement.
    let q = posts::table().query((posts::preload::user(), posts::then_load::user()));
    assert_eq!(q.mapper_mods().len(), 1);
    assert_eq!(q.loaders().len(), 1);
    assert!(q.hooks().is_empty());
    assert_eq!(q.query_type(), keelson_core::QueryType::Select);
}

#[test]
fn a_nested_path_is_one_loader_and_leaves_the_statement_alone() {
    // Nesting lives inside the level, not on the query: however deep the
    // path, the caller's query carries exactly one loader, and the statement
    // it builds is the same unadorned SELECT. That is what lets a level
    // deduplicate and batch before the next one runs.
    let q = posts::table().query(
        posts::then_load::user().then(users::then_load::posts().then(posts::then_load::user())),
    );
    assert_eq!(q.loaders().len(), 1);
    assert!(
        q.mapper_mods().is_empty(),
        "a then-load, nested or not, adds nothing to the statement"
    );
    assert_psql(&q, &format!(r#"SELECT {POST_COLS} FROM "posts""#));
}

// ─────────────────── end-to-end against PostgreSQL 17 ───────────────────

#[cfg(feature = "live-docker")]
mod live {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicI32, Ordering};

    use keelson_exec::{BeginExt as _, ExecError, Execute as _, Executor};
    use keelson_models::{null, set};
    use keelson_psql::{Chain as _, arg, quote, select};

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

    /// The whole model flow on one transaction, then rolled back: partial
    /// setter insert (defaults come back through RETURNING), the
    /// before-insert setter rewrite, the after-insert hook's write observed
    /// *inside* the same transaction, preload and then-load — and after the
    /// rollback, none of it happened, the hook's write included. That last
    /// assertion is the proof the hook ran on the caller's transaction.
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
                // Unset columns took their schema defaults, read back via
                // RETURNING.
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

                // Update through a partial setter; delete through a filter.
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

    /// An executor that records the statements it runs, so a load path's cost
    /// can be asserted rather than assumed.
    #[derive(Debug)]
    struct Counting<E> {
        inner: E,
        sql: std::sync::Mutex<Vec<String>>,
    }

    impl<E> Counting<E> {
        fn new(inner: E) -> Self {
            Counting {
                inner,
                sql: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn seen(&self) -> Vec<String> {
            self.sql.lock().unwrap().clone()
        }
    }

    impl<E: Executor> Executor for Counting<E> {
        fn family(&self) -> keelson_exec::Family {
            self.inner.family()
        }

        fn fetch(
            &self,
            stmt: keelson_exec::Statement,
        ) -> keelson_exec::ExecFuture<'_, Result<Vec<keelson_exec::Row>, ExecError>> {
            self.sql.lock().unwrap().push(stmt.sql.clone());
            self.inner.fetch(stmt)
        }

        fn execute(
            &self,
            stmt: keelson_exec::Statement,
        ) -> keelson_exec::ExecFuture<'_, Result<keelson_exec::ExecResult, ExecError>> {
            self.sql.lock().unwrap().push(stmt.sql.clone());
            self.inner.execute(stmt)
        }
    }

    /// The nested path against the real server, inside one transaction:
    /// posts → author → the author's posts, three queries, correctly
    /// associated, the shared author fetched once — and a cyclic path that
    /// terminates where it was written.
    #[tokio::test]
    async fn nested_loads_arrive_associated_and_cost_one_query_per_level() {
        let db = pool().await;
        let uid = key();
        let uid2 = key();
        let pid = key();
        let pid2 = key();
        let pid3 = key();

        let out: Result<(), ExecError> = db
            .within(async |tx| {
                for (id, name) in [(uid, "Stephen"), (uid2, "Ada")] {
                    users::table()
                        .insert(users::Setter {
                            id: set(id),
                            name: set(name),
                            ..Default::default()
                        })
                        .one(tx)
                        .await?;
                }
                for (id, user_id, title) in [
                    (pid, uid, "keel laid"),
                    (pid2, uid, "second"),
                    (pid3, uid2, "notes"),
                ] {
                    posts::table()
                        .insert(posts::Setter {
                            id: set(id),
                            user_id: set(user_id),
                            title: set(title),
                            ..Default::default()
                        })
                        .one(tx)
                        .await?;
                }

                let counting = Counting::new(tx);
                let loaded = posts::table()
                    .query((
                        posts::then_load::user().then(users::then_load::posts()),
                        posts::id().in_([pid, pid2, pid3]),
                        select::order_by(posts::id()),
                    ))
                    .all(&counting)
                    .await?;

                let sql = counting.seen();
                assert_eq!(
                    sql.len(),
                    3,
                    "one query per level, not one per row: {sql:#?}"
                );
                assert_eq!(
                    sql[1].matches('$').count(),
                    2,
                    "three posts, two distinct authors — the key is deduplicated"
                );

                let authors: Vec<&str> = loaded
                    .iter()
                    .map(|p| p.rel.user.as_ref().expect("author").name.as_str())
                    .collect();
                assert_eq!(authors, vec!["Stephen", "Stephen", "Ada"]);
                let stephens: Vec<&str> = loaded[0]
                    .rel
                    .user
                    .as_ref()
                    .unwrap()
                    .rel
                    .posts
                    .iter()
                    .map(|p| p.title.as_str())
                    .collect();
                assert_eq!(stephens, vec!["keel laid", "second"]);
                assert_eq!(
                    loaded[0].rel.user, loaded[1].rel.user,
                    "the shared author was loaded once, its own relation already filled"
                );

                // A cyclic path terminates at the depth it was written to.
                let counting = Counting::new(tx);
                let cyclic = posts::table()
                    .query((
                        posts::then_load::user()
                            .then(users::then_load::posts().then(posts::then_load::user())),
                        posts::id().eq(pid),
                    ))
                    .one(&counting)
                    .await?;
                assert_eq!(counting.seen().len(), 4, "four levels, four queries");
                let author = cyclic.rel.user.as_ref().expect("author");
                let again = author.rel.posts[0]
                    .rel
                    .user
                    .as_ref()
                    .expect("the author again");
                assert_eq!(again.id, author.id);
                assert!(
                    again.rel.posts.is_empty(),
                    "the cycle stopped where the path stopped"
                );

                Err(ExecError::other("deliberate rollback"))
            })
            .await;
        assert_eq!(out.unwrap_err().to_string(), "deliberate rollback");
    }

    /// The commit half: on a plain pool the hook's write persists with the
    /// insert.
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
        keelson_psql::delete((
            keelson_psql::delete::from(quote("tags")),
            keelson_psql::delete::where_(quote("name").eq(arg(format!("audit-user-{uid}")))),
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
}
