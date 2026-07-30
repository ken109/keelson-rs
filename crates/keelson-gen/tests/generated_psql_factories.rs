//! The PostgreSQL half of the factory acceptance: the checked-in generated
//! psql models and factories (`tests/generated/psql_factories`, pinned by
//! `generate_factories.rs`) are compiled here — which is itself the proof
//! that the emitter's output type-checks against a different type set
//! (`i32` keys, `chrono::DateTime<Utc>` timestamps) — and run through the
//! factory spec's assertions: build without a database, seeded reproduction,
//! and, under `--features live-docker`, the parent chain and hooks against
//! the real PostgreSQL 17.
//!
//! Row counts in the live half are **deltas** against a baseline taken in the
//! same test: the server is shared and persistent across this process's test
//! binaries, unlike the SQLite lane's fresh file per test.

#[allow(unreachable_pub, dead_code)]
#[rustfmt::skip]
#[path = "generated/psql_factories/mod.rs"]
mod models;

use keelson_factory::Faker;
use keelson_models::Set;

use models::factories::{comments, posts, tags, users};

/// The application's hand-written hooks, outside the generated tree.
#[allow(unreachable_pub)]
mod hooks {
    pub mod users {
        use keelson_exec::{ExecError, ExecFuture, Execute as _, Executor};
        use keelson_models::Set;
        use keelson_psql::{arg, insert, quote};

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

#[test]
fn build_produces_a_setter_with_no_executor_in_sight() {
    let mut f = Faker::seeded(1);
    let s = users::factory(()).build(&mut f);
    assert!(matches!(s.id, Set::Value(_)), "sequence-based id");
    match &s.name {
        Set::Value(n) => assert!(n.starts_with("user-"), "got {n}"),
        other => panic!("expected a generated name, got {other:?}"),
    }
    assert!(matches!(s.email, Set::Value(_)));
    assert!(matches!(s.age, Set::Value(_)));
    // `is_active boolean DEFAULT true` and `created_at timestamptz DEFAULT
    // now()` are the database's to fill.
    assert!(s.is_active.is_unset());
    assert!(s.created_at.is_unset());

    let s = posts::factory(()).build(&mut f);
    assert!(s.user_id.is_unset(), "the chain needs a database");
    assert_eq!(
        posts::factory(posts::user_id(77)).build(&mut f).user_id,
        Set::Value(77)
    );

    // The non-key unique column is sequence-backed.
    match &tags::factory(()).build(&mut f).name {
        Set::Value(n) => assert!(n.starts_with("tag-"), "got {n}"),
        other => panic!("expected a generated name, got {other:?}"),
    }
}

#[test]
fn mods_are_values_and_seeded_random_sources_reproduce() {
    let s = users::factory((users::id(42), users::name("Ada"), users::email_null()))
        .build(&mut Faker::seeded(0));
    assert_eq!(s.id, Set::Value(42));
    assert_eq!(s.name, Set::Value("Ada".to_owned()));
    assert_eq!(s.email, Set::Null);

    let a = users::factory(()).build(&mut Faker::seeded(7));
    let b = users::factory(()).build(&mut Faker::seeded(7));
    assert_eq!(a.name, b.name);
    assert_ne!(a.id, b.id, "sequences are outside the seed");

    // The optional parent stays absent.
    assert!(
        comments::factory(())
            .build(&mut Faker::seeded(0))
            .user_id
            .is_unset()
    );
}

// ───────────────── end to end against PostgreSQL 17 ─────────────────

#[cfg(feature = "live-docker")]
mod live {
    use keelson_exec::{BeginExt as _, ExecError, Executor};
    use keelson_psql::{Chain as _, arg, quote, select};

    use super::models::factories::{comments, users};

    async fn pool() -> keelson_sqlx::psql::Pool {
        let url = tokio::task::spawn_blocking(|| keelson_sqlcheck::live::psql_url().to_owned())
            .await
            .unwrap();
        keelson_sqlx::psql::Pool::connect(&url)
            .await
            .expect("connecting to the live PostgreSQL")
    }

    async fn count(db: &dyn Executor, table: &'static str) -> i64 {
        use keelson_exec::Execute as _;
        keelson_psql::select((select::columns("count(*)"), select::from(quote(table))))
            .fetch_scalar(db)
            .await
            .unwrap()
    }

    async fn audit_tag_count(db: &dyn Executor, user_id: i32) -> i64 {
        use keelson_exec::Execute as _;
        keelson_psql::select((
            select::columns("count(*)"),
            select::from(quote("tags")),
            select::where_(quote("name").eq(arg(format!("audit-user-{user_id}")))),
        ))
        .fetch_scalar(db)
        .await
        .unwrap()
    }

    /// One create makes the whole chain, hooks fire on the caller's
    /// executor, and the rollback proves both.
    #[tokio::test]
    async fn the_generated_factories_chain_and_fire_hooks() {
        let db = pool().await;
        let before_users = count(&db, "users").await;
        let before_posts = count(&db, "posts").await;
        let before_comments = count(&db, "comments").await;

        let out: Result<(), ExecError> = db
            .within(async |tx| {
                let cs = comments::factory(()).create_many(tx, 5).await?;
                assert_eq!(cs.len(), 5);
                assert_eq!(count(tx, "comments").await - before_comments, 5);
                assert_eq!(count(tx, "posts").await - before_posts, 5, "a post each");
                assert_eq!(count(tx, "users").await - before_users, 5, "a user each");
                for c in &cs {
                    assert_eq!(c.user_id, None, "the optional parent stayed absent");
                }

                let u = users::factory(users::email("ADA@Example.COM"))
                    .create(tx)
                    .await?;
                assert_eq!(u.email.as_deref(), Some("ada@example.com"));
                assert_eq!(audit_tag_count(tx, u.id).await, 1);

                Err(ExecError::other("deliberate rollback"))
            })
            .await;
        assert_eq!(out.unwrap_err().to_string(), "deliberate rollback");
        assert_eq!(count(&db, "users").await, before_users, "all rolled back");
    }
}
