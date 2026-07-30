//! Introspection: a connection string in, a [`Schema`] out.
//!
//! Direct catalog queries per dialect — `sqlite_master` + pragma functions
//! through rusqlite, `pg_catalog` through the sync postgres client. The
//! sea-schema evaluation and rejection is recorded in the crate docs.
//!
//! Determinism contract: tables sorted by name, columns in ordinal order,
//! foreign keys sorted by their column lists (SQLite's
//! `pragma_foreign_key_list` enumerates keys in reverse declaration order,
//! PostgreSQL by whatever the constraint names sort to — sorting by columns
//! makes both dialects, and reruns, agree).

pub(crate) mod psql;
pub(crate) mod sqlite;

use crate::config::{Config, Dialect};
use crate::error::{GenError, Result};
use crate::schema::Schema;

/// Introspect the database the config points at.
pub fn introspect(config: &Config) -> Result<Schema> {
    let url = config.url.as_deref().ok_or_else(|| {
        GenError::Config("no `url` in the config (and none given on the command line)".to_owned())
    })?;
    match config.dialect {
        Dialect::Sqlite => sqlite::introspect(url),
        Dialect::Psql => psql::introspect(url, &config.schema),
        Dialect::Mysql => Err(GenError::Unsupported(
            "MySQL introspection is not implemented yet".to_owned(),
        )),
    }
}

/// Sort a schema into the deterministic order the emitter expects.
pub(crate) fn canonicalise(schema: &mut Schema) {
    schema.tables.sort_by(|a, b| a.name.cmp(&b.name));
    for t in &mut schema.tables {
        t.foreign_keys.sort_by(|a, b| a.columns.cmp(&b.columns));
    }
}
