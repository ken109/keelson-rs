//! Validation against real database engines, behind the `live` feature.
//!
//! The grammar parsers in the parent module answer "is this syntactically legal".
//! A real engine answers a strictly harder question, because `PREPARE` runs the
//! parser *and* the analyser: it resolves tables and columns, checks the shape of
//! set operations, and enforces rules that are not expressible in a grammar.
//!
//! ```text
//! SELECT * FROM users WHERE count(*) > 1     -- aggregate in WHERE
//! SELECT id FROM users UNION SELECT id, name FROM posts  -- arity mismatch
//! SELECT missing FROM users                  -- unknown column
//! ```
//!
//! Every one of those parses cleanly and is still wrong. That gap is why this
//! module exists.
//!
//! Because the analyser resolves names, statements must refer to real tables —
//! hence the shared schema in `tests/schema/`. Grammar tests name those tables so
//! the same SQL can be checked at both levels.

use std::sync::OnceLock;

/// The shared schema for a dialect, as SQL statements.
pub fn schema_sql(dialect: super::Dialect) -> &'static str {
    // Baked in at compile time so a test binary needs no working directory.
    match dialect {
        super::Dialect::Psql => include_str!("../../../tests/schema/psql.sql"),
        super::Dialect::Mysql => include_str!("../../../tests/schema/mysql.sql"),
        super::Dialect::Sqlite => include_str!("../../../tests/schema/sqlite.sql"),
    }
}

/// Check `sql` against a real SQLite, with the shared schema applied.
///
/// SQLite is a library rather than a server, so this needs no Docker and is cheap
/// enough to run alongside the grammar checks. `Connection::prepare` performs the
/// full parse and name resolution.
#[cfg(feature = "live")]
pub fn check_sqlite(sql: &str) -> Result<(), String> {
    thread_local! {
        static CONN: rusqlite::Connection = {
            let conn = rusqlite::Connection::open_in_memory()
                .expect("opening an in-memory SQLite database cannot fail");
            conn.execute_batch(schema_sql(super::Dialect::Sqlite))
                .expect("the shared SQLite schema must apply cleanly");
            conn
        };
    }

    CONN.with(|conn| conn.prepare(sql).map(|_| ()).map_err(|e| e.to_string()))
}

/// Which engines this build can reach.
pub fn available() -> &'static [super::Dialect] {
    static AVAILABLE: OnceLock<Vec<super::Dialect>> = OnceLock::new();
    AVAILABLE.get_or_init(|| {
        let mut v = Vec::new();
        if cfg!(feature = "live") {
            v.push(super::Dialect::Sqlite);
        }
        v
    })
}

#[cfg(all(test, feature = "live"))]
mod tests {
    use super::*;
    use crate::{Dialect, check};

    #[test]
    fn schema_applies_and_ordinary_sql_prepares() {
        check_sqlite("SELECT id, name FROM users WHERE age >= ?1").unwrap();
        check_sqlite("SELECT u.name, count(p.id) FROM users u LEFT JOIN posts p ON p.user_id = u.id GROUP BY u.name")
            .unwrap();
        check_sqlite("INSERT INTO users (id, name) VALUES (?1, ?2) ON CONFLICT (id) DO NOTHING RETURNING id")
            .unwrap();
    }

    /// The whole justification for this module: cases the grammar accepts and the
    /// engine rejects. If this test ever passes trivially, the extra tier is
    /// buying nothing and should be dropped.
    #[test]
    fn catches_what_the_grammar_cannot() {
        let semantic_errors = [
            ("unknown column", "SELECT missing_column FROM users"),
            ("unknown table", "SELECT id FROM no_such_table"),
            (
                "set operation arity mismatch",
                "SELECT id FROM users UNION SELECT id, title FROM posts",
            ),
            (
                "aggregate in WHERE",
                "SELECT id FROM users WHERE count(*) > 1",
            ),
            (
                "column count mismatch on insert",
                "INSERT INTO tags (id, name) VALUES (1)",
            ),
        ];

        for (what, sql) in semantic_errors {
            assert!(
                check(Dialect::Sqlite, sql).is_ok(),
                "expected the grammar to accept {what}, so that the engine check is \
                 what catches it — if the grammar now rejects it, move this case: {sql}"
            );
            assert!(
                check_sqlite(sql).is_err(),
                "real SQLite accepted {what}, which it should not: {sql}"
            );
        }
    }

    #[test]
    fn placeholders_survive_preparation() {
        check_sqlite("SELECT * FROM users WHERE id = ?1 AND age > ?2").unwrap();
        check_sqlite("SELECT * FROM users WHERE id = ?").unwrap();
    }
}
