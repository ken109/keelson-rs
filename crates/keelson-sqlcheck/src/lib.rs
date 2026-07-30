//! Validates generated SQL against each dialect's real grammar.
//!
//! This is the independent judge keelson's tests check against. Asserting a SQL
//! string we authored ourselves only proves the builder agrees with our own
//! understanding — if we misread the grammar, the implementation and the expected
//! string are wrong together and the test still passes. Running the output through
//! the dialect's actual parser is what rules that out.
//!
//! There are two tiers, answering different questions.
//!
//! # Tier 1 — the grammars (this module, always available)
//!
//! Catalog-free syntax checks, so they are fast and need no infrastructure.
//!
//! | dialect | backend | what it actually is |
//! | ------- | ------- | ------------------- |
//! | psql | [`pg_query`] | bundles libpg_query — the PostgreSQL server's own parser source |
//! | sqlite | [`sqlite3_parser`] | `lemon-rs`: SQLite's `parse.y` and lexer ported C→Rust, synced 2026-04 |
//! | mysql | [`sqlparser`] | a *generic* SQL parser wearing a MySQL dialect |
//!
//! Trust follows from what each one is, and is measured in the tests rather than
//! assumed. psql is as good as PostgreSQL for syntax. sqlite is a port of the real
//! grammar, so close but able to drift. **MySQL is advisory only**: it accepts
//! PostgreSQL-only `DISTINCT ON` *and* rejects valid multi-table
//! `UPDATE a, b SET …`. Wrong in both directions.
//!
//! # Tier 2 — real engines ([`live`], behind the `live` feature)
//!
//! No grammar can catch a *semantic* error. `PREPARE` on a real engine parses and
//! analyses: it resolves tables and columns, checks set-operation arity, and
//! enforces rules a grammar cannot express. [`live::check_sqlite`] rejects five
//! statements that Tier 1 happily accepts — see its tests.
//!
//! The cost is that the analyser resolves names, so statements must refer to real
//! tables. That is what the shared schema in `tests/schema/` is for, and why
//! grammar tests name its tables rather than inventing their own.

pub mod live;

/// Which grammar to validate against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Psql,
    Mysql,
    Sqlite,
}

impl Dialect {
    /// Parse the name used in the golden fixtures.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "psql" => Some(Self::Psql),
            "mysql" => Some(Self::Mysql),
            "sqlite" => Some(Self::Sqlite),
            _ => None,
        }
    }

    /// How authoritative this dialect's parser is.
    pub fn is_authoritative(self) -> bool {
        matches!(self, Self::Psql | Self::Sqlite)
    }
}

/// Check `sql` against a dialect's grammar, returning the parse error if any.
pub fn check(dialect: Dialect, sql: &str) -> Result<(), String> {
    match dialect {
        Dialect::Psql => check_psql(sql),
        Dialect::Mysql => check_mysql(sql),
        Dialect::Sqlite => check_sqlite(sql),
    }
}

/// Validate against libpg_query, the parser PostgreSQL itself uses.
pub fn check_psql(sql: &str) -> Result<(), String> {
    pg_query::parse(sql).map(|_| ()).map_err(|e| e.to_string())
}

/// Validate against SQLite's grammar.
pub fn check_sqlite(sql: &str) -> Result<(), String> {
    use sqlite3_parser::{Bump, FallibleIterator, lexer::sql::Parser};

    // The parser allocates its AST into an arena that has to outlive it. Only the
    // accept/reject answer matters here, so both are dropped immediately after.
    let arena = Bump::new();
    let mut parser = Parser::new(&arena, sql.as_bytes());

    loop {
        match parser.next() {
            Ok(None) => return Ok(()),
            Ok(Some(_)) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// Validate against a generic SQL parser configured for MySQL.
pub fn check_mysql(sql: &str) -> Result<(), String> {
    sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::MySqlDialect {}, sql)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Assert that `sql` parses.
///
/// # Panics
/// With the parser's own diagnostic when it does not.
#[track_caller]
pub fn assert_valid(dialect: Dialect, sql: &str) {
    if let Err(e) = check(dialect, sql) {
        panic!("{dialect:?} rejected the generated SQL\n  error: {e}\n  sql:   {sql}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Placeholders must survive the parser: our SQL is full of them, and a parser
    // that choked on `$1` would make the oracle useless.
    #[test]
    fn accepts_placeholders() {
        assert_valid(Dialect::Psql, r#"SELECT * FROM users WHERE "id" = $1 AND age > $2"#);
        assert_valid(Dialect::Sqlite, r#"SELECT * FROM users WHERE "id" = ?1"#);
        assert_valid(Dialect::Mysql, "SELECT * FROM users WHERE `id` = ?");
    }

    #[test]
    fn accepts_the_intricate_constructs_we_care_about() {
        assert_valid(
            Dialect::Psql,
            r#"SELECT DISTINCT ON (id) status,
               LEAD(created_date, 1, NOW()) OVER (PARTITION BY presale_id ORDER BY created_date) - created_date AS "difference"
               FROM presales GROUP BY status HAVING count(1) > $1
               ORDER BY status COLLATE "bg-BG-x-icu" ASC
               FOR UPDATE OF presales SKIP LOCKED"#,
        );
        assert_valid(
            Dialect::Psql,
            r#"WITH RECURSIVE c(id, data) AS (SELECT id, data FROM t) SELECT * FROM c"#,
        );
        assert_valid(
            Dialect::Psql,
            r#"INSERT INTO users (id) VALUES ($1) ON CONFLICT (id) DO UPDATE SET email = EXCLUDED.email RETURNING *"#,
        );
        assert_valid(
            Dialect::Sqlite,
            r#"INSERT INTO users ("id") VALUES (?1) ON CONFLICT ("id") DO NOTHING RETURNING *"#,
        );
        assert_valid(
            Dialect::Mysql,
            "INSERT INTO `users` (`id`) VALUES (?) ON DUPLICATE KEY UPDATE `email` = VALUES(`email`)",
        );
    }

    // The point of the oracle is that it says no. A validator that accepts
    // everything is worse than none at all, because it reads as a passing check.
    #[test]
    fn rejects_malformed_sql() {
        let bad_psql = [
            "SELECT FROM",                          // missing select list
            "SELECT * FORM users",                  // typo'd keyword
            "SELECT * FROM users WHERE",            // dangling WHERE
            "SELECT * FROM users GROUP BY",         // dangling GROUP BY
            "SELECT * FROM users ORDER BY id ASC DESC", // contradictory direction
            "SELECT (1 + FROM users",               // unbalanced paren
            "INSERT INTO users VALUES",             // dangling VALUES
        ];
        for sql in bad_psql {
            assert!(
                check_psql(sql).is_err(),
                "libpg_query accepted malformed SQL: {sql:?}"
            );
        }

        for sql in ["SELECT FROM", "SELECT * FORM users", "SELECT (1 + FROM users"] {
            assert!(
                check_sqlite(sql).is_err(),
                "sqlite3-parser accepted malformed SQL: {sql:?}"
            );
            assert!(
                check_mysql(sql).is_err(),
                "sqlparser accepted malformed SQL: {sql:?}"
            );
        }
    }

    // A subtler class: syntactically plausible but wrong for *this* dialect.
    // These are the ones a hand-written expected string happily agrees with.
    #[test]
    fn psql_rejects_foreign_syntax() {
        // Backticks are MySQL's identifier quoting, not PostgreSQL's.
        assert!(check_psql("SELECT `id` FROM users").is_err());
    }

    /// Pins how far the MySQL backend can be trusted.
    ///
    /// `sqlparser` is a generic parser wearing a MySQL hat, not MySQL's own
    /// grammar, and it is permissive: it accepts `DISTINCT ON`, which is
    /// PostgreSQL-only. So a MySQL pass is weak evidence and a MySQL *failure* is
    /// the informative direction.
    ///
    /// This is asserted rather than merely documented so that swapping in a
    /// stricter parser breaks the test and tells us we may raise our confidence.
    #[test]
    fn mysql_backend_is_known_permissive() {
        assert!(
            check_mysql("SELECT DISTINCT ON (id) id FROM users").is_ok(),
            "the MySQL parser got stricter — revisit Dialect::is_authoritative"
        );
        assert!(!Dialect::Mysql.is_authoritative());
        assert!(Dialect::Psql.is_authoritative());
        assert!(Dialect::Sqlite.is_authoritative());
    }
}
