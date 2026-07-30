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
        use keelson_exec::{ExecError, Execute as _, Executor, FromRow, Row};
        use keelson_models::{Column, ModelTable, Set, Table, ThenLoad, View, attach_to_many};
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

            /// Load each user's posts (to-many) with one keyed query per
            /// batch. `.then(…)` hangs the next level off it.
            pub fn posts() -> ThenLoad<Users, super::super::posts::Posts, i64> {
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

    /// The `posts` table.
    pub mod posts {
        use chrono::NaiveDateTime;
        use keelson_core::expr::Expr;
        use keelson_core::mod_fn;
        use keelson_exec::{ExecError, FromRow, Row};
        use keelson_models::{
            Column, ModelSelect, ModelTable, Set, Table, ThenLoad, View, attach_to_one, mapper_mod,
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

            /// Load each post's user (to-one) with one keyed query per batch.
            /// `.then(…)` hangs the next level off it.
            pub fn user() -> ThenLoad<Posts, super::super::users::Users, i64> {
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

// ──────────────────── nested then-load (relations of relations) ────────────

/// An executor that records every statement it runs. Nesting is only worth
/// having if it costs one query per *level* rather than one per row, so the
/// specs assert the count — a regression to N+1 fails the test instead of
/// merely being slower.
#[derive(Debug)]
struct Counting {
    inner: Pool,
    sql: std::sync::Mutex<Vec<String>>,
}

impl Counting {
    fn new(inner: Pool) -> Self {
        Counting {
            inner,
            sql: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// The statements seen since the last [`reset`](Counting::reset).
    fn seen(&self) -> Vec<String> {
        self.sql.lock().unwrap().clone()
    }

    fn reset(&self) {
        self.sql.lock().unwrap().clear();
    }
}

impl Executor for Counting {
    fn family(&self) -> keelson_exec::Family {
        self.inner.family()
    }

    fn fetch(
        &self,
        stmt: Statement,
    ) -> keelson_exec::ExecFuture<'_, Result<Vec<keelson_exec::Row>, ExecError>> {
        self.sql.lock().unwrap().push(stmt.sql.clone());
        self.inner.fetch(stmt)
    }

    fn execute(
        &self,
        stmt: Statement,
    ) -> keelson_exec::ExecFuture<'_, Result<keelson_exec::ExecResult, ExecError>> {
        self.sql.lock().unwrap().push(stmt.sql.clone());
        self.inner.execute(stmt)
    }
}

#[track_caller]
fn assert_sqlite(sql: &str, expected: &str) {
    keelson_sqlcheck::assert_sql(keelson_sqlcheck::Dialect::Sqlite, sql, expected);
}

/// How many bound parameters a recorded statement carries — the size of an
/// `IN` list, which is what batching and deduplication are about.
fn args_in(sql: &str) -> usize {
    sql.matches('?').count()
}

const SQLITE_USER_COLS: &str = concat!(
    r#""users"."id", "users"."name", "users"."email", "users"."age", "#,
    r#""users"."is_active", "users"."created_at""#
);

const SQLITE_POST_COLS: &str = concat!(
    r#""posts"."id", "posts"."user_id", "posts"."title", "posts"."status", "#,
    r#""posts"."views", "posts"."published_at""#
);

/// Stephen with two posts, Ada with one — so the author of two posts is the
/// deduplication case, and every level has something to attach.
async fn seed_two_authors(db: &dyn Executor) {
    for name in ["Stephen", "Ada"] {
        users::table()
            .insert(users::Setter {
                name: set(name),
                ..Default::default()
            })
            .exec(db)
            .await
            .unwrap();
    }
    for (uid, title) in [(1i64, "keel laid"), (1, "second"), (2, "notes")] {
        posts::table()
            .insert(posts::Setter {
                user_id: set(uid),
                title: set(title),
                ..Default::default()
            })
            .exec(db)
            .await
            .unwrap();
    }
}

/// posts → author → the author's posts: three queries for three levels, the
/// nested objects correctly associated, and the shared author loaded once.
#[tokio::test]
async fn a_nested_then_load_costs_one_query_per_level() {
    let db = Counting::new(db().await);
    seed_two_authors(&db).await;
    db.reset();

    let posts = posts::table()
        .query((
            posts::then_load::user().then(users::then_load::posts()),
            select::order_by(posts::id()),
        ))
        .all(&db)
        .await
        .unwrap();

    let sql = db.seen();
    assert_eq!(
        sql.len(),
        3,
        "one query per level — the caller's, the authors', the authors' posts: {sql:#?}"
    );
    assert_sqlite(
        &sql[0],
        &format!(r#"SELECT {SQLITE_POST_COLS} FROM "posts" ORDER BY "posts"."id""#),
    );
    assert_sqlite(
        &sql[1],
        &format!(r#"SELECT {SQLITE_USER_COLS} FROM "users" WHERE ("users"."id" IN (?1, ?2))"#),
    );
    assert_sqlite(
        &sql[2],
        &format!(r#"SELECT {SQLITE_POST_COLS} FROM "posts" WHERE ("posts"."user_id" IN (?1, ?2))"#),
    );
    assert_eq!(
        args_in(&sql[1]),
        2,
        "three posts, two distinct authors: the key is deduplicated before the query"
    );

    // Level 2 arrived, associated to the right parents.
    let authors: Vec<&str> = posts
        .iter()
        .map(|p| p.rel.user.as_ref().expect("author").name.as_str())
        .collect();
    assert_eq!(authors, vec!["Stephen", "Stephen", "Ada"]);

    // Level 3 arrived, associated to the right level-2 object.
    let stephens_posts: Vec<&str> = posts[0]
        .rel
        .user
        .as_ref()
        .unwrap()
        .rel
        .posts
        .iter()
        .map(|p| p.title.as_str())
        .collect();
    assert_eq!(stephens_posts, vec!["keel laid", "second"]);
    let adas_posts: Vec<&str> = posts[2]
        .rel
        .user
        .as_ref()
        .unwrap()
        .rel
        .posts
        .iter()
        .map(|p| p.title.as_str())
        .collect();
    assert_eq!(adas_posts, vec!["notes"]);

    // The shared author was loaded once and cloned into both posts, with its
    // own relation already filled — not loaded twice, not filled for one post
    // and empty for the other.
    assert_eq!(posts[0].rel.user, posts[1].rel.user);
}

/// A single level is unchanged: still one extra query, still no nesting cost.
#[tokio::test]
async fn one_level_is_still_one_extra_query() {
    let db = Counting::new(db().await);
    seed_two_authors(&db).await;
    db.reset();

    let posts = posts::table()
        .query(posts::then_load::user())
        .all(&db)
        .await
        .unwrap();
    assert_eq!(db.seen().len(), 2, "the caller's query and one keyed query");
    assert!(posts.iter().all(|p| p.rel.user.is_some()));
    assert!(
        posts[0].rel.user.as_ref().unwrap().rel.posts.is_empty(),
        "a level that was not asked for is not loaded"
    );
}

/// A cyclic path — posts → author → the author's posts → *their* authors —
/// terminates, because a path is a finite value and not a graph traversal.
#[tokio::test]
async fn a_cyclic_path_terminates_where_it_was_written() {
    let db = Counting::new(db().await);
    seed_two_authors(&db).await;
    db.reset();

    let posts = posts::table()
        .query((
            posts::then_load::user().then(users::then_load::posts().then(posts::then_load::user())),
            select::order_by(posts::id()),
        ))
        .all(&db)
        .await
        .unwrap();

    assert_eq!(
        db.seen().len(),
        4,
        "four levels written, four queries — the cycle does not run away"
    );

    let author = posts[0].rel.user.as_ref().expect("author");
    let their_post = &author.rel.posts[0];
    let same_author = their_post.rel.user.as_ref().expect("the author again");
    assert_eq!(
        same_author.id, author.id,
        "the cycle closed on the same row"
    );
    assert!(
        same_author.rel.posts.is_empty(),
        "and stopped: the fourth level was the last one written"
    );
}

/// The batch cap, at a size small enough to see: five distinct keys in
/// batches of two are three queries, and the last one is the remainder.
/// `with` shapes every one of them.
#[tokio::test]
async fn a_level_batches_its_keys_and_shapes_every_batch() {
    use keelson_sqlite::Mod as _;

    let db = Counting::new(db().await);
    for name in ["a", "b", "c", "d", "e"] {
        users::table()
            .insert(users::Setter {
                name: set(name),
                ..Default::default()
            })
            .exec(&db)
            .await
            .unwrap();
    }
    for uid in 1..=5i64 {
        posts::table()
            .insert(posts::Setter {
                user_id: set(uid),
                title: set(format!("post {uid}")),
                ..Default::default()
            })
            .exec(&db)
            .await
            .unwrap();
    }
    db.reset();

    let posts = posts::table()
        .query(
            posts::then_load::user()
                .batch(2)
                .with(|q| users::is_active().eq(true).apply(q)),
        )
        .all(&db)
        .await
        .unwrap();

    let sql = db.seen();
    assert_eq!(sql.len(), 4, "the caller's query, then ceil(5 / 2) batches");
    assert_eq!(
        sql[1..].iter().map(|s| args_in(s)).collect::<Vec<_>>(),
        vec![3, 3, 2],
        "two keys plus the shaping mod's argument, then the remainder"
    );
    assert_sqlite(
        &sql[3],
        &format!(
            concat!(
                r#"SELECT {} FROM "users" "#,
                r#"WHERE ("users"."id" IN (?1)) AND ("users"."is_active" = ?2)"#
            ),
            SQLITE_USER_COLS
        ),
    );
    assert!(
        posts.iter().all(|p| p.rel.user.is_some()),
        "every batch attached"
    );
}

/// The default cap is real: one key over [`KEY_BATCH`] is two queries, and
/// the engine answers both. Seeded with raw SQL because 901 rows through the
/// model layer is 901 statements.
#[tokio::test]
async fn the_default_batch_boundary_holds_against_the_engine() {
    let db = Counting::new(db().await);
    let n = keelson_models::KEY_BATCH + 1;
    db.execute(Statement::new(
        format!(
            "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c WHERE n < {n}) \
             INSERT INTO users (id, name) SELECT n, 'user ' || n FROM c"
        ),
        vec![],
    ))
    .await
    .unwrap();
    db.execute(Statement::new(
        format!(
            "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c WHERE n < {n}) \
             INSERT INTO posts (id, user_id, title) SELECT n, n, 'post ' || n FROM c"
        ),
        vec![],
    ))
    .await
    .unwrap();
    db.reset();

    let posts = posts::table()
        .query((posts::then_load::user(), select::order_by(posts::id())))
        .all(&db)
        .await
        .unwrap();

    let sql = db.seen();
    assert_eq!(posts.len(), n);
    assert_eq!(
        sql.len(),
        3,
        "the caller's query plus two batches — {} keys is one over the cap",
        n
    );
    assert_eq!(
        sql[1..].iter().map(|s| args_in(s)).collect::<Vec<_>>(),
        vec![keelson_models::KEY_BATCH, 1],
        "a full batch and the one key that did not fit"
    );
    assert!(
        posts
            .iter()
            .all(|p| p.rel.user.as_ref().is_some_and(|u| u.id == p.user_id)),
        "every row across both batches got its own author"
    );
}

/// A batch of no keys makes no progress; substituting a size silently would
/// hide the caller's mistake.
#[test]
#[should_panic(expected = "at least 1")]
fn a_zero_batch_is_refused() {
    let _ = posts::then_load::user().batch(0);
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
