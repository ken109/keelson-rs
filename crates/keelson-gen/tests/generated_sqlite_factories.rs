//! The factory acceptance: the checked-in **generated** SQLite factories
//! (`tests/generated/sqlite_factories`, pinned byte-for-byte by
//! `generate_factories.rs`) are compiled here and run through the **same
//! assertions the hand-written factory spec runs**
//! (`keelson-factory/tests/spec_sqlite.rs`) — build without a database,
//! seeded reproduction, mods as values, hooks firing through the model path,
//! parent chains, `create_many` uniqueness at n=100, existing and shaped
//! parents, has-many children.
//!
//! On top of the spec's set: a **non-key unique column** (`tags.name`) is
//! sequence-backed, which the hand-written spec's three tables never
//! exercise.

// `pub` throughout the generated files because that is what the generator
// emits into an application's crates; this test binary has no external
// readers, and not every generated item is exercised.
#[allow(unreachable_pub, dead_code)]
// The fixture is prettyplease-formatted by the generator; rustfmt must not
// rewrite it, or the byte-identical freshness test would fight `cargo fmt`.
#[rustfmt::skip]
#[path = "generated/sqlite_factories/mod.rs"]
mod models;

use keelson_exec::{Executor, Statement};
use keelson_factory::Faker;
use keelson_models::Set;
use keelson_sqlite::{quote, select};
use keelson_sqlx::sqlite::Pool;

use models::factories::{comments, posts, tags, users};

/// The application's hand-written hooks, outside the generated tree — the
/// same pair the model spec has: normalise the email before insert, write an
/// audit tag on the caller's executor after insert.
#[allow(unreachable_pub)]
mod hooks {
    pub mod users {
        use keelson_exec::{ExecError, ExecFuture, Execute as _, Executor};
        use keelson_models::Set;
        use keelson_sqlite::{arg, insert, quote};

        use crate::models::users::{Setter, User};

        pub fn before_insert<'a>(
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

        pub fn after_insert<'a>(
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

// ────────────────────────── build(): no database ──────────────────────────

#[test]
fn build_produces_a_setter_with_no_executor_in_sight() {
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
    // cannot create the chain — that is create()'s job.
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
    assert_eq!(a.name, b.name);
    assert_eq!(a.email, b.email);
    assert_eq!(a.age, b.age);
    // The sequence-backed unique column deliberately does not reproduce: it
    // is uniqueness machinery, outside the seed.
    assert_ne!(a.id, b.id);

    // A Gen source (random_id) is inside the seed.
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

/// The non-key unique column the spec's tables do not have: `tags.name` is
/// `UNIQUE`, so its default is sequence-backed rather than random.
#[test]
fn a_non_key_unique_column_is_sequence_backed() {
    let mut names = std::collections::HashSet::new();
    for _ in 0..50 {
        let s = tags::factory(()).build(&mut Faker::seeded(3));
        match &s.name {
            Set::Value(n) => {
                assert!(n.starts_with("tag-"), "{n}");
                assert!(names.insert(n.clone()), "unique column repeated {n}");
            }
            other => panic!("expected a generated name, got {other:?}"),
        }
    }
}

#[test]
fn the_built_insert_is_judged_sql() {
    use keelson_core::Query as _;
    use keelson_models::Table as _;

    let s = users::factory((users::id(1), users::name("Ada"), users::email_null()))
        .build(&mut Faker::seeded(0));
    let q = models::users::Users::insert_query(models::users::Setter {
        age: Set::Unset, // pin: the unseeded age came from the faker
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
        "keelson-gen-factories-{}-{}.db",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&path);
    let pool = Pool::connect(&format!("sqlite://{}", path.display()))
        .await
        .expect("opening the SQLite database");
    for ddl in include_str!("fixtures/sqlite_schema.sql")
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
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

/// Factories fire model hooks — through the *generated* model's insert path,
/// which delegates to the application's hooks module.
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

    let p = models::posts::table()
        .query(models::posts::id().eq(c.post_id))
        .one(&db)
        .await
        .unwrap();
    let owner = models::users::table()
        .query(models::users::id().eq(p.user_id))
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
    let mut post_ids: Vec<i64> = cs.iter().map(|c| c.post_id).collect();
    post_ids.sort_unstable();
    post_ids.dedup();
    assert_eq!(post_ids.len(), 10);
    let found = models::posts::table()
        .query(models::posts::id().in_(post_ids))
        .all(&db)
        .await
        .unwrap();
    assert_eq!(found.len(), 10);
}

/// Uniqueness at n=100, and the hook fired for every row.
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

    let u = users::factory(users::name("owner"))
        .create(&db)
        .await
        .unwrap();
    let p = posts::factory(posts::user(&u)).create(&db).await.unwrap();
    assert_eq!(p.user_id, u.id);
    assert_eq!(count(&db, "users").await, 1);

    let c = comments::factory(comments::for_post(posts::factory((
        posts::title("shaped"),
        posts::user(&u),
    ))))
    .create(&db)
    .await
    .unwrap();
    let shaped = models::posts::table()
        .query(models::posts::id().eq(c.post_id))
        .one(&db)
        .await
        .unwrap();
    assert_eq!(shaped.title, "shaped");
    assert_eq!(shaped.user_id, u.id);
    assert_eq!(count(&db, "users").await, 1, "still no invented users");

    let c = comments::factory((comments::post(&p), comments::user(&u)))
        .create(&db)
        .await
        .unwrap();
    assert_eq!(c.post_id, p.id);
    assert_eq!(c.user_id, Some(u.id));

    let c = comments::factory((
        comments::post(&p),
        comments::for_user(users::factory(users::name("commenter"))),
    ))
    .create(&db)
    .await
    .unwrap();
    let commenter = models::users::table()
        .query(models::users::id().eq(c.user_id.unwrap()))
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

    let ps = models::posts::table()
        .query(models::posts::user_id().eq(u.id))
        .all(&db)
        .await
        .unwrap();
    let mut titles: Vec<&str> = ps.iter().map(|p| p.title.as_str()).collect();
    titles.sort_unstable();
    assert_eq!(titles, vec!["first", "second"]);
    assert_eq!(count(&db, "users").await, 1, "children reuse their creator");
}

/// Seeded creates reproduce the random columns across two databases.
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
