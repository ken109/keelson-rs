//! The checked-in generated PostgreSQL models compiled and run through the
//! **same SQL-shape assertions the hand-written spec runs**
//! (`keelson-models/tests/spec_psql.rs`) — judged by the real PostgreSQL
//! grammar, no server required. With `--features live-docker`, the spec's
//! end-to-end transaction tests run too, against the containerised
//! PostgreSQL 17.

// `pub` throughout the generated files because that is what the generator
// emits into an application's models crate; this test binary has no
// external readers.
#[allow(unreachable_pub, dead_code)]
// The fixture is prettyplease-formatted by the generator; rustfmt must not
// rewrite it, or the byte-identical freshness test would fight `cargo fmt`.
#[rustfmt::skip]
#[path = "generated/psql/mod.rs"]
mod models;

use keelson_core::{Query as _, QueryExtensions as _, Value};
use keelson_models::{null, set};
use keelson_psql::select;
use keelson_sqlcheck::Dialect;

use models::{posts, users};

/// The application's hand-written hooks — the psql twin of the sqlite
/// behaviour test's, with the spec model's behaviour.
// `pub` because the generated delegations call through `crate::hooks::…`;
// this test binary has no external readers.
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
    let q = models::users::Users::insert_query(users::Setter {
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
    let q = models::users::Users::insert_query(users::Setter {
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
    let q = models::users::Users::insert_query(users::Setter::default());
    assert_psql(
        &q,
        &format!(r#"INSERT INTO "users" DEFAULT VALUES RETURNING {USER_COLS}"#),
    );
}

#[test]
fn update_sets_only_the_set_fields_and_filters_typed() {
    use keelson_models::Table as _;
    use keelson_psql::Mod as _;
    let mut q = models::users::Users::update_query();
    users::id().eq(7).apply(&mut q);
    models::users::Users::apply_setter(
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
    let mut q = models::users::Users::delete_query();
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
fn raw_fragments_and_dialect_mods_mix_into_a_model_query() {
    // The spec runs this on its hand-written SELECT-only projection; the
    // generated set has no such projection over `users`, so the same mixing
    // is asserted on the full model.
    let q = users::table().query((
        users::email().is_not_null(),
        select::where_(r#""users"."age" IS NOT NULL"#),
        select::order_by(users::id()).desc(),
        select::limit(5),
    ));
    assert_psql(
        &q,
        &format!(
            concat!(
                r#"SELECT {} FROM "users" "#,
                r#"WHERE ("users"."email" IS NOT NULL) AND "users"."age" IS NOT NULL "#,
                r#"ORDER BY "users"."id" DESC LIMIT 5"#
            ),
            USER_COLS
        ),
    );
}

#[test]
fn aliased_as_follows_a_table_alias() {
    use keelson_psql::quote;
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
    let q = posts::table().query((posts::preload::user(), posts::then_load::user()));
    assert_eq!(q.mapper_mods().len(), 1);
    assert_eq!(q.loaders().len(), 1);
    assert!(q.hooks().is_empty());
    assert_eq!(q.query_type(), keelson_core::QueryType::Select);
}

// ─────────────────── end-to-end against PostgreSQL 17 ───────────────────

#[cfg(feature = "live-docker")]
mod live {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicI32, Ordering};

    use keelson_exec::{BeginExt as _, ExecError, Execute as _, Executor};
    use keelson_models::{null, set};
    use keelson_psql::{Chain as _, arg, quote, select};

    use super::models::{messages, post_authors, posts, threads, user_emails, users};

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

    /// The spec's whole-model-flow transaction test, on the generated
    /// models: partial setter insert with RETURNING defaults, the
    /// before-insert rewrite, the after-insert write observed inside the
    /// same transaction, preload and then-load — then rolled back, hook
    /// write included.
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
                assert_eq!(u.email.as_deref(), Some("stephen@example.com"));
                assert!(u.is_active);
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
                assert_eq!(author.email.as_deref(), Some("stephen@example.com"));

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
        let ghosts = users::table()
            .query(users::id().in_([uid, uid2]))
            .all(&db)
            .await
            .unwrap();
        assert!(ghosts.is_empty());
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

    /// Relations involving views, against the real server: `post_authors` and
    /// `user_emails` are `SELECT`-only models, every relation below came from
    /// a `[[relationships]]` block with its `cardinality` declared, and both
    /// loading strategies have to bring back the same rows. Rolled back, so a
    /// shared server keeps nothing.
    #[tokio::test]
    async fn view_relations_load_against_the_server() {
        let db = pool().await;
        let uid = key();
        let pid = key();

        let out: Result<(), ExecError> = db
            .within(async |tx| {
                users::table()
                    .insert(users::Setter {
                        id: set(uid),
                        name: set("Grace"),
                        email: set("grace@example.com"),
                        ..Default::default()
                    })
                    .one(tx)
                    .await?;
                posts::table()
                    .insert(posts::Setter {
                        id: set(pid),
                        user_id: set(uid),
                        title: set("compiler"),
                        ..Default::default()
                    })
                    .one(tx)
                    .await?;

                // The view as the target of a to-one relation, both ways.
                let preloaded = posts::table()
                    .query((posts::preload::authorship(), posts::id().eq(pid)))
                    .one(tx)
                    .await?;
                let a = preloaded.rel.authorship.as_ref().expect("view row");
                assert_eq!(a.user_name.as_deref(), Some("Grace"));

                let then_loaded = posts::table()
                    .query((posts::then_load::authorship(), posts::id().eq(pid)))
                    .one(tx)
                    .await?;
                assert_eq!(then_loaded.rel.authorship, preloaded.rel.authorship);

                // The view as the holder of a relation.
                let from_view = post_authors::view()
                    .query((
                        post_authors::then_load::user(),
                        post_authors::post_id().eq(pid),
                    ))
                    .one(tx)
                    .await?;
                assert_eq!(from_view.rel.user.as_ref().unwrap().id, uid);

                // The declared cardinality decides the back-reference shape:
                // a `Vec` for many_to_one, a boxed `Option` for one_to_one.
                let back = users::table()
                    .query((
                        users::id().eq(uid),
                        users::then_load::post_authors(),
                        users::then_load::user_emails(),
                    ))
                    .one(tx)
                    .await?;
                assert_eq!(back.rel.post_authors.len(), 1);
                assert_eq!(
                    back.rel
                        .user_emails
                        .as_deref()
                        .and_then(|e| e.email.clone()),
                    Some("grace@example.com".to_owned())
                );

                // PostgreSQL calls `user_emails` auto-updatable, but the
                // config declares no key for it, so it is still SELECT-only:
                // updatability is permission to write, not an identity to
                // write by.
                let rows = user_emails::view()
                    .query(user_emails::id().eq(uid))
                    .all(tx)
                    .await?;
                assert_eq!(rows.len(), 1);

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
    /// and around the cycle. Rolled back, so a shared server keeps nothing.
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
