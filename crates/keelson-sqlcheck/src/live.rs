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

/// The PostgreSQL image the engine tier runs against.
#[cfg(feature = "live-docker")]
pub const PSQL_IMAGE_TAG: &str = "17-alpine";

/// The MySQL image the engine tier runs against.
#[cfg(feature = "live-docker")]
pub const MYSQL_IMAGE_TAG: &str = "8.4";

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

/// Check `sql` against a real PostgreSQL, with the shared schema applied.
///
/// `Client::prepare` sends a Parse message, so the server runs its full
/// parse-and-analyse pass — the same work `PREPARE` does.
///
/// The container is started once per test binary and deliberately leaked, since it
/// must outlive every test and the process is about to exit anyway.
#[cfg(feature = "live-docker")]
pub fn check_psql(sql: &str) -> Result<(), String> {
    use std::sync::Mutex;

    use testcontainers::{ImageExt as _, runners::SyncRunner};

    static PG: OnceLock<Mutex<postgres::Client>> = OnceLock::new();

    let client = PG.get_or_init(|| {
        // Pinned deliberately: testcontainers-modules still defaults to
        // postgres:11-alpine, which is long EOL and rejects syntax we support. A
        // judge running an ancient server would report false failures.
        let container = testcontainers_modules::postgres::Postgres::default()
            .with_tag(PSQL_IMAGE_TAG)
            .start()
            .expect("starting the PostgreSQL container (is Docker running?)");
        let port = container
            .get_host_port_ipv4(5432)
            .expect("mapped PostgreSQL port");

        let mut client = postgres::Client::connect(
            &format!("host=127.0.0.1 port={port} user=postgres password=postgres dbname=postgres"),
            postgres::NoTls,
        )
        .expect("connecting to the PostgreSQL container");
        client
            .batch_execute(schema_sql(super::Dialect::Psql))
            .expect("the shared psql schema must apply cleanly");

        // Keep the container alive for the process; dropping it would stop it.
        Box::leak(Box::new(container));
        Mutex::new(client)
    });

    let mut client = client.lock().expect("PostgreSQL client mutex poisoned");
    client.prepare(sql).map(|_| ()).map_err(|e| e.to_string())
}

/// Check `sql` against a real MySQL, with the shared schema applied.
///
/// This is the important one: MySQL's Tier 1 backend is a generic parser that is
/// wrong in both directions, so the server is the only source of truth we have for
/// this dialect. `Conn::prep` issues a server-side `PREPARE`.
#[cfg(feature = "live-docker")]
pub fn check_mysql(sql: &str) -> Result<(), String> {
    use std::sync::Mutex;

    use mysql::prelude::Queryable as _;
    use testcontainers::{ImageExt as _, runners::SyncRunner};

    static MY: OnceLock<Mutex<mysql::Conn>> = OnceLock::new();

    let conn = MY.get_or_init(|| {
        let container = testcontainers_modules::mysql::Mysql::default()
            .with_tag(MYSQL_IMAGE_TAG)
            .start()
            .expect("starting the MySQL container (is Docker running?)");
        let port = container.get_host_port_ipv4(3306).expect("mapped MySQL port");

        let url = format!("mysql://root@127.0.0.1:{port}/test");
        let mut conn = mysql::Conn::new(mysql::Opts::from_url(&url).expect("MySQL url"))
            .expect("connecting to the MySQL container");
        for stmt in schema_sql(super::Dialect::Mysql).split(';') {
            if stmt.trim().is_empty() {
                continue;
            }
            conn.query_drop(stmt)
                .expect("the shared mysql schema must apply cleanly");
        }

        Box::leak(Box::new(container));
        Mutex::new(conn)
    });

    let mut conn = conn.lock().expect("MySQL connection mutex poisoned");
    conn.prep(sql).map(|_| ()).map_err(|e| e.to_string())
}

/// Check `sql` against the real engine for `dialect`.
#[cfg_attr(not(feature = "live"), allow(unused_variables))]
pub fn check(dialect: super::Dialect, sql: &str) -> Result<(), String> {
    match dialect {
        #[cfg(feature = "live")]
        super::Dialect::Sqlite => check_sqlite(sql),
        #[cfg(feature = "live-docker")]
        super::Dialect::Psql => check_psql(sql),
        #[cfg(feature = "live-docker")]
        super::Dialect::Mysql => check_mysql(sql),
        #[allow(unreachable_patterns)]
        d => Err(format!("no live engine compiled in for {d:?}")),
    }
}

/// Assert that a real engine accepts `sql`.
///
/// # Panics
/// With the engine's own diagnostic when it does not.
#[track_caller]
pub fn assert_valid(dialect: super::Dialect, sql: &str) {
    if let Err(e) = check(dialect, sql) {
        panic!("real {dialect:?} rejected the generated SQL\n  error: {e}\n  sql:   {sql}");
    }
}

/// Which engines this build can reach.
pub fn available() -> &'static [super::Dialect] {
    static AVAILABLE: OnceLock<Vec<super::Dialect>> = OnceLock::new();
    AVAILABLE.get_or_init(|| {
        let mut v = Vec::new();
        if cfg!(feature = "live") {
            v.push(super::Dialect::Sqlite);
        }
        if cfg!(feature = "live-docker") {
            v.push(super::Dialect::Psql);
            v.push(super::Dialect::Mysql);
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
