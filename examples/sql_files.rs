//! **Layer 4: hand-written `.sql` files, compiled to typed Rust.**
//!
//!     cargo run -p keelson-examples --example sql_files
//!
//! `queries/blog.sql` is the source of truth; `src/queries/blog.rs` was
//! generated from it. Each query gets a parameter struct, a row struct with
//! nullability inferred from the parse tree plus the schema (every decision
//! written into the generated file as the rule that made it), and a verb that
//! runs it.
//!
//! The part that is not sqlc-or-cornucopia: **each query has two faces.**
//!
//! 1. A **query object** that runs the file's own SQL, as written.
//! 2. A **mod** that merges the same clauses *flat* into a host statement --
//!    its `WHERE` `AND`ed onto the host's, its `FROM` contributed only if the
//!    host has none. Nothing nests as a sub-select.
//!
//! Both faces slice the same bytes of the same file, so they cannot disagree,
//! and a `const` assertion on the file's length fails the build if the `.sql`
//! changed and nobody re-ran the generator.

use keelson::exec::ExecError;
use keelson::prelude::*;
use keelson::sqlite::{self, arg, quote, select};
use keelson_examples::Sandbox;
use keelson_examples::models::posts;
use keelson_examples::queries::blog;

#[tokio::main]
async fn main() -> Result<(), ExecError> {
    let sandbox = Sandbox::seeded().await?;
    let db = &sandbox.db;

    // ── face 1: run the file's own SQL ──────────────────────────────────
    //
    // `:many` gives a `Vec`. The parameters are typed and named after what
    // they are compared with, and a tuple converts into the parameter struct
    // so the call site stays short.
    let mine = blog::posts_for_user(db, (1i64, 10i64)).await?;
    println!("── posts_for_user (:many)");
    for p in &mine {
        // `status` is `Option<String>` because the column is nullable;
        // `views` is `i64` because it is NOT NULL. Rule N1, in both cases.
        println!("  {} — {:?}, {} views", p.title, p.status, p.views);
    }
    assert_eq!(mine.len(), 2);

    // `:one` means exactly one -- zero rows is an error.
    let ada = blog::user_by_id(db, 1i64).await?;
    println!("\n── user_by_id (:one)\n  {} <{:?}>", ada.name, ada.email);
    assert!(blog::user_by_id(db, 999i64).await.is_err());

    // `:optional` is the may-not-exist verb.
    let none = blog::user_by_email(db, "nobody@example.com".to_owned()).await?;
    assert!(none.is_none());

    // `:exec` runs for the side effect: no row struct at all.
    let bumped = blog::bump_views(db, 1i64).await?;
    assert_eq!(bumped.rows_affected, 1);

    // ── nullability that a LEFT JOIN forces ─────────────────────────────
    //
    // `users` is LEFT-joined through a nullable foreign key, so the whole
    // side became one `Option<…Author>` -- and inside it, each field keeps
    // its own nullability. The anonymous comment reads back as `None`.
    let thread = blog::comments_with_author(db, 1i64).await?;
    println!("\n── comments_with_author (LEFT JOIN → Option)");
    for c in &thread {
        match &c.author {
            Some(a) => println!("  {:?} by {}", c.body, a.name),
            None => println!("  {:?} by nobody", c.body),
        }
    }
    let anonymous = blog::comments_with_author(db, 3i64).await?;
    assert!(anonymous[0].author.is_none());

    // ── aggregates, and what NULL means for each ────────────────────────
    //
    // `count` is never NULL even over an empty group; every other aggregate
    // is; `coalesce` is NULL only when all of its arguments are. The
    // generated types say so.
    let stats = blog::user_stats(db, ()).await?;
    println!("\n── user_stats (aggregate nullability)");
    for s in &stats {
        println!(
            "  {}: {} posts, best {:?}, total {}",
            s.name, s.post_count, s.best_views, s.total_views
        );
    }
    // The types, not the data, are the point: `count` came back `i64`,
    // `max` came back `Option<i64>`, and `coalesce(sum(...), 0)` came back
    // `i64` because one of its arguments cannot be NULL.
    let _: i64 = stats[0].post_count;
    let _: Option<i64> = stats[0].best_views;
    let _: i64 = stats[0].total_views;

    // ── a to-many shape, folded ─────────────────────────────────────────
    //
    // The dotted alias `t.name AS "tags.name"` collects the repeated side
    // into a `Vec` of a nested struct, and the generated verb folds the flat
    // result rows into it.
    let tagged = blog::posts_with_tags(db, ()).await?;
    println!("\n── posts_with_tags (to-many)");
    for p in &tagged {
        let names: Vec<&str> = p.tags.iter().map(|t| t.name.as_str()).collect();
        println!("  {} {names:?}", p.title);
    }
    assert_eq!(tagged.len(), 4, "four posts, not one row per tag");

    // ── annotations, where inference will not guess ─────────────────────
    //
    // `upper()` is not in the generator's function table, so the `.sql` file
    // states the column's type and nullability. Refusing to guess is the
    // rule; the annotation is how you answer.
    let shouty = blog::shouty_titles(db, "%o%".to_owned()).await?;
    println!("\n── shouty_titles (annotated)\n  {:?}", shouty[0].shouty);
    assert_eq!(shouty[0].shouty, "HELLO");

    // ── face 2: the same query as a mod ─────────────────────────────────
    //
    // Here is the difference from a sub-select: the hand-written `WHERE` and
    // `ORDER BY` merge into the host statement, which keeps its own
    // projection. One flat statement, and the placeholders are renumbered by
    // the host's writer in render order.
    let hosted = sqlite::select((
        select::columns((quote(("p", "id")), quote(("p", "title")))),
        // The host has no FROM of its own, so the fragment's `FROM posts p`
        // (aliases and joins included) is contributed.
        blog::posts_for_user_mod((1i64, 10i64)),
        select::where_(quote(("p", "status")).eq(arg("published"))),
    ));
    let (sql, args) = hosted.build()?;
    println!("\n── the mod face, merged flat\n  {sql}\n  args: {args:?}");
    assert!(!sql.contains("(SELECT"), "nothing nests");
    assert!(sql.contains(r#"WHERE (p.user_id = ?1) AND ("p"."status" = ?2)"#));

    // ── the mod on a *model* query ──────────────────────────────────────
    //
    // The payoff: a hand-written `WHERE` merged into a generated model query,
    // in the same tuple as a typed filter. The model owns the projection, so
    // the rows still decode into `posts::Post`.
    // Two mechanics to notice. The mod is written against the dialect
    // statement (`SelectQuery`) and a model query is a wrapper around one, so
    // it goes in through `apply` -- the same escape hatch every
    // statement-specific mod uses. And `popular_posts` is written without a
    // table alias, because the *host* owns the FROM here: a fragment saying
    // `p.views` would refer to an alias that is not in the merged statement.
    let mut q = posts::table().query((
        posts::status().eq("published"),
        select::order_by(posts::id().expr()),
    ));
    q.apply(blog::popular_posts_mod(100i64));
    let rows = q.all(db).await?;
    println!("\n── a .sql fragment inside a model query");
    for p in &rows {
        println!("  {} ({} views)", p.title, p.views);
    }
    assert_eq!(rows.len(), 2);

    // ── inspecting a query object ───────────────────────────────────────
    //
    // A query object is an ordinary `Query`, so it builds like any other --
    // and it can be nested as a sub-select when that is what you *do* want.
    let q = blog::posts_for_user_query((2i64, 5i64));
    let (sql, args) = q.build()?;
    println!(
        "\n── the query face, verbatim\n  {}\n  args: {args:?}",
        sql.replace('\n', "\n  ")
    );
    assert_eq!(q.params().user_id, 2);

    println!("\nok");
    Ok(())
}
