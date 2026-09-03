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
//! enforces rules a grammar cannot express. `live::check_sqlite` rejects five
//! statements that Tier 1 happily accepts — see its tests.
//!
//! The cost is that the analyser resolves names, so statements must refer to real
//! tables. That is what the shared schema in `tests/schema/` is for, and why
//! grammar tests name its tables rather than inventing their own.

//! # A dialect for crates that have none (`testing`, behind the `testing` feature)
//!
//! Rendering needs a `keelson_core::Dialect`, and `keelson-core` cannot depend on
//! a dialect crate because every dialect crate depends on it. `testing::PgLike`
//! is a stand-in that lives here instead, where the dependency is a
//! dev-dependency cycle Cargo allows.

/// The round-trip suite every execution backend has to pass.
#[cfg(feature = "conformance")]
pub mod conformance;
pub mod coverage;
pub mod live;
pub mod record;
#[cfg(feature = "testing")]
pub mod testing;

pub use record::record;

/// Collapse insignificant whitespace so an expected string can be written
/// readably without pinning the builder's exact line breaks.
///
/// Trim, then collapse every run of ASCII whitespace to one space. That is all —
/// tokens and their order are pinned, formatting is free.
///
/// Whitespace inside string literals is left alone by virtue of only collapsing
/// runs, but a literal containing a newline would still be altered; no test needs
/// one, and a test that does should compare the raw string instead.
pub fn normalize(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut in_space = false;
    for ch in sql.trim().chars() {
        if ch.is_ascii_whitespace() {
            in_space = true;
            continue;
        }
        if in_space && !out.is_empty() {
            out.push(' ');
        }
        in_space = false;
        out.push(ch);
    }
    out
}

/// Which grammar to validate against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Psql,
    Mysql,
    Sqlite,
}

impl Dialect {
    /// Parse a dialect name as written in configuration or test data.
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

    /// The name [`from_name`](Self::from_name) parses — the recording tag.
    pub fn name(self) -> &'static str {
        match self {
            Self::Psql => "psql",
            Self::Mysql => "mysql",
            Self::Sqlite => "sqlite",
        }
    }
}

/// Check `sql` against a dialect's grammar, returning the parse error if any.
///
/// On success the string is also [`record()`]ed when Tier D recording is on
/// (`KEELSON_SQLCHECK_RECORD`); a run without the variable pays one atomic
/// read for this line.
pub fn check(dialect: Dialect, sql: &str) -> Result<(), String> {
    // Each leaf checker records for itself, so `check` adds nothing here and a
    // string is recorded once however it arrives.
    match dialect {
        Dialect::Psql => check_psql(sql),
        Dialect::Mysql => check_mysql(sql),
        Dialect::Sqlite => check_sqlite(sql),
    }
}

/// Validate against libpg_query, the parser PostgreSQL itself uses.
pub fn check_psql(sql: &str) -> Result<(), String> {
    let result = pg_query::parse(sql).map(|_| ()).map_err(|e| e.to_string());
    if result.is_ok() {
        record(Dialect::Psql, sql);
    }
    result
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
            Ok(None) => {
                record(Dialect::Sqlite, sql);
                return Ok(());
            }
            Ok(Some(_)) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// Validate against a generic SQL parser configured for MySQL.
pub fn check_mysql(sql: &str) -> Result<(), String> {
    let result = sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::MySqlDialect {}, sql)
        .map(|_| ())
        .map_err(|e| e.to_string());
    if result.is_ok() {
        record(Dialect::Mysql, sql);
    }
    result
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

/// The assertion every builder test should make: valid, and what we meant.
///
/// Runs all the checking available in this build in one call, so a test cannot
/// accidentally perform only some of it:
///
/// 1. the dialect's grammar accepts `produced`;
/// 2. a real engine accepts it too, when one is compiled in ([`live::available`]);
/// 3. `produced` equals `expected` once [`normalize`]d.
///
/// Steps 1 and 2 answer "is this valid SQL". Only step 3 answers "is this the SQL
/// we meant", and it is worth no more than the provenance of `expected` — derive
/// that from the dialect's grammar, never by pasting whatever the builder happened
/// to emit, or the test degenerates into asserting that the code equals itself.
///
/// # Panics
/// On the first check that fails, naming which one it was.
#[track_caller]
pub fn assert_sql(dialect: Dialect, produced: &str, expected: &str) {
    assert_valid(dialect, produced);

    if live::available().contains(&dialect) {
        live::assert_valid(dialect, produced);
    }

    let got = normalize(produced);
    let want = normalize(expected);
    assert!(
        got == want,
        "{dialect:?} SQL is valid but not what was expected\n  expected: {want}\n  actual:   {got}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_collapses_whitespace_only() {
        assert_eq!(
            normalize("  SELECT\n\tid  FROM users  "),
            "SELECT id FROM users"
        );
        // Parentheses are left exactly as emitted. A normaliser that padded them
        // would hide the very formatting choices docs/sql-rendering.md records.
        assert_eq!(normalize("NOW()"), "NOW()");
        assert_eq!(normalize("a || b"), "a || b");
    }

    // Placeholders must survive the parser: our SQL is full of them, and a parser
    // that choked on `$1` would make the oracle useless.
    #[test]
    fn accepts_placeholders() {
        assert_valid(
            Dialect::Psql,
            r#"SELECT * FROM users WHERE "id" = $1 AND age > $2"#,
        );
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
            "SELECT FROM",                              // missing select list
            "SELECT * FORM users",                      // typo'd keyword
            "SELECT * FROM users WHERE",                // dangling WHERE
            "SELECT * FROM users GROUP BY",             // dangling GROUP BY
            "SELECT * FROM users ORDER BY id ASC DESC", // contradictory direction
            "SELECT (1 + FROM users",                   // unbalanced paren
            "INSERT INTO users VALUES",                 // dangling VALUES
        ];
        for sql in bad_psql {
            assert!(
                check_psql(sql).is_err(),
                "libpg_query accepted malformed SQL: {sql:?}"
            );
        }

        for sql in [
            "SELECT FROM",
            "SELECT * FORM users",
            "SELECT (1 + FROM users",
        ] {
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

#[cfg(all(test, feature = "live"))]
mod assert_sql_tests {
    use super::*;

    #[test]
    fn passes_when_valid_and_matching() {
        assert_sql(
            Dialect::Sqlite,
            "SELECT   id,\n  name\nFROM users",
            "SELECT id, name FROM users",
        );
    }

    #[test]
    #[should_panic(expected = "rejected the generated SQL")]
    fn fails_on_invalid_syntax_even_if_expected_matches() {
        assert_sql(Dialect::Sqlite, "SELECT FORM users", "SELECT FORM users");
    }

    #[test]
    #[should_panic(expected = "real Sqlite rejected")]
    fn fails_on_semantic_error_even_if_expected_matches() {
        // Valid syntax, unknown column. The string comparison alone would pass,
        // which is exactly the hole the engine tier closes.
        assert_sql(
            Dialect::Sqlite,
            "SELECT nope FROM users",
            "SELECT nope FROM users",
        );
    }

    #[test]
    #[should_panic(expected = "not what was expected")]
    fn fails_when_valid_but_different_from_intent() {
        assert_sql(
            Dialect::Sqlite,
            "SELECT id FROM users WHERE id = 1 AND (name = 'a' OR name = 'b')",
            "SELECT id FROM users WHERE (id = 1 AND name = 'a') OR name = 'b'",
        );
    }
}
