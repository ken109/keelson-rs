//! One case per shape, so the exhaustive grammar walk that comes next starts from
//! something known-good at both tiers.
//!
//! Every case goes through [`assert_sql`], which runs the libpg_query grammar, a
//! real PostgreSQL 17 when one is compiled in (`--features live-docker`), and the
//! whitespace-normalised comparison against the string written here.
//!
//! **Where the expected strings come from.** Each is derived from the clause
//! production in the PostgreSQL 17 reference manual — cited in the test where the
//! shape is not obvious — and from the rendering rules `keelson_core::clause`
//! documents: a clause writes its own keyword, an absent clause writes nothing at
//! all, and every operator from the chain parenthesises its own result exactly once.
//! None of them was produced by running the builder and pasting the output.
//!
//! **Every table and column named here is in `tests/schema/psql.sql`.** That is what
//! lets the engine tier resolve names, which is where the sharp failures are.

use keelson_psql as psql;
use keelson_psql::{
    Chain, Expr, IntoExpr, PsqlOps, Query, RawArg, Value, arg, arg_group, case_, cast, cube,
    delete, f, frame, group, grouping_sets, insert, not, or, query, quote, raw, rollup, s, select,
    subquery, template, update, window,
};
use keelson_sqlcheck::{Dialect, assert_sql};

/// Build, then run every check this build can: grammar, engine, and intent.
#[track_caller]
fn check(q: &impl Query, expected: &str) -> Vec<Value> {
    let (sql, args) = q.build().expect("the query should build");
    assert_sql(Dialect::Psql, &sql, expected);
    args
}

// ---------------------------------------------------------------------------
// SELECT
// ---------------------------------------------------------------------------

#[test]
fn select_columns_from_where_with_a_bound_argument() {
    let q = psql::select((
        select::columns((quote("id"), quote("name"))),
        select::from(quote("users")),
        select::where_(quote("age").gte(arg(21i32))),
    ));
    let args = check(
        &q,
        r#"SELECT "id", "name" FROM "users" WHERE ("age" >= $1)"#,
    );
    assert_eq!(args, vec![Value::I32(21)]);
}

#[test]
fn select_with_no_mods_at_all_is_a_star_projection() {
    // `SelectList` is the one clause whose absent rendering is not empty.
    let q = psql::select(select::from(quote("users")));
    assert!(check(&q, r#"SELECT * FROM "users""#).is_empty());
}

/// `SELECT [ ALL | DISTINCT [ ON ( expression [, ...] ) ] ]`, and `DISTINCT ON`
/// requires the `ORDER BY` to start with the same expressions.
#[test]
fn select_distinct_on_with_an_order_by_that_matches() {
    let q = psql::select((
        select::distinct_on(quote("status")),
        select::columns((quote("status"), quote("views"))),
        select::from(quote("posts")),
        select::order_by(quote("status")).asc(),
        select::order_by(quote("views")).desc().nulls_last(),
    ));
    check(
        &q,
        r#"SELECT DISTINCT ON ("status") "status", "views" FROM "posts"
           ORDER BY "status" ASC, "views" DESC NULLS LAST"#,
    );
}

#[test]
fn select_distinct_with_limit_and_a_bound_offset() {
    let q = psql::select((
        select::distinct(),
        select::columns(quote("status")),
        select::from(quote("posts")),
        select::limit(10),
        select::offset(arg(5i32)),
    ));
    // A number is a literal and `arg` is bound — the split `IntoExpr` makes.
    let args = check(
        &q,
        r#"SELECT DISTINCT "status" FROM "posts" LIMIT 10 OFFSET $1"#,
    );
    assert_eq!(args, vec![Value::I32(5)]);
}

/// `FOR { UPDATE | NO KEY UPDATE | SHARE | KEY SHARE } [ OF table_name [, ...] ]
/// [ NOWAIT | SKIP LOCKED ] [...]` — the trailing `[...]` is why this is a list.
#[test]
fn select_carries_several_locking_clauses_each_scoped_to_a_table() {
    let q = psql::select((
        select::columns(quote(("u", "id"))),
        select::from(quote("users")).as_("u"),
        select::cross_join(quote("posts")).as_("p"),
        select::for_share().of(["u"]),
        select::for_key_share().of(["p"]).no_wait(),
    ));
    check(
        &q,
        r#"SELECT "u"."id" FROM "users" AS "u" CROSS JOIN "posts" AS "p"
           FOR SHARE OF "u" FOR KEY SHARE OF "p" NOWAIT"#,
    );
}

#[test]
fn select_for_no_key_update_skip_locked() {
    let q = psql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::for_no_key_update().of(["posts"]).skip_locked(),
    ));
    check(
        &q,
        r#"SELECT "id" FROM "posts" FOR NO KEY UPDATE OF "posts" SKIP LOCKED"#,
    );
}

#[test]
fn select_left_join_group_by_having_with_an_aggregate() {
    let q = psql::select((
        select::columns((quote(("u", "name")), f("count", quote(("p", "id"))))),
        select::from(quote("users")).as_("u"),
        select::left_join(quote("posts"))
            .as_("p")
            .on_eq(quote(("p", "user_id")), quote(("u", "id"))),
        select::group_by(quote(("u", "name"))),
        select::having(f("count", quote(("p", "id"))).into_expr().gt(arg(5i32))),
    ));
    let args = check(
        &q,
        r#"SELECT "u"."name", count("p"."id") FROM "users" AS "u"
           LEFT JOIN "posts" AS "p" ON ("p"."user_id" = "u"."id")
           GROUP BY "u"."name" HAVING (count("p"."id") > $1)"#,
    );
    assert_eq!(args, vec![Value::I32(5)]);
}

/// `from_item [ NATURAL ] join_type from_item [ ON … | USING ( … ) ]`. `USING`
/// merges the named columns, which is what keeps a bare `"id"` unambiguous here.
#[test]
fn select_full_join_using_and_natural_right_join() {
    let using = psql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::full_join(quote("comments")).using(["id"]),
    ));
    check(
        &using,
        r#"SELECT "id" FROM "posts" FULL JOIN "comments" USING ("id")"#,
    );

    let natural = psql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::right_join(quote("comments")).natural(),
    ));
    check(
        &natural,
        r#"SELECT "id" FROM "posts" NATURAL RIGHT JOIN "comments""#,
    );
}

#[test]
fn select_inner_join_with_two_on_conditions_is_and_joined() {
    let q = psql::select((
        select::columns(quote(("p", "id"))),
        select::from(quote("posts")).as_("p"),
        select::inner_join(quote("post_tags"))
            .as_("pt")
            .on(quote(("pt", "post_id")).eq(quote(("p", "id"))))
            .on(quote(("pt", "tag_id")).gt(arg(0i32))),
    ));
    let args = check(
        &q,
        r#"SELECT "p"."id" FROM "posts" AS "p" INNER JOIN "post_tags" AS "pt"
           ON ("pt"."post_id" = "p"."id") AND ("pt"."tag_id" > $1)"#,
    );
    assert_eq!(args, vec![Value::I32(0)]);
}

/// `FROM from_item [, ...]` — a comma-separated list, which means the same thing as
/// `CROSS JOIN`.
#[test]
fn select_from_takes_more_than_one_item() {
    let q = psql::select((
        select::columns((quote(("u", "id")), quote(("t", "name")))),
        select::from(quote("users")).as_("u"),
        select::from_also(quote("tags")).as_("t"),
    ));
    check(
        &q,
        r#"SELECT "u"."id", "t"."name" FROM "users" AS "u", "tags" AS "t""#,
    );
}

/// `LATERAL` is what lets a joined sub-query see the columns of the item it is
/// joined to — and the reason the flag lives on the joined `from_item`, in front of
/// it, rather than on the join keyword.
#[test]
fn select_lateral_join_onto_a_correlated_subquery() {
    let recent = psql::select((
        select::columns(quote("title")),
        select::from(quote("posts")),
        select::where_(quote(("posts", "user_id")).eq(quote(("u", "id")))),
        select::limit(1),
    ));
    let q = psql::select((
        select::columns((quote(("u", "id")), quote(("p", "title")))),
        select::from(quote("users")).as_("u"),
        select::inner_join(subquery(recent))
            .lateral()
            .as_("p")
            .on(raw("true")),
    ));
    check(
        &q,
        r#"SELECT "u"."id", "p"."title" FROM "users" AS "u"
           INNER JOIN LATERAL (SELECT "title" FROM "posts" WHERE ("posts"."user_id" = "u"."id") LIMIT 1)
           AS "p" ON true"#,
    );
}

/// `gram.y`: `table_ref: relation_expr opt_alias_clause tablesample_clause`, so the
/// sampling clause comes *after* the alias while `ONLY` comes before the name.
#[test]
fn select_from_only_a_sampled_table() {
    let q = psql::select((
        select::columns(quote("id")),
        select::from(quote("users"))
            .only()
            .as_("u")
            .tablesample("SYSTEM", 10)
            .repeatable(5),
    ));
    check(
        &q,
        r#"SELECT "id" FROM ONLY "users" AS "u" TABLESAMPLE SYSTEM (10) REPEATABLE (5)"#,
    );
}

#[test]
fn select_from_set_returning_functions_plainly_and_as_rows_from() {
    // The casts are not decoration: a bare `$1` in a set-returning function's
    // argument leaves PostgreSQL unable to determine the parameter's type, and it
    // says so at PREPARE time. The engine tier is what found that.
    let one = psql::select(
        select::from_function([f(
            "generate_series",
            (cast(arg(1i32), "int"), cast(arg(3i32), "int")),
        )])
        .as_("g"),
    );
    let args = check(
        &one,
        r#"SELECT * FROM generate_series(CAST($1 AS int), CAST($2 AS int)) AS "g""#,
    );
    assert_eq!(args, vec![Value::I32(1), Value::I32(3)]);

    // Two or more become `ROWS FROM (…)`; `WITH ORDINALITY` follows the item and
    // precedes the alias.
    let many = psql::select(
        select::from_function([f("generate_series", (1, 3)), f("generate_series", (4, 6))])
            .with_ordinality()
            .as_("g"),
    );
    check(
        &many,
        r#"SELECT * FROM ROWS FROM (generate_series(1, 3), generate_series(4, 6))
           WITH ORDINALITY AS "g""#,
    );
}

/// Both window forms, in the only arrangement PostgreSQL accepts.
///
/// `OVER "w"` *references* the window; `("base" ORDER BY …)` *copies* `base` and adds
/// to it, which is only legal because `base` has no frame clause of its own — a
/// definition that copies a framed window is refused outright, with
/// `HINT: Omit the parentheses in this OVER clause`. That is what
/// [`over_name`](keelson_psql::Function::over_name) exists for, and it is the engine
/// tier that found it.
#[test]
fn select_with_a_named_window_referenced_by_over_and_one_that_extends_another() {
    let q = psql::select((
        select::columns((quote("id"), f("avg", quote("views")).over_name("w"))),
        select::from(quote("posts")),
        select::window("base", window::partition_by(quote("user_id"))),
        select::window(
            "w",
            (
                window::based_on("base"),
                window::order_by(quote("id")).asc(),
                frame::rows(),
                frame::from_preceding(1),
                frame::to_current_row(),
            ),
        ),
    ));
    check(
        &q,
        r#"SELECT "id", avg("views") OVER "w" FROM "posts"
           WINDOW "base" AS (PARTITION BY "user_id"),
                  "w" AS ("base" ORDER BY "id" ASC
                          ROWS BETWEEN 1 PRECEDING AND CURRENT ROW)"#,
    );
}

/// `{ RANGE | ROWS | GROUPS } BETWEEN frame_start AND frame_end [ frame_exclusion ]`.
/// `GROUPS` counts peer groups, so PostgreSQL requires the window to be ordered.
#[test]
fn select_with_an_inline_groups_frame_and_an_exclusion() {
    let q = psql::select((
        select::columns(f("count", "*").over((
            window::partition_by(quote("user_id")),
            window::order_by(quote("views")),
            frame::groups(),
            frame::from_unbounded_preceding(),
            frame::to_current_row(),
            frame::exclude_ties(),
        ))),
        select::from(quote("posts")),
    ));
    check(
        &q,
        r#"SELECT count(*) OVER (PARTITION BY "user_id" ORDER BY "views"
                                 GROUPS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW
                                 EXCLUDE TIES) FROM "posts""#,
    );
}

/// Without an end bound there is no `BETWEEN`; the exclusion still follows.
#[test]
fn select_with_a_range_frame_that_has_no_end_bound() {
    let q = psql::select((
        select::columns(f("sum", quote("views")).over((
            window::order_by(quote("id")),
            frame::range(),
            frame::from_unbounded_preceding(),
            frame::exclude_current_row(),
        ))),
        select::from(quote("posts")),
    ));
    check(
        &q,
        r#"SELECT sum("views") OVER (ORDER BY "id" RANGE UNBOUNDED PRECEDING
                                    EXCLUDE CURRENT ROW) FROM "posts""#,
    );
}

/// 4.2.7 and 4.2.8: `DISTINCT` sits inside the argument list and `FILTER` after it.
#[test]
fn select_a_distinct_filtered_aggregate() {
    let q = psql::select((
        select::columns(
            f("count", quote("id"))
                .distinct()
                .filter(quote("is_active")),
        ),
        select::from(quote("users")),
    ));
    check(
        &q,
        r#"SELECT count(DISTINCT "id") FILTER (WHERE "is_active") FROM "users""#,
    );
}

#[test]
fn select_group_by_distinct_over_a_cube() {
    let q = psql::select((
        select::columns((quote("status"), f("count", quote("id")))),
        select::from(quote("posts")),
        select::group_by_distinct(true),
        select::group_by(cube((quote("status"), quote("views")))),
    ));
    check(
        &q,
        r#"SELECT "status", count("id") FROM "posts"
           GROUP BY DISTINCT CUBE ("status", "views")"#,
    );
}

/// `grouping_element` again: several elements are comma-separated, and the empty
/// grouping set is written `()` — not an empty `group`, which would be the one-null
/// row `(NULL)`.
#[test]
fn select_group_by_rollup_and_explicit_grouping_sets() {
    let q = psql::select((
        select::columns(quote("status")),
        select::from(quote("posts")),
        select::group_by(rollup(quote("status"))),
        select::group_by(grouping_sets((group(quote("views")), raw("()")))),
    ));
    check(
        &q,
        r#"SELECT "status" FROM "posts"
           GROUP BY ROLLUP ("status"), GROUPING SETS (("views"), ())"#,
    );
}

#[test]
fn select_from_a_materialized_cte_with_renamed_columns() {
    let recent = psql::select((
        select::columns((quote("id"), quote("user_id"))),
        select::from(quote("posts")),
        select::where_(quote("views").gt(arg(100i32))),
    ));
    let q = psql::select((
        select::with("recent", recent)
            .columns(["pid", "uid"])
            .materialized(),
        select::columns(quote("pid")),
        select::from(quote("recent")),
    ));
    let args = check(
        &q,
        r#"WITH "recent" ("pid", "uid") AS MATERIALIZED
             (SELECT "id", "user_id" FROM "posts" WHERE ("views" > $1))
           SELECT "pid" FROM "recent""#,
    );
    assert_eq!(args, vec![Value::I32(100)]);
}

/// `WITH RECURSIVE`, `SEARCH` and `CYCLE` all at once. The recursive term has to be
/// `non-recursive UNION [ALL] recursive`, which is exactly what a leading query with
/// no trailing clauses plus one `Combine` renders.
#[test]
fn select_from_a_recursive_cte_with_search_and_cycle() {
    let step = psql::select((
        select::columns(quote(("p", "id"))),
        select::from(quote("posts")).as_("p"),
        select::inner_join(quote("t")).on_eq(quote(("t", "id")), quote(("p", "user_id"))),
    ));
    let body = psql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::where_(quote("id").eq(arg(1i32))),
        select::union_all(step),
    ));
    let q = psql::select((
        select::recursive(true),
        select::with("t", body)
            .columns(["id"])
            .search_depth("ord", ["id"])
            .cycle("is_cycle", "path", ["id"])
            .cycle_value(raw("true"), raw("false")),
        select::columns(quote("id")),
        select::from(quote("t")),
    ));
    let args = check(
        &q,
        r#"WITH RECURSIVE "t" ("id") AS
             (SELECT "id" FROM "posts" WHERE ("id" = $1)
              UNION ALL (SELECT "p"."id" FROM "posts" AS "p"
                         INNER JOIN "t" ON ("t"."id" = "p"."user_id")))
             SEARCH DEPTH FIRST BY "id" SET "ord"
             CYCLE "id" SET "is_cycle" TO true DEFAULT false USING "path"
           SELECT "id" FROM "t""#,
    );
    assert_eq!(args, vec![Value::I32(1)]);
}

/// > `ORDER BY` and `LIMIT` … can be attached to a subexpression if it is enclosed
/// > in parentheses. Without parentheses, these clauses will be taken to apply to
/// > the result of the `UNION`, not to its right-hand input expression.
///
/// So the leading query is parenthesised exactly because it has a `LIMIT` of its
/// own, and the combination's own trailing clauses land after the last operand.
#[test]
fn union_all_parenthesises_the_leading_query_that_has_its_own_limit() {
    let other = psql::select((
        select::columns(quote("id")),
        select::from(quote("comments")),
    ));
    let q = psql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::limit(1),
        select::union_all(other),
        select::order_by_combined(1),
        select::limit_combined(5),
        select::offset_combined(0),
    ));
    check(
        &q,
        r#"(SELECT "id" FROM "posts" LIMIT 1) UNION ALL (SELECT "id" FROM "comments")
           ORDER BY 1 LIMIT 5 OFFSET 0"#,
    );
}

#[test]
fn intersect_and_except_all_chain_without_parenthesising_the_leading_query() {
    let tagged = psql::select((
        select::columns(quote("post_id")),
        select::from(quote("post_tags")),
    ));
    let commented = psql::select((
        select::columns(quote("post_id")),
        select::from(quote("comments")),
    ));
    let q = psql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::intersect(tagged),
        select::except_all(commented),
    ));
    check(
        &q,
        r#"SELECT "id" FROM "posts"
           INTERSECT (SELECT "post_id" FROM "post_tags")
           EXCEPT ALL (SELECT "post_id" FROM "comments")"#,
    );
}

/// `FETCH { FIRST | NEXT } [ count ] { ROW | ROWS } { ONLY | WITH TIES }` — the
/// synonyms are collapsed to one spelling each, and `WITH TIES` needs the
/// `ORDER BY`.
#[test]
fn select_fetch_with_ties_and_a_combined_fetch() {
    let q = psql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::order_by(quote("views")).desc(),
        select::fetch(3).with_ties(),
    ));
    check(
        &q,
        r#"SELECT "id" FROM "posts" ORDER BY "views" DESC FETCH NEXT 3 ROWS WITH TIES"#,
    );

    let other = psql::select((
        select::columns(quote("id")),
        select::from(quote("comments")),
    ));
    let combined = psql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::union(other),
        select::order_by_combined(1),
        select::fetch_combined(2),
    ));
    check(
        &combined,
        r#"SELECT "id" FROM "posts" UNION (SELECT "id" FROM "comments")
           ORDER BY 1 FETCH NEXT 2 ROWS ONLY"#,
    );
}

/// `ORDER BY expression [ COLLATE … ] [ ASC | DESC | USING operator ]
/// [ NULLS { FIRST | LAST } ]`, and `LIMIT { count | ALL }`.
#[test]
fn select_order_by_collate_using_and_limit_all() {
    let q = psql::select((
        select::columns(quote("name")),
        select::from(quote("users")),
        select::order_by(quote("name"))
            .collate("C")
            .desc()
            .nulls_last(),
        select::order_by(quote("id")).using(">"),
        select::limit_all(),
    ));
    check(
        &q,
        r#"SELECT "name" FROM "users"
           ORDER BY "name" COLLATE "C" DESC NULLS LAST, "id" USING > LIMIT ALL"#,
    );
}

#[test]
fn select_a_case_expression_with_psql_only_operators() {
    let q = psql::select((
        select::columns(
            case_()
                .when(quote("status").eq(arg("hot")), s("high"))
                .else_(s("low"))
                .as_("tier"),
        ),
        select::from(quote("posts")),
        select::where_(quote("title").ilike(arg("%rust%"))),
        select::where_(not(quote("status").is_null())),
    ));
    let args = check(
        &q,
        r#"SELECT (CASE WHEN ("status" = $1) THEN 'high' ELSE 'low' END) AS "tier"
           FROM "posts" WHERE ("title" ILIKE $2) AND NOT ("status" IS NULL)"#,
    );
    assert_eq!(
        args,
        vec![Value::Text("hot".into()), Value::Text("%rust%".into())]
    );
}

#[test]
fn select_with_an_or_group_and_a_row_constructor_in_a_list_of_rows() {
    let q = psql::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        select::where_(or((quote("id").eq(arg(1i32)), quote("id").eq(arg(2i32))))),
        select::where_(
            group((quote("id"), quote("age"))).in_((arg_group([3i32, 4]), arg_group([5i32, 6]))),
        ),
    ));
    let args = check(
        &q,
        r#"SELECT "id" FROM "users"
           WHERE (("id" = $1) OR ("id" = $2))
             AND (("id", "age") IN (($3, $4), ($5, $6)))"#,
    );
    assert_eq!(args.len(), 6);
    assert_eq!(args[5], Value::I32(6));
}

#[test]
fn a_raw_template_rewrites_its_placeholders_into_dollar_positions() {
    let q = psql::select((
        select::from(quote("users")),
        select::where_(template(
            r#""age" > ? AND "age" < ?"#,
            [RawArg::value(1i32), RawArg::value(9i32)],
        )),
    ));
    let args = check(
        &q,
        r#"SELECT * FROM "users" WHERE "age" > $1 AND "age" < $2"#,
    );
    assert_eq!(args, vec![Value::I32(1), Value::I32(9)]);
}

// ---------------------------------------------------------------------------
// INSERT
// ---------------------------------------------------------------------------

#[test]
fn insert_upsert_with_an_excluded_assignment_and_a_row_filter() {
    let q = psql::insert((
        insert::into(quote("users")).columns(["id", "name"]),
        insert::values((arg(1i32), arg("ada"))),
        insert::on_conflict(quote("id")).do_update((
            insert::set_excluded(["name"]),
            insert::where_(quote(("users", "is_active"))),
        )),
        insert::returning(quote("id")),
    ));
    let args = check(
        &q,
        r#"INSERT INTO "users" ("id", "name") VALUES ($1, $2)
           ON CONFLICT ("id") DO UPDATE SET "name" = EXCLUDED."name"
           WHERE "users"."is_active" RETURNING "id""#,
    );
    assert_eq!(args, vec![Value::I32(1), Value::Text("ada".into())]);
}

/// `conflict_target: … | ON CONSTRAINT constraint_name`, which names an index
/// directly instead of inferring one.
#[test]
fn insert_on_conflict_on_constraint_do_nothing() {
    let q = psql::insert((
        insert::into(quote("tags")).columns(["id", "name"]),
        insert::values((arg(1i32), arg("rust"))),
        insert::on_conflict_on_constraint("tags_pkey").do_nothing(),
    ));
    check(
        &q,
        r#"INSERT INTO "tags" ("id", "name") VALUES ($1, $2)
           ON CONFLICT ON CONSTRAINT "tags_pkey" DO NOTHING"#,
    );
}

/// With no target at all, `ON CONFLICT DO NOTHING` covers every unique constraint.
#[test]
fn insert_on_conflict_with_no_target_and_default_values() {
    let q = psql::insert((
        insert::into(quote("tags")),
        insert::on_conflict(()).do_nothing(),
    ));
    check(
        &q,
        r#"INSERT INTO "tags" DEFAULT VALUES ON CONFLICT DO NOTHING"#,
    );
}

/// `{ DEFAULT VALUES | VALUES ( { expression | DEFAULT } [, ...] ) [, ...] | query }`
/// — several rows, and `DEFAULT` as a cell.
#[test]
fn insert_several_rows_one_of_which_defaults_a_column() {
    let q = psql::insert((
        insert::into(quote("posts")).columns(["id", "user_id", "title", "views"]),
        insert::rows([
            (arg(1i32), arg(1i32), arg("a"), Expr::raw("DEFAULT")),
            (arg(2i32), arg(2i32), arg("b"), Expr::raw("DEFAULT")),
        ]),
    ));
    let args = check(
        &q,
        r#"INSERT INTO "posts" ("id", "user_id", "title", "views")
           VALUES ($1, $2, $3, DEFAULT), ($4, $5, $6, DEFAULT)"#,
    );
    assert_eq!(args.len(), 6);
}

#[test]
fn insert_from_a_select_rather_than_from_rows() {
    let source = psql::select((
        select::columns((quote("id"), arg(1i32))),
        select::from(quote("posts")),
        select::where_(quote("views").gt(arg(0i32))),
    ));
    let q = psql::insert((
        insert::into(quote("post_tags")).columns(["post_id", "tag_id"]),
        insert::query(source),
        insert::returning("*"),
    ));
    let args = check(
        &q,
        r#"INSERT INTO "post_tags" ("post_id", "tag_id")
           SELECT "id", $1 FROM "posts" WHERE ("views" > $2) RETURNING *"#,
    );
    assert_eq!(args, vec![Value::I32(1), Value::I32(0)]);
}

/// `INSERT INTO table [ AS alias ] [ ( column [, ...] ) ]
/// [ OVERRIDING { SYSTEM | USER } VALUE ] { … }` — the clause sits between the
/// column list and the row source.
///
/// No column of the shared schema is an identity column, so this would fail at
/// *execution*; `PREPARE` only parses and analyses, which is exactly the level the
/// judge works at, so both spellings are still checked end to end.
#[test]
fn insert_overriding_an_identity_column() {
    let system = psql::insert((
        insert::into(quote("users")).columns(["id", "name"]),
        insert::overriding_system(),
        insert::values((arg(1i32), arg("ada"))),
    ));
    check(
        &system,
        r#"INSERT INTO "users" ("id", "name") OVERRIDING SYSTEM VALUE VALUES ($1, $2)"#,
    );

    let user = psql::insert((
        insert::into(quote("users")).columns(["id", "name"]),
        insert::overriding_user(),
        insert::values((arg(1i32), arg("ada"))),
    ));
    check(
        &user,
        r#"INSERT INTO "users" ("id", "name") OVERRIDING USER VALUE VALUES ($1, $2)"#,
    );
}

/// `[ WHERE condition | WHERE CURRENT OF cursor_name ]` — an alternative to a
/// condition rather than an addition to one, so a statement using it should have no
/// other `WHERE`.
#[test]
fn update_and_delete_where_current_of_a_cursor() {
    let updated = psql::update((
        update::table(quote("posts")),
        update::set_col("views").to_arg(0i32),
        update::where_current_of("c"),
    ));
    let args = check(
        &updated,
        r#"UPDATE "posts" SET "views" = $1 WHERE CURRENT OF "c""#,
    );
    assert_eq!(args, vec![Value::I32(0)]);

    let deleted = psql::delete((delete::from(quote("posts")), delete::where_current_of("c")));
    check(&deleted, r#"DELETE FROM "posts" WHERE CURRENT OF "c""#);
}

#[test]
fn insert_into_an_aliased_table_from_a_cte() {
    let ids = psql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::limit(1),
    ));
    let from_cte = psql::select((select::columns(quote("id")), select::from(quote("chosen"))));
    let q = psql::insert((
        insert::with("chosen", ids),
        insert::into(quote("post_tags"))
            .as_("pt")
            .columns(["post_id", "tag_id"]),
        insert::query(psql::select((
            select::columns((quote("id"), arg(7i32))),
            select::from(subquery(from_cte)).as_("c"),
        ))),
    ));
    let args = check(
        &q,
        r#"WITH "chosen" AS (SELECT "id" FROM "posts" LIMIT 1)
           INSERT INTO "post_tags" AS "pt" ("post_id", "tag_id")
           SELECT "id", $1 FROM (SELECT "id" FROM "chosen") AS "c""#,
    );
    assert_eq!(args, vec![Value::I32(7)]);
}

// ---------------------------------------------------------------------------
// UPDATE
// ---------------------------------------------------------------------------

#[test]
fn update_with_a_from_item_two_assignments_and_returning() {
    let q = psql::update((
        update::table(quote("posts")).as_("p"),
        update::set_col("views").to(quote(("p", "views")).plus(arg(1i32))),
        update::set_col("status").to_arg("hot"),
        update::from(quote("users")).as_("u"),
        update::where_(quote(("u", "id")).eq(quote(("p", "user_id")))),
        update::where_(quote(("u", "is_active"))),
        update::returning(quote(("p", "id"))),
    ));
    let args = check(
        &q,
        r#"UPDATE "posts" AS "p" SET "views" = ("p"."views" + $1), "status" = $2
           FROM "users" AS "u"
           WHERE ("u"."id" = "p"."user_id") AND "u"."is_active"
           RETURNING "p"."id""#,
    );
    assert_eq!(args, vec![Value::I32(1), Value::Text("hot".into())]);
}

/// `( column_name [, ...] ) = ( sub-SELECT )` — one assignment with a row on each
/// side, which is why an assignment is a whole expression here and not a pair.
#[test]
fn update_only_with_a_multi_column_assignment_from_a_subselect() {
    let source = psql::select((
        select::columns((quote("title"), quote("status"))),
        select::from(quote("posts")),
        select::limit(1),
    ));
    let q = psql::update((
        update::table(quote("users")).only(),
        update::set(Expr::binary(
            group((quote("name"), quote("email"))),
            "=",
            subquery(source),
        )),
        update::where_(quote("id").eq(arg(1i32))),
    ));
    let args = check(
        &q,
        r#"UPDATE ONLY "users"
           SET ("name", "email") = (SELECT "title", "status" FROM "posts" LIMIT 1)
           WHERE ("id" = $1)"#,
    );
    assert_eq!(args, vec![Value::I32(1)]);
}

/// The joins of an `UPDATE` attach to the `FROM` item, never to the target — which
/// is the whole reason `update::table` and `update::from` are different mods.
#[test]
fn update_with_a_cte_and_a_joined_from_item() {
    let active = psql::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        select::where_(quote("is_active")),
    ));
    let q = psql::update((
        update::with("active", active),
        update::table(quote("posts")).as_("p"),
        update::set_col("status").to_arg("archived"),
        update::from(quote("active")).as_("a"),
        update::inner_join(quote("comments"))
            .as_("c")
            .on_eq(quote(("c", "user_id")), quote(("a", "id"))),
        update::where_(quote(("p", "user_id")).eq(quote(("a", "id")))),
    ));
    let args = check(
        &q,
        r#"WITH "active" AS (SELECT "id" FROM "users" WHERE "is_active")
           UPDATE "posts" AS "p" SET "status" = $1
           FROM "active" AS "a" INNER JOIN "comments" AS "c" ON ("c"."user_id" = "a"."id")
           WHERE ("p"."user_id" = "a"."id")"#,
    );
    assert_eq!(args, vec![Value::Text("archived".into())]);
}

/// `ON` and `USING` are alternatives in the grammar, and nothing in the clause layer
/// forbids writing both — the mods are what the caller picks between. This pins that
/// the judge, not the builder, is what says no, so a later phase does not mistake
/// the omission for a rendering bug.
#[test]
fn a_join_with_both_on_and_using_is_refused_by_postgresql_not_by_the_builder() {
    let q = psql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::inner_join(quote("comments"))
            .on(raw("true"))
            .using(["id"]),
    ));
    let (sql, _) = q
        .build()
        .expect("it still builds — rendering is not validation");
    assert_eq!(
        sql,
        r#"SELECT "id" FROM "posts" INNER JOIN "comments" ON true USING ("id")"#
    );
    assert!(
        keelson_sqlcheck::check_psql(&sql).is_err(),
        "libpg_query should refuse a join with both ON and USING: {sql}"
    );
}

// ---------------------------------------------------------------------------
// DELETE
// ---------------------------------------------------------------------------

#[test]
fn delete_using_another_table_with_returning() {
    let q = psql::delete((
        delete::from(quote("comments")).as_("c"),
        delete::using(quote("posts")).as_("p"),
        delete::where_(quote(("c", "post_id")).eq(quote(("p", "id")))),
        delete::where_(quote(("p", "status")).eq(arg("draft"))),
        delete::returning(quote(("c", "id"))),
    ));
    let args = check(
        &q,
        r#"DELETE FROM "comments" AS "c" USING "posts" AS "p"
           WHERE ("c"."post_id" = "p"."id") AND ("p"."status" = $1)
           RETURNING "c"."id""#,
    );
    assert_eq!(args, vec![Value::Text("draft".into())]);
}

#[test]
fn delete_only_with_a_cte_and_a_nested_subquery_in_the_predicate() {
    let stale = psql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::where_(quote("views").eq(arg(0i32))),
    ));
    let from_cte = psql::select((select::columns(quote("id")), select::from(quote("stale"))));
    let q = psql::delete((
        delete::with("stale", stale),
        delete::from(quote("post_tags")).only(),
        delete::where_(quote("post_id").in_(query(from_cte))),
    ));
    let args = check(
        &q,
        r#"WITH "stale" AS (SELECT "id" FROM "posts" WHERE ("views" = $1))
           DELETE FROM ONLY "post_tags" WHERE ("post_id" IN (SELECT "id" FROM "stale"))"#,
    );
    assert_eq!(args, vec![Value::I32(0)]);
}

#[test]
fn delete_using_two_items() {
    let q = psql::delete((
        delete::from(quote("post_tags")).as_("pt"),
        delete::using(quote("posts")).as_("p"),
        delete::using_also(quote("tags")).as_("t"),
        delete::where_(quote(("pt", "post_id")).eq(quote(("p", "id")))),
        delete::where_(quote(("pt", "tag_id")).eq(quote(("t", "id")))),
        delete::where_(quote(("t", "name")).eq(arg("stale"))),
    ));
    let args = check(
        &q,
        r#"DELETE FROM "post_tags" AS "pt" USING "posts" AS "p", "tags" AS "t"
           WHERE ("pt"."post_id" = "p"."id") AND ("pt"."tag_id" = "t"."id")
             AND ("t"."name" = $1)"#,
    );
    assert_eq!(args, vec![Value::Text("stale".into())]);
}
