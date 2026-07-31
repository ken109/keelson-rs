//! **The parts of `SELECT` past `WHERE`**, in PostgreSQL: joins, aggregates,
//! CTEs (including a recursive one), sub-queries, window functions and
//! `DISTINCT ON`.
//!
//!     cargo run -p keelson-examples --example joins_and_ctes
//!
//! PostgreSQL here rather than SQLite because these are where the dialects
//! stop agreeing, and `keelson-psql` is written to PostgreSQL's own grammar --
//! `DISTINCT ON`, `LATERAL`, `MATERIALIZED` and `SEARCH BREADTH FIRST` are on
//! this dialect because PostgreSQL has them, and are absent from the others
//! because those do not.

use keelson::prelude::*;
use keelson::psql::{self, arg, f, query, quote, s, select, subquery, window};
use keelson_examples::show;

fn main() -> keelson::Result<()> {
    // ── joins ───────────────────────────────────────────────────────────
    //
    // A join is a chain: pick the kind, alias it, then say how it joins.
    // `on_eq(a, b)` is the common case spelled once; `on(…)` takes any
    // expression, and `using([…])` takes column names.
    let q = psql::select((
        select::columns((
            quote(("u", "name")),
            quote(("p", "title")),
            quote(("c", "body")),
        )),
        select::from(quote("users")).as_("u"),
        select::inner_join(quote("posts"))
            .as_("p")
            .on_eq(quote(("p", "user_id")), quote(("u", "id"))),
        // LEFT JOIN, so a post with no comments still produces a row -- with
        // NULLs on the right-hand side, which is exactly why the generated
        // Layer 4 code turns a LEFT-joined side into an `Option`.
        select::left_join(quote("comments"))
            .as_("c")
            .on(quote(("c", "post_id"))
                .eq(quote(("p", "id")))
                .and(quote(("c", "body")).is_not_null())),
        select::where_(quote(("u", "is_active")).eq(arg(true))),
        select::order_by(quote(("p", "id"))),
    ));
    let (sql, args) = q.build()?;
    show("joins", &sql, &args);
    assert!(sql.contains("INNER JOIN \"posts\" AS \"p\" ON"));
    assert!(sql.contains("LEFT JOIN \"comments\" AS \"c\" ON"));

    // ── aggregates, GROUP BY, HAVING ────────────────────────────────────
    //
    // `f(name, args)` is a function call, and `.as_(alias)` names the output
    // column. `s(…)` is a string literal (as opposed to `quote(…)`, an
    // identifier, and `arg(…)`, a bound value).
    let q = psql::select((
        select::columns((
            quote(("u", "id")),
            f("count", quote(("p", "id"))).as_("posts"),
            f("coalesce", (f("sum", quote(("p", "views"))), arg(0))).as_("views"),
            f("string_agg", (quote(("p", "status")), s(","))).as_("statuses"),
        )),
        select::from(quote("users")).as_("u"),
        select::left_join(quote("posts"))
            .as_("p")
            .on_eq(quote(("p", "user_id")), quote(("u", "id"))),
        select::group_by(quote(("u", "id"))),
        // `f(…)` builds a `Function`, which carries the call's own options
        // (`DISTINCT`, `FILTER`, `OVER`). `into_expr()` ends that chain and
        // starts the expression one.
        select::having(f("count", quote(("p", "id"))).into_expr().gt(arg(2))),
        select::order_by(quote("views")).desc().nulls_last(),
    ));
    let (sql, args) = q.build()?;
    show("aggregates", &sql, &args);
    assert!(sql.contains("GROUP BY \"u\".\"id\" HAVING"));
    assert!(sql.contains("ORDER BY \"views\" DESC NULLS LAST"));

    // ── a common table expression ───────────────────────────────────────
    //
    // The CTE's body is a query, and a query is an expression -- so it goes
    // in directly. `materialized()` is PostgreSQL's optimiser fence, and is
    // on this dialect because PostgreSQL is the one that has it.
    let popular = psql::select((
        select::columns((quote("user_id"), f("count", quote("id")).as_("n"))),
        select::from(quote("posts")),
        select::where_(quote("views").gt(arg(100))),
        select::group_by(quote("user_id")),
    ));
    let q = psql::select((
        select::with("popular", popular).materialized(),
        select::columns((quote(("u", "name")), quote(("pop", "n")))),
        select::from(quote("users")).as_("u"),
        select::inner_join(quote("popular"))
            .as_("pop")
            .on_eq(quote(("pop", "user_id")), quote(("u", "id"))),
    ));
    let (sql, args) = q.build()?;
    show("a CTE", &sql, &args);
    assert!(sql.starts_with("WITH \"popular\" AS MATERIALIZED (SELECT"));

    // ── a recursive CTE ─────────────────────────────────────────────────
    //
    // The two arms are two statements combined with UNION ALL, and
    // `recursive(true)` writes the keyword. Nothing is special-cased: the
    // recursive reference is just a name in the second arm's FROM.
    let base = psql::select((select::columns((arg(1).as_("n"),)),));
    let step = psql::select((
        select::columns((quote("n").plus(arg(1)),)),
        select::from(quote("countdown")),
        select::where_(quote("n").lt(arg(10))),
    ));
    let q = psql::select((
        select::recursive(true),
        select::with(
            "countdown",
            psql::select((
                select::columns((quote("n"),)),
                select::from(subquery(base)).as_("seed"),
                select::union_all(step),
            )),
        )
        .columns(["n"]),
        select::from(quote("countdown")),
    ));
    let (sql, args) = q.build()?;
    show("a recursive CTE", &sql, &args);
    assert!(sql.starts_with("WITH RECURSIVE \"countdown\" (\"n\") AS ("));

    // ── a sub-query in WHERE ────────────────────────────────────────────
    //
    // Two spellings, and the difference matters: `query(q)` renders the
    // statement bare, `subquery(q)` wraps it in parentheses. `IN` supplies
    // its own parentheses, so `query` is the one that belongs here --
    // `subquery` would double them. Either way the placeholders are
    // renumbered by the *outer* writer, in render order.
    let authors = psql::select((
        select::columns(quote("user_id")),
        select::from(quote("posts")),
        select::where_(quote("status").eq(arg("published"))),
    ));
    let q = psql::select((
        select::from(quote("users")),
        select::where_(quote("id").in_(query(authors))),
        select::where_(quote("age").gte(arg(18))),
    ));
    let (sql, args) = q.build()?;
    show("a sub-query", &sql, &args);
    assert!(sql.contains(r#""id" IN (SELECT "user_id" FROM "posts""#));
    assert_eq!(args.len(), 2, "both arms' arguments, in render order");

    // ── window functions ────────────────────────────────────────────────
    //
    // `.over(…)` takes the same window mods a named `WINDOW` clause does, so
    // an inline window and a named one are written the same way.
    let q = psql::select((
        select::columns((
            quote("title"),
            f("row_number", ())
                .over((
                    window::partition_by(quote("user_id")),
                    window::order_by(quote("views")).desc(),
                ))
                .as_("rank"),
            f("sum", quote("views")).over_name("w").as_("running"),
        )),
        select::from(quote("posts")),
        select::window("w", window::order_by(quote("id"))),
    ));
    let (sql, args) = q.build()?;
    show("window functions", &sql, &args);
    assert!(sql.contains("OVER (PARTITION BY \"user_id\" ORDER BY \"views\" DESC)"));
    assert!(sql.contains("WINDOW \"w\" AS (ORDER BY \"id\")"));

    // ── DISTINCT ON ─────────────────────────────────────────────────────
    //
    // "the newest post per user", in one statement. PostgreSQL-only, and so
    // it exists only on `keelson-psql`: `keelson_sqlite::select::distinct_on`
    // is not a function you can call, because SQLite has no such construct.
    let q = psql::select((
        select::distinct_on(quote("user_id")),
        select::from(quote("posts")),
        select::order_by(quote("user_id")),
        select::order_by(quote("published_at")).desc(),
    ));
    let (sql, _) = q.build()?;
    show("DISTINCT ON", &sql, &[]);
    assert!(sql.starts_with("SELECT DISTINCT ON (\"user_id\") *"));

    println!("ok");
    Ok(())
}
