//! The default type table (`docs/type-mappings.md`, column-type column read
//! backwards) plus the config's override machinery.
//!
//! Resolution order per column, first hit wins:
//!
//! 1. `[[types.override]]` — the first override whose scope and matcher fit;
//! 2. `[types.map]` — the normalised database type;
//! 3. the built-in default table below;
//! 4. otherwise [`GenError::UnmappedType`] — an unmapped type is a loud
//!    failure, never a silent `String`.
//!
//! Every type that arrives via 1 or 2 is recorded as an override so the
//! emitter writes the `assert_bind` line for it.

use crate::config::{Dialect, Matcher, Types};
use crate::error::{GenError, Result};
use crate::schema::{ColumnDef, TableDef};

/// A resolved column type: the Rust path to emit, and whether it came from
/// configuration (and so needs its `assert_bind` line).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedType {
    /// The Rust type path, e.g. `i64` or `chrono::NaiveDateTime`.
    pub rust_type: String,
    /// True when the type came from `[types.map]` or `[[types.override]]`.
    pub overridden: bool,
}

/// Normalise a declared database type for matching: lowercased, precision
/// stripped (`NUMERIC(10, 2)` → `numeric`, `character varying(255)` →
/// `character varying`).
pub(crate) fn normalise(db_type: &str) -> String {
    let base = match db_type.find('(') {
        Some(i) => &db_type[..i],
        None => db_type,
    };
    base.trim().to_lowercase()
}

/// Resolve one column's Rust type.
pub(crate) fn resolve(
    dialect: Dialect,
    types: &Types,
    table: &TableDef,
    column: &ColumnDef,
) -> Result<ResolvedType> {
    for o in &types.overrides {
        let in_scope = o.tables.is_empty() || o.tables.contains(&table.name);
        if in_scope && matches(&o.matcher, column) {
            return Ok(ResolvedType {
                rust_type: o.rust_type.clone(),
                overridden: true,
            });
        }
    }
    let norm = normalise(&column.db_type);
    if let Some(t) = types.map.get(&norm) {
        return Ok(ResolvedType {
            rust_type: t.clone(),
            overridden: true,
        });
    }
    match default_type(dialect, &norm, column) {
        Some(t) => Ok(ResolvedType {
            rust_type: t.to_owned(),
            overridden: false,
        }),
        None => Err(GenError::UnmappedType {
            column: format!("{}.{}", table.name, column.name),
            db_type: column.db_type.clone(),
        }),
    }
}

fn matches(m: &Matcher, c: &ColumnDef) -> bool {
    if let Some(name) = &m.name
        && *name != c.name
    {
        return false;
    }
    if let Some(db_type) = &m.db_type
        && normalise(db_type) != normalise(&c.db_type)
    {
        return false;
    }
    if let Some(nullable) = m.nullable
        && nullable != c.nullable
    {
        return false;
    }
    if let Some(default) = &m.default
        && Some(default.as_str()) != c.default.as_deref()
    {
        return false;
    }
    if let Some(autoincrement) = m.autoincrement
        && autoincrement != c.autoincrement
    {
        return false;
    }
    if let Some(comment) = &m.comment
        && Some(comment.as_str()) != c.comment.as_deref()
    {
        return false;
    }
    true
}

/// The built-in default table, per dialect.
fn default_type(dialect: Dialect, norm: &str, column: &ColumnDef) -> Option<&'static str> {
    match dialect {
        Dialect::Psql => psql_default(norm),
        Dialect::Sqlite => sqlite_default(norm, column),
        // MySQL emission is not implemented; its type table would land here
        // with it.
        Dialect::Mysql => None,
    }
}

/// PostgreSQL: keys are `format_type` output, normalised.
fn psql_default(norm: &str) -> Option<&'static str> {
    Some(match norm {
        "smallint" | "int2" => "i16",
        "integer" | "int4" => "i32",
        "bigint" | "int8" => "i64",
        "real" | "float4" => "f32",
        "double precision" | "float8" => "f64",
        "boolean" | "bool" => "bool",
        "text" | "character varying" | "varchar" | "character" | "bpchar" | "citext" | "name" => {
            "String"
        }
        "bytea" => "Vec<u8>",
        "date" => "chrono::NaiveDate",
        "time" | "time without time zone" => "chrono::NaiveTime",
        "timestamp" | "timestamp without time zone" => "chrono::NaiveDateTime",
        "timestamptz" | "timestamp with time zone" => "chrono::DateTime<chrono::Utc>",
        "uuid" => "uuid::Uuid",
        "numeric" | "decimal" => "rust_decimal::Decimal",
        "json" | "jsonb" => "serde_json::Value",
        _ => return None,
    })
}

/// SQLite: declared-type text, by affinity-style contains-rules plus the
/// honest extras the spec records — a declared `BOOLEAN` is a real `bool`,
/// and a `TEXT` column whose *default* writes `CURRENT_TIMESTAMP` is a
/// `NaiveDateTime` (the default's naive space-separated form is what the
/// column will actually hold). Other datetime-intent `TEXT` columns are
/// `String` until a `[[types.override]]` says otherwise — the schema simply
/// does not carry the information.
fn sqlite_default(norm: &str, column: &ColumnDef) -> Option<&'static str> {
    // Exact names first: the intent-carrying declarations.
    match norm {
        "boolean" | "bool" => return Some("bool"),
        "datetime" | "timestamp" => return Some("chrono::NaiveDateTime"),
        "date" => return Some("chrono::NaiveDate"),
        "time" => return Some("chrono::NaiveTime"),
        "" => return Some("Vec<u8>"), // no declared type: BLOB affinity
        _ => {}
    }
    if norm.contains("int") {
        return Some("i64"); // SQLite integers are 64-bit; there is no i32 column type
    }
    if norm.contains("char") || norm.contains("clob") || norm.contains("text") {
        if column
            .default
            .as_deref()
            .is_some_and(|d| d.trim().eq_ignore_ascii_case("current_timestamp"))
        {
            return Some("chrono::NaiveDateTime");
        }
        return Some("String");
    }
    if norm.contains("blob") {
        return Some("Vec<u8>");
    }
    if norm.contains("real") || norm.contains("floa") || norm.contains("doub") {
        return Some("f64");
    }
    if norm.contains("dec") || norm.contains("numeric") {
        return Some("rust_decimal::Decimal");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::schema::TableKind;

    fn col(name: &str, db_type: &str, nullable: bool) -> ColumnDef {
        ColumnDef {
            name: name.to_owned(),
            db_type: db_type.to_owned(),
            nullable,
            default: None,
            autoincrement: false,
            comment: None,
        }
    }

    fn table(name: &str, columns: Vec<ColumnDef>) -> TableDef {
        TableDef {
            name: name.to_owned(),
            kind: TableKind::Table,
            columns,
            primary_key: vec![],
            foreign_keys: vec![],
        }
    }

    fn plain(dialect: Dialect, db_type: &str) -> String {
        let c = col("c", db_type, false);
        let t = table("t", vec![c.clone()]);
        resolve(dialect, &Types::default(), &t, &c)
            .unwrap()
            .rust_type
    }

    #[test]
    fn the_psql_defaults_follow_the_type_table() {
        assert_eq!(plain(Dialect::Psql, "integer"), "i32");
        assert_eq!(plain(Dialect::Psql, "bigint"), "i64");
        assert_eq!(plain(Dialect::Psql, "text"), "String");
        assert_eq!(plain(Dialect::Psql, "character varying(255)"), "String");
        assert_eq!(plain(Dialect::Psql, "boolean"), "bool");
        assert_eq!(
            plain(Dialect::Psql, "timestamp with time zone"),
            "chrono::DateTime<chrono::Utc>"
        );
        assert_eq!(
            plain(Dialect::Psql, "timestamp without time zone"),
            "chrono::NaiveDateTime"
        );
        assert_eq!(plain(Dialect::Psql, "date"), "chrono::NaiveDate");
        assert_eq!(plain(Dialect::Psql, "uuid"), "uuid::Uuid");
        assert_eq!(
            plain(Dialect::Psql, "numeric(10,2)"),
            "rust_decimal::Decimal"
        );
        assert_eq!(plain(Dialect::Psql, "jsonb"), "serde_json::Value");
    }

    #[test]
    fn the_sqlite_defaults_follow_declared_types_and_the_spec_notes() {
        assert_eq!(plain(Dialect::Sqlite, "INTEGER"), "i64");
        assert_eq!(plain(Dialect::Sqlite, "TEXT"), "String");
        assert_eq!(plain(Dialect::Sqlite, "BOOLEAN"), "bool");
        assert_eq!(plain(Dialect::Sqlite, "VARCHAR(80)"), "String");
        assert_eq!(plain(Dialect::Sqlite, "BLOB"), "Vec<u8>");
        assert_eq!(plain(Dialect::Sqlite, "REAL"), "f64");
        assert_eq!(plain(Dialect::Sqlite, "DATETIME"), "chrono::NaiveDateTime");

        // The spec's rule: TEXT whose default is CURRENT_TIMESTAMP holds the
        // naive datetime the default writes.
        let mut c = col("created_at", "TEXT", false);
        c.default = Some("CURRENT_TIMESTAMP".to_owned());
        let t = table("users", vec![c.clone()]);
        assert_eq!(
            resolve(Dialect::Sqlite, &Types::default(), &t, &c)
                .unwrap()
                .rust_type,
            "chrono::NaiveDateTime"
        );
    }

    #[test]
    fn an_unmapped_type_is_a_loud_error_naming_the_column() {
        let c = col("shape", "polygon", false);
        let t = table("zones", vec![c.clone()]);
        let err = resolve(Dialect::Psql, &Types::default(), &t, &c).unwrap_err();
        assert!(err.to_string().contains("zones.shape"), "{err}");
        assert!(err.to_string().contains("polygon"), "{err}");
    }

    #[test]
    fn overrides_win_and_are_marked_for_assert_bind() {
        let cfg = Config::from_toml(
            r#"
            dialect = "sqlite"
            [types.map]
            "numeric" = "MyMoney"
            [[types.override]]
            tables = ["posts"]
            rust_type = "chrono::NaiveDateTime"
            [types.override.match]
            name = "published_at"
            db_type = "text"
            "#,
        )
        .unwrap();

        let c = col("published_at", "TEXT", true);
        let t = table("posts", vec![c.clone()]);
        let r = resolve(Dialect::Sqlite, &cfg.types, &t, &c).unwrap();
        assert_eq!(r.rust_type, "chrono::NaiveDateTime");
        assert!(r.overridden);

        // Same column on another table: out of scope, default applies.
        let t2 = table("drafts", vec![c.clone()]);
        let r2 = resolve(Dialect::Sqlite, &cfg.types, &t2, &c).unwrap();
        assert_eq!(r2.rust_type, "String");
        assert!(!r2.overridden);

        // The db-type map catches what overrides do not.
        let c3 = col("price", "NUMERIC(10,2)", false);
        let t3 = table("orders", vec![c3.clone()]);
        let r3 = resolve(Dialect::Sqlite, &cfg.types, &t3, &c3).unwrap();
        assert_eq!(r3.rust_type, "MyMoney");
        assert!(r3.overridden);
    }

    #[test]
    fn matcher_fields_are_conjunctive() {
        let cfg = Config::from_toml(
            r#"
            dialect = "sqlite"
            [[types.override]]
            rust_type = "X"
            [types.override.match]
            db_type = "integer"
            nullable = false
            "#,
        )
        .unwrap();
        let yes = col("a", "INTEGER", false);
        let no = col("a", "INTEGER", true);
        let t = table("t", vec![yes.clone(), no.clone()]);
        assert!(
            resolve(Dialect::Sqlite, &cfg.types, &t, &yes)
                .unwrap()
                .overridden
        );
        assert!(
            !resolve(Dialect::Sqlite, &cfg.types, &t, &no)
                .unwrap()
                .overridden
        );
    }
}
