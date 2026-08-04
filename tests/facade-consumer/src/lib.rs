//! What a new reader writes, compiled the way they will compile it.
//!
//! This crate depends on `keelson` and nothing else, so every path the derives
//! emit has to resolve through the facade's re-exports. That is the one thing
//! the rest of the workspace cannot check: see this package's `Cargo.toml`.
//!
//! Nothing here is clever on purpose. It is the README's worked example and the
//! two derives it advertises, and its only job is to still compile.

#![allow(dead_code)]

use keelson::sqlite::{self, Chain as _, Query as _, arg, insert, quote, select};
use keelson::{Bind, FromRow};

/// `#[derive(FromRow)]` emits keelson-exec's `FromRow`, `Row` and `ExecError`.
/// A facade-only dependant reaches them as `keelson::exec`.
#[derive(Debug, PartialEq, FromRow)]
struct Crew {
    id: i64,
    name: String,
}

/// A generic struct is the case where `FromRow` also has to name keelson-core,
/// for the `where` clause it writes out.
#[derive(FromRow)]
struct Wrapper<T> {
    value: T,
}

/// `#[derive(Bind)]` emits keelson-core's `ToValue`, `FromValue`, `Value` and
/// `Error` — reached as `keelson::core` from here.
#[derive(Bind)]
struct CrewId(i64);

/// `#[keelson(flatten)]` makes the derive name `FromRow` on a field type, which
/// is a different emission path from a plain column read.
#[derive(FromRow)]
struct WithFlattened {
    id: i64,
    #[keelson(flatten)]
    crew: Crew,
}

/// `#[keelson(rename = "…")]` reads a column whose name differs from the field.
#[derive(FromRow)]
struct Renamed {
    #[keelson(rename = "name")]
    display_name: String,
}

/// The builder half needs no macro at all, but it is what the reader writes
/// around the derives, so it belongs in the same compile.
fn build_a_select() -> keelson::Result<(String, usize)> {
    let only_adults = Some(select::where_(quote("age").gte(arg(21))));
    let q = sqlite::select((
        select::columns((quote("id"), quote("name"))),
        select::from(quote("crew")),
        only_adults,
        select::where_("name IS NOT NULL"),
        select::order_by(quote("id")),
    ));
    let (sql, args) = q.build()?;
    Ok((sql, args.len()))
}

/// Two spellings of the same table, to keep the difference honest: a bare
/// `&str` is raw SQL and goes through untouched, `quote(…)` is an identifier
/// and the dialect decides how to quote it.
fn build_an_insert() -> keelson::Result<(String, String)> {
    let raw = sqlite::insert((
        insert::into("crew").columns(["id", "name"]),
        insert::values((arg(1), arg("Ada"))),
    ));
    let quoted = sqlite::insert((
        insert::into(quote("crew")).columns(["id", "name"]),
        insert::values((arg(1), arg("Ada"))),
    ));
    Ok((raw.build()?.0, quoted.build()?.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_readme_statement_still_renders() {
        let (sql, args) = build_a_select().expect("builds");
        assert_eq!(
            sql,
            r#"SELECT "id", "name" FROM "crew" WHERE ("age" >= ?1) AND name IS NOT NULL ORDER BY "id""#
        );
        assert_eq!(args, 1);
    }

    #[test]
    fn an_insert_still_renders() {
        let (raw, quoted) = build_an_insert().expect("builds");
        assert_eq!(raw, r#"INSERT INTO crew ("id", "name") VALUES (?1, ?2)"#);
        assert_eq!(
            quoted,
            r#"INSERT INTO "crew" ("id", "name") VALUES (?1, ?2)"#
        );
    }

    /// Rows decode through the derived impl, against a real database, so the
    /// emitted paths are exercised and not merely type-checked.
    #[tokio::test]
    async fn a_derived_row_decodes() -> Result<(), Box<dyn std::error::Error>> {
        use keelson::exec::{Execute as _, Executor as _, Statement};
        use keelson::sqlx::sqlite::Pool;

        let db = Pool::connect("sqlite::memory:").await?;
        db.execute(Statement::new(
            "CREATE TABLE crew (id INTEGER PRIMARY KEY, name TEXT NOT NULL, age INTEGER)",
            vec![],
        ))
        .await?;
        sqlite::insert((
            insert::into("crew").columns(["id", "name", "age"]),
            insert::values((arg(1), arg("Ada"), arg(36))),
        ))
        .execute(&db)
        .await?;

        let crew: Vec<Crew> = sqlite::select((
            select::columns((quote("id"), quote("name"))),
            select::from(quote("crew")),
        ))
        .fetch_all(&db)
        .await?;

        assert_eq!(
            crew,
            vec![Crew {
                id: 1,
                name: "Ada".into()
            }]
        );
        Ok(())
    }
}
