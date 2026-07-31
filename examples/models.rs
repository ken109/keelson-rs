//! **Layer 3: the generated models.** Typed columns, three-state setters,
//! hooks, and Layer 1 mods still in the same tuple.
//!
//!     cargo run -p keelson-examples --example models
//!
//! Everything under `src/models/` was written by `keelson-gen` from
//! `schema.sql`; it is committed, readable and steppable, which is the point
//! of a CLI generator rather than a proc macro. `src/hooks.rs` is the
//! hand-written half it delegates to.
//!
//! keelson's Layer 3 is a thin typed shell over Layer 1, not an ORM: there is
//! no change tracking and no identity map, and every call below is a statement
//! you can predict from the call site.

use keelson::exec::ExecError;
use keelson::models::{null, set};
use keelson::prelude::*;
use keelson::sqlite::{quote, select};
use keelson_examples::Sandbox;
use keelson_examples::models::{comments, post_authors, posts, users};

#[tokio::main]
async fn main() -> Result<(), ExecError> {
    let sandbox = Sandbox::seeded().await?;
    let db = &sandbox.db;

    // ── typed filters, and Layer 1 in the same tuple ────────────────────
    //
    // `users::age()` is one `Column<i64>` that is the expression, the filter
    // origin and the alias carrier at once. `age().gte(21)` compiles;
    // `age().gte("x")` does not.
    let adults = users::table()
        .query((
            users::age().gte(21),
            users::is_active().eq(true),
            // A Layer 1 mod, in the same tuple as the typed ones...
            select::order_by(users::name().expr()),
            select::limit(10),
            // ...and a raw fragment, in the same tuple again.
            select::where_(r#""users"."name" <> 'nobody'"#),
        ))
        .all(db)
        .await?;
    println!("── typed query");
    for u in &adults {
        println!("  {} ({:?})", u.name, u.age);
    }
    assert_eq!(adults.len(), 2);

    // The statement is inspectable before it runs -- it is a Layer 1 query
    // underneath, with nothing hidden.
    let q = users::table().query(users::age().gte(21));
    println!("\n── the SQL it builds\n  {}", q.as_select().build()?.0);

    // `one` means exactly one; `optional` is the may-not-exist verb.
    let ada = users::table()
        .query(users::name().eq("Ada"))
        .one(db)
        .await?;
    let missing = users::table()
        .query(users::name().eq("Nobody"))
        .optional(db)
        .await?;
    assert_eq!(ada.id, 1);
    assert!(missing.is_none());

    // ── the three-state setter ──────────────────────────────────────────
    //
    // `Set<T>` is `Unset | Null | Value(T)`, and `Default` is `Unset`. An
    // unset field does not appear in the statement at all -- which is the
    // difference between "leave the column alone" and "write NULL", and the
    // reason `Option<T>` is the wrong type for this job.
    let created = users::table()
        .insert(users::Setter {
            name: set("Stephen"),
            // The hook in src/hooks.rs lowercases this before it is written.
            email: set("STEPHEN@Example.COM"),
            age: set(41),
            // `is_active` and `created_at` stay `Unset`, so the column is
            // omitted and the schema's DEFAULT applies.
            ..Default::default()
        })
        .one(db)
        .await?;
    println!(
        "\n── insert\n  id {} email {:?} is_active {} created_at {}",
        created.id, created.email, created.is_active, created.created_at
    );
    assert_eq!(created.email.as_deref(), Some("stephen@example.com"));
    assert!(created.is_active, "the column's DEFAULT applied");

    // The `after_insert` hook wrote an audit row -- on the caller's executor,
    // so it is part of whatever transaction the caller was in.
    let audits: i64 = keelson::sqlite::select((
        select::columns(keelson::sqlite::f("count", quote("id"))),
        select::from(quote("audit_logs")),
    ))
    .fetch_scalar(db)
    .await?;
    assert_eq!(audits, 1);

    // ── update: unset, value, and NULL ──────────────────────────────────
    //
    // `exec` runs it for the side effect and answers how many rows changed.
    let done = users::table()
        .update(
            users::Setter {
                age: set(42),         // write 42
                email: null(),        // write NULL
                ..Default::default()  // everything else: not in the statement
            },
            users::id().eq(created.id),
        )
        .exec(db)
        .await?;
    assert_eq!(done.rows_affected, 1);

    let after = users::table()
        .query(users::id().eq(created.id))
        .one(db)
        .await?;
    println!(
        "\n── update\n  age {:?} email {:?} name still {:?}",
        after.age, after.email, after.name
    );
    assert_eq!(after.age, Some(42));
    assert_eq!(after.email, None, "null() erased it");
    assert_eq!(after.name, "Stephen", "unset columns were left alone");

    // To get the rows back instead, ask for a `RETURNING` -- the generated
    // model does not add one to `UPDATE`, because a statement that returns
    // rows costs more than one that does not and the caller is the one who
    // knows whether the rows are wanted.
    let returned = users::table()
        .update(
            users::Setter {
                age: set(43),
                ..Default::default()
            },
            (
                users::id().eq(created.id),
                keelson::sqlite::update::returning(keelson::sqlite::raw("*")),
            ),
        )
        .all(db)
        .await?;
    println!("  with RETURNING: age is now {:?}", returned[0].age);
    assert_eq!(returned[0].age, Some(43));

    // On MySQL neither spelling is available: the generated model there has an
    // `exec`-only update surface, because the engine cannot return the rows
    // and a second statement the caller did not ask for would be a lie about
    // what happened.

    // ── delete ──────────────────────────────────────────────────────────
    let gone = users::table()
        .delete(users::id().eq(created.id))
        .exec(db)
        .await?;
    assert_eq!(gone.rows_affected, 1);
    assert!(
        users::table()
            .query(users::id().eq(created.id))
            .optional(db)
            .await?
            .is_none()
    );

    // ── a model over a view ─────────────────────────────────────────────
    //
    // `post_authors` is a database view. It has no key, so the generator
    // emits `View` (SELECT-only) rather than `Table`: `post_authors::view()`
    // has `query`, and `insert`/`update`/`delete` are not methods on it. That
    // is a compile error, not a runtime one.
    let authored = post_authors::view()
        .query((
            post_authors::user_name().eq("Ada"),
            select::order_by(post_authors::post_id().expr()),
        ))
        .all(db)
        .await?;
    println!("\n── a view model");
    for a in &authored {
        // Every column of a SQLite view comes back nullable: the catalog
        // cannot prove a view column NOT NULL, and the generator will not
        // claim what the schema does not say.
        println!("  {:?} — {:?}", a.title, a.user_name);
    }
    assert_eq!(authored.len(), 2);

    // ── composite keys and nullable foreign keys ────────────────────────
    //
    // `comments.user_id` is nullable, so the generated field is an `Option`
    // and the seeded anonymous comment reads back as `None`.
    let anonymous = comments::table()
        .query(comments::user_id().is_null())
        .all(db)
        .await?;
    assert_eq!(anonymous.len(), 1);
    assert_eq!(anonymous[0].user_id, None);

    // Aggregates are Layer 1's job; a model query is a Layer 1 query, so they
    // compose. `posts::views()` is a typed column and also an expression.
    let total: i64 = keelson::sqlite::select((
        select::columns(keelson::sqlite::f("sum", posts::views().expr())),
        select::from(quote("posts")),
    ))
    .fetch_scalar(db)
    .await?;
    println!("\n── aggregate over a model's column\n  {total} views in total");
    assert_eq!(total, 1110);

    println!("\nok");
    Ok(())
}
