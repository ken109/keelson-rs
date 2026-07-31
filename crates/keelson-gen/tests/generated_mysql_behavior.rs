//! The MySQL acceptance: the checked-in generated MySQL models and factories
//! (`tests/generated/mysql`, pinned byte-for-byte by `generate_mysql.rs`) are
//! compiled here and run through the **same assertions the hand-written specs
//! run** — `keelson-models/tests/spec_mysql.rs` for the model (SQL shape
//! through the judges, then end to end against MySQL 8.4) and
//! `keelson-factory/tests/spec_*.rs` for the factories.
//!
//! The end-to-end half needs `--features live-docker`; everything above it
//! runs in a plain `cargo test`, because a judged SQL string needs no server.
//!
//! Counting rows: the live MySQL is **shared and persistent** across this
//! process's test binaries, so every count here is a delta against a baseline
//! taken in the same test, never an absolute — the one honest difference from
//! the SQLite factory acceptance, which owns a fresh database per test.

// `pub` throughout the generated files because that is what the generator
// emits into an application's crates; this test binary has no external
// readers, and not every generated item is exercised.
#[allow(unreachable_pub, dead_code)]
#[rustfmt::skip]
#[path = "generated/mysql/mod.rs"]
mod models;

use keelson_core::Value;
use keelson_models::{Set, null, set};
use keelson_mysql::select;
use keelson_sqlcheck::Dialect;

use models::{posts, users};

/// The application's hand-written hooks, outside the generated tree.
#[allow(unreachable_pub)]
mod hooks {
    pub mod users {
        use keelson_exec::{ExecError, ExecFuture, Execute as _, Executor};
        use keelson_models::Set;
        use keelson_mysql::{arg, insert, quote};

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
}

// ─────────────────────────── SQL shape (judged) ───────────────────────────

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

#[test]
fn the_generated_query_is_the_specs_query() {
    let q = users::table().query((users::age().gte(21), select::limit(20)));
    let args = assert_mysql(
        &q,
        &format!("SELECT {USER_COLS} FROM `users` WHERE (`users`.`age` >= ?) LIMIT 20"),
    );
    assert_eq!(args, vec![Value::I32(21)]);
}

#[test]
fn the_generated_insert_carries_no_returning() {
    use keelson_models::Table as _;
    let q = users::Users::insert_query(users::Setter {
        name: set("Stephen"),
        email: set("stephen@example.com"),
        ..Default::default()
    });
    let args = assert_mysql(&q, "INSERT INTO `users` (`name`, `email`) VALUES (?, ?)");
    assert_eq!(args.len(), 2);
}

#[test]
fn an_all_unset_setter_is_mysqls_values_parens() {
    use keelson_models::Table as _;
    let q = users::Users::insert_query(users::Setter::default());
    assert!(assert_mysql(&q, "INSERT INTO `users` VALUES ()").is_empty());
}

#[test]
fn null_and_unset_are_different_statements() {
    use keelson_models::Table as _;
    let q = users::Users::insert_query(users::Setter {
        name: set("kay"),
        email: null(),
        ..Default::default()
    });
    let args = assert_mysql(&q, "INSERT INTO `users` (`name`, `email`) VALUES (?, ?)");
    assert_eq!(args, vec![Value::Text("kay".into()), Value::Null]);
}

/// The generated read-back that stands in for `RETURNING`.
#[test]
fn the_generated_read_back_selects_the_models_own_columns() {
    let args = assert_mysql(
        &users::by_pk(7),
        &format!("SELECT {USER_COLS} FROM `users` WHERE (`users`.`id` = ?)"),
    );
    assert_eq!(args, vec![Value::I32(7)]);
}

/// A composite key reads back on every key column — `post_tags` is the shape
/// the hand-written spec's two tables do not have.
#[test]
fn a_composite_key_reads_back_on_every_column() {
    let args = assert_mysql(
        &models::post_tags::by_pk(1, 2),
        concat!(
            "SELECT `post_tags`.`post_id`, `post_tags`.`tag_id` FROM `post_tags` ",
            "WHERE (`post_tags`.`post_id` = ?) AND (`post_tags`.`tag_id` = ?)"
        ),
    );
    assert_eq!(args, vec![Value::I32(1), Value::I32(2)]);
}

#[test]
fn a_preload_is_one_left_joined_query_with_prefixed_columns() {
    let q = posts::table().query((posts::preload::user(), posts::views().gte(10)));
    let args = assert_mysql(
        &q,
        concat!(
            "SELECT `posts`.`id`, `posts`.`user_id`, `posts`.`title`, `posts`.`status`, ",
            "`posts`.`views`, `posts`.`published_at`, ",
            "`users`.`id` AS `user.id`, `users`.`name` AS `user.name`, ",
            "`users`.`email` AS `user.email`, `users`.`age` AS `user.age`, ",
            "`users`.`is_active` AS `user.is_active`, ",
            "`users`.`created_at` AS `user.created_at` ",
            "FROM `posts` LEFT JOIN `users` ON (`users`.`id` = `posts`.`user_id`) ",
            "WHERE (`posts`.`views` >= ?)"
        ),
    );
    assert_eq!(args, vec![Value::I32(10)]);
}

// ───────────────── the generated factories, without a database ─────────────

#[test]
fn the_generated_factories_build_the_specs_setter() {
    use keelson_factory::Faker;
    use models::factories::{comments, posts, tags, users as fac_users};

    let mut f = Faker::seeded(1);
    let s = fac_users::factory(()).build(&mut f);
    assert!(matches!(s.id, Set::Value(_)), "sequence-based id");
    match &s.name {
        Set::Value(n) => assert!(n.starts_with("user-"), "got {n}"),
        other => panic!("expected a generated name, got {other:?}"),
    }
    assert!(matches!(s.email, Set::Value(_)));
    // MySQL's `is_active TINYINT(1) NOT NULL DEFAULT 1` and
    // `created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP` are the
    // database's to fill.
    assert!(s.is_active.is_unset());
    assert!(s.created_at.is_unset());

    // The required FK stays unset until create() makes the chain.
    let s = posts::factory(()).build(&mut f);
    assert!(s.user_id.is_unset());
    assert_eq!(
        posts::factory(posts::user_id(77)).build(&mut f).user_id,
        Set::Value(77)
    );

    // The optional FK is absent by default.
    let s = comments::factory(()).build(&mut f);
    assert!(s.user_id.is_unset());

    // The non-key unique column is sequence-backed.
    match &tags::factory(()).build(&mut f).name {
        Set::Value(n) => assert!(n.starts_with("tag-"), "got {n}"),
        other => panic!("expected a generated name, got {other:?}"),
    }
}

#[test]
fn seeded_fakers_reproduce_random_sources_while_sequences_stay_unique() {
    use keelson_factory::Faker;
    use models::factories::users as fac_users;

    let a = fac_users::factory(()).build(&mut Faker::seeded(7));
    let b = fac_users::factory(()).build(&mut Faker::seeded(7));
    assert_eq!(a.name, b.name);
    assert_eq!(a.email, b.email);
    assert_ne!(a.id, b.id, "sequences are outside the seed");
}

// ───────────────────── end to end against MySQL 8.4 ─────────────────────

#[cfg(feature = "live-docker")]
mod live {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicI32, Ordering};

    use keelson_exec::{BeginExt as _, ExecError, Execute as _, Executor};
    use keelson_factory::Faker;
    use keelson_models::{null, set};
    use keelson_mysql::{Chain as _, arg, quote, select};

    use super::models::factories::{comments, users as fac_users};
    use super::models::{messages, posts, threads, users};

    /// Process-unique positive i32 keys, so runs against the shared server
    /// never collide.
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
        let url = tokio::task::spawn_blocking(|| keelson_sqlcheck::live::mysql_url().to_owned())
            .await
            .unwrap();
        keelson_sqlx::mysql::Pool::connect(&url)
            .await
            .expect("connecting to the live MySQL")
    }

    async fn count(db: &dyn Executor, table: &'static str) -> i64 {
        keelson_mysql::select((select::columns("count(*)"), select::from(quote(table))))
            .fetch_scalar(db)
            .await
            .unwrap()
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

    /// The generated model's whole flow on one transaction, rolled back —
    /// the spec's end-to-end test, run against generated code: the keyed
    /// read-back returns the schema's defaults, the hooks run on the caller's
    /// transaction, preload and then-load work, update/delete are `exec`-only.
    #[tokio::test]
    async fn the_generated_model_flow_runs_inside_the_callers_transaction() {
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
                assert_eq!(u.email.as_deref(), Some("stephen@example.com"));
                assert!(u.is_active, "the read-back carries the schema default");
                assert_eq!(u.age, None);
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

                let loaded = posts::table()
                    .query((posts::preload::user(), posts::id().eq(pid)))
                    .one(tx)
                    .await?;
                let author = loaded.rel.user.as_ref().expect("preloaded user");
                assert_eq!(author.id, uid);

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

                Err(ExecError::other("deliberate rollback"))
            })
            .await;
        assert_eq!(out.unwrap_err().to_string(), "deliberate rollback");

        assert_eq!(audit_tag_count(&db, uid).await, 0);
        assert!(
            users::table()
                .query(users::id().in_([uid, uid2]))
                .all(&db)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// The generated factories against the real engine: a comment chains a
    /// post chains a user, ten times over, each with its own chain — and the
    /// model's hooks fired for every created user.
    #[tokio::test]
    async fn the_generated_factories_build_their_chains() {
        let db = pool().await;
        let before_users = count(&db, "users").await;
        let before_posts = count(&db, "posts").await;
        let before_comments = count(&db, "comments").await;

        let out: Result<(), ExecError> = db
            .within(async |tx| {
                let cs = comments::factory(()).create_many(tx, 10).await?;
                assert_eq!(cs.len(), 10);
                assert_eq!(count(tx, "comments").await - before_comments, 10);
                assert_eq!(count(tx, "posts").await - before_posts, 10);
                assert_eq!(count(tx, "users").await - before_users, 10);
                for c in &cs {
                    assert_eq!(
                        c.user_id, None,
                        "the nullable FK stayed NULL — factories invent no optional rows"
                    );
                }
                let mut post_ids: Vec<i32> = cs.iter().map(|c| c.post_id).collect();
                post_ids.sort_unstable();
                post_ids.dedup();
                assert_eq!(post_ids.len(), 10, "each comment got its own post");

                // Hooks fire through the generated model's insert path.
                let u = fac_users::factory(fac_users::email("ADA@Example.COM"))
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

    /// Seeded creates reproduce the random columns; the sequence-backed key
    /// stays unique. Rolled back, so the server is left as it was.
    #[tokio::test]
    async fn seeded_creates_reproduce_against_the_server() {
        let db = pool().await;
        let out: Result<(), ExecError> = db
            .within(async |tx| {
                let mut fa = Faker::seeded(99);
                let mut fb = Faker::seeded(99);
                let a = fac_users::factory(()).create_with(tx, &mut fa).await?;
                let b = fac_users::factory(()).create_with(tx, &mut fb).await?;
                assert_eq!(a.name, b.name);
                assert_eq!(a.email, b.email);
                assert_ne!(a.id, b.id);
                Err(ExecError::other("deliberate rollback"))
            })
            .await;
        assert_eq!(out.unwrap_err().to_string(), "deliberate rollback");
    }

    /// The mutually referencing base tables against the real server:
    /// `threads.first_message_id → messages` and `messages.thread_id →
    /// threads`, so `Thread.rel.first_message` and `Message.rel.thread`
    /// refer to each other. That this module compiles at all is the boxing
    /// rule working; this test is that the relation still loads, both ways
    /// and around the cycle. Rolled back, so the server is left as it was.
    #[tokio::test]
    async fn the_mutually_referencing_pair_loads_against_the_server() {
        let db = pool().await;
        let tid = key();
        let mid = key();

        let out: Result<(), ExecError> = db
            .within(async |tx| {
                threads::table()
                    .insert(threads::Setter {
                        id: set(tid),
                        title: set("keel laid"),
                        ..Default::default()
                    })
                    .one(tx)
                    .await?;
                messages::table()
                    .insert(messages::Setter {
                        id: set(mid),
                        thread_id: set(tid),
                        body: set("first"),
                    })
                    .one(tx)
                    .await?;
                threads::table()
                    .update(
                        threads::Setter {
                            first_message_id: set(mid),
                            ..Default::default()
                        },
                        threads::id().eq(tid),
                    )
                    .exec(tx)
                    .await?;

                // Same-query preload, thread → its opening message.
                let thread = threads::table()
                    .query((threads::preload::first_message(), threads::id().eq(tid)))
                    .one(tx)
                    .await?;
                assert_eq!(
                    thread
                        .rel
                        .first_message
                        .as_deref()
                        .expect("the opening message")
                        .body,
                    "first"
                );

                // Then-load, the other way: message → its thread.
                let message = messages::table()
                    .query((messages::then_load::thread(), messages::id().eq(mid)))
                    .one(tx)
                    .await?;
                assert_eq!(
                    message.rel.thread.as_deref().expect("the thread").title,
                    "keel laid"
                );

                // And once around the cycle as a two-level path.
                let round = messages::table()
                    .query((
                        messages::then_load::thread().then(threads::then_load::first_message()),
                        messages::id().eq(mid),
                    ))
                    .one(tx)
                    .await?;
                assert_eq!(
                    round
                        .rel
                        .thread
                        .as_ref()
                        .expect("the thread")
                        .rel
                        .first_message
                        .as_ref()
                        .expect("its opening message")
                        .id,
                    mid,
                );

                Err(ExecError::other("deliberate rollback"))
            })
            .await;
        assert_eq!(out.unwrap_err().to_string(), "deliberate rollback");
    }
}
