//! PostgreSQL introspection: `pg_catalog` through the sync `postgres`
//! client (already in the workspace for keelson-sqlcheck's live judge).
//!
//! `information_schema` was considered and passed over for the column list:
//! `pg_catalog` + `format_type` reports exactly the type spelling the
//! type map keys on, includes materialised views, and needs no
//! per-privilege visibility caveats.

use postgres::types::Oid;
use postgres::{Client, NoTls};

use crate::error::{GenError, Result};
use crate::schema::{ColumnDef, ForeignKey, Schema, TableDef, TableKind};

fn err(e: postgres::Error) -> GenError {
    GenError::Introspect(format!("postgres: {e}"))
}

pub(crate) fn introspect(url: &str, schema_name: &str) -> Result<Schema> {
    let mut client = Client::connect(url, NoTls).map_err(err)?;

    // Updatability, PostgreSQL's rule: `pg_relation_is_updatable` returns a
    // bit mask over `CmdType`, so `UPDATE` is 1 << 2, `INSERT` 1 << 3 and
    // `DELETE` 1 << 4 — 28 with all three set. `information_schema.views`
    // computes `is_updatable` / `is_insertable_into` from exactly this
    // function; asking it directly keeps one catalog query per relation and
    // covers materialised views (never updatable) in the same expression.
    // The `true` asks it to count `INSTEAD OF` triggers, which are the other
    // way a PostgreSQL view becomes writable.
    let rels = client
        .query(
            "SELECT c.oid, c.relname, c.relkind::text, \
                    (pg_catalog.pg_relation_is_updatable(c.oid, true) & 28) = 28 \
             FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relkind IN ('r', 'p', 'v', 'm') \
             ORDER BY c.relname",
            &[&schema_name],
        )
        .map_err(err)?;

    let mut tables = Vec::new();
    for rel in rels {
        let oid: Oid = rel.get(0);
        let name: String = rel.get(1);
        let relkind: String = rel.get(2);
        let updatable: bool = rel.get(3);
        let kind = match (relkind.as_str(), updatable) {
            ("r" | "p", _) => TableKind::Table,
            (_, true) => TableKind::UpdatableView,
            (_, false) => TableKind::View,
        };
        tables.push(table_def(&mut client, oid, &name, kind)?);
    }

    let mut schema = Schema { tables };
    super::canonicalise(&mut schema);
    Ok(schema)
}

fn table_def(client: &mut Client, oid: Oid, name: &str, kind: TableKind) -> Result<TableDef> {
    let cols = client
        .query(
            "SELECT a.attname, \
                    pg_catalog.format_type(a.atttypid, a.atttypmod), \
                    a.attnotnull, \
                    pg_catalog.pg_get_expr(ad.adbin, ad.adrelid), \
                    (a.attidentity <> '')::bool, \
                    pg_catalog.col_description(a.attrelid, a.attnum) \
             FROM pg_catalog.pg_attribute a \
             LEFT JOIN pg_catalog.pg_attrdef ad \
               ON ad.adrelid = a.attrelid AND ad.adnum = a.attnum \
             WHERE a.attrelid = $1 AND a.attnum > 0 AND NOT a.attisdropped \
             ORDER BY a.attnum",
            &[&oid],
        )
        .map_err(err)?;
    let columns = cols
        .iter()
        .map(|r| {
            let default: Option<String> = r.get(3);
            let identity: bool = r.get(4);
            let autoincrement = identity
                || default
                    .as_deref()
                    .is_some_and(|d| d.starts_with("nextval("));
            ColumnDef {
                name: r.get(0),
                db_type: r.get(1),
                nullable: !r.get::<_, bool>(2),
                default,
                autoincrement,
                comment: r.get(5),
            }
        })
        .collect();

    let pk = client
        .query(
            "SELECT a.attname \
             FROM pg_catalog.pg_constraint con \
             CROSS JOIN LATERAL unnest(con.conkey) WITH ORDINALITY AS k(attnum, ord) \
             JOIN pg_catalog.pg_attribute a \
               ON a.attrelid = con.conrelid AND a.attnum = k.attnum \
             WHERE con.conrelid = $1 AND con.contype = 'p' \
             ORDER BY k.ord",
            &[&oid],
        )
        .map_err(err)?;
    let primary_key = pk.iter().map(|r| r.get(0)).collect();

    let fks = client
        .query(
            "SELECT con.conname, ref.relname, a.attname, af.attname \
             FROM pg_catalog.pg_constraint con \
             JOIN pg_catalog.pg_class ref ON ref.oid = con.confrelid \
             CROSS JOIN LATERAL unnest(con.conkey) WITH ORDINALITY AS k(attnum, ord) \
             CROSS JOIN LATERAL unnest(con.confkey) WITH ORDINALITY AS fk(attnum, ord) \
             JOIN pg_catalog.pg_attribute a \
               ON a.attrelid = con.conrelid AND a.attnum = k.attnum \
             JOIN pg_catalog.pg_attribute af \
               ON af.attrelid = con.confrelid AND af.attnum = fk.attnum \
             WHERE con.conrelid = $1 AND con.contype = 'f' AND fk.ord = k.ord \
             ORDER BY con.conname, k.ord",
            &[&oid],
        )
        .map_err(err)?;
    let mut foreign_keys: Vec<ForeignKey> = Vec::new();
    let mut last: Option<String> = None;
    for r in fks {
        let conname: String = r.get(0);
        if last.as_deref() != Some(&conname) {
            foreign_keys.push(ForeignKey {
                columns: vec![],
                ref_table: r.get(1),
                ref_columns: vec![],
            });
            last = Some(conname);
        }
        let fk = foreign_keys.last_mut().expect("just pushed");
        fk.columns.push(r.get(2));
        fk.ref_columns.push(r.get(3));
    }

    // Declared UNIQUE constraints (`contype = 'u'`); a bare `CREATE UNIQUE
    // INDEX` is an index rather than a constraint and is deliberately not
    // read (see `TableDef::unique_keys`).
    let uniq = client
        .query(
            "SELECT con.conname, a.attname \
             FROM pg_catalog.pg_constraint con \
             CROSS JOIN LATERAL unnest(con.conkey) WITH ORDINALITY AS k(attnum, ord) \
             JOIN pg_catalog.pg_attribute a \
               ON a.attrelid = con.conrelid AND a.attnum = k.attnum \
             WHERE con.conrelid = $1 AND con.contype = 'u' \
             ORDER BY con.conname, k.ord",
            &[&oid],
        )
        .map_err(err)?;
    let mut unique_keys: Vec<Vec<String>> = Vec::new();
    let mut last: Option<String> = None;
    for r in uniq {
        let conname: String = r.get(0);
        if last.as_deref() != Some(&conname) {
            unique_keys.push(vec![]);
            last = Some(conname);
        }
        unique_keys.last_mut().expect("just pushed").push(r.get(1));
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
