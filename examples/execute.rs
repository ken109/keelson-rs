//! **Layer 2: running a statement.** Against a real SQLite database created
//! in a temporary file and deleted at the end.
//!
//!     cargo run -p keelson-examples --example execute
//!
//! Layer 1 built `(String, Vec<Value>)` with no driver in sight. This layer is
//! everything between that pair and a Rust value coming back:
//!
//! - `Executor` -- object-safe, three methods, `&self`. A pool, a connection
//!   and a transaction all implement it, so `&dyn Executor` is what
//!   application code and generated models pass around.
//! - `Execute` -- the verbs, blanket-implemented on every query:
//!   `fetch_all`, `fetch_one`, `fetch_optional`, `fetch_scalar`,
//!   `fetch_scalars`, `execute`.
//! - `FromRow` -- mapping a row onto a struct, by column name, with the
//!   failing column named in the error.
//!
//! Swapping engines is a feature flag and a pool type: `keelson::sqlx::psql`
//! and `keelson::sqlx::mysql` are the same three verbs over the same traits.

use keelson::exec::{ExecError, Executor, Statement};
use keelson::prelude::*;
use keelson::sqlite::{self, arg, insert, quote, select};
use keelson::{FromRow, Value};
use keelson_examples::Sandbox;

/// Mapping is by field name. `rename` is for when the column disagrees;
/// `flatten` reads several of this row's columns into a nested struct, which
/// is how a joined-in side is kept as its own type.
#[derive(Debug, PartialEq, FromRow)]
struct Post {
    id: i64,
    #[keelson(rename = "title")]
    heading: String,
    views: i64,
    #[keelson(flatten)]
    author: Author,
}

#[derive(Debug, PartialEq, FromRow)]
struct Author {
    #[keelson(rename = "author_name")]
    name: String,
    // A nullable column is an `Option`; a `NULL` in a non-`Option` field is a
    // decode error that names the column (see `errors.rs`).
    #[keelson(rename = "author_email")]
    email: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), ExecError> {
    let sandbox = Sandbox::seeded().await?;
    let db = &sandbox.db;

    // ── fetch_all: rows mapped onto a struct ────────────────────────────
    let posts: Vec<Post> = sqlite::select((
        select::columns((
            quote(("p", "id")),
            quote(("p", "title")),
            quote(("p", "views")),
            quote(("u", "name")).as_("author_name"),
            quote(("u", "email")).as_("author_email"),
        )),
        select::from(quote("posts")).as_("p"),
        select::inner_join(quote("users"))
            .as_("u")
            .on_eq(quote(("u", "id")), quote(("p", "user_id"))),
        select::where_(quote(("p", "views")).gt(arg(100))),
        select::order_by(quote(("p", "views"))).desc(),
    ))
    .fetch_all(db)
    .await?;

    println!("── fetch_all");
    for p in &posts {
        println!("  {} ({} views) by {}", p.heading, p.views, p.author.name);
    }
    assert_eq!(posts.len(), 2);
    assert_eq!(posts[0].heading, "Compilers");
    assert_eq!(posts[0].author.name, "Grace");

    // ── fetch_one / fetch_optional ──────────────────────────────────────
    //
    // `fetch_one` means one: zero rows is an error, and so is more than one.
    // `fetch_optional` is the "may not exist" verb.
    let by_id = |id: i64| {
        sqlite::select((
            select::columns((quote("id"), quote("name"))),
            select::from(quote("users")),
            select::where_(quote("id").eq(arg(id))),
        ))
    };

    #[derive(Debug, FromRow)]
    struct User {
        id: i64,
        name: String,
    }

    let ada: User = by_id(1).fetch_one(db).await?;
    println!("\n── fetch_one\n  {} = {}", ada.id, ada.name);
    assert_eq!(ada.name, "Ada");

    let missing: Option<User> = by_id(999).fetch_optional(db).await?;
    assert!(missing.is_none());

    // ── fetch_scalar / fetch_scalars: one column ────────────────────────
    //
    // No struct at all when the answer is a single column.
    let count: i64 = sqlite::select((
        select::columns(sqlite::f("count", quote("id"))),
        select::from(quote("posts")),
    ))
    .fetch_scalar(db)
    .await?;

    let titles: Vec<String> = sqlite::select((
        select::columns(quote("title")),
        select::from(quote("posts")),
        select::order_by(quote("id")),
    ))
    .fetch_scalars(db)
    .await?;

    println!("\n── scalars\n  {count} posts: {titles:?}");
    assert_eq!(count, 4);
    assert_eq!(titles.len(), 4);

    // ── execute: statements that return no rows ─────────────────────────
    let result = sqlite::insert((
        insert::into(quote("tags")).columns(["name"]),
        insert::values(arg("async")),
    ))
    .execute(db)
    .await?;
    println!(
        "\n── execute\n  {} row(s), last insert id {:?}",
        result.rows_affected, result.last_insert_id
    );
    assert_eq!(result.rows_affected, 1);
    // SQLite and MySQL report an auto-increment id here; PostgreSQL answers
    // `None` and expects `RETURNING`, which keelson does not paper over.
    assert!(result.last_insert_id.is_some());

    // ── &dyn Executor ───────────────────────────────────────────────────
    //
    // The trait is object-safe, so a helper takes `&dyn Executor` and works
    // for a pool, a connection or a transaction without being generic.
    async fn count_posts(db: &dyn Executor, user_id: i64) -> Result<i64, ExecError> {
        sqlite::select((
            select::columns(sqlite::f("count", quote("id"))),
            select::from(quote("posts")),
            select::where_(quote("user_id").eq(arg(user_id))),
        ))
        .fetch_scalar(db)
        .await
    }
    assert_eq!(count_posts(db, 1).await?, 2);

    // ── raw statements ──────────────────────────────────────────────────
    //
    // The executor's own surface is `Statement`: SQL plus arguments. This is
    // the seam DDL goes through (keelson is DML-only -- migrations belong to
    // a migration tool), and the escape hatch for SQL no layer above built.
    db.execute(Statement::new(
        "UPDATE posts SET views = views + ?1 WHERE id = ?2",
        vec![Value::I64(5), Value::I64(1)],
    ))
    .await?;

    // Rows without a struct: `fetch_rows` hands back `Row`s, which are
    // accessed by name and decode on demand.
    let rows = sqlite::select((
        select::columns((quote("id"), quote("views"))),
        select::from(quote("posts")),
        select::where_(quote("id").eq(arg(1))),
    ))
    .fetch_rows(db)
    .await?;
    let views: i64 = rows[0].get("views")?;
    println!("\n── raw statement + rows\n  post 1 now has {views} views");
    assert_eq!(views, 125);

    println!("\nok");
    Ok(())
}
