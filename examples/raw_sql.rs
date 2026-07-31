//! **Raw SQL is a first-class expression.** Anywhere keelson accepts an
//! expression, it accepts a hand-written fragment -- in the same tuple, next
//! to structured mods, with no escape-hatch API to reach for.
//!
//!     cargo run -p keelson-examples --example raw_sql
//!
//! This is deliberate. A query builder that cannot express something your
//! database can is a wall; keelson's answer is that you write that part
//! yourself and keep everything else. The trade is equally deliberate: keelson
//! does not parse what you hand it, so a raw fragment is yours to get right.
//!
//! The other half of the story is at the end: where keelson *cannot* render
//! something, `build()` returns an error that says so. It never guesses and
//! never silently drops a clause.

use keelson::prelude::*;
use keelson::psql::{
    self, RawArg, arg, arg_group, args, f, placeholders, quote, raw, s, select, template,
};
use keelson_examples::show;

fn main() -> keelson::Result<()> {
    // ── the four ways to put text in a statement ────────────────────────
    //
    //   quote(…)  an identifier, quoted the dialect's way
    //   s(…)      a string literal, escaped, in the SQL text
    //   arg(…)    a value, bound to a placeholder, never in the text
    //   raw(…)    bytes, verbatim
    //
    // Which one you get is always the call's decision.
    let q = psql::select((
        select::columns((
            quote(("u", "name")),
            s("literal string"),
            arg("bound value"),
            raw("now() - interval '7 days'"),
        )),
        select::from(quote("users")).as_("u"),
    ));
    let (sql, args_) = q.build()?;
    show("identifier / literal / bound / raw", &sql, &args_);
    assert_eq!(args_.len(), 1, "only `arg` binds");
    assert!(sql.contains("'literal string'"));
    assert!(sql.contains("now() - interval '7 days'"));

    // ── a raw fragment in the same tuple as typed mods ──────────────────
    //
    // Two `where_` mods are `AND`ed whatever they were built from, so the
    // hand-written one composes with the structured one instead of replacing
    // it.
    let q = psql::select((
        select::from(quote("posts")),
        select::where_(quote("views").gt(arg(100))),
        select::where_("status = 'published' AND published_at > now() - interval '1 day'"),
        select::order_by(raw("random()")),
    ));
    let (sql, args_) = q.build()?;
    show("raw beside typed", &sql, &args_);
    assert!(sql.contains("AND status = 'published'"));
    assert!(sql.ends_with("ORDER BY random()"));

    // ── raw with bound arguments ────────────────────────────────────────
    //
    // `template` rewrites each `?` into the dialect's placeholder and binds
    // the matching argument -- so a hand-written fragment still never
    // interpolates a value. `RawArg::expr` splices an expression instead of a
    // value, which is how a raw fragment can refer to a quoted identifier.
    let q = psql::select((
        select::from(quote("posts")),
        select::where_(template(
            "to_tsvector('english', ?) @@ plainto_tsquery(?)",
            [RawArg::expr(quote("body")), RawArg::value("index scan")],
        )),
    ));
    let (sql, args_) = q.build()?;
    show("template with bound args", &sql, &args_);
    assert_eq!(
        sql,
        concat!(
            r#"SELECT * FROM "posts" "#,
            r#"WHERE to_tsvector('english', "body") @@ plainto_tsquery($1)"#
        )
    );
    assert_eq!(args_, vec![keelson::Value::Text("index scan".to_owned())]);

    // ── binding a list ──────────────────────────────────────────────────
    //
    // `args` binds a list as bare placeholders (the operand of `IN`);
    // `arg_group` wraps them in parentheses (a row constructor);
    // `placeholders(n)` writes n placeholders that each bind NULL, so the
    // *shape* of the statement is right and whatever rebinds it supplies the
    // values.
    let ids = vec![4, 8, 15, 16];
    let (sql, args_) = psql::select((
        select::from(quote("posts")),
        select::where_(quote("id").in_(args(ids))),
        select::where_(
            psql::group((quote("user_id"), quote("status"))).eq(arg_group(["1", "published"])),
        ),
    ))
    .build()?;
    show("lists: args vs arg_group", &sql, &args_);
    assert!(sql.contains(r#""id" IN ($1, $2, $3, $4)"#));
    assert!(sql.contains(r#"("user_id", "status") = ($5, $6)"#));

    let (sql, args_) = psql::select((
        select::from(quote("t")),
        select::where_(quote("id").in_(placeholders(2))),
    ))
    .build()?;
    show("placeholders bound to NULL", &sql, &args_);
    assert!(sql.contains("IN ($1, $2)"));
    assert_eq!(args_, vec![keelson::Value::Null; 2]);

    // ── raw as a from-item, and as a function ───────────────────────────
    let q = psql::select((
        select::columns((quote(("s", "n")), f("pg_typeof", quote(("s", "n"))))),
        select::from(raw("generate_series(1, 10)"))
            .as_("s")
            .columns(["n"]),
    ));
    let (sql, _) = q.build()?;
    show("a raw from-item", &sql, &[]);
    assert!(sql.contains("FROM generate_series(1, 10) AS \"s\" (\"n\")"));

    // ── the other half: unrenderable is an error ────────────────────────
    //
    // `build()` is infallible up to the point it returns: writing SQL cannot
    // fail mid-way, so a construct that cannot be rendered *records* an error
    // on the writer and `build()` surfaces it once, at the end. What you
    // never get is a statement that is silently missing a clause.
    let broken = psql::select((
        select::from(quote("users")),
        // ROLLUP with no grouping columns is not a construct: the `GROUP BY `
        // in front of it would be left dangling.
        select::group_by(psql::rollup(())),
    ));
    match broken.build() {
        Err(e) => println!("── refused, as it should be\n{e}\n"),
        Ok((sql, _)) => panic!("expected a refusal, got {sql}"),
    }

    // Same rule for a construct used where the grammar does not allow it:
    // PostgreSQL puts `LATERAL` only in front of a sub-query or a function,
    // never a bare table name.
    let broken = psql::select((
        select::from(quote("users")),
        select::left_join(quote("posts")).lateral().on("true"),
    ));
    match broken.build() {
        Err(e) => println!("── refused, and it names the rule\n{e}\n"),
        Ok((sql, _)) => panic!("expected a refusal, got {sql}"),
    }

    println!("ok");
    Ok(())
}
