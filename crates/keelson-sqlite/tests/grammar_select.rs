//! `SELECT`, checked against SQLite's grammar, a real SQLite, and an expectation
//! derived from <https://www.sqlite.org/lang_select.html>.
//!
//! ```text
//! [ WITH [ RECURSIVE ] common-table-expression [, ...] ]
//! select-core [ compound-operator select-core ]*
//! [ ORDER BY ordering-term [, ...] ]
//! [ LIMIT expr [ ( OFFSET | , ) expr ] ]
//!
//! select-core:
//!     SELECT [ DISTINCT | ALL ] result-column [, ...]
//!         [ FROM table-or-subquery [, ...] | join-clause ]
//!         [ WHERE expr ] [ GROUP BY expr [, ...] [ HAVING expr ] ]
//!         [ WINDOW window-name AS window-defn [, ...] ]
//!   | VALUES ( expr [, ...] ) [, ...]
//! ```
//!
//! Every statement names only the tables of `tests/schema/sqlite.sql`, so the
//! engine tier resolves them and reports semantic errors the grammar cannot see.

use keelson_sqlcheck::{Dialect, assert_sql};
use keelson_sqlite as sqlite;
use keelson_sqlite::{
    Chain, Expr, Query, SqliteOps, Value, arg, args, case_, cast, f, frame, named, not, or,
    placeholders, quote, raw, s, select, subquery, window,
};

/// Build, check both tiers and the expectation, and hand back the arguments so a
/// test can assert them directly.
#[track_caller]
fn built(q: impl Query, expected: &str) -> Vec<Value> {
    let (sql, args) = q.build().expect("the query must build");
    assert_sql(Dialect::Sqlite, &sql, expected);
    args
}

// ---------------------------------------------------------------------------
// The select core
// ---------------------------------------------------------------------------

/// `FROM` is optional in the grammar, but a `*` result-column has nothing to expand
/// against without one and a real SQLite answers *no tables specified* — so the
/// shortest statement the engine tier accepts still names a table.
#[test]
fn an_empty_select_expands_to_a_star_over_the_from_item() {
    built(
        sqlite::select(select::from(quote("users"))),
        r#"SELECT * FROM "users""#,
    );
}

#[test]
fn result_columns_are_comma_separated() {
    built(
        sqlite::select((
            select::columns((quote("id"), quote("name"))),
            select::from(quote("users")),
        )),
        r#"SELECT "id", "name" FROM "users""#,
    );
}

#[test]
fn a_raw_table_name_is_written_verbatim() {
    // Progressive enhancement: an unquoted `&str` is raw SQL, which is legal here
    // because none of the schema's names is a reserved word.
    built(sqlite::select(select::from("users")), "SELECT * FROM users");
}

#[test]
fn distinct_precedes_the_result_columns() {
    built(
        sqlite::select((
            select::distinct(),
            select::columns(quote("age")),
            select::from(quote("users")),
        )),
        r#"SELECT DISTINCT "age" FROM "users""#,
    );
    built(
        sqlite::select((select::distinct(), select::from(quote("users")))),
        r#"SELECT DISTINCT * FROM "users""#,
    );
}

#[test]
fn a_table_alias_qualifies_its_columns() {
    built(
        sqlite::select((
            select::columns(quote(("u", "email"))),
            select::from(quote("users")).as_("u"),
        )),
        r#"SELECT "u"."email" FROM "users" AS "u""#,
    );
}

#[test]
fn a_result_column_may_be_aliased() {
    built(
        sqlite::select((
            select::columns((quote("id").as_("pk"), f("count", "*").as_("n"))),
            select::from(quote("posts")),
        )),
        r#"SELECT "id" AS "pk", count(*) AS "n" FROM "posts""#,
    );
}

#[test]
fn preload_columns_render_after_the_selected_ones() {
    built(
        sqlite::select((
            select::columns(quote(("p", "id"))),
            select::preload_columns(quote(("u", "id"))),
            select::from(quote("posts")).as_("p"),
            select::inner_join(quote("users"))
                .as_("u")
                .on_eq(quote(("u", "id")), quote(("p", "user_id"))),
        )),
        r#"SELECT "p"."id", "u"."id" FROM "posts" AS "p" INNER JOIN "users" AS "u" ON ("u"."id" = "p"."user_id")"#,
    );
}

// ---------------------------------------------------------------------------
// WHERE
// ---------------------------------------------------------------------------

#[test]
fn a_bound_condition_uses_a_numbered_placeholder() {
    let args = built(
        sqlite::select((
            select::columns(quote("id")),
            select::from(quote("users")),
            select::where_(quote("age").gte(arg(21i32))),
        )),
        r#"SELECT "id" FROM "users" WHERE ("age" >= ?1)"#,
    );
    assert_eq!(args, vec![Value::I32(21)]);
}

#[test]
fn several_conditions_are_and_joined() {
    let args = built(
        sqlite::select((
            select::from(quote("users")),
            select::where_(quote("age").gte(arg(21i32))),
            select::where_(quote("is_active").eq(arg(1i32))),
            select::where_(quote("email").is_not_null()),
        )),
        r#"SELECT * FROM "users" WHERE ("age" >= ?1) AND ("is_active" = ?2) AND ("email" IS NOT NULL)"#,
    );
    assert_eq!(args, vec![Value::I32(21), Value::I32(1)]);
}

#[test]
fn a_disjunction_is_one_condition() {
    built(
        sqlite::select((
            select::from(quote("posts")),
            select::where_(or((
                quote("status").eq(s("draft")),
                quote("status").is_null(),
            ))),
        )),
        r#"SELECT * FROM "posts" WHERE (("status" = 'draft') OR ("status" IS NULL))"#,
    );
}

#[test]
fn not_takes_one_pair_of_parentheses() {
    built(
        sqlite::select((
            select::from(quote("users")),
            select::where_(not(quote("is_active").eq(1))),
        )),
        r#"SELECT * FROM "users" WHERE NOT ("is_active" = 1)"#,
    );
}

#[test]
fn in_with_a_bound_list() {
    let bound = built(
        sqlite::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::where_(quote("id").in_(args([1i32, 2, 3]))),
        )),
        r#"SELECT "id" FROM "posts" WHERE ("id" IN (?1, ?2, ?3))"#,
    );
    assert_eq!(
        bound,
        vec![Value::I32(1), Value::I32(2), Value::I32(3)],
        "one argument per placeholder, in write order"
    );
}

#[test]
fn between_keeps_its_three_part_shape() {
    let args = built(
        sqlite::select((
            select::from(quote("users")),
            select::where_(quote("age").between(arg(18i32), arg(65i32))),
        )),
        r#"SELECT * FROM "users" WHERE ("age" BETWEEN ?1 AND ?2)"#,
    );
    assert_eq!(args, vec![Value::I32(18), Value::I32(65)]);
}

/// `expr GLOB pattern` and `expr LIKE pattern ESCAPE expr`, from
/// <https://www.sqlite.org/lang_expr.html#like>.
#[test]
fn the_sqlite_only_pattern_operators() {
    built(
        sqlite::select((
            select::from(quote("users")),
            select::where_(quote("name").glob(s("Ada*"))),
        )),
        r#"SELECT * FROM "users" WHERE ("name" GLOB 'Ada*')"#,
    );
    built(
        sqlite::select((
            select::from(quote("users")),
            select::where_(quote("name").like_escape(s("100\\%"), s("\\"))),
        )),
        r#"SELECT * FROM "users" WHERE ("name" LIKE '100\%' ESCAPE '\')"#,
    );
}

/// `expr IS expr` is SQLite's null-safe equality, and predates the standard
/// `IS NOT DISTINCT FROM` that SQLite 3.39 also accepts.
#[test]
fn is_and_is_not_are_null_safe_comparisons() {
    built(
        sqlite::select((
            select::from(quote("users")),
            select::where_(quote("email").is_(quote("name"))),
            select::where_(quote("age").is_not(raw("NULL"))),
        )),
        r#"SELECT * FROM "users" WHERE ("email" IS "name") AND ("age" IS NOT NULL)"#,
    );
}

#[test]
fn the_json_arrow_operators() {
    built(
        sqlite::select((
            select::columns(quote("body").json_get_text(s("$.title"))),
            select::from(quote("comments")),
            select::where_(quote("body").json_get(s("$.tags")).is_not_null()),
        )),
        r#"SELECT ("body" ->> '$.title') FROM "comments" WHERE (("body" -> '$.tags') IS NOT NULL)"#,
    );
}

#[test]
fn a_named_parameter_binds_nothing_and_takes_no_position() {
    let args = built(
        sqlite::select((
            select::columns(quote("id")),
            select::from(quote("users")),
            select::where_(quote("age").gt(named("min_age"))),
            select::where_(quote("id").eq(arg(7i32))),
        )),
        r#"SELECT "id" FROM "users" WHERE ("age" > :min_age) AND ("id" = ?1)"#,
    );
    assert_eq!(
        args,
        vec![Value::I32(7)],
        "a named parameter contributes no argument, so the positional one is still ?1"
    );
}

#[test]
fn unbound_placeholders_keep_their_positions() {
    let args = built(
        sqlite::select((
            select::columns(placeholders(2)),
            select::from(quote("users")),
        )),
        "SELECT ?1, ?2 FROM \"users\"",
    );
    assert_eq!(args, vec![Value::Null, Value::Null]);
}

#[test]
fn cast_and_case_are_ordinary_expressions() {
    built(
        sqlite::select((
            select::columns((
                cast(quote("age"), "TEXT"),
                case_()
                    .when(quote("is_active").eq(1), s("yes"))
                    .else_(s("no")),
            )),
            select::from(quote("users")),
        )),
        // `CAST(…)` is self-delimiting and so is not wrapped; a `CASE` is not, and
        // `CaseBuilder` applies the parenthesisation rule to its own result.
        r#"SELECT CAST("age" AS TEXT), (CASE WHEN ("is_active" = 1) THEN 'yes' ELSE 'no' END) FROM "users""#,
    );
}

// ---------------------------------------------------------------------------
// GROUP BY / HAVING
// ---------------------------------------------------------------------------

#[test]
fn group_by_and_having() {
    let args = built(
        sqlite::select((
            select::columns((quote("age"), f("count", "*"))),
            select::from(quote("users")),
            select::group_by(quote("age")),
            // `Expr::func` rather than `f` because a condition is a chain, and a
            // `Function` is a builder that ends in one rather than being one.
            select::having(Expr::func("count", "*").gt(arg(1i32))),
        )),
        r#"SELECT "age", count(*) FROM "users" GROUP BY "age" HAVING (count(*) > ?1)"#,
    );
    assert_eq!(args, vec![Value::I32(1)]);
}

#[test]
fn several_grouping_expressions_accumulate() {
    built(
        sqlite::select((
            select::columns(f("count", "*")),
            select::from(quote("posts")),
            select::group_by(quote("user_id")),
            select::group_by(quote("status")),
        )),
        r#"SELECT count(*) FROM "posts" GROUP BY "user_id", "status""#,
    );
}

/// SQLite's `HAVING` hangs off the `GROUP BY` in the diagram but is accepted
/// without one, where it filters the single implicit group.
#[test]
fn having_without_group_by() {
    built(
        sqlite::select((
            select::columns(f("count", "*")),
            select::from(quote("users")),
            select::having(Expr::func("count", "*").gt(1)),
        )),
        r#"SELECT count(*) FROM "users" HAVING (count(*) > 1)"#,
    );
}

// ---------------------------------------------------------------------------
// Joins — https://www.sqlite.org/syntax/join-clause.html
// ---------------------------------------------------------------------------

#[test]
fn an_inner_join_with_an_on_condition() {
    built(
        sqlite::select((
            select::columns((quote(("u", "name")), quote(("p", "title")))),
            select::from(quote("users")).as_("u"),
            select::inner_join(quote("posts"))
                .as_("p")
                .on_eq(quote(("p", "user_id")), quote(("u", "id"))),
        )),
        r#"SELECT "u"."name", "p"."title" FROM "users" AS "u" INNER JOIN "posts" AS "p" ON ("p"."user_id" = "u"."id")"#,
    );
}

#[test]
fn two_on_conditions_become_one_conjunction() {
    let args = built(
        sqlite::select((
            select::from(quote("posts")).as_("p"),
            select::left_join(quote("comments"))
                .as_("c")
                .on_eq(quote(("c", "post_id")), quote(("p", "id")))
                .on(quote(("c", "user_id")).eq(arg(1i32))),
        )),
        r#"SELECT * FROM "posts" AS "p" LEFT JOIN "comments" AS "c" ON ("c"."post_id" = "p"."id") AND ("c"."user_id" = ?1)"#,
    );
    assert_eq!(args, vec![Value::I32(1)]);
}

/// `USING ( column-name [, ...] )` merges equally named columns; both tables must
/// have every one of them, which is what the engine tier checks.
#[test]
fn a_join_using_named_columns() {
    built(
        sqlite::select((
            select::columns(quote("id")),
            select::from(quote("users")),
            select::inner_join(quote("comments")).using(["id", "created_at"]),
        )),
        r#"SELECT "id" FROM "users" INNER JOIN "comments" USING ("id", "created_at")"#,
    );
}

/// SQLite gained `RIGHT JOIN` and `FULL JOIN` in 3.39 (2022). Both are checked
/// against the linked-in engine, so this test is also the version assertion.
#[test]
fn right_and_full_outer_joins() {
    built(
        sqlite::select((
            select::columns(quote(("u", "id"))),
            select::from(quote("users")).as_("u"),
            select::right_join(quote("posts"))
                .as_("p")
                .on_eq(quote(("p", "user_id")), quote(("u", "id"))),
        )),
        r#"SELECT "u"."id" FROM "users" AS "u" RIGHT JOIN "posts" AS "p" ON ("p"."user_id" = "u"."id")"#,
    );
    built(
        sqlite::select((
            select::columns(quote(("u", "id"))),
            select::from(quote("users")).as_("u"),
            select::full_join(quote("posts"))
                .as_("p")
                .on_eq(quote(("p", "user_id")), quote(("u", "id"))),
        )),
        r#"SELECT "u"."id" FROM "users" AS "u" FULL JOIN "posts" AS "p" ON ("p"."user_id" = "u"."id")"#,
    );
}

/// The difference from PostgreSQL that is easiest to get wrong: in SQLite the
/// `join-constraint` is a production of its own, so a `CROSS JOIN` takes an `ON`.
/// Writing `CROSS JOIN` rather than `JOIN` is how the planner is told not to
/// reorder the two tables.
#[test]
fn a_cross_join_takes_a_condition_in_sqlite() {
    built(
        sqlite::select((
            select::columns(quote(("u", "id"))),
            select::from(quote("users")).as_("u"),
            select::cross_join(quote("posts"))
                .as_("p")
                .on_eq(quote(("p", "user_id")), quote(("u", "id"))),
        )),
        r#"SELECT "u"."id" FROM "users" AS "u" CROSS JOIN "posts" AS "p" ON ("p"."user_id" = "u"."id")"#,
    );
}

#[test]
fn a_cross_join_without_a_condition() {
    built(
        sqlite::select((
            select::columns(quote(("users", "id"))),
            select::from(quote("users")),
            select::cross_join(quote("tags")),
        )),
        r#"SELECT "users"."id" FROM "users" CROSS JOIN "tags""#,
    );
}

#[test]
fn natural_precedes_the_join_operator() {
    built(
        sqlite::select((
            select::columns(quote("post_id")),
            select::from(quote("post_tags")),
            select::left_join(quote("comments")).natural(),
        )),
        r#"SELECT "post_id" FROM "post_tags" NATURAL LEFT JOIN "comments""#,
    );
}

#[test]
fn joins_chain_left_to_right() {
    built(
        sqlite::select((
            select::columns(f("count", "*")),
            select::from(quote("posts")).as_("p"),
            select::inner_join(quote("post_tags"))
                .as_("pt")
                .on_eq(quote(("pt", "post_id")), quote(("p", "id"))),
            select::inner_join(quote("tags"))
                .as_("t")
                .on_eq(quote(("t", "id")), quote(("pt", "tag_id"))),
        )),
        r#"SELECT count(*) FROM "posts" AS "p" INNER JOIN "post_tags" AS "pt" ON ("pt"."post_id" = "p"."id") INNER JOIN "tags" AS "t" ON ("t"."id" = "pt"."tag_id")"#,
    );
}

/// A `,` is one of SQLite's `join-operator`s, so a comma list and a join clause are
/// the same production and mix freely.
#[test]
fn a_comma_separated_from_list() {
    built(
        sqlite::select((
            select::columns(quote(("u", "id"))),
            select::from(quote("users")).as_("u"),
            select::from_also(quote("posts")).as_("p"),
            select::where_(quote(("p", "user_id")).eq(quote(("u", "id")))),
        )),
        r#"SELECT "u"."id" FROM "users" AS "u", "posts" AS "p" WHERE ("p"."user_id" = "u"."id")"#,
    );
}

#[test]
fn a_join_and_a_further_comma_item_together() {
    built(
        sqlite::select((
            select::columns(quote(("u", "id"))),
            select::from(quote("users")).as_("u"),
            select::inner_join(quote("posts"))
                .as_("p")
                .on_eq(quote(("p", "user_id")), quote(("u", "id"))),
            select::from_also(quote("tags")).as_("t"),
        )),
        r#"SELECT "u"."id" FROM "users" AS "u" INNER JOIN "posts" AS "p" ON ("p"."user_id" = "u"."id"), "tags" AS "t""#,
    );
}

// ---------------------------------------------------------------------------
// INDEXED BY — https://www.sqlite.org/syntax/qualified-table-name.html
// ---------------------------------------------------------------------------

/// `INDEXED BY index-name` is a hard constraint on the planner rather than a hint:
/// SQLite raises an error if the named index cannot be used. The index named here
/// is the one SQLite creates for `tags.name UNIQUE` in the shared schema.
#[test]
fn indexed_by_names_the_index_the_planner_must_use() {
    built(
        sqlite::select((
            select::columns(quote("id")),
            select::from(quote("tags")).indexed_by("sqlite_autoindex_tags_1"),
            select::where_(quote("name").eq(arg("rust"))),
        )),
        r#"SELECT "id" FROM "tags" INDEXED BY "sqlite_autoindex_tags_1" WHERE ("name" = ?1)"#,
    );
}

#[test]
fn not_indexed_follows_the_alias() {
    built(
        sqlite::select((
            select::columns(quote(("t", "id"))),
            select::from(quote("tags")).as_("t").not_indexed(),
            select::where_(quote(("t", "name")).eq(arg("rust"))),
        )),
        r#"SELECT "t"."id" FROM "tags" AS "t" NOT INDEXED WHERE ("t"."name" = ?1)"#,
    );
}

#[test]
fn an_index_directive_on_a_joined_table() {
    built(
        sqlite::select((
            select::columns(quote(("pt", "post_id"))),
            select::from(quote("post_tags")).as_("pt"),
            select::inner_join(quote("tags"))
                .as_("t")
                .not_indexed()
                .on_eq(quote(("t", "id")), quote(("pt", "tag_id"))),
        )),
        r#"SELECT "pt"."post_id" FROM "post_tags" AS "pt" INNER JOIN "tags" AS "t" NOT INDEXED ON ("t"."id" = "pt"."tag_id")"#,
    );
}

// ---------------------------------------------------------------------------
// Sub-queries
// ---------------------------------------------------------------------------

#[test]
fn a_parenthesised_sub_query_as_a_from_item() {
    let inner = sqlite::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        select::where_(quote("is_active").eq(arg(1i32))),
    ));
    built(
        sqlite::select((
            select::columns(quote(("t", "id"))),
            select::from(subquery(inner)).as_("t"),
        )),
        r#"SELECT "t"."id" FROM (SELECT "id" FROM "users" WHERE ("is_active" = ?1)) AS "t""#,
    );
}

/// Unlike PostgreSQL, SQLite does not require an alias on a `FROM` sub-query.
#[test]
fn a_from_sub_query_needs_no_alias() {
    let inner = sqlite::select((select::columns(quote("id")), select::from(quote("users"))));
    built(
        sqlite::select((select::columns(quote("id")), select::from(subquery(inner)))),
        r#"SELECT "id" FROM (SELECT "id" FROM "users")"#,
    );
}

#[test]
fn a_scalar_sub_query_in_the_result_columns() {
    let inner = sqlite::select((
        select::columns(f("max", quote("id"))),
        select::from(quote("posts")),
    ));
    built(
        sqlite::select((
            select::columns((quote("id"), subquery(inner).as_("newest"))),
            select::from(quote("users")),
        )),
        r#"SELECT "id", (SELECT max("id") FROM "posts") AS "newest" FROM "users""#,
    );
}

#[test]
fn a_sub_query_on_the_right_of_in() {
    let inner = sqlite::select((
        select::columns(quote("user_id")),
        select::from(quote("posts")),
        select::where_(quote("status").eq(arg("published"))),
    ));
    let args = built(
        sqlite::select((
            select::columns(quote("id")),
            select::from(quote("users")),
            select::where_(quote("id").in_(subquery(inner))),
        )),
        r#"SELECT "id" FROM "users" WHERE ("id" IN ((SELECT "user_id" FROM "posts" WHERE ("status" = ?1))))"#,
    );
    assert_eq!(args, vec![Value::Text("published".into())]);
}

#[test]
fn a_correlated_exists_sub_query() {
    let inner = sqlite::select((
        select::columns(raw("1")),
        select::from(quote("posts")),
        select::where_(quote(("posts", "user_id")).eq(quote(("users", "id")))),
    ));
    built(
        sqlite::select((
            select::columns(quote("id")),
            select::from(quote("users")),
            select::where_(keelson_sqlite::Expr::prefix("EXISTS", subquery(inner))),
        )),
        r#"SELECT "id" FROM "users" WHERE EXISTS (SELECT 1 FROM "posts" WHERE ("posts"."user_id" = "users"."id"))"#,
    );
}

/// Placeholders belong to the writer, not to the query, so a sub-query continues
/// the outer numbering rather than restarting it.
#[test]
fn nesting_continues_the_placeholder_run() {
    let inner = sqlite::select((
        select::columns(quote("user_id")),
        select::from(quote("posts")),
        select::where_(quote("views").gt(arg(10i32))),
    ));
    let args = built(
        sqlite::select((
            select::columns(quote("id")),
            select::from(quote("users")),
            select::where_(quote("age").gte(arg(21i32))),
            select::where_(quote("id").in_(subquery(inner))),
            select::where_(quote("name").ne(arg("root"))),
        )),
        r#"SELECT "id" FROM "users" WHERE ("age" >= ?1) AND ("id" IN ((SELECT "user_id" FROM "posts" WHERE ("views" > ?2)))) AND ("name" <> ?3)"#,
    );
    assert_eq!(
        args,
        vec![Value::I32(21), Value::I32(10), Value::Text("root".into())]
    );
}

/// A table-valued function is just an expression in the from slot, so SQLite needs
/// no counterpart to PostgreSQL's `ROWS FROM (…)` and has no `from_function` mod.
#[test]
fn a_table_valued_function_as_a_from_item() {
    built(
        sqlite::select((
            select::columns(quote(("t", "name"))),
            select::from(f("pragma_table_info", s("users"))).as_("t"),
        )),
        r#"SELECT "t"."name" FROM pragma_table_info('users') AS "t""#,
    );
}

// ---------------------------------------------------------------------------
// WITH
// ---------------------------------------------------------------------------

#[test]
fn a_common_table_expression() {
    let body = sqlite::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::where_(quote("views").gt(arg(100i32))),
    ));
    built(
        sqlite::select((
            select::with("popular", body),
            select::columns(quote("id")),
            select::from(quote("popular")),
        )),
        r#"WITH "popular" AS (SELECT "id" FROM "posts" WHERE ("views" > ?1)) SELECT "id" FROM "popular""#,
    );
}

#[test]
fn a_cte_may_name_its_output_columns() {
    let body = sqlite::select((select::columns(quote("id")), select::from(quote("posts"))));
    built(
        sqlite::select((
            select::with("p", body).columns(["pid"]),
            select::columns(quote("pid")),
            select::from(quote("p")),
        )),
        r#"WITH "p" ("pid") AS (SELECT "id" FROM "posts") SELECT "pid" FROM "p""#,
    );
}

/// `AS [ [ NOT ] MATERIALIZED ] ( select-stmt )` — SQLite 3.35 and later.
#[test]
fn the_materialisation_hints() {
    let body = sqlite::select((select::columns(quote("id")), select::from(quote("posts"))));
    built(
        sqlite::select((
            select::with("p", body.clone()).materialized(),
            select::columns(quote("id")),
            select::from(quote("p")),
        )),
        r#"WITH "p" AS MATERIALIZED (SELECT "id" FROM "posts") SELECT "id" FROM "p""#,
    );
    built(
        sqlite::select((
            select::with("p", body).not_materialized(),
            select::columns(quote("id")),
            select::from(quote("p")),
        )),
        r#"WITH "p" AS NOT MATERIALIZED (SELECT "id" FROM "posts") SELECT "id" FROM "p""#,
    );
}

#[test]
fn two_ctes_are_comma_separated() {
    let a = sqlite::select((select::columns(quote("id")), select::from(quote("users"))));
    let b = sqlite::select((select::columns(quote("id")), select::from(quote("posts"))));
    built(
        sqlite::select((
            select::with("u", a),
            select::with("p", b),
            select::columns(quote(("u", "id"))),
            select::from(quote("u")),
            select::cross_join(quote("p")),
        )),
        r#"WITH "u" AS (SELECT "id" FROM "users"), "p" AS (SELECT "id" FROM "posts") SELECT "u"."id" FROM "u" CROSS JOIN "p""#,
    );
}

/// The idiom `WITH RECURSIVE` exists for: a `VALUES` seed core compounded with a
/// self-referencing `SELECT`.
#[test]
fn a_recursive_cte_seeded_by_a_values_core() {
    let step = sqlite::select((
        select::columns(quote("x").plus(1)),
        select::from(quote("counter")),
        select::where_(quote("x").lt(5)),
    ));
    let body = sqlite::select((select::values(1), select::union_all(step)));
    built(
        sqlite::select((
            select::recursive(true),
            select::with("counter", body).columns(["x"]),
            select::columns(quote("x")),
            select::from(quote("counter")),
        )),
        r#"WITH RECURSIVE "counter" ("x") AS (VALUES (1) UNION ALL SELECT ("x" + 1) FROM "counter" WHERE ("x" < 5)) SELECT "x" FROM "counter""#,
    );
}

// ---------------------------------------------------------------------------
// Compound SELECTs
// ---------------------------------------------------------------------------

/// The operand is a bare `select-core`: SQLite's `compound-select-stmt` has no
/// parentheses anywhere, and adding them is a syntax error.
#[test]
fn every_compound_operator() {
    let posts = || sqlite::select((select::columns(quote("id")), select::from(quote("posts"))));

    built(
        sqlite::select((
            select::columns(quote("id")),
            select::from(quote("users")),
            select::union(posts()),
        )),
        r#"SELECT "id" FROM "users" UNION SELECT "id" FROM "posts""#,
    );
    built(
        sqlite::select((
            select::columns(quote("id")),
            select::from(quote("users")),
            select::union_all(posts()),
        )),
        r#"SELECT "id" FROM "users" UNION ALL SELECT "id" FROM "posts""#,
    );
    built(
        sqlite::select((
            select::columns(quote("id")),
            select::from(quote("users")),
            select::intersect(posts()),
        )),
        r#"SELECT "id" FROM "users" INTERSECT SELECT "id" FROM "posts""#,
    );
    built(
        sqlite::select((
            select::columns(quote("id")),
            select::from(quote("users")),
            select::except(posts()),
        )),
        r#"SELECT "id" FROM "users" EXCEPT SELECT "id" FROM "posts""#,
    );
}

#[test]
fn compound_operands_chain_left_to_right() {
    let posts = sqlite::select((select::columns(quote("id")), select::from(quote("posts"))));
    let comments = sqlite::select((
        select::columns(quote("id")),
        select::from(quote("comments")),
    ));
    built(
        sqlite::select((
            select::columns(quote("id")),
            select::from(quote("users")),
            select::union(posts),
            select::intersect(comments),
        )),
        r#"SELECT "id" FROM "users" UNION SELECT "id" FROM "posts" INTERSECT SELECT "id" FROM "comments""#,
    );
}

/// The `ORDER BY` and `LIMIT` after the last operand belong to the whole compound,
/// and there is nowhere else they could go — which is why this dialect has one set
/// of them where PostgreSQL has two.
#[test]
fn the_tail_clauses_of_a_compound_follow_the_last_operand() {
    let posts = sqlite::select((select::columns(quote("id")), select::from(quote("posts"))));
    built(
        sqlite::select((
            select::columns(quote("id")),
            select::from(quote("users")),
            select::union_all(posts),
            select::order_by(raw("1")).desc(),
            select::limit(10),
            select::offset(5),
        )),
        r#"SELECT "id" FROM "users" UNION ALL SELECT "id" FROM "posts" ORDER BY 1 DESC LIMIT 10 OFFSET 5"#,
    );
}

#[test]
fn a_sub_query_is_how_an_interior_compound_is_parenthesised() {
    let inner = sqlite::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::union(sqlite::select((
            select::columns(quote("id")),
            select::from(quote("comments")),
        ))),
    ));
    built(
        sqlite::select((select::columns(quote("id")), select::from(subquery(inner)))),
        r#"SELECT "id" FROM (SELECT "id" FROM "posts" UNION SELECT "id" FROM "comments")"#,
    );
}

// ---------------------------------------------------------------------------
// VALUES as a select-core
// ---------------------------------------------------------------------------

#[test]
fn a_values_statement_stands_alone() {
    let args = built(
        sqlite::select(select::rows([(arg(1i32), arg("a")), (arg(2i32), arg("b"))])),
        "VALUES (?1, ?2), (?3, ?4)",
    );
    assert_eq!(
        args,
        vec![
            Value::I32(1),
            Value::Text("a".into()),
            Value::I32(2),
            Value::Text("b".into())
        ]
    );
}

#[test]
fn a_values_core_compounds_with_a_select_core() {
    let users = sqlite::select((select::columns(quote("id")), select::from(quote("users"))));
    built(
        sqlite::select((select::values(0), select::union_all(users))),
        r#"VALUES (0) UNION ALL SELECT "id" FROM "users""#,
    );
}

// ---------------------------------------------------------------------------
// ORDER BY / LIMIT
// ---------------------------------------------------------------------------

/// `expr [ COLLATE collation-name ] [ ASC | DESC ] [ NULLS { FIRST | LAST } ]` —
/// the order of the three modifiers is the diagram's.
#[test]
fn an_ordering_term_carries_collation_direction_and_null_placement() {
    built(
        sqlite::select((
            select::columns(quote("name")),
            select::from(quote("users")),
            select::order_by(quote("name")).collate("NOCASE").asc(),
            select::order_by(quote("email")).desc().nulls_last(),
            select::order_by(quote("age")).nulls_first(),
        )),
        r#"SELECT "name" FROM "users" ORDER BY "name" COLLATE "NOCASE" ASC, "email" DESC NULLS LAST, "age" NULLS FIRST"#,
    );
}

#[test]
fn ordering_by_a_result_column_ordinal() {
    built(
        sqlite::select((
            select::columns((quote("id"), quote("name"))),
            select::from(quote("users")),
            select::order_by(2).desc(),
        )),
        r#"SELECT "id", "name" FROM "users" ORDER BY 2 DESC"#,
    );
}

#[test]
fn a_plain_limit_is_a_literal_and_a_bound_one_is_a_placeholder() {
    built(
        sqlite::select((select::from(quote("users")), select::limit(10))),
        r#"SELECT * FROM "users" LIMIT 10"#,
    );
    let args = built(
        sqlite::select((
            select::from(quote("users")),
            select::limit(arg(10i64)),
            select::offset(arg(20i64)),
        )),
        r#"SELECT * FROM "users" LIMIT ?1 OFFSET ?2"#,
    );
    assert_eq!(args, vec![Value::I64(10), Value::I64(20)]);
}

/// SQLite's `LIMIT expr` takes a whole expression, so a sub-select works — which is
/// not true of every dialect.
#[test]
fn a_limit_may_be_a_sub_select() {
    let inner = sqlite::select((
        select::columns(f("count", "*")),
        select::from(quote("posts")),
    ));
    built(
        sqlite::select((select::from(quote("users")), select::limit(subquery(inner)))),
        r#"SELECT * FROM "users" LIMIT (SELECT count(*) FROM "posts")"#,
    );
}

// ---------------------------------------------------------------------------
// Window functions
// ---------------------------------------------------------------------------

/// `OVER window-name` is a *reference* to a `WINDOW` entry; the parenthesised form
/// would copy it, which SQLite refuses when the named window has a frame.
#[test]
fn a_named_window_referenced_by_name() {
    built(
        sqlite::select((
            select::columns(f("row_number", ()).over_name("w").as_("rn")),
            select::from(quote("posts")),
            select::window(
                "w",
                (
                    window::partition_by(quote("user_id")),
                    window::order_by(quote("views")),
                ),
            ),
        )),
        r#"SELECT row_number() OVER "w" AS "rn" FROM "posts" WINDOW "w" AS (PARTITION BY "user_id" ORDER BY "views")"#,
    );
}

#[test]
fn two_named_windows_are_comma_separated() {
    built(
        sqlite::select((
            select::columns((
                f("count", "*").over_name("w"),
                f("sum", quote("views")).over_name("v"),
            )),
            select::from(quote("posts")),
            select::window("w", window::order_by(quote("id"))),
            select::window("v", window::partition_by(quote("status"))),
        )),
        r#"SELECT count(*) OVER "w", sum("views") OVER "v" FROM "posts" WINDOW "w" AS (ORDER BY "id"), "v" AS (PARTITION BY "status")"#,
    );
}

#[test]
fn an_inline_window_definition() {
    built(
        sqlite::select((
            select::columns(f("avg", quote("views")).over((
                window::partition_by(quote("user_id")),
                window::order_by(quote("id")),
            ))),
            select::from(quote("posts")),
        )),
        r#"SELECT avg("views") OVER (PARTITION BY "user_id" ORDER BY "id") FROM "posts""#,
    );
}

#[test]
fn an_empty_over_means_the_whole_partition() {
    built(
        sqlite::select((
            select::columns(f("count", "*").over(())),
            select::from(quote("users")),
        )),
        r#"SELECT count(*) OVER () FROM "users""#,
    );
}

/// `BETWEEN` appears exactly when there is an end bound, and the offset may be a
/// bound argument.
#[test]
fn a_rows_frame_with_a_bound_offset() {
    let args = built(
        sqlite::select((
            select::columns(f("sum", quote("views")).over((
                window::order_by(quote("id")),
                frame::rows(),
                frame::from_preceding(arg(3i32)),
                frame::to_current_row(),
            ))),
            select::from(quote("posts")),
        )),
        r#"SELECT sum("views") OVER (ORDER BY "id" ROWS BETWEEN ?1 PRECEDING AND CURRENT ROW) FROM "posts""#,
    );
    assert_eq!(args, vec![Value::I32(3)]);
}

/// With no end bound there is no `BETWEEN`; and the mode defaults to `RANGE`, so
/// setting only an exclusion still renders a complete frame.
#[test]
fn a_frame_with_only_a_start_bound_omits_between() {
    built(
        sqlite::select((
            select::columns(
                f("count", "*").over((window::order_by(quote("id")), frame::exclude_no_others())),
            ),
            select::from(quote("posts")),
        )),
        r#"SELECT count(*) OVER (ORDER BY "id" RANGE UNBOUNDED PRECEDING EXCLUDE NO OTHERS) FROM "posts""#,
    );
}

#[test]
fn a_groups_frame_with_an_exclusion() {
    built(
        sqlite::select((
            select::columns(f("count", "*").over((
                window::order_by(quote("status")),
                frame::groups(),
                frame::from_preceding(1),
                frame::to_following(1),
                frame::exclude_ties(),
            ))),
            select::from(quote("posts")),
        )),
        r#"SELECT count(*) OVER (ORDER BY "status" GROUPS BETWEEN 1 PRECEDING AND 1 FOLLOWING EXCLUDE TIES) FROM "posts""#,
    );
}

#[test]
fn a_range_frame_spanning_the_whole_partition() {
    built(
        sqlite::select((
            select::columns(f("sum", quote("views")).over((
                window::order_by(quote("id")),
                frame::range(),
                frame::from_unbounded_preceding(),
                frame::to_unbounded_following(),
            ))),
            select::from(quote("posts")),
        )),
        r#"SELECT sum("views") OVER (ORDER BY "id" RANGE BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING) FROM "posts""#,
    );
}

/// A window definition may name a base window, which copies its `PARTITION BY`.
#[test]
fn a_window_definition_based_on_another() {
    built(
        sqlite::select((
            select::columns(
                f("count", "*").over((window::based_on("w"), window::order_by(quote("id")))),
            ),
            select::from(quote("posts")),
            select::window("w", window::partition_by(quote("user_id"))),
        )),
        r#"SELECT count(*) OVER ("w" ORDER BY "id") FROM "posts" WINDOW "w" AS (PARTITION BY "user_id")"#,
    );
}

// ---------------------------------------------------------------------------
// Aggregate function decorations
// ---------------------------------------------------------------------------

#[test]
fn a_distinct_aggregate() {
    built(
        sqlite::select((
            select::columns(f("count", quote("age")).distinct()),
            select::from(quote("users")),
        )),
        r#"SELECT count(DISTINCT "age") FROM "users""#,
    );
}

/// `FILTER ( WHERE expr )` — SQLite 3.30 and later. Several conditions become one
/// conjunction inside a single `WHERE`.
#[test]
fn a_filtered_aggregate() {
    let args = built(
        sqlite::select((
            select::columns(
                f("count", "*")
                    .filter(quote("is_active").eq(arg(1i32)))
                    .filter(quote("email").is_not_null())
                    .as_("active_with_email"),
            ),
            select::from(quote("users")),
        )),
        r#"SELECT count(*) FILTER (WHERE ("is_active" = ?1) AND ("email" IS NOT NULL)) AS "active_with_email" FROM "users""#,
    );
    assert_eq!(args, vec![Value::I32(1)]);
}

/// The aggregate's own `ORDER BY`, inside the argument list — SQLite 3.44 and later.
#[test]
fn an_ordered_aggregate() {
    built(
        sqlite::select((
            select::columns(f("group_concat", quote("name")).order_by(quote("id"))),
            select::from(quote("users")),
        )),
        r#"SELECT group_concat("name" ORDER BY "id") FROM "users""#,
    );
}

#[test]
fn a_filter_on_a_window_function() {
    built(
        sqlite::select((
            select::columns(
                f("count", "*")
                    .filter(quote("views").gt(0))
                    .over(window::partition_by(quote("status"))),
            ),
            select::from(quote("posts")),
        )),
        r#"SELECT count(*) FILTER (WHERE ("views" > 0)) OVER (PARTITION BY "status") FROM "posts""#,
    );
}
