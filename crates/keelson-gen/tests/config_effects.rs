//! The configuration inventory, observed in the output: filters, aliases,
//! inflections, manual relationships, back-reference suppression, serde
//! derives, hooks validation, type overrides (and their `assert_bind`
//! lines), and the honest MySQL refusal.

use keelson_gen::Config;
use keelson_gen::schema::{ColumnDef, ForeignKey, Schema, TableDef, TableKind};

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

fn table(name: &str, columns: Vec<ColumnDef>, pk: &[&str], fks: Vec<ForeignKey>) -> TableDef {
    TableDef {
        name: name.to_owned(),
        kind: TableKind::Table,
        columns,
        primary_key: pk.iter().map(|s| (*s).to_owned()).collect(),
        foreign_keys: fks,
    }
}

fn fk(column: &str, ref_table: &str, ref_column: &str) -> ForeignKey {
    ForeignKey {
        columns: vec![column.to_owned()],
        ref_table: ref_table.to_owned(),
        ref_columns: vec![ref_column.to_owned()],
    }
}

/// authors ← books (FK), plus a `secrets` table to filter away.
fn library() -> Schema {
    Schema {
        tables: vec![
            table(
                "authors",
                vec![col("id", "INTEGER", false), col("name", "TEXT", false)],
                &["id"],
                vec![],
            ),
            table(
                "books",
                vec![
                    col("id", "INTEGER", false),
                    col("author_id", "INTEGER", false),
                    col("title", "TEXT", false),
                    col("price", "NUMERIC(10,2)", true),
                ],
                &["id"],
                vec![fk("author_id", "authors", "id")],
            ),
            table(
                "secrets",
                vec![col("id", "INTEGER", false), col("token", "TEXT", false)],
                &["id"],
                vec![],
            ),
        ],
    }
}

fn generate(schema: &Schema, toml: &str) -> Vec<(String, String)> {
    let config = Config::from_toml(toml).expect("config");
    keelson_gen::generate_from_schema(schema, &config).expect("generation")
}

fn file<'a>(files: &'a [(String, String)], name: &str) -> &'a str {
    &files
        .iter()
        .find(|(n, _)| n == name)
        .unwrap_or_else(|| panic!("{name} was not generated"))
        .1
}

#[test]
fn only_keeps_the_named_tables_and_drops_their_dangling_relations() {
    let files = generate(&library(), "dialect = \"sqlite\"\nonly = [\"books\"]");
    let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, vec!["mod.rs", "books.rs"]);
    // The FK target is not generated: no relation surface at all.
    let books = file(&files, "books.rs");
    assert!(!books.contains("pub mod preload"));
    assert!(!books.contains("pub mod then_load"));
    assert!(!books.contains("rel.author"));
}

#[test]
fn except_removes_a_table_everywhere() {
    let files = generate(&library(), "dialect = \"sqlite\"\nexcept = [\"secrets\"]");
    assert!(files.iter().all(|(n, _)| n != "secrets.rs"));
    assert!(!file(&files, "mod.rs").contains("secrets"));
}

#[test]
fn column_filters_drop_fns_and_a_lost_primary_key_demotes_to_a_view() {
    let files = generate(
        &library(),
        r#"
        dialect = "sqlite"
        [tables.books]
        except_columns = ["price"]
        "#,
    );
    let books = file(&files, "books.rs");
    assert!(!books.contains("price"));
    assert!(books.contains("impl keelson_models::Table for Books"));

    // Filter the pk away: SELECT-only is all that is sound to emit.
    let files = generate(
        &library(),
        r#"
        dialect = "sqlite"
        [tables.secrets]
        only_columns = ["token"]
        "#,
    );
    let secrets = file(&files, "secrets.rs");
    assert!(secrets.contains("pub fn view()"));
    assert!(!secrets.contains("impl keelson_models::Table for Secrets"));
    assert!(!secrets.contains("Setter"));
}

#[test]
fn aliases_rename_rust_names_but_never_sql_names() {
    let files = generate(
        &library(),
        r#"
        dialect = "sqlite"
        [aliases.authors]
        singular = "writer"
        [aliases.books]
        plural = "catalogue"
        [aliases.books.columns]
        title = "heading"
        [aliases.books.relationships]
        author = "writer"
        "#,
    );
    let authors = file(&files, "authors.rs");
    assert!(
        authors.contains("pub struct Writer {"),
        "row struct renamed"
    );
    let books = file(&files, "books.rs");
    // Column alias: the fn and field are renamed, the SQL name stays.
    assert!(books.contains("pub fn heading()"));
    assert!(books.contains(r#"Column::new("books", "title")"#));
    assert!(books.contains(r#"heading: row.take("title")?"#));
    // Relationship alias on the belongs-to; plural alias on the back-ref.
    assert!(books.contains("pub writer: Option<super::authors::Writer>"));
    assert!(books.contains("pub fn writer()"));
    assert!(authors.contains("pub catalogue: Vec<super::books::Book>"));
}

#[test]
fn inflections_fix_irregular_plurals() {
    let schema = Schema {
        tables: vec![table(
            "people",
            vec![col("id", "INTEGER", false), col("name", "TEXT", false)],
            &["id"],
            vec![],
        )],
    };
    let files = generate(
        &schema,
        "dialect = \"sqlite\"\n[inflections]\npeople = \"person\"",
    );
    assert!(file(&files, "people.rs").contains("pub struct Person {"));
}

#[test]
fn no_back_referencing_suppresses_the_has_many_side_only() {
    let files = generate(
        &library(),
        "dialect = \"sqlite\"\nno_back_referencing = true",
    );
    let authors = file(&files, "authors.rs");
    assert!(!authors.contains("Vec<super::books::Book>"));
    assert!(!authors.contains("pub mod then_load"));
    let books = file(&files, "books.rs");
    assert!(books.contains("pub author: Option<super::authors::Author>"));
    assert!(books.contains("pub mod preload"));
}

#[test]
fn manual_relationships_join_what_the_schema_does_not() {
    // No FK between these two; the config declares the key, names it, and
    // suppresses the back-reference.
    let schema = Schema {
        tables: vec![
            table(
                "audits",
                vec![
                    col("id", "INTEGER", false),
                    col("actor_name", "TEXT", false),
                ],
                &["id"],
                vec![],
            ),
            table(
                "users",
                vec![col("id", "INTEGER", false), col("name", "TEXT", false)],
                &["id"],
                vec![],
            ),
        ],
    };
    let files = generate(
        &schema,
        r#"
        dialect = "sqlite"
        [[relationships]]
        table = "audits"
        column = "actor_name"
        ref_table = "users"
        ref_column = "name"
        name = "actor"
        no_back_reference = true
        "#,
    );
    let audits = file(&files, "audits.rs");
    assert!(audits.contains("pub actor: Option<super::users::User>"));
    assert!(audits.contains("pub fn actor()"));
    // String keys clone; the generated loader keys on the named column.
    assert!(audits.contains("r.actor_name.clone()"));
    let users = file(&files, "users.rs");
    assert!(!users.contains("audits"), "back-reference suppressed");
}

#[test]
fn serde_output_derives_on_row_and_rel_structs() {
    let files = generate(&library(), "dialect = \"sqlite\"\n[output]\nserde = true");
    let authors = file(&files, "authors.rs");
    assert!(authors.contains(
        "#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]\npub struct Author {"
    ));
    assert!(authors.contains(
        "#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]\npub struct Rel {"
    ));
    // The Setter stays serde-free: a three-state Set<T> has no agreed wire
    // form, and none is invented here.
    assert!(authors.contains("#[derive(Debug, Clone, Default)]\npub struct Setter {"));
}

#[test]
fn type_overrides_change_the_emitted_type_and_assert_the_bind() {
    let files = generate(
        &library(),
        r#"
        dialect = "sqlite"
        [types.map]
        numeric = "rust_decimal::Decimal"
        [[types.override]]
        tables = ["books"]
        rust_type = "crate::types::Heading"
        [types.override.match]
        name = "title"
        "#,
    );
    let books = file(&files, "books.rs");
    // The per-column override.
    assert!(books.contains("pub title: crate::types::Heading"));
    assert!(books.contains("keelson_models::Column<crate::types::Heading>"));
    assert!(books.contains("const _: () = keelson_exec::assert_bind::<crate::types::Heading>();"));
    // The db-type map counts as an override too — it must also bind.
    assert!(books.contains("pub price: Option<rust_decimal::Decimal>"));
    assert!(books.contains("const _: () = keelson_exec::assert_bind::<rust_decimal::Decimal>();"));
}

#[test]
fn hooks_on_a_select_only_model_are_a_config_error() {
    let mut schema = library();
    schema.tables[0].primary_key.clear(); // authors: keyless → view model
    let config = Config::from_toml(
        r#"
        dialect = "sqlite"
        [tables.authors]
        hooks = ["before_insert"]
        "#,
    )
    .unwrap();
    let err = keelson_gen::generate_from_schema(&schema, &config).unwrap_err();
    assert!(err.to_string().contains("before_insert"), "{err}");
    assert!(err.to_string().contains("after_select"), "{err}");
}

#[test]
fn mysql_emission_refuses_honestly() {
    let config = Config::from_toml("dialect = \"mysql\"").unwrap();
    let err = keelson_gen::generate_from_schema(&library(), &config).unwrap_err();
    assert!(err.to_string().contains("MySQL emission"), "{err}");
}

#[test]
fn past_sixteen_columns_the_projection_falls_back_to_a_vec() {
    // The tuple `IntoExprList` impls stop at 16 elements; a wider table
    // projects through `Vec<Expr>` instead — same SQL, and the per-column
    // types stay on the column fns.
    let mut cols = vec![col("id", "INTEGER", false)];
    for i in 1..17 {
        cols.push(col(&format!("c{i:02}"), "TEXT", true));
    }
    let schema = Schema {
        tables: vec![table("wide", cols, &["id"], vec![])],
    };
    let files = generate(&schema, "dialect = \"sqlite\"");
    let wide = file(&files, "wide.rs");
    assert!(wide.contains("fn all_columns() -> Vec<keelson_core::expr::Expr>"));
    assert!(wide.contains("all.push(id().expr());"));
    assert!(wide.contains("all.push(c16().expr());"));
    assert!(wide.contains("pub fn c16() -> keelson_models::Column<String>"));
}
