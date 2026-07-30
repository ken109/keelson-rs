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

/// Set this to a libpq-style URL (`postgresql://user:pass@host:port/db`) to
/// judge against an already-running PostgreSQL instead of starting a container
/// per test binary. See `docs/testing-tiers.md` for how to run one.
#[cfg(feature = "live-docker")]
pub const PSQL_URL_ENV: &str = "KEELSON_LIVE_PSQL_URL";

/// Set this to a `mysql://user@host:port/db` URL to judge against an
/// already-running MySQL instead of starting a container per test binary. The
/// URL must name a database the tests may fill with the shared schema.
#[cfg(feature = "live-docker")]
pub const MYSQL_URL_ENV: &str = "KEELSON_LIVE_MYSQL_URL";

/// Removal of this process's containers when it exits.
///
/// testcontainers 0.27 ships no reaper — the "ryuk" sidecar of the Java and Go
/// implementations does not exist in the Rust port, so a container is removed
/// only by its `Drop` impl (or, with the `watchdog` feature, on a signal).
/// Ours must outlive every test in the binary, so it sits in a `static`, and
/// statics are never dropped — which is exactly how leaked containers used to
/// accumulate, one per engine per test binary. C `atexit` still runs on the
/// harness's normal `process::exit`, so removal happens there instead: by
/// container id, through the `docker` CLI, because no async runtime is alive
/// that late.
#[cfg(feature = "live-docker")]
mod exit_cleanup {
    use std::sync::{Mutex, Once};

    unsafe extern "C" {
        // C89, so present in every libc/CRT Rust links against.
        fn atexit(callback: extern "C" fn()) -> std::ffi::c_int;
    }

    static CONTAINER_IDS: Mutex<Vec<String>> = Mutex::new(Vec::new());

    extern "C" fn remove_registered_containers() {
        let ids = match CONTAINER_IDS.lock() {
            Ok(guard) => guard.clone(),
            // A panic elsewhere cannot have left a Vec<String> invalid; better
            // to take it anyway than to leak the containers.
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        for id in ids {
            let removed = std::process::Command::new("docker")
                .args(["rm", "-f", "-v", &id])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|status| status.success());
            if !removed {
                eprintln!("keelson-sqlcheck: could not remove container {id}; remove it manually");
            }
        }
    }

    /// Remove the container with this id when the process exits normally.
    pub(super) fn remove_at_exit(id: &str) {
        static HOOK: Once = Once::new();
        HOOK.call_once(|| {
            // SAFETY: the callback is a non-unwinding `extern "C" fn`, which is
            // all `atexit` requires; registration is idempotent via `Once`.
            let _ = unsafe { atexit(remove_registered_containers) };
        });
        CONTAINER_IDS
            .lock()
            .expect("container-id registry mutex poisoned")
            .push(id.to_owned());
    }
}

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
/// The container is started once per test binary, held in a `static` so it
/// outlives every test, and removed at process exit by `exit_cleanup`. Set
/// [`PSQL_URL_ENV`] to skip the container and share one server across binaries.
#[cfg(feature = "live-docker")]
pub fn check_psql(sql: &str) -> Result<(), String> {
    use std::sync::Mutex;

    use testcontainers::{ImageExt as _, runners::SyncRunner};

    struct Live {
        client: Mutex<postgres::Client>,
        // Never dropped (statics are not), which is what keeps the container
        // running until process exit; dropping it would remove it mid-run.
        _container: Option<testcontainers::Container<testcontainers_modules::postgres::Postgres>>,
    }

    static PG: OnceLock<Live> = OnceLock::new();

    let live = PG.get_or_init(|| {
        let (url, container) = match std::env::var(PSQL_URL_ENV) {
            Ok(url) => (url, None),
            Err(_) => {
                // Pinned deliberately: testcontainers-modules still defaults to
                // postgres:11-alpine, which is long EOL and rejects syntax we
                // support. A judge running an ancient server would report false
                // failures.
                let container = testcontainers_modules::postgres::Postgres::default()
                    .with_tag(PSQL_IMAGE_TAG)
                    .start()
                    .expect("starting the PostgreSQL container (is Docker running?)");
                let port = container
                    .get_host_port_ipv4(5432)
                    .expect("mapped PostgreSQL port");
                exit_cleanup::remove_at_exit(container.id());
                (
                    format!(
                        "host=127.0.0.1 port={port} user=postgres password=postgres dbname=postgres"
                    ),
                    Some(container),
                )
            }
        };

        let mut client = postgres::Client::connect(&url, postgres::NoTls)
            .expect("connecting to PostgreSQL (container, or the KEELSON_LIVE_PSQL_URL server)");
        ensure_psql_schema(&mut client);
        Live {
            client: Mutex::new(client),
            _container: container,
        }
    });

    let mut client = live
        .client
        .lock()
        .expect("PostgreSQL client mutex poisoned");
    client.prepare(sql).map(|_| ()).map_err(|e| e.to_string())
}

/// Apply the shared psql schema unless it is already there.
///
/// A per-binary container is always fresh, but a server named by
/// [`PSQL_URL_ENV`] outlives many test binaries, so application must be
/// idempotent — and locked, because binaries launched concurrently (e.g. by
/// nextest) would otherwise race to apply it.
#[cfg(feature = "live-docker")]
fn ensure_psql_schema(client: &mut postgres::Client) {
    // Arbitrary fixed key ("keelson" as ASCII); it only needs to be the same in
    // every keelson test process sharing the server.
    const LOCK_KEY: i64 = 0x006b_6565_6c73_6f6e;
    client
        .execute("SELECT pg_advisory_lock($1)", &[&LOCK_KEY])
        .expect("taking the schema advisory lock");
    let present: bool = client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM information_schema.tables \
             WHERE table_schema = current_schema() AND table_name = 'users')",
            &[],
        )
        .expect("probing for the shared schema")
        .get(0);
    if !present {
        client
            .batch_execute(schema_sql(super::Dialect::Psql))
            .expect("the shared psql schema must apply cleanly");
    }
    client
        .execute("SELECT pg_advisory_unlock($1)", &[&LOCK_KEY])
        .expect("releasing the schema advisory lock");
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

    struct Live {
        conn: Mutex<mysql::Conn>,
        // Never dropped (statics are not), which is what keeps the container
        // running until process exit; dropping it would remove it mid-run.
        _container: Option<testcontainers::Container<testcontainers_modules::mysql::Mysql>>,
    }

    static MY: OnceLock<Live> = OnceLock::new();

    let live = MY.get_or_init(|| {
        let (url, container) = match std::env::var(MYSQL_URL_ENV) {
            Ok(url) => (url, None),
            Err(_) => {
                let container = testcontainers_modules::mysql::Mysql::default()
                    .with_tag(MYSQL_IMAGE_TAG)
                    .start()
                    .expect("starting the MySQL container (is Docker running?)");
                let port = container
                    .get_host_port_ipv4(3306)
                    .expect("mapped MySQL port");
                exit_cleanup::remove_at_exit(container.id());
                (
                    format!("mysql://root@127.0.0.1:{port}/test"),
                    Some(container),
                )
            }
        };

        let mut conn = mysql::Conn::new(mysql::Opts::from_url(&url).expect("MySQL url"))
            .expect("connecting to MySQL (container, or the KEELSON_LIVE_MYSQL_URL server)");
        ensure_mysql_schema(&mut conn);
        Live {
            conn: Mutex::new(conn),
            _container: container,
        }
    });

    let mut conn = live.conn.lock().expect("MySQL connection mutex poisoned");
    conn.prep(sql).map(|_| ()).map_err(|e| e.to_string())
}

/// Apply the shared mysql schema unless it is already there.
///
/// Same story as [`ensure_psql_schema`]: a server named by [`MYSQL_URL_ENV`]
/// outlives many test binaries, so application is checked first and serialized
/// through `GET_LOCK`.
#[cfg(feature = "live-docker")]
fn ensure_mysql_schema(conn: &mut mysql::Conn) {
    use mysql::prelude::Queryable as _;

    let acquired: Option<i64> = conn
        .query_first("SELECT GET_LOCK('keelson_live_schema', 120)")
        .expect("taking the schema lock");
    assert_eq!(acquired, Some(1), "timed out waiting for the schema lock");
    let present: Option<i64> = conn
        .query_first(
            "SELECT 1 FROM information_schema.tables \
             WHERE table_schema = DATABASE() AND table_name = 'users'",
        )
        .expect("probing for the shared schema");
    if present.is_none() {
        for stmt in schema_sql(super::Dialect::Mysql).split(';') {
            if stmt.trim().is_empty() {
                continue;
            }
            conn.query_drop(stmt)
                .expect("the shared mysql schema must apply cleanly");
        }
    }
    conn.query_drop("SELECT RELEASE_LOCK('keelson_live_schema')")
        .expect("releasing the schema lock");
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
        check_sqlite(
            "INSERT INTO users (id, name) VALUES (?1, ?2) ON CONFLICT (id) DO NOTHING RETURNING id",
        )
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
