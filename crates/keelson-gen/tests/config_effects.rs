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
        unique_keys: vec![],
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
    assert!(books.contains("pub writer: Option<Box<super::authors::Writer>>"));
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
    assert!(books.contains("pub author: Option<Box<super::authors::Author>>"));
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
    assert!(audits.contains("pub actor: Option<Box<super::users::User>>"));
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

/// MySQL emits — with the recorded no-`RETURNING` divergences, not a copy of
/// the `RETURNING` path.
#[test]
fn mysql_emission_takes_the_no_returning_shape() {
    let files = generate(&library(), "dialect = \"mysql\"");
    let books = file(&files, "books.rs");
    assert!(books.contains("keelson_mysql::InsertQuery"));
    assert!(!books.contains("returning"), "MySQL has no RETURNING");
    assert!(
        books.contains("pub fn by_pk("),
        "the keyed read-back instead"
    );
    assert!(
        books.contains("pub fn table() -> Books"),
        "the marker, not ModelTable, carries the MySQL verbs"
    );
}

/// What MySQL does *not* cover is still a loud, named failure — never a
/// silent fallback.
#[test]
fn unmapped_mysql_types_and_ignored_config_are_loud() {
    // A type with no honest mapping names the column.
    let schema = Schema {
        tables: vec![table(
            "readings",
            vec![col("id", "INTEGER", false), col("taken", "YEAR", false)],
            &["id"],
            vec![],
        )],
    };
    let config = Config::from_toml("dialect = \"mysql\"").unwrap();
    let err = keelson_gen::generate_from_schema(&schema, &config).unwrap_err();
    assert!(err.to_string().contains("readings.taken"), "{err}");

    // A MySQL schema *is* a database, so `schema` cannot mean anything here;
    // saying so beats silently ignoring it.
    let config = Config::from_toml(
        r#"
        dialect = "mysql"
        url = "mysql://root@127.0.0.1:1/app"
        schema = "other"
        "#,
    )
    .unwrap();
    let err = keelson_gen::introspect::introspect(&config).unwrap_err();
    assert!(err.to_string().contains("schema"), "{err}");
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

// ─────────────────── relations involving views ───────────────────
//
// A view has no foreign keys and no key, so the configuration is the only
// place a relation touching one can come from — and every part of the
// declaration the catalog *can* check is checked at generation time, naming
// the config key that is wrong. These tests are that contract, one rejection
// at a time. `docs/views.md` is the reader's version.

fn view(name: &str, columns: Vec<ColumnDef>, kind: TableKind) -> TableDef {
    TableDef {
        name: name.to_owned(),
        kind,
        columns,
        primary_key: vec![],
        foreign_keys: vec![],
        unique_keys: vec![],
    }
}

/// `library()` plus `author_names`, a view over `authors` the engine will not
/// write through, and `sales`, one it will.
fn library_with_views() -> Schema {
    let mut schema = library();
    schema.tables.push(view(
        "author_names",
        vec![col("author_id", "INTEGER", true), col("name", "TEXT", true)],
        TableKind::View,
    ));
    schema.tables.push(view(
        "sales",
        vec![
            col("book_id", "INTEGER", true),
            col("total", "INTEGER", true),
        ],
        TableKind::UpdatableView,
    ));
    schema
}

/// The declaration a view needs, and what it buys: a `Rel` field and both mod
/// modules on a `SELECT`-only model, with the back-reference's shape decided
/// by the declared cardinality.
#[test]
fn a_declared_relation_gives_a_view_the_relation_surface() {
    let files = generate(
        &library_with_views(),
        r#"
        dialect = "sqlite"
        [[relationships]]
        table = "author_names"
        column = "author_id"
        ref_table = "authors"
        ref_column = "id"
        cardinality = "one_to_one"
        "#,
    );

    let names = file(&files, "author_names.rs");
    assert!(names.contains("pub fn view()"), "still SELECT-only");
    assert!(!names.contains("impl keelson_models::Table for AuthorNames"));
    assert!(names.contains("pub rel: Rel"), "a view can hold relations");
    assert!(names.contains("pub author: Option<Box<super::authors::Author>>"));
    assert!(names.contains("pub mod preload"));
    assert!(names.contains("pub mod then_load"));

    // `one_to_one` makes the back-reference an `Option`, boxed because the
    // view's own belongs-to points straight back here.
    let authors = file(&files, "authors.rs");
    assert!(
        authors.contains("pub author_names: Option<Box<super::author_names::AuthorName>>"),
        "{authors}"
    );

    // `many_to_one` would have made it a `Vec` instead.
    let files = generate(
        &library_with_views(),
        r#"
        dialect = "sqlite"
        [[relationships]]
        table = "author_names"
        column = "author_id"
        ref_table = "authors"
        ref_column = "id"
        cardinality = "many_to_one"
        "#,
    );
    assert!(
        file(&files, "authors.rs")
            .contains("pub author_names: Vec<super::author_names::AuthorName>")
    );
}

fn view_config_error(toml: &str) -> String {
    let config = Config::from_toml(toml).expect("config");
    keelson_gen::generate_from_schema(&library_with_views(), &config)
        .unwrap_err()
        .to_string()
}

/// Cardinality is not optional once a view is involved: the catalog says
/// nothing about how many rows sit on each end, and guessing is the one thing
/// the generator will not do.
#[test]
fn a_relation_touching_a_view_must_declare_its_cardinality() {
    let err = view_config_error(
        r#"
        dialect = "sqlite"
        [[relationships]]
        table = "author_names"
        column = "author_id"
        ref_table = "authors"
        ref_column = "id"
        "#,
    );
    assert!(err.contains("[[relationships]] #1"), "{err}");
    assert!(err.contains("`cardinality` is required"), "{err}");
    assert!(err.contains("`author_names` is a view"), "{err}");
    assert!(err.contains("many_to_one"), "{err}");

    // Between two base tables the referenced key answers it, so it stays
    // optional there.
    let files = generate(
        &library_with_views(),
        r#"
        dialect = "sqlite"
        [[relationships]]
        table = "books"
        column = "title"
        ref_table = "authors"
        ref_column = "name"
        name = "namesake"
        "#,
    );
    assert!(file(&files, "books.rs").contains("pub namesake:"));
}

/// A typo in a relation name is a generation-time error naming the key, not a
/// compile error in generated code.
#[test]
fn a_declared_relation_naming_an_unknown_table_is_refused_by_name() {
    let err = view_config_error(
        r#"
        dialect = "sqlite"
        [[relationships]]
        table = "author_nmaes"
        column = "author_id"
        ref_table = "authors"
        ref_column = "id"
        cardinality = "one_to_one"
        "#,
    );
    assert!(err.contains("[[relationships]] #1"), "{err}");
    assert!(err.contains("`table = \"author_nmaes\"`"), "{err}");
    assert!(err.contains("names no table or view"), "{err}");
    assert!(err.contains("author_names"), "the schema is listed: {err}");

    let err = view_config_error(
        r#"
        dialect = "sqlite"
        [[relationships]]
        table = "author_names"
        column = "author_id"
        ref_table = "authores"
        ref_column = "id"
        cardinality = "one_to_one"
        "#,
    );
    assert!(err.contains("`ref_table = \"authores\"`"), "{err}");
}

/// Same for a column typo, on either end.
#[test]
fn a_declared_relation_naming_an_unknown_column_is_refused_by_name() {
    let err = view_config_error(
        r#"
        dialect = "sqlite"
        [[relationships]]
        table = "author_names"
        column = "authr_id"
        ref_table = "authors"
        ref_column = "id"
        cardinality = "one_to_one"
        "#,
    );
    assert!(err.contains("[[relationships]] #1"), "{err}");
    assert!(err.contains("`column = \"authr_id\"`"), "{err}");
    assert!(
        err.contains("names no column of view `author_names`"),
        "{err}"
    );
    assert!(
        err.contains("author_id, name"),
        "the columns are listed: {err}"
    );

    let err = view_config_error(
        r#"
        dialect = "sqlite"
        [[relationships]]
        table = "author_names"
        column = "author_id"
        ref_table = "authors"
        ref_column = "ident"
        cardinality = "one_to_one"
        "#,
    );
    assert!(err.contains("`ref_column = \"ident\"`"), "{err}");
    assert!(err.contains("names no column of table `authors`"), "{err}");
}

/// A join between columns of different types would not compile; the generator
/// says so where the mistake is, in the config.
#[test]
fn a_declared_relation_between_incomparable_columns_is_refused() {
    let err = view_config_error(
        r#"
        dialect = "sqlite"
        [[relationships]]
        table = "author_names"
        column = "name"
        ref_table = "authors"
        ref_column = "id"
        cardinality = "one_to_one"
        "#,
    );
    assert!(err.contains("[[relationships]] #1"), "{err}");
    assert!(err.contains("not comparable"), "{err}");
    assert!(err.contains("`author_names.name` is `String`"), "{err}");
    assert!(err.contains("`authors.id` is `i64`"), "{err}");
}

/// A declaration whose end the filters removed is an error too — the filter
/// took the table away, but the hand-written declaration is still there.
#[test]
fn a_declared_relation_whose_end_was_filtered_out_is_refused() {
    let err = view_config_error(
        r#"
        dialect = "sqlite"
        except = ["author_names"]
        [[relationships]]
        table = "author_names"
        column = "author_id"
        ref_table = "authors"
        ref_column = "id"
        cardinality = "one_to_one"
        "#,
    );
    assert!(err.contains("`author_names`"), "{err}");
    assert!(err.contains("only`/`except"), "{err}");

    let err = view_config_error(
        r#"
        dialect = "sqlite"
        [tables.author_names]
        except_columns = ["author_id"]
        [[relationships]]
        table = "author_names"
        column = "author_id"
        ref_table = "authors"
        ref_column = "id"
        cardinality = "one_to_one"
        "#,
    );
    assert!(err.contains("`author_names.author_id`"), "{err}");
    assert!(err.contains("except_columns"), "{err}");
}

/// Writability is the catalog's answer, not the configuration's. A key on a
/// view the engine will not write through is refused, with the three engines'
/// rules spelled out.
#[test]
fn a_key_on_a_read_only_view_is_refused_with_the_engines_rules() {
    let err = view_config_error(
        r#"
        dialect = "sqlite"
        [tables.author_names]
        key = ["author_id"]
        "#,
    );
    assert!(err.contains("[tables.author_names] key"), "{err}");
    assert!(err.contains("will not write through"), "{err}");
    assert!(err.contains("PostgreSQL"), "{err}");
    assert!(err.contains("MySQL"), "{err}");
    assert!(err.contains("INSTEAD OF"), "{err}");
}

/// And on a relation that already has a primary key it is meaningless.
#[test]
fn a_key_on_a_table_that_already_has_one_is_refused() {
    let err = view_config_error(
        r#"
        dialect = "sqlite"
        [tables.books]
        key = ["id"]
        "#,
    );
    assert!(err.contains("[tables.books] key"), "{err}");
    assert!(err.contains("already has a primary key"), "{err}");
}

/// The key has to name real, generated columns, once each.
#[test]
fn a_key_naming_a_column_that_is_not_generated_is_refused() {
    let err = view_config_error(
        r#"
        dialect = "sqlite"
        [tables.sales]
        key = ["boook_id"]
        "#,
    );
    assert!(err.contains("[tables.sales] key"), "{err}");
    assert!(err.contains("not a generated column"), "{err}");
    assert!(err.contains("book_id, total"), "{err}");

    let err = view_config_error(
        r#"
        dialect = "sqlite"
        [tables.sales]
        key = ["book_id", "book_id"]
        "#,
    );
    assert!(err.contains("listed twice"), "{err}");
}

/// The accepted case: an updatable view plus a declared key is the whole
/// write surface — and declaring a column as key asserts it is never NULL,
/// which is why the row field stops being an `Option`.
#[test]
fn a_key_on_an_updatable_view_earns_the_table_surface() {
    let files = generate(
        &library_with_views(),
        r#"
        dialect = "sqlite"
        [tables.sales]
        key = ["book_id"]
        "#,
    );
    let sales = file(&files, "sales.rs");
    assert!(sales.contains("pub fn table()"), "{sales}");
    assert!(sales.contains("impl keelson_models::Table for Sales"));
    assert!(sales.contains("type Pk = i64;"));
    assert!(
        sales.contains("pub book_id: i64,"),
        "the key is not optional"
    );
    assert!(sales.contains("pub total: Option<i64>,"), "the rest is");

    // Without the key it stays SELECT-only, updatable or not.
    let files = generate(&library_with_views(), "dialect = \"sqlite\"");
    let sales = file(&files, "sales.rs");
    assert!(sales.contains("pub fn view()"));
    assert!(!sales.contains("impl keelson_models::Table for Sales"));
}

/// Factories draw distinct values from auto-increment columns and unique
/// constraints. A view reports neither, so the combination is refused rather
/// than emitted and left to collide on the second row.
#[test]
fn factories_over_a_writable_view_are_refused_out_loud() {
    let err = view_config_error(
        r#"
        dialect = "sqlite"
        [output]
        factories = true
        [tables.sales]
        key = ["book_id"]
        "#,
    );
    assert!(err.contains("unsupported"), "{err}");
    assert!(err.contains("`sales` is a writable view"), "{err}");
    assert!(err.contains("factories"), "{err}");
}

/// A view is never in a factory's world: no template of its own, no `Parent`
/// field for a foreign key pointing at one, no `with_new_…` for a
/// back-reference from one.
#[test]
fn factories_leave_views_out_entirely() {
    let files = generate(
        &library_with_views(),
        r#"
        dialect = "sqlite"
        [output]
        factories = true
        [[relationships]]
        table = "author_names"
        column = "author_id"
        ref_table = "authors"
        ref_column = "id"
        cardinality = "many_to_one"
        "#,
    );
    let fac = file(&files, "factories.rs");
    assert!(!fac.contains("mod author_names"), "no template for a view");
    assert!(!fac.contains("mod sales"));
    assert!(
        !fac.contains("with_new_author_name"),
        "no child mod for a view back-reference: {fac}"
    );
}

/// A view on the *referenced* side with no back-reference: the view keeps no
/// `Rel` at all, and the referencing model still gets its to-one — the two
/// sides are decided independently.
#[test]
fn a_suppressed_back_reference_leaves_the_view_without_a_rel() {
    let files = generate(
        &library_with_views(),
        r#"
        dialect = "sqlite"
        [[relationships]]
        table = "books"
        column = "id"
        ref_table = "sales"
        ref_column = "book_id"
        name = "revenue"
        cardinality = "one_to_one"
        no_back_reference = true
        "#,
    );
    let books = file(&files, "books.rs");
    assert!(books.contains("pub revenue: Option<Box<super::sales::Sale>>"));
    assert!(books.contains("pub fn revenue()"), "preload and then_load");

    let sales = file(&files, "sales.rs");
    assert!(!sales.contains("pub struct Rel"), "{sales}");
    assert!(!sales.contains("pub rel:"), "{sales}");
}
