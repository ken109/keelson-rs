//! The hand-written SQLite model — the same codegen specification as
//! `spec_psql.rs`, emitted for the SQLite dialect, and the **always-on
//! end-to-end lane**: every test here runs against real SQLite (in-process,
//! via keelson-sqlx's `sqlite` feature) in a plain `cargo test`.
//!
//! What honestly differs from the psql twin, per `docs/type-mappings.md` and
//! the schema:
//! - `INTEGER` columns are `i64` (SQLite integers are 64-bit; there is no i32
//!   column type to map).
//! - `created_at TEXT DEFAULT CURRENT_TIMESTAMP` is `NaiveDateTime`: the
//!   default writes the naive space-separated form, so `timestamptz`
//!   semantics would be a lie here.
//! - `is_active` is declared `BOOLEAN` (keelson-sqlx reads a declared-BOOLEAN
//!   column back as a real `bool`; the shared grammar schema spells it
//!   `INTEGER` only because grammar tests never execute).

use keelson_core::{Query as _, Value};
use keelson_exec::{BeginExt as _, ExecError, Executor, Statement};
use keelson_models::{hook, null, set};
use keelson_sqlite::{quote, select};
use keelson_sqlx::sqlite::Pool;

use crate::model::{posts, users};

/// What the generator will write for the SQLite rendition of the schema.
// `pub` throughout because that is what the generator will emit into an
// application's models crate; this test binary has no external readers.
#[allow(unreachable_pub, dead_code)]
mod model {
    /// The `users` table, with the same application-written hooks as the psql
    /// twin.
    pub mod users {
        use chrono::NaiveDateTime;
        use keelson_core::expr::Expr;
        use keelson_core::mod_fn;
        use keelson_exec::{ExecError, Execute as _, Executor, FromRow, Row};
        use keelson_models::{
            Column, ModelSelect, ModelTable, Set, Table, View, attach_to_many, loader,
        };
        use keelson_sqlite::{Mod, arg, delete, insert, quote, select, update};

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
            /// Relations, filled by `then_load` mods.
            pub rel: Rel,
        }

        /// `users`' relations.
        #[derive(Debug, Clone, PartialEq, Default)]
        pub struct Rel {
            /// Has-many `posts`.
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
        pub fn is_active() -> Column<bool> {
            Column::new("users", "is_active")
        }
        pub fn created_at() -> Column<NaiveDateTime> {
            Column::new("users", "created_at")
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
            (id(), name(), email(), age(), is_active(), created_at())
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
            ) -> keelson_exec::ExecFuture<'a, Result<(), ExecError>> {
                Box::pin(async move {
                    if let Set::Value(email) = &mut setter.email {
                        *email = email.to_lowercase();
                    }
                    Ok(())
                })
            }

            /// Write an audit tag on the caller's executor — the hook the
            /// transaction tests pin.
            fn after_insert<'a>(
                db: &'a dyn Executor,
                rows: &'a [User],
            ) -> keelson_exec::ExecFuture<'a, Result<(), ExecError>> {
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

        /// Then-load mods.
        pub mod then_load {
            use super::*;

            /// Load each user's posts (to-many) with one keyed query.
            pub fn posts() -> impl Mod<ModelSelect<Users>> {
                mod_fn(|q: &mut ModelSelect<Users>| {
                    q.add_loader(loader(|db, users| Box::pin(load_posts(db, users))));
                })
            }

            async fn load_posts(db: &dyn Executor, users: &mut [User]) -> Result<(), ExecError> {
                let mut ids: Vec<i64> = users.iter().map(|u| u.id).collect();
                ids.sort_unstable();
                ids.dedup();
                if ids.is_empty() {
                    return Ok(());
                }
                let posts = super::super::posts::table()
                    .query(super::super::posts::user_id().in_(ids))
                    .all(db)
                    .await?;
                attach_to_many(
                    users,
                    posts,
                    |u| u.id,
                    |p| p.user_id,
                    |u, ps| {
                        u.rel.posts = ps;
                    },
                );
                Ok(())
            }
        }
    }

    /// The `posts` table.
    pub mod posts {
        use chrono::NaiveDateTime;
        use keelson_core::expr::Expr;
        use keelson_core::mod_fn;
        use keelson_exec::{ExecError, Executor, FromRow, Row};
        use keelson_models::{
            Column, ModelSelect, ModelTable, Set, Table, View, attach_to_one, loader, mapper_mod,
        };
        use keelson_sqlite::{Chain as _, Mod, delete, insert, quote, select, update};

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
            /// Relations, filled by `preload`/`then_load` mods.
            pub rel: Rel,
        }

        /// `posts`' relations.
        #[derive(Debug, Clone, PartialEq, Default)]
        pub struct Rel {
            /// Belongs-to `users`.
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
        pub fn status() -> Column<String> {
            Column::new("posts", "status")
        }
        pub fn views() -> Column<i64> {
            Column::new("posts", "views")
        }
        pub fn published_at() -> Column<NaiveDateTime> {
            Column::new("posts", "published_at")
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
            (id(), user_id(), title(), status(), views(), published_at())
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

        /// Preload mods.
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
                        post.rel.user = user_from_preload(row)?;
                        Ok(())
                    }));
                })
            }

            /// Decode the prefixed columns; the PK column decides a `LEFT
            /// JOIN` miss.
            pub fn user_from_preload(
                row: &mut Row,
            ) -> Result<Option<super::super::users::User>, ExecError> {
                if matches!(
                    row.value("user.id"),
                    None | Some(keelson_sqlite::Value::Null)
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

            /// Load each post's user (to-one) with one keyed second query.
            pub fn user() -> impl Mod<ModelSelect<Posts>> {
                mod_fn(|q: &mut ModelSelect<Posts>| {
                    q.add_loader(loader(|db, posts| Box::pin(load_user(db, posts))));
                })
            }

            async fn load_user(db: &dyn Executor, posts: &mut [Post]) -> Result<(), ExecError> {
                let mut ids: Vec<i64> = posts.iter().map(|p| p.user_id).collect();
                ids.sort_unstable();
                ids.dedup();
                if ids.is_empty() {
                    return Ok(());
                }
                let users = super::super::users::table()
                    .query(super::super::users::id().in_(ids))
                    .all(db)
                    .await?;
                attach_to_one(
                    posts,
                    users,
                    |p| p.user_id,
                    |u| u.id,
                    |p, u| {
                        p.rel.user = u;
                    },
                );
                Ok(())
            }
        }
    }
}

// ─────────────────────────── SQL shape (judged) ───────────────────────────

#[test]
fn the_sqlite_rendition_of_the_agreed_call_site() {
    let q = users::table().query((users::age().gte(21), select::limit(20)));
    let (sql, args) = q.build().unwrap();
    keelson_sqlcheck::assert_sql(
        keelson_sqlcheck::Dialect::Sqlite,
        &sql,
        concat!(
            r#"SELECT "users"."id", "users"."name", "users"."email", "users"."age", "#,
            r#""users"."is_active", "users"."created_at" FROM "users" "#,
            r#"WHERE ("users"."age" >= ?1) LIMIT 20"#
        ),
    );
    assert_eq!(args, vec![Value::I64(21)]);
}

/// The LEFT JOIN miss: the mapper mod turns an all-NULL prefix into `None`.
/// Unit-shaped because the schema's `user_id` is `NOT NULL`, so a live miss
/// cannot be provoked — which is itself worth knowing about the mapping.
#[test]
fn a_preload_miss_maps_to_none() {
    use keelson_exec::{Column as ExecColumn, Row};
    use std::sync::Arc;

    let columns: Arc<[ExecColumn]> = ["user.id", "user.name"]
        .into_iter()
        .map(ExecColumn::new)
        .collect::<Vec<_>>()
        .into();
    let mut row = Row::new(columns, vec![Value::Null, Value::Null]);
    let loaded = posts::preload::user_from_preload(&mut row).unwrap();
    assert_eq!(loaded, None);
}

// ─────────────────────── end-to-end (real SQLite) ───────────────────────

async fn db() -> Pool {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "keelson-models-{}-{}.db",
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
        "CREATE TABLE tags (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE)",
    ] {
        pool.execute(Statement::new(ddl, vec![])).await.unwrap();
    }
    pool
}

async fn tag_count(db: &dyn Executor, name: String) -> i64 {
    use keelson_exec::Execute as _;
    use keelson_sqlite::{Chain as _, arg};
    keelson_sqlite::select((
        select::columns("count(*)"),
        select::from(quote("tags")),
        select::where_(quote("name").eq(arg(name))),
    ))
    .fetch_scalar(db)
    .await
    .unwrap()
}

#[tokio::test]
async fn a_partial_setter_inserts_and_defaults_come_back() {
    let db = db().await;
    // id unset: SQLite assigns the rowid. is_active/created_at unset: the
    // schema defaults come back through RETURNING.
    let u = users::table()
        .insert(users::Setter {
            name: set("Stephen"),
            ..Default::default()
        })
        .one(&db)
        .await
        .unwrap();
    assert_eq!(u.id, 1);
    assert_eq!(u.name, "Stephen");
    assert_eq!(u.email, None);
    assert!(u.is_active, "schema default, read back via RETURNING");
    assert!(u.created_at.and_utc().timestamp() > 0);
}

#[tokio::test]
async fn typed_queries_and_layer_1_mods_run_together() {
    let db = db().await;
    for (name, age) in [("kid", 12i64), ("teen", 19), ("ada", 36), ("bob", 41)] {
        users::table()
            .insert(users::Setter {
                name: set(name),
                age: set(age),
                ..Default::default()
            })
            .exec(&db)
            .await
            .unwrap();
    }

    let adults = users::table()
        .query((
            users::age().gte(21i64),
            select::where_(r#""users"."name" <> 'bob'"#), // raw fragment, same tuple
            select::order_by(users::age()).desc(),
            select::limit(20),
        ))
        .all(&db)
        .await
        .unwrap();
    assert_eq!(
        adults.iter().map(|u| u.name.as_str()).collect::<Vec<_>>(),
        vec!["ada"]
    );

    // one / optional semantics match the execution layer's.
    let one = users::table().query(users::name().eq("ada")).one(&db).await;
    assert_eq!(one.unwrap().age, Some(36));
    let none = users::table()
        .query(users::name().eq("nobody"))
        .optional(&db)
        .await
        .unwrap();
    assert!(none.is_none());
    let too_many = users::table().query(()).one(&db).await;
    assert!(matches!(too_many, Err(ExecError::TooManyRows)));
}

/// The hooks contract, end to end: `before_insert` rewrote the setter,
/// `after_insert` wrote on the **caller's transaction** — visible inside it,
/// gone after its rollback, kept after a commit.
#[tokio::test]
async fn hooks_observe_the_callers_transaction() {
    let db = db().await;

    // Rollback half.
    let out: Result<(), ExecError> = db
        .within(async |tx| {
            let u = users::table()
                .insert(users::Setter {
                    name: set("Stephen"),
                    email: set("STEPHEN@Example.COM"),
                    ..Default::default()
                })
                .one(tx)
                .await?;
            assert_eq!(
                u.email.as_deref(),
                Some("stephen@example.com"),
                "before_insert normalised the setter"
            );
            assert_eq!(
                tag_count(tx, format!("audit-user-{}", u.id)).await,
                1,
                "after_insert's write is visible inside the transaction"
            );
            Err(ExecError::other("deliberate rollback"))
        })
        .await;
    assert!(out.is_err());
    assert_eq!(
        tag_count(&db, "audit-user-1".to_owned()).await,
        0,
        "the hook's write rolled back with the caller — it ran on the same transaction"
    );
    let none = users::table().query(()).all(&db).await.unwrap();
    assert!(none.is_empty());

    // Commit half.
    let committed: Result<i64, ExecError> = db
        .within(async |tx| {
            let u = users::table()
                .insert(users::Setter {
                    name: set("kept"),
                    ..Default::default()
                })
                .one(tx)
                .await?;
            Ok(u.id)
        })
        .await;
    let uid = committed.unwrap();
    assert_eq!(tag_count(&db, format!("audit-user-{uid}")).await, 1);
}

/// Ad-hoc `ExecHook`s — the `QueryExtensions` hook channel — run before the
/// query, on the caller's executor.
#[tokio::test]
async fn query_attached_hooks_run_in_the_callers_transaction() {
    use keelson_exec::Execute as _;
    use keelson_sqlite::{arg, insert};

    let db = db().await;
    let mut q = users::table().query(());
    q.add_hook(hook(|db| {
        Box::pin(async move {
            keelson_sqlite::insert((
                insert::into(quote("tags")).columns(["name"]),
                insert::values(arg("hook-ran")),
            ))
            .execute(db)
            .await?;
            Ok(())
        })
    }));

    let out: Result<(), ExecError> = db
        .within(async |tx| {
            q.all(tx).await?;
            assert_eq!(tag_count(tx, "hook-ran".to_owned()).await, 1);
            Err(ExecError::other("deliberate rollback"))
        })
        .await;
    assert!(out.is_err());
    assert_eq!(tag_count(&db, "hook-ran".to_owned()).await, 0);
}

#[tokio::test]
async fn preload_and_then_load_fill_rel_both_ways() {
    let db = db().await;
    let stephen = users::table()
        .insert(users::Setter {
            name: set("Stephen"),
            ..Default::default()
        })
        .one(&db)
        .await
        .unwrap();
    let ada = users::table()
        .insert(users::Setter {
            name: set("Ada"),
            ..Default::default()
        })
        .one(&db)
        .await
        .unwrap();
    for (uid, title) in [
        (stephen.id, "keel laid"),
        (stephen.id, "second"),
        (ada.id, "notes"),
    ] {
        posts::table()
            .insert(posts::Setter {
                user_id: set(uid),
                title: set(title),
                ..Default::default()
            })
            .exec(&db)
            .await
            .unwrap();
    }

    // Preload: to-one via LEFT JOIN in the same query.
    let loaded = posts::table()
        .query((posts::preload::user(), posts::title().eq("keel laid")))
        .one(&db)
        .await
        .unwrap();
    let author = loaded.rel.user.expect("preloaded user");
    assert_eq!(author.name, "Stephen");

    // Then-load, to-many: each user gets exactly their own posts.
    let with_posts = users::table()
        .query((users::then_load::posts(), select::order_by(users::id())))
        .all(&db)
        .await
        .unwrap();
    assert_eq!(with_posts[0].rel.posts.len(), 2);
    assert_eq!(with_posts[1].rel.posts.len(), 1);
    assert_eq!(with_posts[1].rel.posts[0].title, "notes");

    // Then-load, to-one.
    let with_user = posts::table()
        .query((posts::then_load::user(), posts::title().eq("notes")))
        .one(&db)
        .await
        .unwrap();
    assert_eq!(with_user.rel.user.unwrap().name, "Ada");
}

#[tokio::test]
async fn update_and_delete_flow_through_setter_and_filters() {
    let db = db().await;
    for name in ["a", "b", "c"] {
        users::table()
            .insert(users::Setter {
                name: set(name),
                email: set(format!("{name}@x.dev")),
                ..Default::default()
            })
            .exec(&db)
            .await
            .unwrap();
    }

    // Partial setter: age set, email erased with an explicit NULL; the other
    // columns untouched.
    let done = users::table()
        .update(
            users::Setter {
                age: set(30i64),
                email: null(),
                ..Default::default()
            },
            users::name().eq("b"),
        )
        .exec(&db)
        .await
        .unwrap();
    assert_eq!(done.rows_affected, 1);

    let b = users::table()
        .query(users::name().eq("b"))
        .one(&db)
        .await
        .unwrap();
    assert_eq!(b.age, Some(30));
    assert_eq!(b.email, None, "null() erased it");
    assert!(b.is_active, "unset columns stayed untouched");

    let done = users::table()
        .delete(users::name().in_(["a", "c"]))
        .exec(&db)
        .await
        .unwrap();
    assert_eq!(done.rows_affected, 2);
    let left = users::table().query(()).all(&db).await.unwrap();
    assert_eq!(left.len(), 1);
}

/// Progressive enhancement on a typed insert: a dialect `INSERT` mod rides in
/// through `.with(…)`.
#[tokio::test]
async fn dialect_insert_mods_mix_in_through_with() {
    use keelson_sqlite::insert;

    let db = db().await;
    users::table()
        .insert(users::Setter {
            id: set(7i64),
            name: set("first"),
            ..Default::default()
        })
        .exec(&db)
        .await
        .unwrap();

    // Same PK again: ON CONFLICT DO NOTHING turns the violation into a no-op.
    let done = users::table()
        .insert(users::Setter {
            id: set(7i64),
            name: set("second"),
            ..Default::default()
        })
        .with(insert::on_conflict("id").do_nothing())
        .exec(&db)
        .await
        .unwrap();
    assert_eq!(done.rows_affected, 0);
    let u = users::table()
        .query(users::id().eq(7i64))
        .one(&db)
        .await
        .unwrap();
    assert_eq!(u.name, "first");
}
