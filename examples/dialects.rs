//! **One intent, three dialects.** "Insert a user; if that email is already
//! taken, update the existing row instead."
//!
//!     cargo run -p keelson-examples --example dialects
//!
//! keelson has no shared AST that every database is squeezed through. Your
//! statement type *is* `psql::InsertQuery` or `mysql::InsertQuery` or
//! `sqlite::InsertQuery`, and each is written to its own engine's grammar.
//! What that buys, and what it costs:
//!
//! - **Costs:** you cannot build one statement and render it for whichever
//!   database the customer brought. If that is your requirement, use
//!   [sea-query](https://github.com/SeaQL/sea-query) -- it is the right tool
//!   and keelson makes it impossible by construction.
//! - **Buys:** constructs a common denominator cannot express, and the
//!   guarantee that what compiles is grammatical for the engine you compiled
//!   it for. MySQL has no `RETURNING`, so `keelson_mysql::insert::returning`
//!   is not a function -- the mistake is a compile error, not a runtime one.
//!
//! Notice, too, that quoting and placeholders differ per dialect and are never
//! something you spell yourself.

use keelson::prelude::*;
use keelson_examples::show;

fn main() -> keelson::Result<()> {
    upsert_postgres()?;
    upsert_mysql()?;
    upsert_sqlite()?;
    merge_postgres()?;
    replace_variants()?;
    println!("ok");
    Ok(())
}

/// PostgreSQL: `ON CONFLICT … DO UPDATE`, `EXCLUDED`, `RETURNING`,
/// `$n` placeholders and `"double quotes"`.
fn upsert_postgres() -> keelson::Result<()> {
    use keelson::psql::{self, arg, insert, quote};

    let q = psql::insert((
        insert::into(quote("users")).columns(["email", "name"]),
        insert::values((arg("ada@example.com"), arg("Ada"))),
        insert::on_conflict(quote("email")).do_update(insert::set_excluded(["name"])),
        // PostgreSQL can hand the written row back, so a write is one round
        // trip. This is why a generated PostgreSQL model has `.one(&db)` on
        // its insert and update.
        insert::returning((quote("id"), quote("name"))),
    ));
    let (sql, args) = q.build()?;
    show("PostgreSQL upsert", &sql, &args);
    assert_eq!(
        sql,
        concat!(
            r#"INSERT INTO "users" ("email", "name") VALUES ($1, $2) "#,
            r#"ON CONFLICT ("email") DO UPDATE SET "name" = EXCLUDED."name" "#,
            r#"RETURNING "id", "name""#
        )
    );
    Ok(())
}

/// MySQL: `ON DUPLICATE KEY UPDATE`, a row alias instead of `EXCLUDED`,
/// `?` placeholders, `` `backticks` `` -- and no `RETURNING` at all.
fn upsert_mysql() -> keelson::Result<()> {
    use keelson::mysql::{self, arg, insert, quote};

    let q = mysql::insert((
        insert::into(quote("users")).columns(["email", "name"]),
        insert::values((arg("ada@example.com"), arg("Ada"))),
        // MySQL 8.0.19 names the incoming row instead of MySQL's older
        // `VALUES(col)`; `set_values(["name"])` writes the deprecated form
        // for a server that predates it.
        insert::as_("new"),
        insert::on_duplicate_key_update(insert::set_row("new", ["name"])),
    ));
    let (sql, args) = q.build()?;
    show("MySQL upsert", &sql, &args);
    assert_eq!(
        sql,
        concat!(
            "INSERT INTO `users` (`email`, `name`) VALUES (?, ?) AS `new` ",
            "ON DUPLICATE KEY UPDATE `name` = `new`.`name`"
        )
    );

    // `insert::returning(…)` does not exist on this dialect. Uncommenting the
    // next line is a compile error -- which is the point:
    //
    //     insert::returning(quote("id"))
    //     ^^^^^^^^^^^^^^^^^ not found in `keelson::mysql::insert`
    //
    // A generated MySQL model has no `update(…).all()` for the same reason:
    // it cannot return the updated rows, and pretending otherwise would mean
    // a second query the caller did not ask for.
    Ok(())
}

/// SQLite: PostgreSQL's `ON CONFLICT` spelling, `?n` placeholders, and
/// `RETURNING` since 3.35.
fn upsert_sqlite() -> keelson::Result<()> {
    use keelson::sqlite::{self, arg, insert, quote};

    let q = sqlite::insert((
        insert::into(quote("users")).columns(["email", "name"]),
        insert::values((arg("ada@example.com"), arg("Ada"))),
        insert::on_conflict(quote("email")).do_update((
            insert::set_excluded(["name"]),
            // The `where_` inside `do_update` filters which conflicting rows
            // get updated -- as opposed to `on_conflict(…).where_(…)`, which
            // names a *partial index*. Two different clauses, two different
            // places, because SQLite has two.
            insert::where_(quote(("users", "is_active")).eq(arg(true))),
        )),
        insert::returning(quote("id")),
    ));
    let (sql, args) = q.build()?;
    show("SQLite upsert", &sql, &args);
    assert!(sql.contains("ON CONFLICT (\"email\") DO UPDATE SET"));
    assert!(sql.ends_with("RETURNING \"id\""));
    assert_eq!(args.len(), 3);
    Ok(())
}

/// `MERGE` -- PostgreSQL 15's statement, and a statement type of its own.
/// Neither of the other two dialects has one, so neither has `merge`.
fn merge_postgres() -> keelson::Result<()> {
    use keelson::psql::{self, arg, merge, quote};

    let q = psql::merge((
        merge::into(quote("users")).as_("t"),
        merge::using(quote("staging_users")).as_("s"),
        merge::on(quote(("t", "email")).eq(quote(("s", "email")))),
        merge::when_matched()
            .and(quote(("s", "name")).is_not_null())
            .then_update(merge::set_col("name").to(quote(("s", "name")))),
        merge::when_not_matched()
            .then_insert()
            .columns(["email", "name"])
            .values((quote(("s", "email")), quote(("s", "name")))),
        merge::when_matched().then_delete(),
        merge::returning(arg(1)),
    ));
    let (sql, args) = q.build()?;
    show("PostgreSQL MERGE", &sql, &args);
    assert!(sql.starts_with("MERGE INTO \"users\" AS \"t\" USING \"staging_users\" AS \"s\" ON"));
    Ok(())
}

/// "Replace the row wholesale" -- two engines, two grammars, and no attempt to
/// make them look alike.
fn replace_variants() -> keelson::Result<()> {
    {
        use keelson::sqlite::{self, arg, insert, quote};
        let q = sqlite::insert((
            insert::or_replace(),
            insert::into(quote("tags")).columns(["id", "name"]),
            insert::values((arg(1), arg("rust"))),
        ));
        let (sql, _) = q.build()?;
        show("SQLite INSERT OR REPLACE", &sql, &[]);
        assert!(sql.starts_with("INSERT OR REPLACE INTO"));
    }
    {
        // MySQL spells it as a statement, not a modifier -- so keelson gives
        // it a starter of its own rather than a flag on `insert`.
        use keelson::mysql::{self, arg, quote, replace};
        let q = mysql::replace((
            replace::into(quote("tags")).columns(["id", "name"]),
            replace::values((arg(1), arg("rust"))),
        ));
        let (sql, _) = q.build()?;
        show("MySQL REPLACE", &sql, &[]);
        assert!(sql.starts_with("REPLACE INTO"));
    }
    Ok(())
}
