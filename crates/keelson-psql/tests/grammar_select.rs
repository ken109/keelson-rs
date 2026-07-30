//! A clause-by-clause walk of PostgreSQL 17's `SELECT`.
//!
//! ```text
//! [ WITH [ RECURSIVE ] with_query [, ...] ]
//! SELECT [ ALL | DISTINCT [ ON ( expression [, ...] ) ] ]
//!     [ * | expression [ [ AS ] output_name ] [, ...] ]
//!     [ FROM from_item [, ...] ]
//!     [ WHERE condition ]
//!     [ GROUP BY [ ALL | DISTINCT ] grouping_element [, ...] ]
//!     [ HAVING condition ]
//!     [ WINDOW window_name AS ( window_definition ) [, ...] ]
//!     [ { UNION | INTERSECT | EXCEPT } [ ALL | DISTINCT ] select ]
//!     [ ORDER BY expression [ ASC | DESC | USING operator ] [ NULLS { FIRST | LAST } ] [, ...] ]
//!     [ LIMIT { count | ALL } ]
//!     [ OFFSET start [ ROW | ROWS ] ]
//!     [ FETCH { FIRST | NEXT } [ count ] { ROW | ROWS } { ONLY | WITH TIES } ]
//!     [ FOR { UPDATE | NO KEY UPDATE | SHARE | KEY SHARE } [ OF table_name [, ...] ]
//!       [ NOWAIT | SKIP LOCKED ] [...] ]
//! ```
//!
//! **Where the expected strings come from.** Every one is derived from that
//! production, from the `from_item` / `grouping_element` / `window_definition` /
//! `frame_clause` sub-productions of the same page, from `gram.y` where the manual's
//! layout is ambiguous about ordering, or from chapter 9 for an operator's spelling.
//! Each test that pins a shape which is not self-evident cites which. None of them
//! was obtained by running the builder and pasting what came out — the whole point
//! of [`assert_sql`] is that the grammar and a real server answer "is this valid",
//! while only the string comparison answers "is this what we meant", and that half is
//! worth exactly as much as its provenance.
//!
//! **Every name is from `tests/schema/psql.sql`** — `users`, `posts`, `comments`,
//! `tags`, `post_tags` — because a real engine resolves names during analysis, and a
//! statement mentioning an invented table cannot be engine-checked at all.
//!
//! The last section leaves the grammar and tests what only keelson has: conditional
//! and collected mods, post-construction `apply`, `clone` independence, and
//! placeholder numbering across nested sub-queries.

use keelson_psql as psql;
use keelson_psql::{
    Chain, Expr, IntoExpr, PsqlOps, Query, RawArg, Value, arg, arg_group, case_, cast, cube, f,
    frame, group, grouping_sets, not, or, query, quote, raw, rollup, s, select, subquery, template,
    window,
};
use keelson_sqlcheck::{Dialect, assert_sql};

/// Build, then run every check this build can: grammar, engine, and intent.
#[track_caller]
fn check(q: &impl Query, expected: &str) -> Vec<Value> {
    let (sql, args) = q.build().expect("the query should build");
    assert_sql(Dialect::Psql, &sql, expected);
    args
}

/// `EXISTS (subquery)`.
///
/// There is no `psql::exists` starter, so this is the route a caller has: a prefix
/// operator over a parenthesised sub-query. Written here once rather than inline, so
/// that adding the starter later has one place to change.
fn exists(q: impl Query + 'static) -> Expr {
    Expr::prefix("EXISTS", subquery(q))
}

/// `SELECT "id" FROM "comments"`, the stock one-column operand.
fn comment_ids() -> psql::SelectQuery {
    psql::select((
        select::columns(quote("id")),
        select::from(quote("comments")),
    ))
}

// ===========================================================================
// The select list
// ===========================================================================

#[test]
fn the_projection_defaults_to_star_and_accumulates_otherwise() {
    // `[ * | expression … ]`: an empty list is `*`, which is the one clause whose
    // absent rendering is not empty.
    check(
        &psql::select(select::from(quote("users"))),
        r#"SELECT * FROM "users""#,
    );

    check(
        &psql::select((
            select::columns((quote("id"), quote("name"), quote("email"))),
            select::from(quote("users")),
        )),
        r#"SELECT "id", "name", "email" FROM "users""#,
    );

    // Two calls accumulate rather than replace.
    check(
        &psql::select((
            select::columns(quote("id")),
            select::columns((quote("name"), quote("age"))),
            select::from(quote("users")),
        )),
        r#"SELECT "id", "name", "age" FROM "users""#,
    );
}

#[test]
fn an_output_name_is_written_as_an_unparenthesised_alias() {
    // `expression [ [ AS ] output_name ]`. `x AS "y"` is deliberately *not*
    // wrapped: `(x AS "y")` is a syntax error in a select list.
    check(
        &psql::select((
            select::columns((
                quote("title").as_("headline"),
                quote("views").plus(1).as_("bumped"),
            )),
            select::from(quote("posts")),
        )),
        r#"SELECT "title" AS "headline", ("views" + 1) AS "bumped" FROM "posts""#,
    );
}

#[test]
fn the_select_list_holds_literals_raw_sql_and_bound_arguments() {
    // A number or a `&str` is written verbatim; `arg` binds. The cast is not
    // decoration — a bare `$1` in a select list leaves the server unable to
    // determine the parameter's type, and it says so at PREPARE time.
    let args = check(
        &psql::select((
            select::columns((s("draft"), raw("1"), cast(arg(2i32), "int"))),
            select::from(quote("users")),
        )),
        r#"SELECT 'draft', 1, CAST($1 AS int) FROM "users""#,
    );
    assert_eq!(args, vec![Value::I32(2)]);
}

#[test]
fn preload_columns_render_last_and_are_counted_apart() {
    let q = psql::select((
        select::columns((quote("id"), quote("name"))),
        select::preload_columns(quote("email")),
        select::from(quote("users")),
    ));
    check(&q, r#"SELECT "id", "name", "email" FROM "users""#);
    assert_eq!(
        q.select_list.count_select_cols(),
        2,
        "a preload column is not one of the caller's"
    );
}

#[test]
fn distinct_drops_duplicate_rows() {
    check(
        &psql::select((
            select::distinct(),
            select::columns(quote("status")),
            select::from(quote("posts")),
        )),
        r#"SELECT DISTINCT "status" FROM "posts""#,
    );
}

/// `DISTINCT [ ON ( expression [, ...] ) ]`, and the manual's requirement that the
/// `ORDER BY` begin with the same expressions.
#[test]
fn distinct_on_takes_one_expression_or_several() {
    check(
        &psql::select((
            select::distinct_on(quote("user_id")),
            select::columns((quote("user_id"), quote("id"))),
            select::from(quote("posts")),
            select::order_by(quote("user_id")),
            select::order_by(quote("id")).desc(),
        )),
        r#"SELECT DISTINCT ON ("user_id") "user_id", "id" FROM "posts"
           ORDER BY "user_id", "id" DESC"#,
    );

    check(
        &psql::select((
            select::distinct_on((quote("user_id"), quote("status"))),
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::order_by(quote("user_id")),
            select::order_by(quote("status")),
        )),
        r#"SELECT DISTINCT ON ("user_id", "status") "id" FROM "posts"
           ORDER BY "user_id", "status""#,
    );
}

#[test]
fn a_scalar_subquery_stands_in_the_select_list() {
    let newest = psql::select((
        select::columns(f("max", quote("created_at"))),
        select::from(quote("comments")),
        select::where_(quote(("comments", "post_id")).eq(quote(("p", "id")))),
    ));
    check(
        &psql::select((
            select::columns((quote(("p", "id")), subquery(newest))),
            select::from(quote("posts")).as_("p"),
        )),
        r#"SELECT "p"."id", (SELECT max("created_at") FROM "comments"
                             WHERE ("comments"."post_id" = "p"."id")) FROM "posts" AS "p""#,
    );
}

#[test]
fn a_case_expression_with_and_without_an_else_branch() {
    let with_else = psql::select((
        select::columns(
            case_()
                .when(quote("views").gt(100), s("hot"))
                .when(quote("views").gt(10), s("warm"))
                .else_(s("cold"))
                .as_("tier"),
        ),
        select::from(quote("posts")),
    ));
    check(
        &with_else,
        r#"SELECT (CASE WHEN ("views" > 100) THEN 'hot'
                        WHEN ("views" > 10) THEN 'warm'
                        ELSE 'cold' END) AS "tier" FROM "posts""#,
    );

    // With no `ELSE` the expression is `NULL` outside every branch, so the clause
    // is simply omitted rather than filled in.
    let without_else = psql::select((
        select::columns(
            case_()
                .when(quote("is_active"), s("live"))
                .end()
                .as_("state"),
        ),
        select::from(quote("users")),
    ));
    check(
        &without_else,
        r#"SELECT (CASE WHEN "is_active" THEN 'live' END) AS "state" FROM "users""#,
    );
}

/// `CAST(expr AS type)` and PostgreSQL's `::` shorthand (4.2.9). `CAST(…)` is
/// self-delimiting so it takes no wrapping parentheses; the operator form is a chain
/// step and therefore does.
#[test]
fn both_cast_spellings() {
    check(
        &psql::select((
            select::columns((cast(quote("age"), "text"), quote("id").cast_to("bigint"))),
            select::from(quote("users")),
        )),
        r#"SELECT CAST("age" AS text), ("id"::bigint) FROM "users""#,
    );
}

/// `COLLATE` binds tighter than any operator (4.2.10), and the collation name is an
/// identifier.
#[test]
fn collate_in_the_select_list_and_in_a_comparison() {
    let args = check(
        &psql::select((
            select::columns(quote("name").collate("C")),
            select::from(quote("users")),
            select::where_(quote("name").collate("C").gt(arg("m"))),
        )),
        r#"SELECT ("name" COLLATE "C") FROM "users" WHERE (("name" COLLATE "C") > $1)"#,
    );
    assert_eq!(args, vec![Value::Text("m".into())]);
}

#[test]
fn string_concatenation_joins_with_the_pipe_operator() {
    check(
        &psql::select((
            select::columns(quote("name").concat((s(" <"), quote("email"), s(">")))),
            select::from(quote("users")),
        )),
        r#"SELECT ("name" || ' <' || "email" || '>') FROM "users""#,
    );
}

// ===========================================================================
// FROM
// ===========================================================================

/// `from_item`: `[ ONLY ] table_name [ * ] [ [ AS ] alias [ ( column_alias … ) ] ]`.
#[test]
fn a_from_item_carries_a_schema_an_alias_and_only() {
    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote(("public", "users"))),
        )),
        r#"SELECT "id" FROM "public"."users""#,
    );

    check(
        &psql::select((
            select::columns(quote(("u", "id"))),
            select::from(quote("users")).as_("u"),
        )),
        r#"SELECT "u"."id" FROM "users" AS "u""#,
    );

    // `ONLY` precedes the name and the alias follows it.
    check(
        &psql::select((
            select::columns(quote(("u", "id"))),
            select::from(quote("users")).only().as_("u"),
        )),
        r#"SELECT "u"."id" FROM ONLY "users" AS "u""#,
    );
}

#[test]
fn a_subquery_from_item_takes_an_alias_and_column_aliases() {
    let sub = psql::select((
        select::columns((quote("id"), quote("name"))),
        select::from(quote("users")),
    ));
    check(
        &psql::select((
            select::columns((quote(("s", "who")), quote(("s", "which")))),
            select::from(subquery(sub))
                .as_("s")
                .columns(["which", "who"]),
        )),
        r#"SELECT "s"."who", "s"."which"
           FROM (SELECT "id", "name" FROM "users") AS "s" ("which", "who")"#,
    );
}

/// `FROM from_item [, ...]` — a comma there means the same thing as `CROSS JOIN`.
#[test]
fn from_takes_a_comma_separated_list() {
    check(
        &psql::select((
            select::columns((quote(("u", "id")), quote(("t", "name")))),
            select::from(quote("users")).as_("u"),
            select::from_also(quote("tags")).as_("t"),
            select::from_also(quote("posts")).as_("p"),
        )),
        r#"SELECT "u"."id", "t"."name" FROM "users" AS "u", "tags" AS "t", "posts" AS "p""#,
    );
}

/// `LATERAL` on a comma-separated item, which is what lets it see the columns of
/// the items before it. The keyword goes in front of the item, not on a join.
#[test]
fn a_lateral_from_item_sees_the_earlier_items() {
    let recent = psql::select((
        select::columns(quote("title")),
        select::from(quote("posts")),
        select::where_(quote(("posts", "user_id")).eq(quote(("u", "id")))),
        select::order_by(quote("published_at")).desc(),
        select::limit(1),
    ));
    check(
        &psql::select((
            select::columns((quote(("u", "id")), quote(("r", "title")))),
            select::from(quote("users")).as_("u"),
            select::from_also(subquery(recent)).lateral().as_("r"),
        )),
        r#"SELECT "u"."id", "r"."title" FROM "users" AS "u",
           LATERAL (SELECT "title" FROM "posts" WHERE ("posts"."user_id" = "u"."id")
                    ORDER BY "published_at" DESC LIMIT 1) AS "r""#,
    );
}

/// `function_name ( … ) [ WITH ORDINALITY ] [ [ AS ] alias ]` and the `ROWS FROM`
/// form, which exists to hold a *list* of functions — so one function is written
/// plainly, because `ROWS FROM (f())` and `f()` mean the same thing.
#[test]
fn a_from_item_that_is_a_set_returning_function() {
    check(
        &psql::select(select::from_function([f("generate_series", (1, 5))]).as_("g")),
        r#"SELECT * FROM generate_series(1, 5) AS "g""#,
    );

    check(
        &psql::select(
            select::from_function([f("generate_series", (1, 3)), f("generate_series", (4, 9))])
                .as_("g"),
        ),
        r#"SELECT * FROM ROWS FROM (generate_series(1, 3), generate_series(4, 9)) AS "g""#,
    );

    // `WITH ORDINALITY` follows the item and precedes the alias.
    check(
        &psql::select(
            select::from_function([f("generate_series", (1, 3))])
                .with_ordinality()
                .as_("g"),
        ),
        r#"SELECT * FROM generate_series(1, 3) WITH ORDINALITY AS "g""#,
    );
}

/// `function_name ( … ) [ AS ] [ alias ] ( column_definition [, ...] )` — the form
/// a record-returning function needs, because its result shape is not in the
/// catalog.
///
/// `gram.y` spells the whole thing as one `func_alias_clause`, so the alias and the
/// column definitions share a single `AS`. That is why the alias here is
/// [`Function::as_table`] and not the from-item chain's `as_`: a second alias
/// would be a second `AS`, which is a syntax error.
#[test]
fn a_function_from_item_names_and_types_its_columns() {
    let args = check(
        &psql::select((
            select::columns((quote("a"), quote("b"))),
            select::from_function([f("json_to_recordset", cast(arg("[]"), "json"))
                .columns([("a", "int"), ("b", "text")])]),
        )),
        r#"SELECT "a", "b" FROM json_to_recordset(CAST($1 AS json))
           AS ("a" int, "b" text)"#,
    );
    assert_eq!(args, vec![Value::Text("[]".into())]);

    check(
        &psql::select((
            select::columns(quote(("r", "a"))),
            select::from_function([f("json_to_recordset", cast(arg("[]"), "json"))
                .as_table("r")
                .columns([("a", "int")])]),
        )),
        r#"SELECT "r"."a" FROM json_to_recordset(CAST($1 AS json)) AS "r" ("a" int)"#,
    );
}

/// `gram.y`: `table_ref: relation_expr opt_alias_clause tablesample_clause`, so the
/// sampling clause comes *after* the alias.
#[test]
fn tablesample_with_both_methods_and_a_repeatable_seed() {
    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("users")).tablesample("SYSTEM", 10),
        )),
        r#"SELECT "id" FROM "users" TABLESAMPLE SYSTEM (10)"#,
    );

    check(
        &psql::select((
            select::columns(quote(("u", "id"))),
            select::from(quote("users"))
                .as_("u")
                .tablesample("BERNOULLI", 25)
                .repeatable(200),
        )),
        r#"SELECT "u"."id" FROM "users" AS "u" TABLESAMPLE BERNOULLI (25) REPEATABLE (200)"#,
    );
}

// ===========================================================================
// Joins
// ===========================================================================

/// `from_item join_type from_item ON condition` for each of the five join types.
#[test]
fn every_join_type_with_an_on_condition() {
    /// The four conditional joins, as `(keyword, starter)`.
    type JoinCase = (&'static str, fn(Expr) -> psql::shared::JoinChain);

    let cases: [JoinCase; 4] = [
        ("INNER JOIN", |t| select::inner_join(t)),
        ("LEFT JOIN", |t| select::left_join(t)),
        ("RIGHT JOIN", |t| select::right_join(t)),
        ("FULL JOIN", |t| select::full_join(t)),
    ];
    for (keyword, join) in cases {
        let q = psql::select((
            select::columns(quote(("p", "id"))),
            select::from(quote("posts")).as_("p"),
            join(quote("comments"))
                .as_("c")
                .on_eq(quote(("c", "post_id")), quote(("p", "id"))),
        ));
        check(
            &q,
            &format!(
                r#"SELECT "p"."id" FROM "posts" AS "p" {keyword} "comments" AS "c"
                   ON ("c"."post_id" = "p"."id")"#
            ),
        );
    }

    // A cross join takes no condition at all, which is why its chain has no `on`.
    check(
        &psql::select((
            select::columns(quote(("p", "id"))),
            select::from(quote("posts")).as_("p"),
            select::cross_join(quote("tags")).as_("t"),
        )),
        r#"SELECT "p"."id" FROM "posts" AS "p" CROSS JOIN "tags" AS "t""#,
    );
}

/// `NATURAL join_type JOIN` — the join columns come from the two items' names, and
/// `NATURAL` precedes the join type rather than following it.
#[test]
fn natural_joins() {
    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::left_join(quote("comments")).natural(),
        )),
        r#"SELECT "id" FROM "posts" NATURAL LEFT JOIN "comments""#,
    );

    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::full_join(quote("comments")).natural(),
        )),
        r#"SELECT "id" FROM "posts" NATURAL FULL JOIN "comments""#,
    );

    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::inner_join(quote("comments")).natural(),
        )),
        r#"SELECT "id" FROM "posts" NATURAL INNER JOIN "comments""#,
    );
}

/// `USING ( join_column [, ...] )` merges the named columns, which is what keeps an
/// unqualified reference to one unambiguous.
#[test]
fn join_using_one_column_and_several() {
    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::inner_join(quote("comments")).using(["id"]),
        )),
        r#"SELECT "id" FROM "posts" INNER JOIN "comments" USING ("id")"#,
    );

    check(
        &psql::select((
            select::columns((quote("id"), quote("user_id"))),
            select::from(quote("posts")),
            select::inner_join(quote("comments")).using(["id", "user_id"]),
        )),
        r#"SELECT "id", "user_id" FROM "posts" INNER JOIN "comments" USING ("id", "user_id")"#,
    );
}

#[test]
fn several_joins_chain_left_to_right() {
    check(
        &psql::select((
            select::columns((quote(("p", "title")), quote(("t", "name")))),
            select::from(quote("posts")).as_("p"),
            select::inner_join(quote("post_tags"))
                .as_("pt")
                .on_eq(quote(("pt", "post_id")), quote(("p", "id"))),
            select::inner_join(quote("tags"))
                .as_("t")
                .on_eq(quote(("t", "id")), quote(("pt", "tag_id"))),
            select::left_join(quote("comments"))
                .as_("c")
                .on_eq(quote(("c", "post_id")), quote(("p", "id"))),
        )),
        r#"SELECT "p"."title", "t"."name" FROM "posts" AS "p"
           INNER JOIN "post_tags" AS "pt" ON ("pt"."post_id" = "p"."id")
           INNER JOIN "tags" AS "t" ON ("t"."id" = "pt"."tag_id")
           LEFT JOIN "comments" AS "c" ON ("c"."post_id" = "p"."id")"#,
    );
}

#[test]
fn two_on_conditions_on_one_join_are_and_joined() {
    let args = check(
        &psql::select((
            select::columns(quote(("p", "id"))),
            select::from(quote("posts")).as_("p"),
            select::inner_join(quote("comments"))
                .as_("c")
                .on(quote(("c", "post_id")).eq(quote(("p", "id"))))
                .on(quote(("c", "user_id")).gt(arg(0i32))),
        )),
        r#"SELECT "p"."id" FROM "posts" AS "p" INNER JOIN "comments" AS "c"
           ON ("c"."post_id" = "p"."id") AND ("c"."user_id" > $1)"#,
    );
    assert_eq!(args, vec![Value::I32(0)]);
}

#[test]
fn a_join_target_may_be_only_a_subquery_or_a_sampled_table() {
    check(
        &psql::select((
            select::columns(quote(("p", "id"))),
            select::from(quote("posts")).as_("p"),
            select::inner_join(quote("comments"))
                .only()
                .as_("c")
                .on_eq(quote(("c", "post_id")), quote(("p", "id"))),
        )),
        r#"SELECT "p"."id" FROM "posts" AS "p" INNER JOIN ONLY "comments" AS "c"
           ON ("c"."post_id" = "p"."id")"#,
    );

    let counted = psql::select((
        select::columns((quote("post_id"), f("count", "*").as_("n"))),
        select::from(quote("comments")),
        select::group_by(quote("post_id")),
    ));
    check(
        &psql::select((
            select::columns((quote(("p", "id")), quote(("c", "n")))),
            select::from(quote("posts")).as_("p"),
            select::left_join(subquery(counted))
                .as_("c")
                .columns(["post_id", "n"])
                .on_eq(quote(("c", "post_id")), quote(("p", "id"))),
        )),
        r#"SELECT "p"."id", "c"."n" FROM "posts" AS "p"
           LEFT JOIN (SELECT "post_id", count(*) AS "n" FROM "comments" GROUP BY "post_id")
           AS "c" ("post_id", "n") ON ("c"."post_id" = "p"."id")"#,
    );

    check(
        &psql::select((
            select::columns(quote(("u", "id"))),
            select::from(quote("users")).as_("u"),
            select::inner_join(quote("posts"))
                .as_("p")
                .tablesample("SYSTEM", 5)
                .on_eq(quote(("p", "user_id")), quote(("u", "id"))),
        )),
        r#"SELECT "u"."id" FROM "users" AS "u"
           INNER JOIN "posts" AS "p" TABLESAMPLE SYSTEM (5) ON ("p"."user_id" = "u"."id")"#,
    );
}

/// A `LATERAL` join, and the reason `INNER JOIN LATERAL … ON true` is the shape a
/// correlated sub-query needs: `LATERAL` is a property of the joined item, so the
/// join still wants a condition.
#[test]
fn a_lateral_join_onto_a_correlated_subquery() {
    let top = psql::select((
        select::columns(quote("title")),
        select::from(quote("posts")),
        select::where_(quote(("posts", "user_id")).eq(quote(("u", "id")))),
        select::order_by(quote("views")).desc(),
        select::limit(3),
    ));
    check(
        &psql::select((
            select::columns((quote(("u", "name")), quote(("t", "title")))),
            select::from(quote("users")).as_("u"),
            select::left_join(subquery(top))
                .lateral()
                .as_("t")
                .on(raw("true")),
        )),
        r#"SELECT "u"."name", "t"."title" FROM "users" AS "u"
           LEFT JOIN LATERAL (SELECT "title" FROM "posts" WHERE ("posts"."user_id" = "u"."id")
                              ORDER BY "views" DESC LIMIT 3) AS "t" ON true"#,
    );
}

/// A `CROSS JOIN` may still be aliased and restricted, and its chain says so by
/// having those methods and not `on`/`using`/`natural`.
#[test]
fn a_cross_joined_item_keeps_its_decorations() {
    check(
        &psql::select((
            select::columns(quote(("g", "n"))),
            select::from(quote("users")).only().as_("u"),
            select::cross_join(f("generate_series", (1, 2)).into_expr())
                .as_("g")
                .columns(["n"]),
        )),
        r#"SELECT "g"."n" FROM ONLY "users" AS "u"
           CROSS JOIN generate_series(1, 2) AS "g" ("n")"#,
    );
}

// ===========================================================================
// WHERE
// ===========================================================================

#[test]
fn several_where_mods_are_and_joined_in_one_clause() {
    let args = check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("users")),
            select::where_(quote("age").gte(arg(21i32))),
            select::where_(quote("is_active")),
            select::where_(quote("email").is_not_null()),
        )),
        r#"SELECT "id" FROM "users"
           WHERE ("age" >= $1) AND "is_active" AND ("email" IS NOT NULL)"#,
    );
    assert_eq!(args, vec![Value::I32(21)]);
}

#[test]
fn the_boolean_combinators_parenthesise_their_own_result() {
    let args = check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("users")),
            select::where_(or((
                quote("age").lt(arg(18i32)),
                quote("age").gt(arg(65i32)),
                not(quote("is_active")),
            ))),
        )),
        r#"SELECT "id" FROM "users"
           WHERE (("age" < $1) OR ("age" > $2) OR NOT "is_active")"#,
    );
    assert_eq!(args, vec![Value::I32(18), Value::I32(65)]);

    // `NOT` does not wrap itself — it binds looser than anything it can contain.
    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("users")),
            select::where_(not(quote("age").is_null())),
        )),
        r#"SELECT "id" FROM "users" WHERE NOT ("age" IS NULL)"#,
    );
}

/// Chapter 9.2: the comparison operators, each parenthesised exactly once.
#[test]
fn every_comparison_operator_in_a_predicate() {
    let cases: [(Expr, &str); 8] = [
        (quote("age").eq(arg(1i32)), r#"("age" = $1)"#),
        (quote("age").ne(arg(1i32)), r#"("age" <> $1)"#),
        (quote("age").lt(arg(1i32)), r#"("age" < $1)"#),
        (quote("age").lte(arg(1i32)), r#"("age" <= $1)"#),
        (quote("age").gt(arg(1i32)), r#"("age" > $1)"#),
        (quote("age").gte(arg(1i32)), r#"("age" >= $1)"#),
        (
            quote("age").is_distinct_from(arg(1i32)),
            r#"("age" IS DISTINCT FROM $1)"#,
        ),
        (
            quote("age").is_not_distinct_from(arg(1i32)),
            r#"("age" IS NOT DISTINCT FROM $1)"#,
        ),
    ];
    for (predicate, rendered) in cases {
        check(
            &psql::select((
                select::columns(quote("id")),
                select::from(quote("users")),
                select::where_(predicate),
            )),
            &format!(r#"SELECT "id" FROM "users" WHERE {rendered}"#),
        );
    }
}

#[test]
fn in_and_not_in_over_a_value_list_and_over_a_subquery() {
    let args = check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::where_(quote("status").in_((arg("draft"), arg("hot")))),
        )),
        r#"SELECT "id" FROM "posts" WHERE ("status" IN ($1, $2))"#,
    );
    assert_eq!(args.len(), 2);

    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::where_(quote("id").not_in(query(comment_ids()))),
        )),
        r#"SELECT "id" FROM "posts" WHERE ("id" NOT IN (SELECT "id" FROM "comments"))"#,
    );
}

/// A row constructor on the left and a list of row constructors on the right —
/// 9.24.5, and the shape that makes an assignment/comparison of several columns one
/// expression rather than a pair.
#[test]
fn a_row_constructor_compared_against_a_list_of_rows() {
    let args = check(
        &psql::select((
            select::columns(quote("post_id")),
            select::from(quote("post_tags")),
            select::where_(
                group((quote("post_id"), quote("tag_id")))
                    .in_((arg_group([1i32, 2]), arg_group([3i32, 4]))),
            ),
        )),
        r#"SELECT "post_id" FROM "post_tags"
           WHERE (("post_id", "tag_id") IN (($1, $2), ($3, $4)))"#,
    );
    assert_eq!(args.len(), 4);
    assert_eq!(args[3], Value::I32(4));
}

/// `BETWEEN` and its `SYMMETRIC` form (9.2), which accepts the bounds either way
/// round.
#[test]
fn the_range_predicates() {
    let args = check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("users")),
            select::where_(quote("age").between(arg(18i32), arg(65i32))),
            select::where_(quote("id").not_between(arg(1i32), arg(9i32))),
        )),
        r#"SELECT "id" FROM "users"
           WHERE ("age" BETWEEN $1 AND $2) AND ("id" NOT BETWEEN $3 AND $4)"#,
    );
    assert_eq!(args.len(), 4);

    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("users")),
            select::where_(quote("age").between_symmetric(65, 18)),
            select::where_(quote("id").not_between_symmetric(9, 1)),
        )),
        r#"SELECT "id" FROM "users"
           WHERE ("age" BETWEEN SYMMETRIC 65 AND 18)
             AND ("id" NOT BETWEEN SYMMETRIC 9 AND 1)"#,
    );
}

/// 9.1: the three-valued boolean tests, all postfix.
#[test]
fn the_boolean_and_null_tests() {
    let cases: [(Expr, &str); 8] = [
        (quote("is_active").is_true(), r#"("is_active" IS TRUE)"#),
        (
            quote("is_active").is_not_true(),
            r#"("is_active" IS NOT TRUE)"#,
        ),
        (quote("is_active").is_false(), r#"("is_active" IS FALSE)"#),
        (
            quote("is_active").is_not_false(),
            r#"("is_active" IS NOT FALSE)"#,
        ),
        (
            quote("is_active").is_unknown(),
            r#"("is_active" IS UNKNOWN)"#,
        ),
        (
            quote("is_active").is_not_unknown(),
            r#"("is_active" IS NOT UNKNOWN)"#,
        ),
        (quote("email").is_null(), r#"("email" IS NULL)"#),
        (quote("email").is_not_null(), r#"("email" IS NOT NULL)"#),
    ];
    for (predicate, rendered) in cases {
        check(
            &psql::select((
                select::columns(quote("id")),
                select::from(quote("users")),
                select::where_(predicate),
            )),
            &format!(r#"SELECT "id" FROM "users" WHERE {rendered}"#),
        );
    }
}

/// 9.7: the pattern-matching operators. `ILIKE` and the `~` family are
/// PostgreSQL's, which is why they live in `PsqlOps` rather than in core.
#[test]
fn every_pattern_matching_operator() {
    let cases: [(Expr, &str); 9] = [
        (quote("title").like(arg("a%")), r#"("title" LIKE $1)"#),
        (
            quote("title").not_like(arg("a%")),
            r#"("title" NOT LIKE $1)"#,
        ),
        (quote("title").ilike(arg("a%")), r#"("title" ILIKE $1)"#),
        (
            quote("title").not_ilike(arg("a%")),
            r#"("title" NOT ILIKE $1)"#,
        ),
        (
            quote("title").similar_to(arg("a+")),
            r#"("title" SIMILAR TO $1)"#,
        ),
        (
            quote("title").not_similar_to(arg("a+")),
            r#"("title" NOT SIMILAR TO $1)"#,
        ),
        (quote("title").matches(arg("^a")), r#"("title" ~ $1)"#),
        (quote("title").imatches(arg("^a")), r#"("title" ~* $1)"#),
        (quote("title").not_matches(arg("^a")), r#"("title" !~ $1)"#),
    ];
    for (predicate, rendered) in cases {
        let args = check(
            &psql::select((
                select::columns(quote("id")),
                select::from(quote("posts")),
                select::where_(predicate),
            )),
            &format!(r#"SELECT "id" FROM "posts" WHERE {rendered}"#),
        );
        assert_eq!(args.len(), 1);
    }
}

/// 9.23: `EXISTS (subquery)`, and `NOT` over one.
#[test]
fn exists_and_not_exists_over_a_correlated_subquery() {
    let has_comment = || {
        psql::select((
            select::columns(raw("1")),
            select::from(quote("comments")).as_("c"),
            select::where_(quote(("c", "post_id")).eq(quote(("p", "id")))),
        ))
    };

    check(
        &psql::select((
            select::columns(quote(("p", "id"))),
            select::from(quote("posts")).as_("p"),
            select::where_(exists(has_comment())),
        )),
        r#"SELECT "p"."id" FROM "posts" AS "p"
           WHERE EXISTS (SELECT 1 FROM "comments" AS "c" WHERE ("c"."post_id" = "p"."id"))"#,
    );

    check(
        &psql::select((
            select::columns(quote(("p", "id"))),
            select::from(quote("posts")).as_("p"),
            select::where_(not(exists(has_comment()))),
        )),
        r#"SELECT "p"."id" FROM "posts" AS "p"
           WHERE NOT (EXISTS (SELECT 1 FROM "comments" AS "c"
                              WHERE ("c"."post_id" = "p"."id")))"#,
    );
}

/// 9.24.3 and 9.24.4: `op ANY (subquery)` and `op ALL (subquery)`.
#[test]
fn the_quantified_comparisons() {
    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::where_(quote("id").eq_any(query(comment_ids()))),
        )),
        r#"SELECT "id" FROM "posts" WHERE ("id" = ANY (SELECT "id" FROM "comments"))"#,
    );

    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::where_(quote("id").ne_all(query(comment_ids()))),
        )),
        r#"SELECT "id" FROM "posts" WHERE ("id" <> ALL (SELECT "id" FROM "comments"))"#,
    );

    let views = psql::select((
        select::columns(quote("views")),
        select::from(quote("posts")),
        select::where_(quote("status").eq(arg("draft"))),
    ));
    let args = check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::where_(quote("views").all(">", query(views))),
        )),
        r#"SELECT "id" FROM "posts"
           WHERE ("views" > ALL (SELECT "views" FROM "posts" WHERE ("status" = $1)))"#,
    );
    assert_eq!(args, vec![Value::Text("draft".into())]);

    // The same operator against an array, which is the other operand shape.
    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("users")),
            select::where_(quote("age").any(">", cast(arg("{20,30}"), "int[]"))),
        )),
        r#"SELECT "id" FROM "users" WHERE ("age" > ANY (CAST($1 AS int[])))"#,
    );
}

#[test]
fn a_raw_template_rewrites_its_placeholders_into_dollar_positions() {
    let args = check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("users")),
            select::where_(template(
                r#""age" > ? AND "age" < ?"#,
                [RawArg::value(1i32), RawArg::value(9i32)],
            )),
        )),
        r#"SELECT "id" FROM "users" WHERE "age" > $1 AND "age" < $2"#,
    );
    assert_eq!(args, vec![Value::I32(1), Value::I32(9)]);
}

// ===========================================================================
// GROUP BY and HAVING
// ===========================================================================

#[test]
fn group_by_one_expression_several_and_an_ordinal() {
    check(
        &psql::select((
            select::columns((quote("status"), f("count", "*"))),
            select::from(quote("posts")),
            select::group_by(quote("status")),
        )),
        r#"SELECT "status", count(*) FROM "posts" GROUP BY "status""#,
    );

    check(
        &psql::select((
            select::columns((quote("status"), quote("user_id"), f("sum", quote("views")))),
            select::from(quote("posts")),
            select::group_by(quote("status")),
            select::group_by(quote("user_id")),
        )),
        r#"SELECT "status", "user_id", sum("views") FROM "posts"
           GROUP BY "status", "user_id""#,
    );

    // A grouping element may be an output-column ordinal or an expression.
    check(
        &psql::select((
            select::columns((quote("status"), f("count", "*"))),
            select::from(quote("posts")),
            select::group_by(1),
        )),
        r#"SELECT "status", count(*) FROM "posts" GROUP BY 1"#,
    );

    check(
        &psql::select((
            select::columns((quote("views").plus(1), f("count", "*"))),
            select::from(quote("posts")),
            select::group_by(quote("views").plus(1)),
        )),
        r#"SELECT ("views" + 1), count(*) FROM "posts" GROUP BY ("views" + 1)"#,
    );
}

/// `grouping_element`: `ROLLUP (…)`, `CUBE (…)`, `GROUPING SETS (…)`, and the empty
/// grouping set `()`.
#[test]
fn the_grouping_elements() {
    check(
        &psql::select((
            select::columns((quote("status"), quote("user_id"), f("count", "*"))),
            select::from(quote("posts")),
            select::group_by(rollup((quote("status"), quote("user_id")))),
        )),
        r#"SELECT "status", "user_id", count(*) FROM "posts"
           GROUP BY ROLLUP ("status", "user_id")"#,
    );

    check(
        &psql::select((
            select::columns((quote("status"), quote("user_id"), f("count", "*"))),
            select::from(quote("posts")),
            select::group_by(cube((quote("status"), quote("user_id")))),
        )),
        r#"SELECT "status", "user_id", count(*) FROM "posts"
           GROUP BY CUBE ("status", "user_id")"#,
    );

    // The empty grouping set is `()`, written raw: an empty `group` would be
    // `(NULL)`, a row of one null, which is a different thing.
    check(
        &psql::select((
            select::columns((quote("status"), quote("user_id"), f("count", "*"))),
            select::from(quote("posts")),
            select::group_by(grouping_sets((
                group(quote("status")),
                group((quote("status"), quote("user_id"))),
                raw("()"),
            ))),
        )),
        r#"SELECT "status", "user_id", count(*) FROM "posts"
           GROUP BY GROUPING SETS (("status"), ("status", "user_id"), ())"#,
    );

    // Several elements are comma-separated, and a plain expression mixes with them.
    check(
        &psql::select((
            select::columns((quote("status"), quote("user_id"), f("count", "*"))),
            select::from(quote("posts")),
            select::group_by(quote("status")),
            select::group_by(rollup(quote("user_id"))),
        )),
        r#"SELECT "status", "user_id", count(*) FROM "posts"
           GROUP BY "status", ROLLUP ("user_id")"#,
    );
}

/// `GROUP BY [ ALL | DISTINCT ]` — `DISTINCT` de-duplicates the grouping sets a
/// `CUBE` expands to. `ALL` is the default and adds nothing, so it is not
/// representable.
#[test]
fn group_by_distinct_over_a_cube() {
    check(
        &psql::select((
            select::columns((quote("status"), f("count", "*"))),
            select::from(quote("posts")),
            select::group_by_distinct(true),
            select::group_by(cube((quote("status"), quote("status")))),
        )),
        r#"SELECT "status", count(*) FROM "posts"
           GROUP BY DISTINCT CUBE ("status", "status")"#,
    );

    // Setting it back off leaves the default spelling.
    check(
        &psql::select((
            select::columns(quote("status")),
            select::from(quote("posts")),
            select::group_by_distinct(true),
            select::group_by_distinct(false),
            select::group_by(quote("status")),
        )),
        r#"SELECT "status" FROM "posts" GROUP BY "status""#,
    );
}

#[test]
fn having_filters_groups_and_several_conditions_are_and_joined() {
    let args = check(
        &psql::select((
            select::columns((quote("user_id"), f("count", "*"))),
            select::from(quote("posts")),
            select::group_by(quote("user_id")),
            select::having(f("count", "*").into_expr().gt(arg(2i32))),
            select::having(f("sum", quote("views")).into_expr().lt(arg(1000i32))),
        )),
        r#"SELECT "user_id", count(*) FROM "posts" GROUP BY "user_id"
           HAVING (count(*) > $1) AND (sum("views") < $2)"#,
    );
    assert_eq!(args, vec![Value::I32(2), Value::I32(1000)]);
}

#[test]
fn having_compares_an_aggregate_against_a_subquery() {
    let average = psql::select((
        select::columns(f("count", "*")),
        select::from(quote("comments")),
    ));
    check(
        &psql::select((
            select::columns((quote("user_id"), f("count", "*"))),
            select::from(quote("posts")),
            select::group_by(quote("user_id")),
            select::having(f("count", "*").into_expr().gte(subquery(average))),
        )),
        r#"SELECT "user_id", count(*) FROM "posts" GROUP BY "user_id"
           HAVING (count(*) >= (SELECT count(*) FROM "comments"))"#,
    );
}

/// 4.2.7: `DISTINCT` sits inside the argument list, the aggregate's own `ORDER BY`
/// follows the arguments, `WITHIN GROUP` moves it out, and `FILTER` follows the
/// call.
#[test]
fn the_aggregate_decorations() {
    check(
        &psql::select((
            select::columns(f("count", quote("user_id")).distinct()),
            select::from(quote("posts")),
        )),
        r#"SELECT count(DISTINCT "user_id") FROM "posts""#,
    );

    check(
        &psql::select((
            select::columns(f("array_agg", quote("id")).order_by(quote("views"))),
            select::from(quote("posts")),
        )),
        r#"SELECT array_agg("id" ORDER BY "views") FROM "posts""#,
    );

    check(
        &psql::select((
            select::columns(
                f("percentile_cont", cast(arg(0.5f64), "double precision"))
                    .within_group()
                    .order_by(quote("views")),
            ),
            select::from(quote("posts")),
        )),
        r#"SELECT percentile_cont(CAST($1 AS double precision))
           WITHIN GROUP (ORDER BY "views") FROM "posts""#,
    );

    check(
        &psql::select((
            select::columns(
                f("count", "*")
                    .filter(quote("is_active"))
                    .filter(quote("age").gt(30)),
            ),
            select::from(quote("users")),
        )),
        r#"SELECT count(*) FILTER (WHERE "is_active" AND ("age" > 30)) FROM "users""#,
    );
}

// ===========================================================================
// WINDOW and the frame clause
// ===========================================================================

/// `window_definition`: `[ existing_window_name ] [ PARTITION BY … ]
/// [ ORDER BY … ] [ frame_clause ]`, every part optional — `OVER ()` included.
#[test]
fn the_window_definition_parts() {
    check(
        &psql::select((
            select::columns(f("row_number", ()).over(())),
            select::from(quote("posts")),
        )),
        r#"SELECT row_number() OVER () FROM "posts""#,
    );

    check(
        &psql::select((
            select::columns(f("count", "*").over(window::partition_by(quote("user_id")))),
            select::from(quote("posts")),
        )),
        r#"SELECT count(*) OVER (PARTITION BY "user_id") FROM "posts""#,
    );

    check(
        &psql::select((
            select::columns(f("rank", ()).over((
                window::partition_by((quote("user_id"), quote("status"))),
                window::order_by(quote("views")).desc().nulls_last(),
            ))),
            select::from(quote("posts")),
        )),
        r#"SELECT rank() OVER (PARTITION BY "user_id", "status"
                               ORDER BY "views" DESC NULLS LAST) FROM "posts""#,
    );

    check(
        &psql::select((
            select::columns(f("lag", (quote("views"), 1)).over(window::order_by(quote("id")))),
            select::from(quote("posts")),
        )),
        r#"SELECT lag("views", 1) OVER (ORDER BY "id") FROM "posts""#,
    );
}

/// `frame_clause`: `{ RANGE | ROWS | GROUPS } BETWEEN frame_start AND frame_end`,
/// and the one-bound form with no `BETWEEN`.
#[test]
fn every_frame_mode_and_bound_form() {
    let framed = |definition: Expr| {
        psql::select((select::columns(definition), select::from(quote("posts"))))
    };

    check(
        &framed(f("sum", quote("views")).over((
            window::order_by(quote("id")),
            frame::rows(),
            frame::from_unbounded_preceding(),
            frame::to_current_row(),
        ))),
        r#"SELECT sum("views") OVER (ORDER BY "id"
                ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) FROM "posts""#,
    );

    check(
        &framed(f("sum", quote("views")).over((
            window::order_by(quote("id")),
            frame::rows(),
            frame::from_preceding(1),
            frame::to_following(2),
        ))),
        r#"SELECT sum("views") OVER (ORDER BY "id"
                ROWS BETWEEN 1 PRECEDING AND 2 FOLLOWING) FROM "posts""#,
    );

    check(
        &framed(f("sum", quote("views")).over((
            window::order_by(quote("id")),
            frame::rows(),
            frame::from_following(1),
            frame::to_following(3),
        ))),
        r#"SELECT sum("views") OVER (ORDER BY "id"
                ROWS BETWEEN 1 FOLLOWING AND 3 FOLLOWING) FROM "posts""#,
    );

    check(
        &framed(f("sum", quote("views")).over((
            window::order_by(quote("id")),
            frame::rows(),
            frame::from_current_row(),
            frame::to_unbounded_following(),
        ))),
        r#"SELECT sum("views") OVER (ORDER BY "id"
                ROWS BETWEEN CURRENT ROW AND UNBOUNDED FOLLOWING) FROM "posts""#,
    );

    // `RANGE` compares values against the ordering key, so the offsets are of the
    // key's own type and there must be exactly one ordering column.
    check(
        &framed(f("avg", quote("views")).over((
            window::order_by(quote("views")),
            frame::range(),
            frame::from_preceding(10),
            frame::to_following(10),
        ))),
        r#"SELECT avg("views") OVER (ORDER BY "views"
                RANGE BETWEEN 10 PRECEDING AND 10 FOLLOWING) FROM "posts""#,
    );

    // No end bound, so no `BETWEEN`. `RANGE` is also the grammar's default mode,
    // which is why `range()` only ever documents intent.
    check(
        &framed(f("avg", quote("views")).over((
            window::order_by(quote("id")),
            frame::range(),
            frame::from_unbounded_preceding(),
        ))),
        r#"SELECT avg("views") OVER (ORDER BY "id" RANGE UNBOUNDED PRECEDING) FROM "posts""#,
    );

    // `GROUPS` counts peer groups, so the window must be ordered.
    check(
        &framed(f("count", "*").over((
            window::order_by(quote("status")),
            frame::groups(),
            frame::from_preceding(1),
            frame::to_current_row(),
        ))),
        r#"SELECT count(*) OVER (ORDER BY "status"
                GROUPS BETWEEN 1 PRECEDING AND CURRENT ROW) FROM "posts""#,
    );

    // A bound offset, which is legal and is what a parameterised window needs. The
    // cast is required: `ROWS $1 PRECEDING` leaves the parameter's type unknown.
    let args = check(
        &framed(f("sum", quote("views")).over((
            window::order_by(quote("id")),
            frame::rows(),
            frame::from_preceding(cast(arg(2i32), "bigint")),
            frame::to_current_row(),
        ))),
        r#"SELECT sum("views") OVER (ORDER BY "id"
                ROWS BETWEEN CAST($1 AS bigint) PRECEDING AND CURRENT ROW) FROM "posts""#,
    );
    assert_eq!(args, vec![Value::I32(2)]);
}

/// `frame_exclusion`: all four spellings, written after the bounds.
#[test]
fn every_frame_exclusion() {
    let cases: [(Expr, &str); 4] = [
        (
            f("count", "*").over((
                window::order_by(quote("id")),
                frame::rows(),
                frame::from_unbounded_preceding(),
                frame::to_current_row(),
                frame::exclude_no_others(),
            )),
            "EXCLUDE NO OTHERS",
        ),
        (
            f("count", "*").over((
                window::order_by(quote("id")),
                frame::rows(),
                frame::from_unbounded_preceding(),
                frame::to_current_row(),
                frame::exclude_current_row(),
            )),
            "EXCLUDE CURRENT ROW",
        ),
        (
            f("count", "*").over((
                window::order_by(quote("id")),
                frame::rows(),
                frame::from_unbounded_preceding(),
                frame::to_current_row(),
                frame::exclude_group(),
            )),
            "EXCLUDE GROUP",
        ),
        (
            f("count", "*").over((
                window::order_by(quote("id")),
                frame::rows(),
                frame::from_unbounded_preceding(),
                frame::to_current_row(),
                frame::exclude_ties(),
            )),
            "EXCLUDE TIES",
        ),
    ];
    for (definition, exclusion) in cases {
        check(
            &psql::select((select::columns(definition), select::from(quote("posts")))),
            &format!(
                r#"SELECT count(*) OVER (ORDER BY "id"
                   ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW {exclusion}) FROM "posts""#
            ),
        );
    }

    // The exclusion still follows a one-bound frame, where there is no `BETWEEN`.
    check(
        &psql::select((
            select::columns(f("count", "*").over((
                window::order_by(quote("id")),
                frame::rows(),
                frame::from_current_row(),
                frame::exclude_current_row(),
            ))),
            select::from(quote("posts")),
        )),
        r#"SELECT count(*) OVER (ORDER BY "id" ROWS CURRENT ROW EXCLUDE CURRENT ROW)
           FROM "posts""#,
    );
}

/// The two bounds `frame_start`/`frame_end` list and PostgreSQL still refuses: a
/// frame cannot start at the end or end at the beginning.
///
/// The refusal is not in the BNF the manual prints — the manual lists all five
/// bounds for both ends and states the restriction in prose — so `frame::*` can
/// build them and the judge is what says no. libpg_query says it too, in `gram.y`'s
/// post-parse check (`frame start cannot be UNBOUNDED FOLLOWING`), so both tiers
/// reject and neither can be reached through [`assert_sql`].
///
/// Pinned so a later phase does not mistake the rendering for a bug.
#[test]
fn the_two_frame_bounds_postgresql_refuses_render_and_are_refused() {
    let cases = [
        (
            psql::select((
                select::columns(f("count", "*").over((
                    window::order_by(quote("id")),
                    frame::rows(),
                    frame::from_unbounded_following(),
                ))),
                select::from(quote("posts")),
            )),
            r#"SELECT count(*) OVER (ORDER BY "id" ROWS UNBOUNDED FOLLOWING) FROM "posts""#,
        ),
        (
            psql::select((
                select::columns(f("count", "*").over((
                    window::order_by(quote("id")),
                    frame::rows(),
                    frame::from_current_row(),
                    frame::to_unbounded_preceding(),
                ))),
                select::from(quote("posts")),
            )),
            r#"SELECT count(*) OVER (ORDER BY "id"
               ROWS BETWEEN CURRENT ROW AND UNBOUNDED PRECEDING) FROM "posts""#,
        ),
    ];

    for (q, expected) in cases {
        let (sql, _) = q
            .build()
            .expect("it still builds — rendering is not validation");
        assert_eq!(
            keelson_sqlcheck::normalize(&sql),
            keelson_sqlcheck::normalize(expected)
        );
        assert!(
            keelson_sqlcheck::check_psql(&sql).is_err(),
            "PostgreSQL should refuse this frame: {sql}"
        );
        if keelson_sqlcheck::live::available().contains(&Dialect::Psql) {
            assert!(
                keelson_sqlcheck::live::check(Dialect::Psql, &sql).is_err(),
                "the server should refuse this frame too: {sql}"
            );
        }
    }
}

/// `WINDOW window_name AS ( window_definition ) [, ...]`, and the two ways a call
/// reaches one.
///
/// `OVER "w"` **references** the entry; `OVER ("w" …)` *copies* it, which the server
/// refuses when the copied window has a frame clause.
#[test]
fn the_window_clause_declares_windows_that_calls_reference_or_extend() {
    check(
        &psql::select((
            select::columns((quote("id"), f("rank", ()).over_name("w"))),
            select::from(quote("posts")),
            select::window(
                "w",
                (
                    window::partition_by(quote("user_id")),
                    window::order_by(quote("views")).desc(),
                ),
            ),
        )),
        r#"SELECT "id", rank() OVER "w" FROM "posts"
           WINDOW "w" AS (PARTITION BY "user_id" ORDER BY "views" DESC)"#,
    );

    // A second entry may be based on the first, and a call may copy it — legal
    // precisely because neither has a frame clause.
    check(
        &psql::select((
            select::columns((
                f("rank", ()).over_name("ordered"),
                f("count", "*").over(window::based_on("partitioned")),
            )),
            select::from(quote("posts")),
            select::window("partitioned", window::partition_by(quote("user_id"))),
            select::window(
                "ordered",
                (
                    window::based_on("partitioned"),
                    window::order_by(quote("id")).asc(),
                ),
            ),
        )),
        r#"SELECT rank() OVER "ordered", count(*) OVER ("partitioned") FROM "posts"
           WINDOW "partitioned" AS (PARTITION BY "user_id"),
                  "ordered" AS ("partitioned" ORDER BY "id" ASC)"#,
    );

    // A framed named window can only be reached by reference, which is what
    // `over_name` exists for.
    check(
        &psql::select((
            select::columns(f("sum", quote("views")).over_name("running")),
            select::from(quote("posts")),
            select::window(
                "running",
                (
                    window::order_by(quote("id")),
                    frame::rows(),
                    frame::from_unbounded_preceding(),
                    frame::to_current_row(),
                ),
            ),
        )),
        r#"SELECT sum("views") OVER "running" FROM "posts"
           WINDOW "running" AS (ORDER BY "id"
                                ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW)"#,
    );
}

/// 4.2.8: `[ FILTER ( WHERE … ) ] OVER ( … )` — `FILTER` first.
#[test]
fn a_filtered_window_aggregate() {
    check(
        &psql::select((
            select::columns(
                f("count", "*")
                    .filter(quote("status").eq(s("hot")))
                    .over(window::partition_by(quote("user_id"))),
            ),
            select::from(quote("posts")),
        )),
        r#"SELECT count(*) FILTER (WHERE ("status" = 'hot'))
           OVER (PARTITION BY "user_id") FROM "posts""#,
    );
}

// ===========================================================================
// Set operations
// ===========================================================================

/// `{ UNION | INTERSECT | EXCEPT } [ ALL | DISTINCT ] select`. `DISTINCT` is the
/// default and adds nothing, so it is not representable; the operand is always
/// parenthesised, which is what stops a later `ORDER BY` re-associating it.
#[test]
fn every_set_operation_with_and_without_all() {
    let posts = || (select::columns(quote("id")), select::from(quote("posts")));

    check(
        &psql::select((posts(), select::union(comment_ids()))),
        r#"SELECT "id" FROM "posts" UNION (SELECT "id" FROM "comments")"#,
    );
    check(
        &psql::select((posts(), select::union_all(comment_ids()))),
        r#"SELECT "id" FROM "posts" UNION ALL (SELECT "id" FROM "comments")"#,
    );
    check(
        &psql::select((posts(), select::intersect(comment_ids()))),
        r#"SELECT "id" FROM "posts" INTERSECT (SELECT "id" FROM "comments")"#,
    );
    check(
        &psql::select((posts(), select::intersect_all(comment_ids()))),
        r#"SELECT "id" FROM "posts" INTERSECT ALL (SELECT "id" FROM "comments")"#,
    );
    check(
        &psql::select((posts(), select::except(comment_ids()))),
        r#"SELECT "id" FROM "posts" EXCEPT (SELECT "id" FROM "comments")"#,
    );
    check(
        &psql::select((posts(), select::except_all(comment_ids()))),
        r#"SELECT "id" FROM "posts" EXCEPT ALL (SELECT "id" FROM "comments")"#,
    );
}

#[test]
fn set_operations_chain_left_to_right() {
    let tagged = psql::select((
        select::columns(quote("post_id")),
        select::from(quote("post_tags")),
    ));
    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::union(comment_ids()),
            select::except(tagged),
        )),
        r#"SELECT "id" FROM "posts" UNION (SELECT "id" FROM "comments")
           EXCEPT (SELECT "post_id" FROM "post_tags")"#,
    );
}

/// > `ORDER BY` and `LIMIT` … can be attached to a subexpression if it is enclosed
/// > in parentheses. Without parentheses, these clauses will be taken to apply to
/// > the result of the `UNION`.
///
/// So the leading query is wrapped exactly when it has one of its own, and the
/// combination's own trailing clauses land after the last operand.
#[test]
fn the_leading_query_is_parenthesised_only_when_it_has_a_tail_clause_of_its_own() {
    // No tail clause: no parentheses, even though a set operation is present.
    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::where_(quote("views").gt(0)),
            select::union(comment_ids()),
        )),
        r#"SELECT "id" FROM "posts" WHERE ("views" > 0)
           UNION (SELECT "id" FROM "comments")"#,
    );

    // An `ORDER BY` of its own: wrapped.
    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::order_by(quote("id")),
            select::union(comment_ids()),
        )),
        r#"(SELECT "id" FROM "posts" ORDER BY "id") UNION (SELECT "id" FROM "comments")"#,
    );

    // A `LIMIT`/`OFFSET` of its own: wrapped, and the combination gets its own.
    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::limit(10),
            select::offset(5),
            select::union_all(comment_ids()),
            select::order_by_combined(1),
            select::limit_combined(3),
            select::offset_combined(1),
        )),
        r#"(SELECT "id" FROM "posts" LIMIT 10 OFFSET 5)
           UNION ALL (SELECT "id" FROM "comments") ORDER BY 1 LIMIT 3 OFFSET 1"#,
    );

    // A `FETCH` of its own: wrapped, which needs the `ORDER BY` to come with it.
    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::order_by(quote("id")),
            select::fetch(1),
            select::union(comment_ids()),
        )),
        r#"(SELECT "id" FROM "posts" ORDER BY "id" FETCH NEXT 1 ROWS ONLY)
           UNION (SELECT "id" FROM "comments")"#,
    );
}

/// A locking clause counts as a tail clause too, so it wraps the leading query —
/// and then PostgreSQL refuses the statement anyway:
/// `FOR UPDATE is not allowed with UNION/INTERSECT/EXCEPT`. The parentheses do not
/// help, because the restriction is on the whole set operation.
///
/// So the wrapping is pinned here rather than through [`assert_sql`], which would
/// run the engine and be told no. This is the fifth arm of
/// `SelectQuery::has_tail_clauses`, and the only one with no valid statement to
/// demonstrate it in.
#[test]
fn a_locking_clause_wraps_the_leading_query_even_though_the_result_is_refused() {
    let q = psql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::for_update(),
        select::union(comment_ids()),
    ));
    let (sql, _) = q
        .build()
        .expect("it still builds — rendering is not validation");
    assert_eq!(
        keelson_sqlcheck::normalize(&sql),
        r#"(SELECT "id" FROM "posts" FOR UPDATE) UNION (SELECT "id" FROM "comments")"#
    );
    // libpg_query is a pure parser here, so the restriction only shows up on the
    // server — which is exactly the gap the engine tier exists to cover.
    keelson_sqlcheck::assert_valid(Dialect::Psql, &sql);
    if keelson_sqlcheck::live::available().contains(&Dialect::Psql) {
        assert!(
            keelson_sqlcheck::live::check(Dialect::Psql, &sql).is_err(),
            "FOR UPDATE with a set operation should be refused: {sql}"
        );
    }
}

#[test]
fn the_combined_fetch_applies_to_the_result_of_the_operation() {
    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::union(comment_ids()),
            select::order_by_combined(1),
            select::fetch_combined(2),
        )),
        r#"SELECT "id" FROM "posts" UNION (SELECT "id" FROM "comments")
           ORDER BY 1 FETCH NEXT 2 ROWS ONLY"#,
    );

    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::union(comment_ids()),
            select::order_by_combined(1),
            select::fetch_combined(2).with_ties(),
        )),
        r#"SELECT "id" FROM "posts" UNION (SELECT "id" FROM "comments")
           ORDER BY 1 FETCH NEXT 2 ROWS WITH TIES"#,
    );
}

/// Nesting is the only way to parenthesise an interior sub-expression, since the
/// operations chain strictly left to right.
#[test]
fn an_interior_set_operation_needs_a_nested_query() {
    let inner = psql::select((
        select::columns(quote("post_id")),
        select::from(quote("post_tags")),
        select::intersect(psql::select((
            select::columns(quote("post_id")),
            select::from(quote("comments")),
        ))),
    ));
    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::union(inner),
        )),
        r#"SELECT "id" FROM "posts" UNION (SELECT "post_id" FROM "post_tags"
           INTERSECT (SELECT "post_id" FROM "comments"))"#,
    );
}

// ===========================================================================
// ORDER BY
// ===========================================================================

/// `ORDER BY expression [ COLLATE … ] [ ASC | DESC | USING operator ]
/// [ NULLS { FIRST | LAST } ] [, ...]` — that order, which is `gram.y`'s
/// `sortby: a_expr USING qual_all_Op opt_nulls_order | a_expr opt_asc_desc
/// opt_nulls_order` with the collation part of the expression.
#[test]
fn every_sort_decoration() {
    check(
        &psql::select((
            select::columns(quote("name")),
            select::from(quote("users")),
            select::order_by(quote("name")).asc(),
            select::order_by(quote("age")).desc(),
            select::order_by(quote("email")).nulls_first(),
            select::order_by(quote("id")).nulls_last(),
        )),
        r#"SELECT "name" FROM "users"
           ORDER BY "name" ASC, "age" DESC, "email" NULLS FIRST, "id" NULLS LAST"#,
    );

    // `USING operator` is PostgreSQL's, and is why the direction is not a
    // two-variant enum.
    check(
        &psql::select((
            select::columns(quote("name")),
            select::from(quote("users")),
            select::order_by(quote("name"))
                .collate("C")
                .desc()
                .nulls_last(),
            select::order_by(quote("id")).using(">"),
        )),
        r#"SELECT "name" FROM "users"
           ORDER BY "name" COLLATE "C" DESC NULLS LAST, "id" USING >"#,
    );
}

#[test]
fn a_sort_key_may_be_an_ordinal_an_expression_or_an_aggregate() {
    check(
        &psql::select((
            select::columns((quote("status"), quote("views"))),
            select::from(quote("posts")),
            select::order_by(2).desc(),
            select::order_by(1),
        )),
        r#"SELECT "status", "views" FROM "posts" ORDER BY 2 DESC, 1"#,
    );

    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::order_by(quote("views").plus(quote("id"))).desc(),
        )),
        r#"SELECT "id" FROM "posts" ORDER BY ("views" + "id") DESC"#,
    );

    check(
        &psql::select((
            select::columns((quote("user_id"), f("count", "*"))),
            select::from(quote("posts")),
            select::group_by(quote("user_id")),
            select::order_by(f("count", "*").into_expr()).desc(),
        )),
        r#"SELECT "user_id", count(*) FROM "posts" GROUP BY "user_id"
           ORDER BY count(*) DESC"#,
    );
}

// ===========================================================================
// LIMIT / OFFSET / FETCH
// ===========================================================================

/// `LIMIT { count | ALL }`, `OFFSET start`, and
/// `FETCH NEXT count ROWS { ONLY | WITH TIES }`. A number is a literal because
/// `IntoExpr` makes one; `arg` binds instead.
#[test]
fn the_row_limiting_clauses() {
    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::limit(10),
            select::offset(20),
        )),
        r#"SELECT "id" FROM "posts" LIMIT 10 OFFSET 20"#,
    );

    let args = check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::limit(arg(10i32)),
            select::offset(arg(20i32)),
        )),
        r#"SELECT "id" FROM "posts" LIMIT $1 OFFSET $2"#,
    );
    assert_eq!(args, vec![Value::I32(10), Value::I32(20)]);

    // `LIMIT ALL` is the grammar's other alternative: explicitly no limit.
    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::limit_all(),
            select::offset(1),
        )),
        r#"SELECT "id" FROM "posts" LIMIT ALL OFFSET 1"#,
    );

    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::order_by(quote("id")),
            select::fetch(5),
        )),
        r#"SELECT "id" FROM "posts" ORDER BY "id" FETCH NEXT 5 ROWS ONLY"#,
    );

    // `WITH TIES` also returns the rows tying with the last under the `ORDER BY`,
    // which the statement must therefore have.
    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::order_by(quote("views")).desc(),
            select::fetch(5).with_ties(),
        )),
        r#"SELECT "id" FROM "posts" ORDER BY "views" DESC FETCH NEXT 5 ROWS WITH TIES"#,
    );

    // `OFFSET` and `FETCH` together, in the grammar's order.
    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::order_by(quote("id")),
            select::offset(10),
            select::fetch(5),
        )),
        r#"SELECT "id" FROM "posts" ORDER BY "id" OFFSET 10 FETCH NEXT 5 ROWS ONLY"#,
    );
}

// ===========================================================================
// The locking clause
// ===========================================================================

/// `FOR { UPDATE | NO KEY UPDATE | SHARE | KEY SHARE } [ OF table_name [, ...] ]
/// [ NOWAIT | SKIP LOCKED ] [...]` — the trailing `[...]` is why this is a list.
#[test]
fn every_lock_strength_on_its_own() {
    let cases: [(psql::shared::LockChain, &str); 4] = [
        (select::for_update(), "FOR UPDATE"),
        (select::for_no_key_update(), "FOR NO KEY UPDATE"),
        (select::for_share(), "FOR SHARE"),
        (select::for_key_share(), "FOR KEY SHARE"),
    ];
    for (lock, rendered) in cases {
        check(
            &psql::select((
                select::columns(quote("id")),
                select::from(quote("posts")),
                lock,
            )),
            &format!(r#"SELECT "id" FROM "posts" {rendered}"#),
        );
    }
}

#[test]
fn a_lock_may_name_tables_and_choose_how_to_wait() {
    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::for_update().of(["posts"]).no_wait(),
        )),
        r#"SELECT "id" FROM "posts" FOR UPDATE OF "posts" NOWAIT"#,
    );

    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::for_share().skip_locked(),
        )),
        r#"SELECT "id" FROM "posts" FOR SHARE SKIP LOCKED"#,
    );

    // Two clauses, each scoped to a different table of the statement.
    check(
        &psql::select((
            select::columns(quote(("p", "id"))),
            select::from(quote("posts")).as_("p"),
            select::inner_join(quote("comments"))
                .as_("c")
                .on_eq(quote(("c", "post_id")), quote(("p", "id"))),
            select::for_update().of(["p"]),
            select::for_key_share().of(["c"]).skip_locked(),
        )),
        r#"SELECT "p"."id" FROM "posts" AS "p"
           INNER JOIN "comments" AS "c" ON ("c"."post_id" = "p"."id")
           FOR UPDATE OF "p" FOR KEY SHARE OF "c" SKIP LOCKED"#,
    );

    // `OF` takes a list.
    check(
        &psql::select((
            select::columns(quote(("p", "id"))),
            select::from(quote("posts")).as_("p"),
            select::cross_join(quote("tags")).as_("t"),
            select::for_no_key_update().of(["p", "t"]),
        )),
        r#"SELECT "p"."id" FROM "posts" AS "p" CROSS JOIN "tags" AS "t"
           FOR NO KEY UPDATE OF "p", "t""#,
    );
}

// ===========================================================================
// WITH
// ===========================================================================

/// `WITH [ RECURSIVE ] with_query [, ...]`, where
/// `with_query: name [ ( column_name [, ...] ) ] AS [ [ NOT ] MATERIALIZED ] ( select )`.
#[test]
fn a_plain_cte_with_a_column_list_and_both_materialisations() {
    let body = || {
        psql::select((
            select::columns((quote("id"), quote("user_id"))),
            select::from(quote("posts")),
            select::where_(quote("views").gt(arg(100i32))),
        ))
    };

    check(
        &psql::select((
            select::with("popular", body()),
            select::columns(quote("id")),
            select::from(quote("popular")),
        )),
        r#"WITH "popular" AS (SELECT "id", "user_id" FROM "posts" WHERE ("views" > $1))
           SELECT "id" FROM "popular""#,
    );

    check(
        &psql::select((
            select::with("popular", body()).columns(["pid", "uid"]),
            select::columns(quote("pid")),
            select::from(quote("popular")),
        )),
        r#"WITH "popular" ("pid", "uid") AS
             (SELECT "id", "user_id" FROM "posts" WHERE ("views" > $1))
           SELECT "pid" FROM "popular""#,
    );

    check(
        &psql::select((
            select::with("popular", body()).materialized(),
            select::columns(quote("id")),
            select::from(quote("popular")),
        )),
        r#"WITH "popular" AS MATERIALIZED
             (SELECT "id", "user_id" FROM "posts" WHERE ("views" > $1))
           SELECT "id" FROM "popular""#,
    );

    check(
        &psql::select((
            select::with("popular", body()).not_materialized(),
            select::columns(quote("id")),
            select::from(quote("popular")),
        )),
        r#"WITH "popular" AS NOT MATERIALIZED
             (SELECT "id", "user_id" FROM "posts" WHERE ("views" > $1))
           SELECT "id" FROM "popular""#,
    );
}

#[test]
fn several_ctes_are_comma_separated_and_a_later_one_may_use_an_earlier() {
    let active = psql::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        select::where_(quote("is_active")),
    ));
    let theirs = psql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::where_(quote("user_id").in_(query(psql::select((
            select::columns(quote("id")),
            select::from(quote("active")),
        ))))),
    ));
    check(
        &psql::select((
            select::with("active", active),
            select::with("theirs", theirs),
            select::columns(quote("id")),
            select::from(quote("theirs")),
        )),
        r#"WITH "active" AS (SELECT "id" FROM "users" WHERE "is_active"),
                "theirs" AS (SELECT "id" FROM "posts"
                             WHERE ("user_id" IN (SELECT "id" FROM "active")))
           SELECT "id" FROM "theirs""#,
    );
}

/// `WITH RECURSIVE` is a property of the whole list — it is what makes every name in
/// it visible to every entry — and the body must be
/// `non-recursive UNION [ALL] recursive`.
#[test]
fn a_recursive_cte_and_the_recursive_flag_both_ways() {
    let step = psql::select((
        select::columns(quote(("p", "id"))),
        select::from(quote("posts")).as_("p"),
        select::inner_join(quote("tree")).on_eq(quote(("tree", "id")), quote(("p", "user_id"))),
    ));
    let body = psql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::where_(quote("id").eq(arg(1i32))),
        select::union_all(step),
    ));
    let args = check(
        &psql::select((
            select::recursive(true),
            select::with("tree", body).columns(["id"]),
            select::columns(quote("id")),
            select::from(quote("tree")),
        )),
        r#"WITH RECURSIVE "tree" ("id") AS
             (SELECT "id" FROM "posts" WHERE ("id" = $1)
              UNION ALL (SELECT "p"."id" FROM "posts" AS "p"
                         INNER JOIN "tree" ON ("tree"."id" = "p"."user_id")))
           SELECT "id" FROM "tree""#,
    );
    assert_eq!(args, vec![Value::I32(1)]);

    // Turning the flag back off leaves a plain `WITH`, keyword and all.
    check(
        &psql::select((
            select::recursive(true),
            select::recursive(false),
            select::with(
                "one",
                psql::select((select::columns(quote("id")), select::from(quote("users")))),
            ),
            select::columns(quote("id")),
            select::from(quote("one")),
        )),
        r#"WITH "one" AS (SELECT "id" FROM "users") SELECT "id" FROM "one""#,
    );
}

/// `SEARCH { BREADTH | DEPTH } FIRST BY column [, ...] SET column`, which adds an
/// ordering column the outer query can sort on.
#[test]
fn a_recursive_cte_with_a_search_clause_either_way_round() {
    let recursive_body = || {
        let step = psql::select((
            select::columns(quote(("p", "id"))),
            select::from(quote("posts")).as_("p"),
            select::inner_join(quote("tree")).on_eq(quote(("tree", "id")), quote(("p", "user_id"))),
        ));
        psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::where_(quote("id").eq(arg(1i32))),
            select::union_all(step),
        ))
    };

    check(
        &psql::select((
            select::recursive(true),
            select::with("tree", recursive_body())
                .columns(["id"])
                .search_breadth("ord", ["id"]),
            select::columns(quote("id")),
            select::from(quote("tree")),
            select::order_by(quote("ord")),
        )),
        r#"WITH RECURSIVE "tree" ("id") AS
             (SELECT "id" FROM "posts" WHERE ("id" = $1)
              UNION ALL (SELECT "p"."id" FROM "posts" AS "p"
                         INNER JOIN "tree" ON ("tree"."id" = "p"."user_id")))
             SEARCH BREADTH FIRST BY "id" SET "ord"
           SELECT "id" FROM "tree" ORDER BY "ord""#,
    );

    check(
        &psql::select((
            select::recursive(true),
            select::with("tree", recursive_body())
                .columns(["id"])
                .search_depth("ord", ["id"]),
            select::columns(quote("id")),
            select::from(quote("tree")),
        )),
        r#"WITH RECURSIVE "tree" ("id") AS
             (SELECT "id" FROM "posts" WHERE ("id" = $1)
              UNION ALL (SELECT "p"."id" FROM "posts" AS "p"
                         INNER JOIN "tree" ON ("tree"."id" = "p"."user_id")))
             SEARCH DEPTH FIRST BY "id" SET "ord"
           SELECT "id" FROM "tree""#,
    );
}

/// `CYCLE column [, ...] SET column [ TO value DEFAULT value ] USING column` — the
/// bracketed group is one optional unit, and the values must be *constants*
/// (`TO AexprConst DEFAULT AexprConst`).
#[test]
fn a_recursive_cte_with_a_cycle_clause_with_and_without_mark_values() {
    let recursive_body = || {
        let step = psql::select((
            select::columns(quote(("p", "id"))),
            select::from(quote("posts")).as_("p"),
            select::inner_join(quote("tree")).on_eq(quote(("tree", "id")), quote(("p", "user_id"))),
        ));
        psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::where_(quote("id").eq(arg(1i32))),
            select::union_all(step),
        ))
    };

    check(
        &psql::select((
            select::recursive(true),
            select::with("tree", recursive_body())
                .columns(["id"])
                .cycle("is_cycle", "path", ["id"]),
            select::columns(quote("id")),
            select::from(quote("tree")),
        )),
        r#"WITH RECURSIVE "tree" ("id") AS
             (SELECT "id" FROM "posts" WHERE ("id" = $1)
              UNION ALL (SELECT "p"."id" FROM "posts" AS "p"
                         INNER JOIN "tree" ON ("tree"."id" = "p"."user_id")))
             CYCLE "id" SET "is_cycle" USING "path"
           SELECT "id" FROM "tree""#,
    );

    // With explicit mark values, and a `SEARCH` in front of it: `SEARCH` precedes
    // `CYCLE`, and `MATERIALIZED` precedes both.
    check(
        &psql::select((
            select::recursive(true),
            select::with("tree", recursive_body())
                .columns(["id"])
                .materialized()
                .search_depth("ord", ["id"])
                .cycle("cycled", "path", ["id"])
                .cycle_value(s("yes"), s("no")),
            select::columns(quote("id")),
            select::from(quote("tree")),
        )),
        r#"WITH RECURSIVE "tree" ("id") AS MATERIALIZED
             (SELECT "id" FROM "posts" WHERE ("id" = $1)
              UNION ALL (SELECT "p"."id" FROM "posts" AS "p"
                         INNER JOIN "tree" ON ("tree"."id" = "p"."user_id")))
             SEARCH DEPTH FIRST BY "id" SET "ord"
             CYCLE "id" SET "cycled" TO 'yes' DEFAULT 'no' USING "path"
           SELECT "id" FROM "tree""#,
    );
}

// ===========================================================================
// Everything at once
// ===========================================================================

/// One statement carrying every clause of the production, so their *order* is
/// pinned and not merely each one's spelling.
#[test]
fn every_clause_in_one_statement_in_the_grammars_order() {
    let recent = psql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::where_(quote("published_at").is_not_null()),
    ));
    let args = check(
        &psql::select((
            select::with("recent", recent),
            select::distinct_on(quote(("p", "user_id"))),
            select::columns((
                quote(("p", "user_id")),
                f("count", "*").over(window::partition_by(quote(("p", "status")))),
            )),
            select::from(quote("posts")).as_("p"),
            select::inner_join(quote("recent"))
                .as_("r")
                .on_eq(quote(("r", "id")), quote(("p", "id"))),
            select::from_also(quote("tags")).as_("t"),
            select::where_(quote(("p", "views")).gt(arg(1i32))),
            select::order_by(quote(("p", "user_id"))),
            select::order_by(quote(("p", "id"))).desc(),
            select::limit(10),
            select::offset(2),
        )),
        r#"WITH "recent" AS (SELECT "id" FROM "posts" WHERE ("published_at" IS NOT NULL))
           SELECT DISTINCT ON ("p"."user_id") "p"."user_id",
                  count(*) OVER (PARTITION BY "p"."status")
           FROM "posts" AS "p" INNER JOIN "recent" AS "r" ON ("r"."id" = "p"."id"),
                "tags" AS "t"
           WHERE ("p"."views" > $1)
           ORDER BY "p"."user_id", "p"."id" DESC
           LIMIT 10 OFFSET 2"#,
    );
    assert_eq!(args, vec![Value::I32(1)]);
}

/// The locking clause could not join the statement above: PostgreSQL refuses
/// `FOR SHARE is not allowed with DISTINCT clause` and
/// `FOR SHARE is not allowed with window functions`. So the tail of the production
/// is pinned on a statement it is compatible with — which is also the only
/// arrangement in which `FOR` follows `OFFSET`/`FETCH` and can be seen to.
#[test]
fn the_tail_of_the_production_with_a_locking_clause_last() {
    let args = check(
        &psql::select((
            select::with(
                "recent",
                psql::select((select::columns(quote("id")), select::from(quote("posts")))),
            ),
            select::columns(quote(("p", "id"))),
            select::from(quote("posts")).as_("p"),
            select::inner_join(quote("recent"))
                .as_("r")
                .on_eq(quote(("r", "id")), quote(("p", "id"))),
            select::where_(quote(("p", "views")).gt(arg(1i32))),
            select::order_by(quote(("p", "id"))).desc(),
            select::offset(2),
            select::fetch(5),
            select::for_no_key_update().of(["p"]).no_wait(),
        )),
        r#"WITH "recent" AS (SELECT "id" FROM "posts")
           SELECT "p"."id" FROM "posts" AS "p"
           INNER JOIN "recent" AS "r" ON ("r"."id" = "p"."id")
           WHERE ("p"."views" > $1)
           ORDER BY "p"."id" DESC
           OFFSET 2 FETCH NEXT 5 ROWS ONLY
           FOR NO KEY UPDATE OF "p" NOWAIT"#,
    );
    assert_eq!(args, vec![Value::I32(1)]);
}

/// The aggregate half of the same idea: `GROUP BY`, `HAVING` and `WINDOW` in place.
#[test]
fn group_by_having_and_the_window_clause_in_the_grammars_order() {
    check(
        &psql::select((
            select::columns((
                quote("user_id"),
                f("count", "*"),
                f("rank", ()).over_name("w"),
            )),
            select::from(quote("posts")),
            select::where_(quote("status").is_not_null()),
            select::group_by(quote("user_id")),
            select::having(f("count", "*").into_expr().gt(1)),
            select::window("w", window::order_by(f("count", "*").into_expr()).desc()),
            select::order_by(quote("user_id")),
            select::limit(5),
        )),
        r#"SELECT "user_id", count(*), rank() OVER "w" FROM "posts"
           WHERE ("status" IS NOT NULL)
           GROUP BY "user_id" HAVING (count(*) > 1)
           WINDOW "w" AS (ORDER BY count(*) DESC)
           ORDER BY "user_id" LIMIT 5"#,
    );
}

// ===========================================================================
// What only keelson has
// ===========================================================================

/// `Option<M>` is how a conditional mod is written — no `if` statement, no `Vec`
/// juggling. Both branches, because a mod that silently applied when it should not
/// is the failure that matters.
#[test]
fn an_optional_mod_applied_and_skipped() {
    let build = |scoped: bool| {
        psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            scoped.then(|| select::where_(quote("status").eq(arg("draft")))),
        ))
    };

    let args = check(
        &build(true),
        r#"SELECT "id" FROM "posts" WHERE ("status" = $1)"#,
    );
    assert_eq!(args, vec![Value::Text("draft".into())]);

    let args = check(&build(false), r#"SELECT "id" FROM "posts""#);
    assert!(
        args.is_empty(),
        "a skipped mod must bind nothing either — an argument without its \
         placeholder would shift every later one"
    );
}

/// A `Vec` and an array of mods are both mods, which is what a list of conditions
/// assembled at run time needs.
#[test]
fn a_collection_of_mods_is_a_mod() {
    let wheres: Vec<_> = [1i32, 2, 3]
        .into_iter()
        .map(|n| select::where_(quote("views").ne(arg(n))))
        .collect();
    let args = check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            wheres,
        )),
        r#"SELECT "id" FROM "posts"
           WHERE ("views" <> $1) AND ("views" <> $2) AND ("views" <> $3)"#,
    );
    assert_eq!(args, vec![Value::I32(1), Value::I32(2), Value::I32(3)]);

    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            [
                select::order_by(quote("views")).desc(),
                select::order_by(quote("id")).asc(),
            ],
        )),
        r#"SELECT "id" FROM "posts" ORDER BY "views" DESC, "id" ASC"#,
    );

    // An empty collection applies nothing at all.
    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            Vec::<psql::shared::LockChain>::new(),
        )),
        r#"SELECT "id" FROM "posts""#,
    );
}

/// Tuples nest, so a mod list is never limited by tuple arity.
#[test]
fn nested_tuples_apply_left_to_right() {
    check(
        &psql::select((
            (select::columns(quote("id")), select::from(quote("posts"))),
            (
                (select::where_(quote("views").gt(0)),),
                (select::order_by(quote("id")).asc(), select::limit(2)),
            ),
            (),
        )),
        r#"SELECT "id" FROM "posts" WHERE ("views" > 0) ORDER BY "id" ASC LIMIT 2"#,
    );
}

/// `apply` after construction, and a `clone` that does not disturb its original —
/// the property that makes a query value reusable as a base.
#[test]
fn apply_after_construction_and_clone_independence() {
    let base = psql::select((select::columns(quote("id")), select::from(quote("posts"))));

    let mut narrowed = base.clone();
    narrowed.apply((
        select::where_(quote("views").gt(arg(10i32))),
        select::order_by(quote("id")).desc(),
    ));

    let mut also = base.clone();
    also.apply(select::limit(1));

    check(&base, r#"SELECT "id" FROM "posts""#);
    let args = check(
        &narrowed,
        r#"SELECT "id" FROM "posts" WHERE ("views" > $1) ORDER BY "id" DESC"#,
    );
    assert_eq!(args, vec![Value::I32(10)]);
    check(&also, r#"SELECT "id" FROM "posts" LIMIT 1"#);

    // Building twice is not destructive either.
    check(
        &narrowed,
        r#"SELECT "id" FROM "posts" WHERE ("views" > $1) ORDER BY "id" DESC"#,
    );
}

/// A `from` applied after a join keeps the join, so replacing the table is not a
/// way to lose one silently.
#[test]
fn replacing_the_from_item_keeps_the_joins_already_on_it() {
    let mut q = psql::select((
        select::columns(quote(("c", "id"))),
        select::inner_join(quote("comments"))
            .as_("c")
            .on_eq(quote(("c", "post_id")), quote(("p", "id"))),
    ));
    q.apply(select::from(quote("posts")).as_("p"));
    check(
        &q,
        r#"SELECT "c"."id" FROM "posts" AS "p"
           INNER JOIN "comments" AS "c" ON ("c"."post_id" = "p"."id")"#,
    );
}

/// **Placeholder numbering across nested sub-queries.**
///
/// The counter belongs to the writer, so a sub-query re-indexes into its container
/// rather than restarting. This is the one piece of machinery whose failure is
/// silent and catastrophic: an off-by-one binds the wrong value to the wrong column
/// and the SQL still parses. The numbers here are the *render* order — `WITH`, then
/// the select list, then `FROM`, then `WHERE`, then `LIMIT` — which is the field
/// order of `SelectQuery`, not the order the mods were written in.
#[test]
fn placeholders_are_numbered_in_render_order_across_every_level_of_nesting() {
    let cte = psql::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        select::where_(quote("age").gt(arg(1i32))),
    ));
    let in_from = psql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::where_(quote("views").gt(arg(3i32))),
    ));
    let in_where = psql::select((
        select::columns(quote("post_id")),
        select::from(quote("comments")),
        select::where_(quote("user_id").eq(arg(4i32))),
    ));

    let q = psql::select((
        // Written last, rendered second.
        select::limit(arg(5i32)),
        select::with("chosen", cte),
        select::columns((quote(("s", "id")), cast(arg(2i32), "int"))),
        select::from(subquery(in_from)).as_("s"),
        select::where_(quote(("s", "id")).in_(query(in_where))),
    ));

    let args = check(
        &q,
        r#"WITH "chosen" AS (SELECT "id" FROM "users" WHERE ("age" > $1))
           SELECT "s"."id", CAST($2 AS int)
           FROM (SELECT "id" FROM "posts" WHERE ("views" > $3)) AS "s"
           WHERE ("s"."id" IN (SELECT "post_id" FROM "comments" WHERE ("user_id" = $4)))
           LIMIT $5"#,
    );
    assert_eq!(
        args,
        vec![
            Value::I32(1),
            Value::I32(2),
            Value::I32(3),
            Value::I32(4),
            Value::I32(5),
        ],
        "the argument list must be in the same order as the placeholders"
    );
}

/// The same property where it is hardest: three levels deep, and across a set
/// operation, whose operands render after every clause of the leading query.
#[test]
fn placeholders_survive_three_levels_of_nesting_and_a_set_operation() {
    let innermost = psql::select((
        select::columns(quote("id")),
        select::from(quote("tags")),
        select::where_(quote("name").eq(arg("rust"))),
    ));
    let middle = psql::select((
        select::columns(quote("tag_id")),
        select::from(quote("post_tags")),
        select::where_(quote("tag_id").in_(query(innermost))),
        select::where_(quote("post_id").gt(arg(2i32))),
    ));
    let operand = psql::select((
        select::columns(quote("id")),
        select::from(quote("comments")),
        select::where_(quote("user_id").eq(arg(4i32))),
    ));

    let q = psql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::where_(quote("id").in_(query(middle))),
        select::order_by(quote("id")),
        select::union_all(operand),
        select::limit_combined(arg(5i32)),
    ));

    let args = check(
        &q,
        r#"(SELECT "id" FROM "posts"
            WHERE ("id" IN (SELECT "tag_id" FROM "post_tags"
                            WHERE ("tag_id" IN (SELECT "id" FROM "tags"
                                                WHERE ("name" = $1)))
                              AND ("post_id" > $2)))
            ORDER BY "id")
           UNION ALL (SELECT "id" FROM "comments" WHERE ("user_id" = $3))
           LIMIT $4"#,
    );
    assert_eq!(
        args,
        vec![
            Value::Text("rust".into()),
            Value::I32(2),
            Value::I32(4),
            Value::I32(5),
        ]
    );
}

/// A sub-query in a slot that supplies its own parentheses versus one that does
/// not: `query` is bare, `subquery` brings a pair. Getting these the wrong way
/// round is a doubled or missing paren, so both are pinned.
#[test]
fn query_is_bare_and_subquery_parenthesises_itself() {
    let ids = || {
        psql::select((
            select::columns(quote("id")),
            select::from(quote("comments")),
        ))
    };

    // `IN (…)` supplies the parentheses, so the operand is bare.
    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::where_(quote("id").in_(query(ids()))),
        )),
        r#"SELECT "id" FROM "posts" WHERE ("id" IN (SELECT "id" FROM "comments"))"#,
    );

    // A `FROM` item's parentheses belong to the sub-query itself.
    check(
        &psql::select((
            select::columns(quote(("x", "id"))),
            select::from(subquery(ids())).as_("x"),
        )),
        r#"SELECT "x"."id" FROM (SELECT "id" FROM "comments") AS "x""#,
    );
}

// ===========================================================================
// Degenerate inputs: what an empty list means
// ===========================================================================

/// A helper handed an empty list must not leave a keyword dangling.
///
/// `GroupBy` writes `GROUP BY ` as soon as it holds one grouping element, and
/// `write_from_list` writes `FROM ` as soon as the from-item has an expression. A
/// fragment that then renders nothing produces SQL that cannot parse — and, before
/// the fix these two cases prompted, `build()` returned it as `Ok`. Every other
/// unfillable construct in the clause layer records `Error::Incomplete`.
#[test]
fn an_empty_grouping_element_or_function_list_is_a_recorded_failure() {
    for element in [rollup(()), cube(()), grouping_sets(())] {
        let q = psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::group_by(element),
        ));
        assert_eq!(
            q.build().unwrap_err().to_string(),
            "query is missing the columns of a grouping element"
        );
    }

    let q = psql::select((select::columns(quote("id")), select::from_function([])));
    assert_eq!(
        q.build().unwrap_err().to_string(),
        "query is missing the functions of a from-item"
    );

    // The non-empty forms are unaffected — `GROUPING SETS (())`, the legal empty
    // grouping *set*, still works, because `raw("()")` is an element.
    check(
        &psql::select((
            select::columns(f("count", "*")),
            select::from(quote("posts")),
            select::group_by(grouping_sets(raw("()"))),
        )),
        r#"SELECT count(*) FROM "posts" GROUP BY GROUPING SETS (())"#,
    );
}

/// `DISTINCT ON ()` is not in the grammar, so an empty `ON` list can only mean a
/// plain `DISTINCT` — which is what the `Option<Distinct>` shape represents and a
/// bare `on.is_empty()` check would not.
#[test]
fn an_empty_distinct_on_list_is_a_plain_distinct() {
    check(
        &psql::select((
            select::distinct_on(()),
            select::columns(quote("status")),
            select::from(quote("posts")),
        )),
        r#"SELECT DISTINCT "status" FROM "posts""#,
    );

    // And a later `distinct()` replaces an earlier `distinct_on(..)` wholesale,
    // because the two are alternatives in the grammar.
    check(
        &psql::select((
            select::distinct_on(quote("user_id")),
            select::distinct(),
            select::columns(quote("status")),
            select::from(quote("posts")),
        )),
        r#"SELECT DISTINCT "status" FROM "posts""#,
    );
}

/// An empty `OF` list leaves a bare lock, and `REPEATABLE` with no `TABLESAMPLE` is
/// dropped — `REPEATABLE` is a modifier of a sampling clause and means nothing
/// alone.
#[test]
fn modifiers_with_nothing_to_modify_are_dropped() {
    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::for_update().of(Vec::<String>::new()),
        )),
        r#"SELECT "id" FROM "posts" FOR UPDATE"#,
    );

    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("users")).repeatable(5),
        )),
        r#"SELECT "id" FROM "users""#,
    );
}

/// A window definition may be empty — `OVER ()` and `WINDOW "w" AS ()` are both
/// legal and mean the whole partition.
#[test]
fn an_empty_window_definition_is_legal() {
    check(
        &psql::select((
            select::columns(f("count", "*").over_name("w")),
            select::from(quote("posts")),
            select::window("w", ()),
        )),
        r#"SELECT count(*) OVER "w" FROM "posts" WINDOW "w" AS ()"#,
    );
}

/// A frame with nothing but an exclusion still renders, because both the mode and
/// the start bound have grammar defaults: `RANGE` and `UNBOUNDED PRECEDING`.
#[test]
fn a_frame_that_is_nothing_but_an_exclusion_uses_the_grammars_defaults() {
    check(
        &psql::select((
            select::columns(f("count", "*").over(frame::exclude_current_row())),
            select::from(quote("posts")),
        )),
        r#"SELECT count(*) OVER (RANGE UNBOUNDED PRECEDING EXCLUDE CURRENT ROW)
           FROM "posts""#,
    );
}

/// A clause set twice keeps the last value: `LIMIT` and `OFFSET` are single-valued
/// slots, unlike `WHERE` and `ORDER BY`, which accumulate.
#[test]
fn a_single_valued_clause_set_twice_keeps_the_last_value() {
    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::limit(1),
            select::limit(2),
            select::offset(3),
            select::offset(4),
        )),
        r#"SELECT "id" FROM "posts" LIMIT 2 OFFSET 4"#,
    );

    // `FETCH` is a chain, so it replaces the whole clause — `with_ties` set by an
    // earlier call does not survive a later `fetch`.
    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::order_by(quote("id")),
            select::fetch(1).with_ties(),
            select::fetch(2),
        )),
        r#"SELECT "id" FROM "posts" ORDER BY "id" FETCH NEXT 2 ROWS ONLY"#,
    );
}

/// `gram.y`'s `sortby: a_expr USING qual_all_Op opt_nulls_order` — a null ordering
/// may follow `USING`, not only `ASC`/`DESC`.
#[test]
fn a_null_ordering_follows_a_using_operator_too() {
    check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::order_by(quote("published_at"))
                .using("<")
                .nulls_first(),
        )),
        r#"SELECT "id" FROM "posts" ORDER BY "published_at" USING < NULLS FIRST"#,
    );
}

/// `FETCH NEXT $1 ROWS ONLY` — the count is an expression, so it binds.
#[test]
fn a_bound_fetch_count() {
    let args = check(
        &psql::select((
            select::columns(quote("id")),
            select::from(quote("posts")),
            select::order_by(quote("id")),
            select::fetch(arg(3i32)),
        )),
        r#"SELECT "id" FROM "posts" ORDER BY "id" FETCH NEXT $1 ROWS ONLY"#,
    );
    assert_eq!(args, vec![Value::I32(3)]);
}

/// `HAVING` without `GROUP BY` is legal: the whole result is one group.
#[test]
fn having_without_a_group_by() {
    check(
        &psql::select((
            select::columns(f("count", "*")),
            select::from(quote("posts")),
            select::having(f("count", "*").into_expr().gt(1)),
        )),
        r#"SELECT count(*) FROM "posts" HAVING (count(*) > 1)"#,
    );
}

/// A definition that copies a named window may not also re-partition it. The
/// grammar allows the shape — `window_definition` lists `existing_window_name` and
/// `PARTITION BY` side by side — and the server is what refuses it, with
/// `cannot override PARTITION BY clause of window "w"`.
///
/// Pinned because it is the second place `over(window::based_on(..))` differs from
/// [`Function::over_name`], and because it is squarely a case only the engine tier
/// can catch.
#[test]
fn copying_a_window_and_re_partitioning_it_is_refused_by_the_server() {
    let q = psql::select((
        select::columns(
            f("count", "*").over((window::based_on("w"), window::partition_by(quote("status")))),
        ),
        select::from(quote("posts")),
        select::window("w", window::partition_by(quote("user_id"))),
    ));
    let (sql, _) = q
        .build()
        .expect("it still builds — rendering is not validation");
    assert_eq!(
        keelson_sqlcheck::normalize(&sql),
        r#"SELECT count(*) OVER ("w" PARTITION BY "status") FROM "posts" WINDOW "w" AS (PARTITION BY "user_id")"#
    );
    keelson_sqlcheck::assert_valid(Dialect::Psql, &sql);
    if keelson_sqlcheck::live::available().contains(&Dialect::Psql) {
        assert!(
            keelson_sqlcheck::live::check(Dialect::Psql, &sql).is_err(),
            "re-partitioning a copied window should be refused: {sql}"
        );
    }
}
