//! Verifies the real-engine tier, and calibrates the grammar tier against it.
//!
//! Requires Docker: `cargo test -p keelson-sqlcheck --features live-docker`.
//!
//! The calibration tests are the point. Tier 1's trust level was previously argued
//! from what each backend *is*; here it is measured against the engine that
//! actually decides.

#![cfg(feature = "live-docker")]

use keelson_sqlcheck::{Dialect, check as grammar, live};

#[test]
fn psql_schema_applies_and_ordinary_sql_prepares() {
    live::assert_valid(Dialect::Psql, "SELECT id, name FROM users WHERE age >= $1");
    live::assert_valid(
        Dialect::Psql,
        "SELECT u.name, count(p.id) FROM users u LEFT JOIN posts p ON p.user_id = u.id GROUP BY u.name HAVING count(p.id) > $1",
    );
    live::assert_valid(
        Dialect::Psql,
        "INSERT INTO users (id, name) VALUES ($1, $2) ON CONFLICT (id) DO UPDATE SET email = EXCLUDED.email RETURNING id",
    );
    live::assert_valid(
        Dialect::Psql,
        "WITH RECURSIVE t(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM t WHERE n < 10) SELECT n FROM t",
    );
}

#[test]
fn mysql_schema_applies_and_ordinary_sql_prepares() {
    live::assert_valid(Dialect::Mysql, "SELECT id, name FROM users WHERE age >= ?");
    live::assert_valid(
        Dialect::Mysql,
        "INSERT INTO users (id, name) VALUES (?, ?) ON DUPLICATE KEY UPDATE email = VALUES(email)",
    );
    live::assert_valid(
        Dialect::Mysql,
        "SELECT u.name FROM users u INNER JOIN posts p ON p.user_id = u.id ORDER BY u.name LIMIT ?",
    );
}

/// The semantic errors a grammar cannot see, now on the server engines.
#[test]
fn engines_catch_what_grammars_cannot() {
    let psql_semantic = [
        "SELECT missing_column FROM users",
        "SELECT id FROM no_such_table",
        "SELECT id FROM users UNION SELECT id, title FROM posts",
        "SELECT id FROM users WHERE count(*) > 1",
    ];
    for sql in psql_semantic {
        assert!(
            grammar(Dialect::Psql, sql).is_ok(),
            "expected libpg_query to accept this, leaving it to the engine: {sql}"
        );
        assert!(
            live::check(Dialect::Psql, sql).is_err(),
            "real PostgreSQL accepted a semantic error: {sql}"
        );
    }

    let mysql_semantic = [
        "SELECT missing_column FROM users",
        "SELECT id FROM no_such_table",
        "SELECT id FROM users UNION SELECT id, title FROM posts",
    ];
    for sql in mysql_semantic {
        assert!(
            live::check(Dialect::Mysql, sql).is_err(),
            "real MySQL accepted a semantic error: {sql}"
        );
    }
}

/// Confirms the false negative that the imported-fixture calibration first exposed.
///
/// `sqlparser` rejects MySQL's multi-table `UPDATE a, b SET …` as a syntax error.
/// Real MySQL accepts it, so the fault was ours, not the SQL's — which is exactly
/// why the MySQL grammar backend is advisory only.
#[test]
fn real_mysql_accepts_what_our_grammar_backend_rejects() {
    let sql = "UPDATE users u, posts p SET u.name = ?, p.title = ? WHERE p.user_id = u.id";

    assert!(
        grammar(Dialect::Mysql, sql).is_err(),
        "the MySQL grammar backend got smarter — revisit Dialect::is_authoritative"
    );
    live::assert_valid(Dialect::Mysql, sql);
}

/// Confirms the other direction of the same weakness.
///
/// `sqlparser` accepts PostgreSQL-only `DISTINCT ON` under its MySQL dialect. Real
/// MySQL rejects it. So a MySQL grammar pass proves nothing.
#[test]
fn real_mysql_rejects_what_our_grammar_backend_accepts() {
    let sql = "SELECT DISTINCT ON (id) id FROM users";

    assert!(
        grammar(Dialect::Mysql, sql).is_ok(),
        "the MySQL grammar backend got stricter — revisit Dialect::is_authoritative"
    );
    assert!(
        live::check(Dialect::Mysql, sql).is_err(),
        "real MySQL accepted PostgreSQL-only DISTINCT ON"
    );
}

/// Sanity: PostgreSQL's grammar backend is libpg_query, so on *syntax* it should
/// never disagree with the server. A disagreement here would mean the crate's
/// bundled parser has drifted from the server version we run.
#[test]
fn psql_grammar_and_engine_agree_on_syntax() {
    let syntactically_bad = [
        "SELECT FROM",
        "SELECT * FORM users",
        "SELECT * FROM users WHERE",
        "SELECT (1 + FROM users",
    ];
    for sql in syntactically_bad {
        assert!(
            grammar(Dialect::Psql, sql).is_err(),
            "grammar accepted: {sql}"
        );
        assert!(
            live::check(Dialect::Psql, sql).is_err(),
            "engine accepted: {sql}"
        );
    }
}
