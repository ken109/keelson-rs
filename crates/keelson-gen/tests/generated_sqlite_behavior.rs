//! The strongest form of the codegen tests: the checked-in generated SQLite
//! models (`tests/generated/sqlite`, pinned byte-for-byte by
//! `generate_sqlite.rs`) are compiled here and run through the **same
//! assertions the hand-written spec runs**
//! (`keelson-models/tests/spec_sqlite.rs`) — SQL shape through the judges,
//! then end to end against real SQLite — proving generated == spec: partial
//! setters, DEFAULT VALUES, typed queries with Layer 1 mods, hooks on the
//! caller's transaction, preload/then-load both ways, update/delete, and
//! dialect INSERT mods through `.with(…)`.
//!
//! On top of the spec's set, the shapes the spec schema does not have:
//! a nullable foreign key (`comments.user_id`), a composite primary key
//! (`post_tags`), and a real database view (`user_emails`).

// `pub` throughout the generated files because that is what the generator
// emits into an application's models crate; this test binary has no external
// readers, and not every generated item is exercised.
#[allow(unreachable_pub, dead_code)]
// The fixture is prettyplease-formatted by the generator; rustfmt must not
// rewrite it, or the byte-identical freshness test would fight `cargo fmt`.
#[rustfmt::skip]
#[path = "generated/sqlite/mod.rs"]
mod models;

use keelson_core::{Query as _, Value};
use keelson_exec::{BeginExt as _, ExecError, Executor};
use keelson_models::{null, set};
use keelson_sqlite::{quote, select};
use keelson_sqlx::sqlite::Pool;

use models::{comments, post_tags, posts, tags, user_emails, users};

/// The application's hand-written hooks, outside the generated tree — the
/// module `[hooks] module = "crate::hooks"` points the delegations at.
/// Behaviour is the spec model's: normalise the email before insert, write
/// an audit tag on the caller's executor after insert.
// `pub` because the generated delegations call through `crate::hooks::…`;
// this test binary has no external readers.
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

// ─────────────────────────── SQL shape (judged) ───────────────────────────

#[test]
fn the_sqlite_rendition_of_the_agreed_call_site() {
    let q = users::table().query((users::age().gte(21i64), select::limit(20)));
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

/// The LEFT JOIN miss: the generated mapper turns an all-NULL prefix into
/// `None`.
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

/// A fresh database from the same fixture DDL generation ran against.
async fn db() -> Pool {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "keelson-gen-behavior-{}-{}.db",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&path);
    let conn = rusqlite::Connection::open(&path).expect("creating the database");
    conn.execute_batch(include_str!("fixtures/sqlite_schema.sql"))
        .expect("applying the fixture DDL");
    drop(conn);
    Pool::connect(&format!("sqlite://{}", path.display()))
        .await
        .expect("opening the SQLite database")
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

/// The hooks contract, end to end, through the *generated delegations*:
/// `before_insert` rewrote the setter, `after_insert` wrote on the caller's
/// transaction — visible inside it, gone after its rollback, kept after a
/// commit.
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

/// Progressive enhancement on a typed insert: a dialect `INSERT` mod rides
/// in through `.with(…)`.
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

// ────────── beyond the spec schema: the shapes it does not have ──────────

/// `comments.user_id` is a *nullable* foreign key — the generated loaders
/// bridge the `Option` on both directions, and a NULL key attaches nothing.
#[tokio::test]
async fn nullable_foreign_keys_load_both_ways() {
    let db = db().await;
    let u = users::table()
        .insert(users::Setter {
            name: set("Stephen"),
            ..Default::default()
        })
        .one(&db)
        .await
        .unwrap();
    let p = posts::table()
        .insert(posts::Setter {
            user_id: set(u.id),
            title: set("keel laid"),
            ..Default::default()
        })
        .one(&db)
        .await
        .unwrap();
    comments::table()
        .insert(comments::Setter {
            post_id: set(p.id),
            user_id: set(u.id),
            body: set("signed"),
            ..Default::default()
        })
        .exec(&db)
        .await
        .unwrap();
    comments::table()
        .insert(comments::Setter {
            post_id: set(p.id),
            user_id: null(),
            body: set("anonymous"),
            ..Default::default()
        })
        .exec(&db)
        .await
        .unwrap();

    // To-one across the nullable key: the NULL comment gets None.
    let cs = comments::table()
        .query((
            comments::then_load::user(),
            select::order_by(comments::id()),
        ))
        .all(&db)
        .await
        .unwrap();
    assert_eq!(cs.len(), 2);
    assert_eq!(cs[0].rel.user.as_ref().unwrap().name, "Stephen");
    assert_eq!(cs[1].rel.user, None);

    // Preload agrees with then-load on the miss.
    let pre = comments::table()
        .query((comments::preload::user(), select::order_by(comments::id())))
        .all(&db)
        .await
        .unwrap();
    assert_eq!(pre[0].rel.user.as_ref().unwrap().name, "Stephen");
    assert_eq!(pre[1].rel.user, None);

    // And the back-reference only gathers the signed one.
    let with_comments = users::table()
        .query(users::then_load::comments())
        .one(&db)
        .await
        .unwrap();
    assert_eq!(with_comments.rel.comments.len(), 1);
    assert_eq!(with_comments.rel.comments[0].body, "signed");
}

/// `post_tags` has a composite primary key: `Pk` is a tuple, and the model
/// writes and loads like any other table.
#[tokio::test]
async fn composite_primary_keys_are_tuples() {
    let db = db().await;
    let u = users::table()
        .insert(users::Setter {
            name: set("Stephen"),
            ..Default::default()
        })
        .one(&db)
        .await
        .unwrap();
    let p = posts::table()
        .insert(posts::Setter {
            user_id: set(u.id),
            title: set("keel laid"),
            ..Default::default()
        })
        .one(&db)
        .await
        .unwrap();
    let t = tags::table()
        .insert(tags::Setter {
            name: set("rust"),
            ..Default::default()
        })
        .one(&db)
        .await
        .unwrap();
    let pt = post_tags::table()
        .insert(post_tags::Setter {
            post_id: set(p.id),
            tag_id: set(t.id),
        })
        .one(&db)
        .await
        .unwrap();
    assert_eq!(
        <models::post_tags::PostTags as keelson_models::Table>::pk(&pt),
        (p.id, t.id)
    );

    // The link table then-loads from both of its parents. (The audit hook
    // wrote its own row into `tags`, so filter to ours.)
    let tagged = tags::table()
        .query((tags::then_load::post_tags(), tags::name().eq("rust")))
        .one(&db)
        .await
        .unwrap();
    assert_eq!(tagged.rel.post_tags.len(), 1);
    assert_eq!(tagged.rel.post_tags[0].post_id, p.id);
}

/// `user_emails` is a real database view: a `View`-only model —
/// `view().query(…)` works, and `.insert(…)` does not exist on it (the spec
/// pins that as a compile error; here the type simply has no `Table` impl).
#[tokio::test]
async fn a_database_view_is_select_only_and_queries() {
    let db = db().await;
    for (name, email) in [("a", Some("a@x.dev")), ("b", None)] {
        let mut s = users::Setter {
            name: set(name),
            ..Default::default()
        };
        if let Some(e) = email {
            s.email = set(e);
        }
        users::table().insert(s).exec(&db).await.unwrap();
    }
    let with_email = user_emails::view()
        .query(user_emails::email().is_not_null())
        .all(&db)
        .await
        .unwrap();
    assert_eq!(with_email.len(), 1);
    assert_eq!(with_email[0].email.as_deref(), Some("a@x.dev"));
}
