//! **Loading relations**: the two strategies, and how they differ.
//!
//!     cargo run -p keelson-examples --example relations
//!
//! keelson has exactly two, both explicit, and neither is lazy loading:
//!
//! - **preload** -- a `LEFT JOIN` in the *same* query, for to-one relations.
//!   One round trip; the joined columns arrive prefixed (`"user.id"`) and a
//!   generated mapper reads them back out of the same row. An all-NULL prefix
//!   is a miss, and maps to `None`.
//! - **then-load** -- one further keyed query *per level*, to-one and
//!   to-many, over the deduplicated set of keys, `KEY_BATCH` keys at a time.
//!   Levels chain: `a().then(b())` is two levels and two extra queries.
//!
//! Loaded rows land in the row's `rel` field: `post.rel.user`,
//! `user.rel.posts`. Every to-one field is `Option<Box<Row>>` -- boxed
//! uniformly, so that a field's type never depends on whether the rest of the
//! schema happens to contain a cycle.

use keelson::exec::ExecError;
use keelson::models::KEY_BATCH;
use keelson::prelude::*;
use keelson::sqlite::select;
use keelson_examples::Sandbox;
use keelson_examples::models::{comments, posts, users};

#[tokio::main]
async fn main() -> Result<(), ExecError> {
    let sandbox = Sandbox::seeded().await?;
    let db = &sandbox.db;

    // ── preload: one query, a LEFT JOIN ─────────────────────────────────
    let loaded = posts::table()
        .query((posts::preload::user(), select::order_by(posts::id().expr())))
        .all(db)
        .await?;
    println!("── preload (one query)");
    for p in &loaded {
        // `rel.user` is `Option<Box<User>>`; the `Box` derefs away when you
        // read through it.
        let author = p.rel.user.as_ref().expect("posts.user_id is NOT NULL");
        println!("  {} by {}", p.title, author.name);
    }
    assert_eq!(loaded[0].rel.user.as_ref().unwrap().name, "Ada");

    // The join and the prefixed columns are in the statement, so you can see
    // exactly what a preload costs.
    let q = posts::table().query(posts::preload::user());
    let (sql, _) = q.as_select().build()?;
    println!("\n── what a preload builds\n  {sql}");
    assert!(sql.contains(r#"LEFT JOIN "users" ON"#));
    assert!(sql.contains(r#""users"."name" AS "user.name""#));

    // ── then-load: a second query, keyed ────────────────────────────────
    //
    // To-many cannot be a join without multiplying the parent rows, so it is
    // a second query keyed by the first's keys. To-one can be either: a
    // then-load costs a round trip but keeps the parent projection narrow.
    let authors = users::table()
        .query((
            users::then_load::posts(),
            select::order_by(users::id().expr()),
        ))
        .all(db)
        .await?;
    println!("\n── then-load (one extra query)");
    for u in &authors {
        let titles: Vec<&str> = u.rel.posts.iter().map(|p| p.title.as_str()).collect();
        println!("  {} wrote {titles:?}", u.name);
    }
    assert_eq!(authors[0].rel.posts.len(), 2);
    assert!(authors[2].rel.posts.is_empty(), "Kid wrote nothing");

    // ── chaining levels ─────────────────────────────────────────────────
    //
    // `.then(…)` hangs the next level off this one, and it is typed by the
    // child model: only a loader over `Post` fits after
    // `users::then_load::posts()`, so a wrong path is a compile error rather
    // than a string that silently loads nothing.
    let deep = users::table()
        .query((
            users::then_load::posts().then(posts::then_load::comments()),
            users::id().eq(1),
        ))
        .all(db)
        .await?;
    let hello = &deep[0].rel.posts[0];
    println!(
        "\n── two levels\n  {} → {} → {} comment(s)",
        deep[0].name,
        hello.title,
        hello.rel.comments.len()
    );
    assert_eq!(hello.rel.comments.len(), 2);

    // ── shaping a level ─────────────────────────────────────────────────
    //
    // `with` runs on every batch query of that level, after the key filter,
    // so a relation can be filtered, ordered, limited -- or can preload its
    // own to-one relation, which is how a to-many of a to-one is loaded
    // without a third query.
    let popular = users::table()
        .query(
            users::then_load::posts()
                .with(|q| posts::views().gt(100).apply(q))
                .with(|q| select::order_by(posts::views().expr()).desc().apply(q)),
        )
        .all(db)
        .await?;
    println!("\n── a shaped then-load");
    for u in &popular {
        println!("  {} has {} popular post(s)", u.name, u.rel.posts.len());
    }
    assert_eq!(popular[0].rel.posts.len(), 1, "only 'Hello' is over 100");

    // ── the to-one direction, and a nullable one ────────────────────────
    //
    // `comments.user_id` is nullable, so a comment may have no author. The
    // loader attaches `None` for it rather than inventing a row.
    let with_authors = comments::table()
        .query((
            comments::then_load::user(),
            select::order_by(comments::id().expr()),
        ))
        .all(db)
        .await?;
    println!("\n── a nullable to-one");
    for c in &with_authors {
        match &c.rel.user {
            Some(u) => println!("  {:?} by {}", c.body, u.name),
            None => println!("  {:?} by nobody", c.body),
        }
    }
    assert!(with_authors[2].rel.user.is_none());

    // ── how many queries, exactly ───────────────────────────────────────
    //
    // One per level, not one per row -- keys are deduplicated and batched
    // `KEY_BATCH` at a time, so a level over 10 000 parents is
    // ceil(10 000 / KEY_BATCH) statements, not 10 000. The batch size is
    // overridable per level with `.batch(n)`.
    println!(
        "\n── batching\n  {} keys per IN list by default; \
         the two-level load above was 1 + 1 + 1 = 3 queries",
        KEY_BATCH
    );

    println!("\nok");
    Ok(())
}
