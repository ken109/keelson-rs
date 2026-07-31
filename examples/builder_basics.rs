//! **Layer 1, the query builder.** No database, no async: this builds
//! statements and prints them.
//!
//!     cargo run -p keelson-examples --example builder_basics
//!
//! The one idea to take away: a statement is a **starter** plus **mods**, and
//! a mod is an ordinary value. `sqlite::select(…)` is the starter;
//! `select::from(…)`, `select::where_(…)` and the rest are mods. A tuple of
//! mods is itself one mod, so mods nest, compose and get passed around like
//! any other value — which is what makes `dynamic_queries.rs` possible.

use keelson::prelude::*;
use keelson::sqlite::{self, arg, delete, insert, quote, select, update};
use keelson_examples::show;

fn main() -> keelson::Result<()> {
    // ── SELECT ──────────────────────────────────────────────────────────
    //
    // `quote` writes an identifier the dialect's own way (SQLite and
    // PostgreSQL use double quotes, MySQL backticks); `arg` binds a value as
    // a placeholder and never interpolates it into the text.
    let q = sqlite::select((
        select::columns((quote("id"), quote("name"))),
        select::from(quote("users")),
        select::where_(quote("age").gte(arg(21))),
        select::order_by(quote("name")),
        select::limit(10),
    ));
    let (sql, args) = q.build()?;
    show("a SELECT", &sql, &args);
    assert_eq!(
        sql,
        r#"SELECT "id", "name" FROM "users" WHERE ("age" >= ?1) ORDER BY "name" LIMIT 10"#
    );
    assert_eq!(args, vec![keelson::Value::I32(21)]);

    // `LIMIT 10` is in the text but `21` is a placeholder: `limit` took a
    // literal, `arg` asked for a bind. Which one you get is the call's
    // decision, never a heuristic -- `select::limit(arg(10))` binds instead.
    let (sql, args) =
        sqlite::select((select::from(quote("users")), select::limit(arg(10)))).build()?;
    assert_eq!(sql, r#"SELECT * FROM "users" LIMIT ?1"#);
    assert_eq!(args, vec![keelson::Value::I32(10)]);

    // ── mods are values ─────────────────────────────────────────────────
    //
    // Bind one to a variable, hand it to a function, store it in a `Vec`.
    // Nothing about a mod knows where it will be used.
    let recent = select::order_by(quote("created_at")).desc();
    let adults = select::where_(quote("age").gte(arg(18)));

    // A tuple of mods is one mod, and tuples nest -- so "the standard listing
    // options" can be a single value that several call sites share.
    let listing = (adults, recent, select::limit(20));
    let (sql, _) = sqlite::select((select::from(quote("users")), listing)).build()?;
    show("mods pulled out into a value", &sql, &[]);
    assert!(sql.contains("ORDER BY \"created_at\" DESC"));

    // ── INSERT ──────────────────────────────────────────────────────────
    //
    // Several `values` mods make several rows; `returning` asks for the
    // written rows back (SQLite and PostgreSQL have it, MySQL does not -- see
    // `dialects.rs`).
    let q = sqlite::insert((
        insert::into(quote("users")).columns(["name", "age"]),
        insert::values((arg("Ada"), arg(36))),
        insert::values((arg("Grace"), arg(45))),
        insert::returning(quote("id")),
    ));
    let (sql, args) = q.build()?;
    show("an INSERT of two rows", &sql, &args);
    assert_eq!(
        sql,
        r#"INSERT INTO "users" ("name", "age") VALUES (?1, ?2), (?3, ?4) RETURNING "id""#
    );
    assert_eq!(args.len(), 4);

    // ── UPDATE ──────────────────────────────────────────────────────────
    //
    // `set_col("x").to(…)` takes any expression, so a column can be updated
    // from itself: `views = views + 1` needs no round trip.
    let q = sqlite::update((
        update::table(quote("posts")),
        update::set_col("views").to(quote("views").plus(arg(1))),
        update::set_col("status").to(arg("published")),
        update::where_(quote("id").eq(arg(7))),
    ));
    let (sql, args) = q.build()?;
    show("an UPDATE", &sql, &args);
    assert_eq!(
        sql,
        r#"UPDATE "posts" SET "views" = ("views" + ?1), "status" = ?2 WHERE ("id" = ?3)"#
    );
    assert_eq!(args.len(), 3);

    // ── DELETE ──────────────────────────────────────────────────────────
    let q = sqlite::delete((
        delete::from(quote("comments")),
        delete::where_(quote("post_id").in_((arg(1), arg(2), arg(3)))),
    ));
    let (sql, args) = q.build()?;
    show("a DELETE", &sql, &args);
    assert_eq!(
        sql,
        r#"DELETE FROM "comments" WHERE ("post_id" IN (?1, ?2, ?3))"#
    );
    assert_eq!(args.len(), 3);

    // ── building is pure ────────────────────────────────────────────────
    //
    // `build()` borrows the statement and renders it; it does not consume or
    // mutate it, so the same query can be inspected, logged and then run.
    let q = sqlite::select((select::from(quote("tags")), select::limit(1)));
    assert_eq!(q.build()?, q.build()?);

    println!("ok");
    Ok(())
}
