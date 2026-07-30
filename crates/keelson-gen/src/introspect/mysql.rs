//! MySQL introspection: `information_schema` through the sync `mysql`
//! client (already in the workspace for keelson-sqlcheck's live judge).
//!
//! MySQL has no `pg_catalog` equivalent to prefer — `information_schema` *is*
//! the catalog's public face, and `SHOW` statements return the same data in a
//! form that has to be parsed out of text. So the queries here read
//! `TABLES`, `COLUMNS`, `KEY_COLUMN_USAGE`, `TABLE_CONSTRAINTS` and
//! `STATISTICS`, all scoped to the connection's own database (`DATABASE()`),
//! which is the MySQL analogue of PostgreSQL's `schema` setting: a MySQL
//! schema *is* a database, so the URL's database name is the namespace and
//! the config's `schema` key does not apply.
//!
//! **Type spellings come from `COLUMN_TYPE`, not `DATA_TYPE`.** `DATA_TYPE`
//! reports `tinyint` for both a boolean and a small integer and drops
//! `unsigned`; `COLUMN_TYPE` is the full declaration (`tinyint(1)`,
//! `int unsigned`, `decimal(10,2)`), which is exactly what the type map keys
//! on — the same reason PostgreSQL introspection uses `format_type`.
//!
//! Determinism: tables by name, columns by `ORDINAL_POSITION`, keys by
//! constraint name then position, then re-sorted by column list in
//! [`canonicalise`](super::canonicalise).

use mysql::prelude::Queryable as _;
use mysql::{Conn, Opts};

use crate::error::{GenError, Result};
use crate::schema::{ColumnDef, ForeignKey, Schema, TableDef, TableKind};

fn err(e: impl std::fmt::Display) -> GenError {
    GenError::Introspect(format!("mysql: {e}"))
}

pub(crate) fn introspect(url: &str) -> Result<Schema> {
    let opts = Opts::from_url(url).map_err(err)?;
    let mut conn = Conn::new(opts).map_err(err)?;

    let rels: Vec<(String, String)> = conn
        .query_map(
            "SELECT TABLE_NAME, TABLE_TYPE FROM information_schema.TABLES \
             WHERE TABLE_SCHEMA = DATABASE() AND TABLE_TYPE IN ('BASE TABLE', 'VIEW') \
             ORDER BY TABLE_NAME",
            |(name, kind): (String, String)| (name, kind),
        )
        .map_err(err)?;

    let mut tables = Vec::with_capacity(rels.len());
    for (name, table_type) in rels {
        let kind = if table_type == "VIEW" {
            TableKind::View
        } else {
            TableKind::Table
        };
        tables.push(table_def(&mut conn, &name, kind)?);
    }

    let mut schema = Schema { tables };
    super::canonicalise(&mut schema);
    Ok(schema)
}

fn table_def(conn: &mut Conn, name: &str, kind: TableKind) -> Result<TableDef> {
    let columns = conn
        .exec_map(
            "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_DEFAULT, EXTRA, COLUMN_COMMENT \
             FROM information_schema.COLUMNS \
             WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ? \
             ORDER BY ORDINAL_POSITION",
            (name,),
            |(col, col_type, nullable, default, extra, comment): (
                String,
                String,
                String,
                Option<String>,
                String,
                String,
            )| {
                ColumnDef {
                    name: col,
                    db_type: col_type,
                    nullable: nullable.eq_ignore_ascii_case("YES"),
                    default,
                    // `EXTRA` also carries `DEFAULT_GENERATED`,
                    // `on update CURRENT_TIMESTAMP` and the generated-column
                    // kinds; only auto_increment is an assigned key.
                    autoincrement: extra.to_ascii_lowercase().contains("auto_increment"),
                    // MySQL has no "no comment" — an uncommented column is the
                    // empty string, which is not the same thing as a comment.
                    comment: (!comment.is_empty()).then_some(comment),
                }
            },
        )
        .map_err(err)?;

    // The primary key is the constraint MySQL always names `PRIMARY`.
    let primary_key: Vec<String> = conn
        .exec_map(
            "SELECT COLUMN_NAME FROM information_schema.KEY_COLUMN_USAGE \
             WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ? \
               AND CONSTRAINT_NAME = 'PRIMARY' \
             ORDER BY ORDINAL_POSITION",
            (name,),
            |col: String| col,
        )
        .map_err(err)?;

    // Foreign keys: one row per referencing column, grouped by constraint.
    let fk_rows: Vec<(String, String, String, String)> = conn
        .exec_map(
            "SELECT CONSTRAINT_NAME, REFERENCED_TABLE_NAME, COLUMN_NAME, REFERENCED_COLUMN_NAME \
             FROM information_schema.KEY_COLUMN_USAGE \
             WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ? \
               AND REFERENCED_TABLE_NAME IS NOT NULL \
             ORDER BY CONSTRAINT_NAME, ORDINAL_POSITION",
            (name,),
            |row: (String, String, String, String)| row,
        )
        .map_err(err)?;
    let mut foreign_keys: Vec<ForeignKey> = Vec::new();
    let mut last: Option<String> = None;
    for (constraint, ref_table, column, ref_column) in fk_rows {
        if last.as_deref() != Some(&constraint) {
            foreign_keys.push(ForeignKey {
                columns: vec![],
                ref_table,
                ref_columns: vec![],
            });
            last = Some(constraint);
        }
        let fk = foreign_keys.last_mut().expect("just pushed");
        fk.columns.push(column);
        fk.ref_columns.push(ref_column);
    }

    // Unique constraints, declared ones only: `TABLE_CONSTRAINTS` is what
    // separates a `UNIQUE` constraint from a bare unique index, which
    // `STATISTICS` alone cannot do (MySQL implements the constraint *as* an
    // index and reports both the same way).
    let uniq_rows: Vec<(String, String)> = conn
        .exec_map(
            "SELECT k.CONSTRAINT_NAME, k.COLUMN_NAME \
             FROM information_schema.KEY_COLUMN_USAGE k \
             JOIN information_schema.TABLE_CONSTRAINTS c \
               ON c.CONSTRAINT_SCHEMA = k.CONSTRAINT_SCHEMA \
              AND c.CONSTRAINT_NAME = k.CONSTRAINT_NAME \
              AND c.TABLE_NAME = k.TABLE_NAME \
             WHERE k.TABLE_SCHEMA = DATABASE() AND k.TABLE_NAME = ? \
               AND c.CONSTRAINT_TYPE = 'UNIQUE' \
             ORDER BY k.CONSTRAINT_NAME, k.ORDINAL_POSITION",
            (name,),
            |row: (String, String)| row,
        )
        .map_err(err)?;
    let mut unique_keys: Vec<Vec<String>> = Vec::new();
    let mut last: Option<String> = None;
    for (constraint, column) in uniq_rows {
        if last.as_deref() != Some(&constraint) {
            unique_keys.push(vec![]);
            last = Some(constraint);
        }
        unique_keys.last_mut().expect("just pushed").push(column);
    }

    Ok(TableDef {
        name: name.to_owned(),
        kind,
        columns,
        primary_key,
        foreign_keys,
        unique_keys,
    })
}
