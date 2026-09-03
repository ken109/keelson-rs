//! Introspection: a connection string in, a [`Schema`] out.
//!
//! Direct catalog queries per dialect — `sqlite_master` + pragma functions
//! through rusqlite, `pg_catalog` through the sync postgres client,
//! `information_schema` through the sync `mysql` client. The sea-schema
//! evaluation and rejection is recorded in the crate docs.
//!
//! Determinism contract: tables sorted by name, columns in ordinal order,
//! foreign keys and unique keys sorted by their column lists (SQLite's
//! `pragma_foreign_key_list` enumerates keys in reverse declaration order,
//! PostgreSQL by whatever the constraint names sort to, MySQL by constraint
//! name — sorting by columns makes every dialect, and every rerun, agree).

pub(crate) mod mysql;
pub(crate) mod psql;
pub(crate) mod sqlite;

use std::path::PathBuf;

use crate::config::{Config, Dialect};
use crate::error::{GenError, Result};
use crate::schema::{Schema, Snapshot};

/// Where a [`Schema`] came from.
///
/// The callers that write ([`crate::run`]) and the callers that compare
/// ([`crate::check`]) both need to know: a schema read from the live catalog
/// is what a snapshot file should be refreshed to (or checked against), and a
/// schema read *from* that file cannot be either.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// Read from the live catalog.
    Database,
    /// Read from the committed snapshot at this path.
    Snapshot(PathBuf),
}

/// Introspect the database the config points at.
///
/// With no `url`, falls back to the configured schema snapshot — see
/// [`introspect_from`] for the rule.
pub fn introspect(config: &Config) -> Result<Schema> {
    introspect_from(config).map(|(schema, _)| schema)
}

/// [`introspect`], saying where the answer came from.
///
/// The rule is one line: **a `url` wins, a snapshot is the fallback.** The
/// live catalog is the truth and is used whenever it is reachable; the
/// snapshot exists so that a checkout without a database is not stuck, not so
/// that anyone can prefer it to the database by accident.
pub fn introspect_from(config: &Config) -> Result<(Schema, Origin)> {
    match (config.url.as_deref(), config.snapshot.as_deref()) {
        (Some(url), _) => Ok((live(config, url)?, Origin::Database)),
        (None, Some(path)) => {
            let path = PathBuf::from(path);
            let mut schema = Snapshot::load(&path)?.schema_for(config.dialect, &path)?;
            // A snapshot is written canonicalised, but a hand-edited one may
            // not be, and the emitter's determinism is not the file's promise
            // to keep.
            canonicalise(&mut schema);
            Ok((schema, Origin::Snapshot(path)))
        }
        (None, None) => Err(GenError::Config(
            "no `url` in the config (and none given on the command line), and no `snapshot` \
             to read instead"
                .to_owned(),
        )),
    }
}

/// Introspect the live catalog.
fn live(config: &Config, url: &str) -> Result<Schema> {
    match config.dialect {
        Dialect::Sqlite => sqlite::introspect(url),
        Dialect::Psql => psql::introspect(url, &config.schema),
        Dialect::Mysql => {
            // A MySQL schema *is* a database, so the namespace is the URL's
            // database name and `schema` can mean nothing here. Saying so
            // beats silently ignoring it.
            if config.schema != "public" {
                return Err(GenError::Unsupported(format!(
                    "dialect = \"mysql\" ignores `schema` (here `{}`): a MySQL schema is a \
                     database, so name it in the connection URL instead",
                    config.schema
                )));
            }
            mysql::introspect(url)
        }
    }
}

/// Sort a schema into the deterministic order the emitter expects.
pub(crate) fn canonicalise(schema: &mut Schema) {
    schema.tables.sort_by(|a, b| a.name.cmp(&b.name));
    for t in &mut schema.tables {
        t.foreign_keys.sort_by(|a, b| a.columns.cmp(&b.columns));
        t.unique_keys.sort();
    }
}
