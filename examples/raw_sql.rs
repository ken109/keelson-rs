//! **Writing SQL yourself.** A whole hand-written statement is an ordinary
//! keelson query: same verbs, same row mapping, same transaction, same
//! placeholder binding.
//!
//!     cargo run -p keelson-examples --example raw_sql
//!
//! There are three ways to hand keelson SQL you wrote, and they are for
//! different situations:
//!
//! 1. **`raw_query(…)`** — a whole statement, right here, run now. What this
//!    example is mostly about.
//! 2. **A raw fragment inside a built statement** — `raw`, `template`, or a
//!    bare `&str` where an expression is expected. For when the *statement* is
//!    keelson's and one *piece* of it is yours.
//! 3. **A `.sql` file compiled by keelson-gen** — for SQL that lives in the
//!    project rather than in a call. That one gets you inferred parameter and
//!    row types, which the two above cannot have; see `sql_files.rs`.
//!
//! What every one of them gives up is the same: keelson does not parse what
//! you hand it, so being grammatical for the engine, and quoting identifiers,
//! is yours. What you keep is the binding, the placeholder rewriting, the row
//! mapping and the tracing span.

use keelson::exec::{ExecError, Execute as _};
use keelson::prelude::*;
use keelson::sqlite::{self, QueryType, arg, quote, select};
use keelson::{FromRow, Value};
use keelson_examples::{Sandbox, show};

#[derive(Debug, FromRow)]
struct TopPost {
    author: String,
    title: String,
    views: i64,
}

#[tokio::main]
async fn main() -> Result<(), ExecError> {
    let sandbox = Sandbox::seeded().await?;
    let db = &sandbox.db;

    // ── 1. a whole statement, written by hand ───────────────────────────
    //
    // A CTE feeding a window function feeding a join. keelson can build this
    // (see `joins_and_ctes.rs`) — but if you would rather write the SQL, this
    // is what that looks like, and nothing about the rest of the API changes.
    //
    // `?` is the placeholder on every dialect and is rewritten as the
    // statement renders: `?1` here, `$1` on PostgreSQL, `?` on MySQL. The
    // value is bound, never interpolated.
    let q = sqlite::raw_query(
        r#"
        WITH ranked AS (
            SELECT p.id, p.title, p.user_id, p.views,
                   row_number() OVER (PARTITION BY p.user_id ORDER BY p.views DESC) AS rank
            FROM posts p
        )
        SELECT u.name AS author, r.title, r.views
        FROM ranked r
        JOIN users u ON u.id = r.user_id
        WHERE r.rank = 1 AND r.views >= ?
        ORDER BY r.views DESC
        "#,
    )
    .bind(50);

    // It is an ordinary query, so `fetch_all::<T>()` maps it onto a struct
    // exactly as it would a built one.
    let top: Vec<TopPost> = q.fetch_all(db).await?;
    println!("── a hand-written statement, mapped onto a struct");
    for p in &top {
        println!("  {}: {} ({} views)", p.author, p.title, p.views);
    }
    assert_eq!(top.len(), 2);
    assert_eq!(top[0].author, "Grace");

    // And it builds like one: inspect the SQL and the arguments before it runs.
    let (_, args) = q.build()?;
    assert_eq!(args, vec![Value::I32(50)], "the 50 is bound, not inlined");

    // ── every verb, unchanged ───────────────────────────────────────────
    let count: i64 = sqlite::raw_query("SELECT count(*) FROM posts WHERE status = ?")
        .bind("published")
        .fetch_scalar(db)
        .await?;

    let done = sqlite::raw_query("UPDATE posts SET views = views + ? WHERE user_id = ?")
        .bind(10)
        .bind(1)
        // Only ever read by the tracing span. The default is `Unknown`:
        // keelson has not read the text, and guessing from the leading
        // keyword would be wrong for `WITH … INSERT`.
        .kind(QueryType::Update)
        .execute(db)
        .await?;

    println!(
        "\n── the other verbs\n  {count} published, {} rows bumped",
        done.rows_affected
    );
    assert_eq!(count, 3);
    assert_eq!(done.rows_affected, 2);

    // Including things the builder has no vocabulary for at all -- keelson is
    // DML-only, so DDL and pragmas are exactly this.
    sqlite::raw_query("PRAGMA foreign_keys = ON")
        .execute(db)
        .await?;

    // ── binding a list ──────────────────────────────────────────────────
    //
    // `bind_expr` splices an expression where a value would go, and it
    // consumes as many placeholder positions as it binds. That is what makes
    // `IN (?)` expand instead of binding one opaque blob.
    let ids = vec![1i64, 2, 3];
    let q = sqlite::raw_query("SELECT title FROM posts WHERE id IN (?) AND views > ? ORDER BY id")
        .bind_expr(sqlite::args(ids))
        .bind(100);
    let (sql, args) = q.build()?;
    show("a list, expanded", &sql, &args);
    assert!(sql.contains("IN (?1, ?2, ?3) AND views > ?4"));

    let titles: Vec<String> = q.fetch_scalars(db).await?;
    assert_eq!(titles, vec!["Hello".to_owned(), "Compilers".to_owned()]);

    // A mismatch between the `?` and what is bound is an error out of
    // `build()`, not a statement that quietly binds the wrong thing.
    let err = sqlite::raw_query("SELECT * FROM posts WHERE id = ? AND views > ?")
        .bind(1)
        .build()
        .unwrap_err();
    println!("── a placeholder with nothing bound to it\n  {err}\n");

    // ── composing with built statements, both directions ────────────────
    //
    // A hand-written statement is an `Expression` too, so it nests as a
    // sub-select in a built one -- and the outer writer renumbers it, so the
    // fragment does not have to know where it landed.
    let built = sqlite::select((
        select::columns((quote("id"), quote("name"))),
        select::from(quote("users")),
        select::where_(quote("id").in_(sqlite::query(
            sqlite::raw_query("SELECT user_id FROM posts WHERE views > ?").bind(500),
        ))),
        select::where_(quote("is_active").eq(arg(true))),
    ));
    let (sql, args) = built.build()?;
    show("hand-written, nested in built", &sql, &args);
    assert!(sql.contains("IN (SELECT user_id FROM posts WHERE views > ?1)"));
    assert_eq!(args.len(), 2, "renumbered in render order");

    // The other direction: a *built* query spliced into a hand-written one,
    // through the same `bind_expr`.
    let recent = sqlite::select((
        select::columns(quote("user_id")),
        select::from(quote("posts")),
        select::where_(quote("status").eq(arg("published"))),
    ));
    let names: Vec<String> =
        sqlite::raw_query("SELECT name FROM users WHERE id IN (?) ORDER BY name")
            .bind_expr(sqlite::query(recent))
            .fetch_scalars(db)
            .await?;
    println!("── built, nested in hand-written\n  {names:?}\n");
    assert_eq!(names, vec!["Ada".to_owned(), "Grace".to_owned()]);

    // ── 2. a fragment inside a built statement ──────────────────────────
    //
    // The other shape: the statement is keelson's, one piece of it is yours.
    // Four ways to put text in, and which one you get is always the call's
    // decision:
    //
    //   quote(…)  an identifier, quoted the dialect's way
    //   s(…)      a string literal, escaped, in the SQL text
    //   arg(…)    a value, bound to a placeholder, never in the text
    //   raw(…)    bytes, verbatim
    //
    // Two `where_` mods are `AND`ed however they were built, so a
    // hand-written condition composes with a structured one rather than
    // replacing it.
    let q = sqlite::select((
        select::from(quote("posts")),
        select::where_(quote("views").gt(arg(100))),
        select::where_("status = 'published'"), // a bare &str is an expression
        select::order_by(sqlite::raw("random()")),
    ));
    let (sql, args) = q.build()?;
    show("a fragment beside a typed mod", &sql, &args);
    assert!(sql.contains("AND status = 'published'"));

    // `template` is `raw` with binding: the same `?` rule as `raw_query`,
    // for a fragment rather than a statement.
    let q = sqlite::select((
        select::from(quote("posts")),
        select::where_(sqlite::template(
            "instr(lower(?), ?) > 0",
            [
                keelson::sqlite::RawArg::expr(quote("title")),
                keelson::sqlite::RawArg::value("hello"),
            ],
        )),
    ));
    let (sql, args) = q.build()?;
    show("a bound fragment", &sql, &args);
    assert_eq!(
        sql,
        r#"SELECT * FROM "posts" WHERE instr(lower("title"), ?1) > 0"#
    );

    // ── 3. and where keelson refuses ────────────────────────────────────
    //
    // Rendering is one pass and cannot fail half way, so a construct that
    // cannot be written records its error on the writer and `build()`
    // surfaces it once. What you never get is a statement quietly missing a
    // clause. (PostgreSQL here because these are its constructs.)
    use keelson::psql;

    let broken = psql::select((
        psql::select::from(psql::quote("users")),
        // ROLLUP with no grouping columns is not a construct: the `GROUP BY `
        // in front of it would be left dangling.
        psql::select::group_by(psql::rollup(())),
    ));
    match broken.build() {
        Err(e) => println!("── refused\n  {e}\n"),
        Ok((sql, _)) => panic!("expected a refusal, got {sql}"),
    }

    let broken = psql::select((
        psql::select::from(psql::quote("users")),
        // PostgreSQL puts LATERAL only in front of a sub-query or a function,
        // never a bare table name.
        psql::select::left_join(psql::quote("posts"))
            .lateral()
            .on("true"),
    ));
    match broken.build() {
        Err(e) => println!("── refused, and it names the rule\n  {e}\n"),
        Ok((sql, _)) => panic!("expected a refusal, got {sql}"),
    }

    println!("ok");
    Ok(())
}
