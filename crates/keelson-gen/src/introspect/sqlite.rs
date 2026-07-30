//! SQLite introspection: `sqlite_master` for the table/view list, the
//! `pragma_table_info` / `pragma_foreign_key_list` table-valued functions
//! for the rest. rusqlite is already in the workspace (keelson-sqlcheck's
//! live judge); no new dependency.

use rusqlite::Connection;

use crate::error::{GenError, Result};
use crate::schema::{ColumnDef, ForeignKey, Schema, TableDef, TableKind};

fn err(e: rusqlite::Error) -> GenError {
    GenError::Introspect(format!("sqlite: {e}"))
}

/// `sqlite://path`, `sqlite:path` or a bare path.
fn path_of(url: &str) -> &str {
    url.strip_prefix("sqlite://")
        .or_else(|| url.strip_prefix("sqlite:"))
        .unwrap_or(url)
}

pub(crate) fn introspect(url: &str) -> Result<Schema> {
    let conn =
        Connection::open_with_flags(path_of(url), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(err)?;

    let mut tables = Vec::new();
    let mut stmt = conn
        .prepare(
            "SELECT name, type FROM sqlite_master \
             WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
             ORDER BY name",
        )
        .map_err(err)?;
    let names: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(err)?
        .collect::<rusqlite::Result<_>>()
        .map_err(err)?;
    drop(stmt);

    for (name, kind) in names {
        let kind = if kind == "view" {
            TableKind::View
        } else {
            TableKind::Table
        };
        tables.push(table_def(&conn, &name, kind)?);
    }

    let mut schema = Schema { tables };
    resolve_fk_targets(&mut schema)?;
    super::canonicalise(&mut schema);
    Ok(schema)
}

fn table_def(conn: &Connection, name: &str, kind: TableKind) -> Result<TableDef> {
    // (name, declared type, notnull, default, pk ordinal)
    let mut stmt = conn
        .prepare("SELECT name, type, \"notnull\", dflt_value, pk FROM pragma_table_info(?1)")
        .map_err(err)?;
    let raw: Vec<(String, String, i64, Option<String>, i64)> = stmt
        .query_map([name], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .map_err(err)?
        .collect::<rusqlite::Result<_>>()
        .map_err(err)?;

    let mut pk_with_ord: Vec<(i64, String)> = raw
        .iter()
        .filter(|(_, _, _, _, pk)| *pk > 0)
        .map(|(n, _, _, _, pk)| (*pk, n.clone()))
        .collect();
    pk_with_ord.sort();
    let primary_key: Vec<String> = pk_with_ord.into_iter().map(|(_, n)| n).collect();

    // The rowid alias: a single-column INTEGER primary key on a base table
    // auto-assigns on insert.
    let rowid_alias = kind == TableKind::Table
        && primary_key.len() == 1
        && raw.iter().any(|(n, t, _, _, _)| {
            *n == primary_key[0] && t.trim().eq_ignore_ascii_case("integer")
        });

    let columns = raw
        .iter()
        .map(|(n, db_type, notnull, default, pk)| ColumnDef {
            name: n.clone(),
            db_type: db_type.clone(),
            // A primary-key column is never honestly NULL, even though the
            // rowid alias lets an INSERT omit it.
            nullable: *notnull == 0 && *pk == 0,
            default: default.clone(),
            autoincrement: rowid_alias && *pk > 0,
            comment: None,
        })
        .collect();

    // (id, seq, referenced table, from, to) — `to` is NULL when the key
    // references the target's primary key implicitly; resolved after every
    // table is loaded.
    let mut stmt = conn
        .prepare("SELECT id, seq, \"table\", \"from\", \"to\" FROM pragma_foreign_key_list(?1)")
        .map_err(err)?;
    let fk_rows: Vec<(i64, i64, String, String, Option<String>)> = stmt
        .query_map([name], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .map_err(err)?
        .collect::<rusqlite::Result<_>>()
        .map_err(err)?;

    let mut fk_rows = fk_rows;
    fk_rows.sort_by_key(|a| (a.0, a.1));
    let mut foreign_keys: Vec<ForeignKey> = Vec::new();
    let mut last_id = None;
    for (id, _, table, from, to) in fk_rows {
        if last_id != Some(id) {
            foreign_keys.push(ForeignKey {
                columns: vec![],
                ref_table: table,
                ref_columns: vec![],
            });
            last_id = Some(id);
        }
        let fk = foreign_keys.last_mut().expect("just pushed");
        fk.columns.push(from);
        if let Some(to) = to {
            fk.ref_columns.push(to);
        }
    }

    Ok(TableDef {
        name: name.to_owned(),
        kind,
        columns,
        primary_key,
        foreign_keys,
    })
}

/// Fill in `ref_columns` for keys that referenced a primary key implicitly.
fn resolve_fk_targets(schema: &mut Schema) -> Result<()> {
    let pks: std::collections::BTreeMap<String, Vec<String>> = schema
        .tables
        .iter()
        .map(|t| (t.name.clone(), t.primary_key.clone()))
        .collect();
    for t in &mut schema.tables {
        let table = t.name.clone();
        for fk in &mut t.foreign_keys {
            if fk.ref_columns.is_empty() {
                let pk = pks.get(&fk.ref_table).ok_or_else(|| {
                    GenError::Introspect(format!(
                        "sqlite: {table} references unknown table {}",
                        fk.ref_table
                    ))
                })?;
                if pk.len() != fk.columns.len() {
                    return Err(GenError::Introspect(format!(
                        "sqlite: {table} references {}'s primary key with {} column(s), \
                         but that key has {}",
                        fk.ref_table,
                        fk.columns.len(),
                        pk.len()
                    )));
                }
                fk.ref_columns = pk.clone();
            }
        }
    }
    Ok(())
}
