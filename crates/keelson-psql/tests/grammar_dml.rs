//! A walk of PostgreSQL 17's `INSERT`, `UPDATE` and `DELETE` grammar.
//!
//! Every case goes through [`assert_sql`], which runs three checks in one call:
//! libpg_query — PostgreSQL's own parser — accepts the SQL, a real PostgreSQL 17
//! accepts it too when one is compiled in (`--features live-docker`), and the SQL
//! equals the whitespace-normalised string written here.
//!
//! **Where the expected strings come from.** Each is derived from the production in
//! the PostgreSQL 17 reference manual — `sql-insert.html`, `sql-update.html`,
//! `sql-delete.html`, and `gram.y` where the manual's summary is ambiguous — cited
//! in the test whenever the shape is not obvious, composed with the rendering rules
//! `keelson_core::clause` documents:
//!
//! * clauses are separated by a single space, and a clause writes its own keyword;
//! * an absent clause writes nothing at all, comma and all;
//! * every operator that comes from [`Chain`] parenthesises its own result exactly
//!   once, so `eq` is `(a = b)` and nesting adds no second pair;
//! * `Expr::binary` is the *un*-parenthesised infix form, which is what an
//!   assignment needs;
//! * `TableRef` writes `[ONLY ][LATERAL ]expr[ AS "alias"][ ("cols")][ joins]`, and
//!   for an `INSERT` that column list is the insert column list.
//!
//! None of them was produced by running the builder and pasting its output.
//!
//! **Every table and column named here is in `tests/schema/psql.sql`**, so the
//! engine tier can resolve names — which is where the semantic failures a grammar
//! cannot see get caught.
//!
//! A handful of tests at the end assert the *opposite*: that the builder renders
//! something PostgreSQL refuses. Rendering is not validation, and pinning where the
//! two part company keeps a later reader from mistaking a known gap for a bug.

use keelson_psql as psql;
use keelson_psql::{
    Chain, Expr, PsqlOps, Query, Value, arg, arg_group, cast, delete, excluded, f, group, insert,
    quote, raw, s, select, subquery, update,
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
// INSERT — the row source
//
//   INSERT INTO table_name [ AS alias ] [ ( column_name [, ...] ) ]
//       [ OVERRIDING { SYSTEM | USER } VALUE ]
//       { DEFAULT VALUES | VALUES ( { expression | DEFAULT } [, ...] ) [, ...] | query }
// ---------------------------------------------------------------------------

#[test]
fn insert_one_row_into_named_columns() {
    let q = psql::insert((
        insert::into(quote("users")).columns(["id", "name", "email"]),
        insert::values((arg(1i32), arg("ada"), arg("ada@example.com"))),
    ));
    let args = check(
        &q,
        r#"INSERT INTO "users" ("id", "name", "email") VALUES ($1, $2, $3)"#,
    );
    assert_eq!(
        args,
        vec![
            Value::I32(1),
            Value::Text("ada".into()),
            Value::Text("ada@example.com".into()),
        ]
    );
}

/// The column list is optional; without one the row covers the table's columns in
/// declaration order, which for `tags` is `(id, name)`.
#[test]
fn insert_without_a_column_list_covers_the_table_positionally() {
    let q = psql::insert((
        insert::into(quote("tags")),
        insert::values((arg(1i32), arg("rust"))),
    ));
    let args = check(&q, r#"INSERT INTO "tags" VALUES ($1, $2)"#);
    assert_eq!(args, vec![Value::I32(1), Value::Text("rust".into())]);
}

/// `VALUES ( … ) [, ...]` — the rows are one comma-separated list, and the
/// placeholders are numbered straight through it.
#[test]
fn insert_three_rows_in_one_statement() {
    let q = psql::insert((
        insert::into(quote("post_tags")).columns(["post_id", "tag_id"]),
        insert::rows([
            (arg(1i32), arg(1i32)),
            (arg(1i32), arg(2i32)),
            (arg(2i32), arg(3i32)),
        ]),
    ));
    let args = check(
        &q,
        r#"INSERT INTO "post_tags" ("post_id", "tag_id")
           VALUES ($1, $2), ($3, $4), ($5, $6)"#,
    );
    assert_eq!(args.len(), 6);
}

/// `values` appends rather than replaces, so it and `rows` compose into one list.
#[test]
fn values_and_rows_accumulate_into_a_single_list() {
    let q = psql::insert((
        insert::into(quote("tags")).columns(["id", "name"]),
        insert::values((arg(1i32), arg("a"))),
        insert::rows([(arg(2i32), arg("b")), (arg(3i32), arg("c"))]),
    ));
    let args = check(
        &q,
        r#"INSERT INTO "tags" ("id", "name") VALUES ($1, $2), ($3, $4), ($5, $6)"#,
    );
    assert_eq!(args.len(), 6);
}

/// `DEFAULT VALUES` is the alternative taken when there is no row source at all.
#[test]
fn insert_default_values_is_what_an_absent_row_source_means() {
    let q = psql::insert(insert::into(quote("tags")));
    assert!(check(&q, r#"INSERT INTO "tags" DEFAULT VALUES"#).is_empty());
}

#[test]
fn insert_default_values_with_returning() {
    let q = psql::insert((
        insert::into(quote("tags")),
        insert::returning((quote("id"), quote("name"))),
    ));
    check(
        &q,
        r#"INSERT INTO "tags" DEFAULT VALUES RETURNING "id", "name""#,
    );
}

/// A cell is `{ expression | DEFAULT }`, so the `DEFAULT` keyword, a literal and a
/// function call all belong in the same row.
#[test]
fn a_row_mixes_bound_arguments_keywords_literals_and_calls() {
    let q = psql::insert((
        insert::into(quote("posts")).columns(["id", "user_id", "title", "views", "published_at"]),
        insert::values((
            arg(1i32),
            arg(1i32),
            s("hello"),
            raw("DEFAULT"),
            f("now", ()),
        )),
    ));
    let args = check(
        &q,
        r#"INSERT INTO "posts" ("id", "user_id", "title", "views", "published_at")
           VALUES ($1, $2, 'hello', DEFAULT, now())"#,
    );
    assert_eq!(args, vec![Value::I32(1), Value::I32(1)]);
}

/// A cell may be a scalar sub-query, which brings its own parentheses via
/// [`subquery`].
#[test]
fn a_cell_may_be_a_scalar_subquery() {
    let newest = psql::select((
        select::columns(f("max", quote("id"))),
        select::from(quote("posts")),
    ));
    let q = psql::insert((
        insert::into(quote("comments")).columns(["id", "post_id", "body"]),
        insert::values((arg(1i32), subquery(newest), arg("nice"))),
    ));
    let args = check(
        &q,
        r#"INSERT INTO "comments" ("id", "post_id", "body")
           VALUES ($1, (SELECT max("id") FROM "posts"), $2)"#,
    );
    assert_eq!(args, vec![Value::I32(1), Value::Text("nice".into())]);
}

/// `INSERT … query` — the third alternative. The query is *not* parenthesised: it
/// stands where `VALUES` would.
#[test]
fn insert_from_a_select_with_its_own_order_and_limit() {
    let source = psql::select((
        select::columns((quote("id"), arg(1i32))),
        select::from(quote("posts")),
        select::order_by(quote("views")).desc(),
        select::limit(5),
    ));
    let q = psql::insert((
        insert::into(quote("post_tags")).columns(["post_id", "tag_id"]),
        insert::query(source),
    ));
    let args = check(
        &q,
        r#"INSERT INTO "post_tags" ("post_id", "tag_id")
           SELECT "id", $1 FROM "posts" ORDER BY "views" DESC LIMIT 5"#,
    );
    assert_eq!(args, vec![Value::I32(1)]);
}

/// `VALUES` and `query` are alternatives, so setting a query discards rows already
/// added — arguments included, since a dropped row must not leave a `$n` behind.
#[test]
fn a_query_row_source_replaces_rows_already_added_arguments_and_all() {
    let source = psql::select((
        select::columns((quote("id"), quote("name"))),
        select::from(quote("tags")),
        select::where_(quote("id").gt(arg(0i32))),
    ));
    let q = psql::insert((
        insert::into(quote("tags")).columns(["id", "name"]),
        insert::values((arg(99i32), arg("dropped"))),
        insert::query(source),
    ));
    let args = check(
        &q,
        r#"INSERT INTO "tags" ("id", "name")
           SELECT "id", "name" FROM "tags" WHERE ("id" > $1)"#,
    );
    assert_eq!(args, vec![Value::I32(0)]);
}

/// The `query` of an `INSERT` is a whole `SelectStmt`, so a set operation is one.
/// The leading operand keeps no parentheses of its own — it has no tail clauses —
/// and the second is always parenthesised, which is what `Combine` renders.
#[test]
fn insert_from_a_union_of_two_selects() {
    let more = psql::select((
        select::columns((quote("id"), quote("name"))),
        select::from(quote("tags")),
        select::where_(quote("id").eq(arg(2i32))),
    ));
    let source = psql::select((
        select::columns((quote("id"), quote("name"))),
        select::from(quote("tags")),
        select::where_(quote("id").eq(arg(1i32))),
        select::union_all(more),
    ));
    let q = psql::insert((
        insert::into(quote("tags")).columns(["id", "name"]),
        insert::query(source),
    ));
    let args = check(
        &q,
        r#"INSERT INTO "tags" ("id", "name")
           SELECT "id", "name" FROM "tags" WHERE ("id" = $1)
           UNION ALL (SELECT "id", "name" FROM "tags" WHERE ("id" = $2))"#,
    );
    assert_eq!(args, vec![Value::I32(1), Value::I32(2)]);
}

/// `INSERT INTO table_name [ AS alias ]` — the alias is how the target is named in
/// `ON CONFLICT`, where the bare table name would be ambiguous with `EXCLUDED`.
#[test]
fn insert_into_an_aliased_table_referenced_by_the_conflict_action() {
    let q = psql::insert((
        insert::into(quote("tags")).as_("t").columns(["id", "name"]),
        insert::values((arg(1i32), arg("rust"))),
        insert::on_conflict(quote("name")).do_update((
            insert::set_excluded(["name"]),
            insert::where_(quote(("t", "id")).gt(arg(0i32))),
        )),
    ));
    let args = check(
        &q,
        r#"INSERT INTO "tags" AS "t" ("id", "name") VALUES ($1, $2)
           ON CONFLICT ("name") DO UPDATE SET "name" = EXCLUDED."name"
           WHERE ("t"."id" > $3)"#,
    );
    assert_eq!(
        args,
        vec![Value::I32(1), Value::Text("rust".into()), Value::I32(0)]
    );
}

/// `gram.y`, `insert_rest`:
/// `'(' insert_column_list ')' OVERRIDING override_kind VALUE_P SelectStmt` — the
/// clause sits between the column list and the row source, whichever the row source
/// is.
#[test]
fn overriding_sits_between_the_column_list_and_a_query_row_source() {
    let source = psql::select((
        select::columns((quote("id"), quote("name"))),
        select::from(quote("users")),
        select::where_(quote("is_active")),
    ));
    let q = psql::insert((
        insert::into(quote("users")).columns(["id", "name"]),
        insert::overriding_system(),
        insert::query(source),
    ));
    assert!(
        check(
            &q,
            r#"INSERT INTO "users" ("id", "name") OVERRIDING SYSTEM VALUE
               SELECT "id", "name" FROM "users" WHERE "is_active""#,
        )
        .is_empty()
    );
}

#[test]
fn overriding_user_value_with_rows() {
    let q = psql::insert((
        insert::into(quote("tags")).columns(["id", "name"]),
        insert::overriding_user(),
        insert::rows([(arg(1i32), arg("a")), (arg(2i32), arg("b"))]),
    ));
    check(
        &q,
        r#"INSERT INTO "tags" ("id", "name") OVERRIDING USER VALUE
           VALUES ($1, $2), ($3, $4)"#,
    );
}

// ---------------------------------------------------------------------------
// INSERT — WITH
// ---------------------------------------------------------------------------

/// `[ WITH [ RECURSIVE ] with_query [, ...] ]` precedes `INSERT`, and its arguments
/// are numbered first because it is rendered first.
#[test]
fn insert_with_two_ctes_feeding_the_row_source() {
    let hot = psql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::where_(quote("views").gt(arg(100i32))),
    ));
    let fresh = psql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::where_(quote("published_at").is_not_null()),
    ));
    let q = psql::insert((
        insert::with("hot", hot),
        insert::with("fresh", fresh),
        insert::into(quote("post_tags")).columns(["post_id", "tag_id"]),
        insert::query(psql::select((
            select::columns((quote(("h", "id")), arg(1i32))),
            select::from(quote("hot")).as_("h"),
            select::inner_join(quote("fresh"))
                .as_("f")
                .on_eq(quote(("h", "id")), quote(("f", "id"))),
        ))),
    ));
    let args = check(
        &q,
        r#"WITH "hot" AS (SELECT "id" FROM "posts" WHERE ("views" > $1)),
                "fresh" AS (SELECT "id" FROM "posts" WHERE ("published_at" IS NOT NULL))
           INSERT INTO "post_tags" ("post_id", "tag_id")
           SELECT "h"."id", $2 FROM "hot" AS "h"
           INNER JOIN "fresh" AS "f" ON ("h"."id" = "f"."id")"#,
    );
    assert_eq!(args, vec![Value::I32(100), Value::I32(1)]);
}

/// `WITH … AS [ NOT ] MATERIALIZED ( query )` — the keyword goes between `AS` and
/// the parenthesised body.
#[test]
fn insert_from_a_not_materialized_cte_with_renamed_columns() {
    let src = psql::select((
        select::columns((quote("id"), quote("name"))),
        select::from(quote("tags")),
    ));
    let q = psql::insert((
        insert::with("src", src)
            .columns(["i", "n"])
            .not_materialized(),
        insert::into(quote("tags")).columns(["id", "name"]),
        insert::query(psql::select((
            select::columns((quote("i"), quote("n"))),
            select::from(quote("src")),
        ))),
    ));
    check(
        &q,
        r#"WITH "src" ("i", "n") AS NOT MATERIALIZED (SELECT "id", "name" FROM "tags")
           INSERT INTO "tags" ("id", "name") SELECT "i", "n" FROM "src""#,
    );
}

/// `WITH RECURSIVE` in front of an `INSERT`. The recursive term is the second
/// operand of the set operation, and `Combine` always parenthesises an operand.
#[test]
fn insert_from_a_recursive_cte() {
    let step = psql::select((
        select::columns(quote(("p", "id"))),
        select::from(quote("posts")).as_("p"),
        select::inner_join(quote("r")).on_eq(quote(("p", "id")), quote(("r", "id"))),
    ));
    let body = psql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::where_(quote("id").eq(arg(1i32))),
        select::union_all(step),
    ));
    let q = psql::insert((
        insert::recursive(true),
        insert::with("r", body).columns(["id"]),
        insert::into(quote("post_tags")).columns(["post_id", "tag_id"]),
        insert::query(psql::select((
            select::columns((quote("id"), arg(2i32))),
            select::from(quote("r")),
        ))),
    ));
    let args = check(
        &q,
        r#"WITH RECURSIVE "r" ("id") AS (SELECT "id" FROM "posts" WHERE ("id" = $1)
             UNION ALL (SELECT "p"."id" FROM "posts" AS "p"
                        INNER JOIN "r" ON ("p"."id" = "r"."id")))
           INSERT INTO "post_tags" ("post_id", "tag_id") SELECT "id", $2 FROM "r""#,
    );
    assert_eq!(args, vec![Value::I32(1), Value::I32(2)]);
}

// ---------------------------------------------------------------------------
// INSERT — ON CONFLICT
//
//   ON CONFLICT [ conflict_target ] conflict_action
//   conflict_target: ( { index_column_name | ( index_expression ) } [, ...] )
//                      [ WHERE index_predicate ]
//                  | ON CONSTRAINT constraint_name
//   conflict_action: DO NOTHING | DO UPDATE SET … [ WHERE condition ]
// ---------------------------------------------------------------------------

#[test]
fn on_conflict_with_no_target_covers_every_unique_constraint() {
    let q = psql::insert((
        insert::into(quote("tags")).columns(["id", "name"]),
        insert::values((arg(1i32), arg("rust"))),
        insert::on_conflict(()).do_nothing(),
    ));
    check(
        &q,
        r#"INSERT INTO "tags" ("id", "name") VALUES ($1, $2) ON CONFLICT DO NOTHING"#,
    );
}

#[test]
fn on_conflict_on_one_column_do_nothing() {
    let q = psql::insert((
        insert::into(quote("tags")).columns(["id", "name"]),
        insert::values((arg(1i32), arg("rust"))),
        insert::on_conflict(quote("name")).do_nothing(),
    ));
    check(
        &q,
        r#"INSERT INTO "tags" ("id", "name") VALUES ($1, $2)
           ON CONFLICT ("name") DO NOTHING"#,
    );
}

/// A composite unique index is inferred from a comma-separated target list —
/// `post_tags` is keyed on both columns.
#[test]
fn on_conflict_on_two_columns_do_nothing() {
    let q = psql::insert((
        insert::into(quote("post_tags")).columns(["post_id", "tag_id"]),
        insert::values((arg(1i32), arg(2i32))),
        insert::on_conflict((quote("post_id"), quote("tag_id"))).do_nothing(),
    ));
    check(
        &q,
        r#"INSERT INTO "post_tags" ("post_id", "tag_id") VALUES ($1, $2)
           ON CONFLICT ("post_id", "tag_id") DO NOTHING"#,
    );
}

/// `gram.y`, `index_elem: ColId … | func_expr_windowless … | '(' a_expr ')' …` — a
/// bare function call is an index element on its own, with no parentheses beyond
/// the target list's own.
#[test]
fn a_conflict_target_may_be_a_function_call() {
    let q = psql::insert((
        insert::into(quote("tags")).columns(["id", "name"]),
        insert::values((arg(1i32), arg("rust"))),
        insert::on_conflict(f("lower", quote("name"))).do_nothing(),
    ));
    check(
        &q,
        r#"INSERT INTO "tags" ("id", "name") VALUES ($1, $2)
           ON CONFLICT (lower("name")) DO NOTHING"#,
    );
}

/// The other `index_elem` alternative, `'(' a_expr ')'`: an arbitrary expression
/// needs parentheses of its own inside the target list's, which is exactly the pair
/// a [`Chain`] operator already adds.
#[test]
fn a_conflict_target_may_be_a_parenthesised_expression() {
    let q = psql::insert((
        insert::into(quote("tags")).columns(["id", "name"]),
        insert::values((arg(1i32), arg("rust"))),
        insert::on_conflict(quote("name").concat(s("!"))).do_nothing(),
    ));
    check(
        &q,
        r#"INSERT INTO "tags" ("id", "name") VALUES ($1, $2)
           ON CONFLICT (("name" || '!')) DO NOTHING"#,
    );
}

/// The target's `WHERE` is the *index* predicate: it selects which partial unique
/// index to infer, and it hangs off the parenthesised column list.
#[test]
fn a_conflict_target_carries_a_partial_indexs_predicate() {
    let q = psql::insert((
        insert::into(quote("tags")).columns(["id", "name"]),
        insert::values((arg(1i32), arg("rust"))),
        insert::on_conflict(quote("name"))
            .where_(quote("id").gt(arg(0i32)))
            .do_nothing(),
    ));
    let args = check(
        &q,
        r#"INSERT INTO "tags" ("id", "name") VALUES ($1, $2)
           ON CONFLICT ("name") WHERE ("id" > $3) DO NOTHING"#,
    );
    assert_eq!(
        args,
        vec![Value::I32(1), Value::Text("rust".into()), Value::I32(0)]
    );
}

/// Both `WHERE`s of one clause, in the order the grammar puts them: the index
/// predicate before `DO`, the row filter after the assignments.
#[test]
fn the_index_predicate_and_the_row_filter_are_different_wheres() {
    let q = psql::insert((
        insert::into(quote("tags")).columns(["id", "name"]),
        insert::values((arg(1i32), arg("rust"))),
        insert::on_conflict(quote("name"))
            .where_(quote("id").is_not_null())
            .do_update((
                insert::set_excluded(["name"]),
                insert::where_(quote(("tags", "id")).gt(arg(0i32))),
            )),
    ));
    let args = check(
        &q,
        r#"INSERT INTO "tags" ("id", "name") VALUES ($1, $2)
           ON CONFLICT ("name") WHERE ("id" IS NOT NULL)
           DO UPDATE SET "name" = EXCLUDED."name" WHERE ("tags"."id" > $3)"#,
    );
    assert_eq!(args.len(), 3);
}

#[test]
fn on_constraint_names_the_index_instead_of_inferring_it() {
    let q = psql::insert((
        insert::into(quote("tags")).columns(["id", "name"]),
        insert::values((arg(1i32), arg("rust"))),
        insert::on_conflict_on_constraint("tags_name_key")
            .do_update(insert::set_excluded(["name"])),
    ));
    check(
        &q,
        r#"INSERT INTO "tags" ("id", "name") VALUES ($1, $2)
           ON CONFLICT ON CONSTRAINT "tags_name_key"
           DO UPDATE SET "name" = EXCLUDED."name""#,
    );
}

#[test]
fn do_update_assigns_several_excluded_columns() {
    let q = psql::insert((
        insert::into(quote("users")).columns(["id", "name", "email"]),
        insert::values((arg(1i32), arg("ada"), arg("ada@example.com"))),
        insert::on_conflict(quote("id")).do_update(insert::set_excluded(["name", "email"])),
    ));
    check(
        &q,
        r#"INSERT INTO "users" ("id", "name", "email") VALUES ($1, $2, $3)
           ON CONFLICT ("id")
           DO UPDATE SET "name" = EXCLUDED."name", "email" = EXCLUDED."email""#,
    );
}

/// An assignment's value may name both the proposed row and the existing one, which
/// is the whole point of `EXCLUDED`.
#[test]
fn do_update_may_combine_the_excluded_row_with_the_stored_one() {
    let q = psql::insert((
        insert::into(quote("posts")).columns(["id", "user_id", "title", "views"]),
        insert::values((arg(1i32), arg(1i32), arg("t"), arg(5i32))),
        insert::on_conflict(quote("id")).do_update(
            insert::set_col("views").to(quote(("posts", "views")).plus(excluded("views"))),
        ),
    ));
    check(
        &q,
        r#"INSERT INTO "posts" ("id", "user_id", "title", "views") VALUES ($1, $2, $3, $4)
           ON CONFLICT ("id") DO UPDATE SET "views" = ("posts"."views" + EXCLUDED."views")"#,
    );
}

/// `DO UPDATE SET { column_name = { expression | DEFAULT } … }` — `DEFAULT` is a
/// value here just as it is in `VALUES`.
#[test]
fn do_update_may_assign_the_column_default() {
    let q = psql::insert((
        insert::into(quote("posts")).columns(["id", "user_id", "title"]),
        insert::values((arg(1i32), arg(1i32), arg("t"))),
        insert::on_conflict(quote("id")).do_update(insert::set_col("status").to(raw("DEFAULT"))),
    ));
    check(
        &q,
        r#"INSERT INTO "posts" ("id", "user_id", "title") VALUES ($1, $2, $3)
           ON CONFLICT ("id") DO UPDATE SET "status" = DEFAULT"#,
    );
}

/// `DO UPDATE` takes the same `set_clause_list` an `UPDATE` does, so the
/// multi-column form works there too: one assignment with a row on each side.
#[test]
fn do_update_takes_a_multi_column_assignment() {
    let q = psql::insert((
        insert::into(quote("posts")).columns(["id", "user_id", "title", "status"]),
        insert::values((arg(1i32), arg(1i32), arg("t"), arg("draft"))),
        insert::on_conflict(quote("id")).do_update(insert::set(Expr::binary(
            group((quote("title"), quote("status"))),
            "=",
            group((excluded("title"), excluded("status"))),
        ))),
    ));
    check(
        &q,
        r#"INSERT INTO "posts" ("id", "user_id", "title", "status") VALUES ($1, $2, $3, $4)
           ON CONFLICT ("id")
           DO UPDATE SET ("title", "status") = (EXCLUDED."title", EXCLUDED."status")"#,
    );
}

/// The row filter may compare the two rows, and `RETURNING` follows the whole
/// conflict clause.
#[test]
fn do_update_filters_rows_by_comparing_excluded_with_the_target() {
    let q = psql::insert((
        insert::into(quote("posts")).columns(["id", "user_id", "title", "views"]),
        insert::values((arg(1i32), arg(1i32), arg("t"), arg(5i32))),
        insert::on_conflict(quote("id")).do_update((
            insert::set_excluded(["views"]),
            insert::where_(excluded("views").gt(quote(("posts", "views")))),
        )),
        insert::returning(quote("id")),
    ));
    check(
        &q,
        r#"INSERT INTO "posts" ("id", "user_id", "title", "views") VALUES ($1, $2, $3, $4)
           ON CONFLICT ("id") DO UPDATE SET "views" = EXCLUDED."views"
           WHERE (EXCLUDED."views" > "posts"."views") RETURNING "id""#,
    );
}

#[test]
fn do_nothing_before_returning() {
    let q = psql::insert((
        insert::into(quote("tags")).columns(["id", "name"]),
        insert::values((arg(1i32), arg("rust"))),
        insert::on_conflict(()).do_nothing(),
        insert::returning("*"),
    ));
    check(
        &q,
        r#"INSERT INTO "tags" ("id", "name") VALUES ($1, $2)
           ON CONFLICT DO NOTHING RETURNING *"#,
    );
}

/// `DO UPDATE` with no assignments has no rendering that parses, so it is refused
/// before the SQL leaves the builder.
#[test]
fn do_update_without_assignments_is_a_build_error() {
    let q = psql::insert((
        insert::into(quote("tags")),
        insert::values((arg(1i32), arg("rust"))),
        insert::on_conflict(quote("name")).do_update(()),
    ));
    let err = q.build().unwrap_err();
    // The substring names the SQL concept (the missing assignments), not the
    // message wording.
    assert!(
        matches!(&err, psql::Error::Incomplete(what) if what.contains("assignments")),
        "got: {err}"
    );
}

/// An index predicate qualifies a column list, so there is nothing for one to hang
/// off without it — `ON CONFLICT WHERE …` is not a statement.
#[test]
fn an_index_predicate_without_a_column_list_is_a_build_error() {
    let q = psql::insert((
        insert::into(quote("tags")),
        insert::values((arg(1i32), arg("rust"))),
        insert::on_conflict(())
            .where_(quote("id").is_not_null())
            .do_nothing(),
    ));
    let err = q.build().unwrap_err();
    // The substring names the SQL concept (the missing column list), not the
    // message wording.
    assert!(
        matches!(&err, psql::Error::Incomplete(what) if what.contains("column list")),
        "got: {err}"
    );
}

/// Placeholders are numbered in render order, which is clause order: `WITH`, the
/// rows, the conflict action's assignments, its row filter, then `RETURNING`. A
/// single-pass writer gets that for free, and any reordering inside the writer would
/// show up here first.
///
/// The `RETURNING` argument is cast because a bare `$n` in a target list gives
/// PostgreSQL nothing to infer a type from — the engine tier is what says so.
#[test]
fn placeholders_are_numbered_across_every_clause_of_one_insert() {
    let hot = psql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::where_(quote("views").gt(arg(10i32))),
    ));
    let q = psql::insert((
        insert::with("hot", hot),
        insert::into(quote("tags")).columns(["id", "name"]),
        insert::values((arg(1i32), arg("rust"))),
        insert::on_conflict(quote("id")).do_update((
            insert::set_col("name").to_arg("rustlang"),
            insert::where_(quote(("tags", "id")).lt(arg(9i32))),
        )),
        insert::returning((quote("id"), cast(arg("label"), "text").as_("label"))),
    ));
    let args = check(
        &q,
        r#"WITH "hot" AS (SELECT "id" FROM "posts" WHERE ("views" > $1))
           INSERT INTO "tags" ("id", "name") VALUES ($2, $3)
           ON CONFLICT ("id") DO UPDATE SET "name" = $4 WHERE ("tags"."id" < $5)
           RETURNING "id", CAST($6 AS text) AS "label""#,
    );
    assert_eq!(
        args,
        vec![
            Value::I32(10),
            Value::I32(1),
            Value::Text("rust".into()),
            Value::Text("rustlang".into()),
            Value::I32(9),
            Value::Text("label".into()),
        ]
    );
}

/// The same for an `UPDATE`, where `SET` precedes `FROM` and both precede `WHERE`.
#[test]
fn placeholders_are_numbered_across_every_clause_of_one_update() {
    let recent = psql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::where_(quote("views").gt(arg(1i32))),
    ));
    let owners = psql::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        select::where_(quote("age").gte(arg(18i32))),
    ));
    let q = psql::update((
        update::with("recent", recent),
        update::table(quote("posts")).as_("p"),
        update::set_col("status").to_arg("live"),
        update::set_col("views").to_arg(3i32),
        update::from(subquery(owners)).as_("o"),
        update::where_(quote(("p", "user_id")).eq(quote(("o", "id")))),
        update::where_(quote(("p", "id")).in_(psql::query(psql::select((
            select::columns(quote("id")),
            select::from(quote("recent")),
        ))))),
        update::returning(cast(arg("done"), "text").as_("state")),
    ));
    let args = check(
        &q,
        r#"WITH "recent" AS (SELECT "id" FROM "posts" WHERE ("views" > $1))
           UPDATE "posts" AS "p" SET "status" = $2, "views" = $3
           FROM (SELECT "id" FROM "users" WHERE ("age" >= $4)) AS "o"
           WHERE ("p"."user_id" = "o"."id") AND ("p"."id" IN (SELECT "id" FROM "recent"))
           RETURNING CAST($5 AS text) AS "state""#,
    );
    assert_eq!(
        args,
        vec![
            Value::I32(1),
            Value::Text("live".into()),
            Value::I32(3),
            Value::I32(18),
            Value::Text("done".into()),
        ]
    );
}

// ---------------------------------------------------------------------------
// RETURNING, on all three statements
// ---------------------------------------------------------------------------

/// `RETURNING` takes a `SELECT` target list: `*`, expressions, and aliases.
#[test]
fn insert_returning_a_star_an_alias_and_an_expression() {
    let q = psql::insert((
        insert::into(quote("posts")).columns(["id", "user_id", "title"]),
        insert::values((arg(1i32), arg(1i32), arg("t"))),
        insert::returning((
            "*",
            quote("id").as_("post_id"),
            quote("views").plus(1i32).as_("next_views"),
        )),
    ));
    check(
        &q,
        r#"INSERT INTO "posts" ("id", "user_id", "title") VALUES ($1, $2, $3)
           RETURNING *, "id" AS "post_id", ("views" + 1) AS "next_views""#,
    );
}

#[test]
fn update_returning_a_cast_and_a_function_call() {
    let q = psql::update((
        update::table(quote("posts")),
        update::set_col("views").to_arg(0i32),
        update::where_(quote("id").eq(arg(1i32))),
        update::returning((
            cast(quote("views"), "text").as_("views_text"),
            f("now", ()).as_("at"),
        )),
    ));
    let args = check(
        &q,
        r#"UPDATE "posts" SET "views" = $1 WHERE ("id" = $2)
           RETURNING CAST("views" AS text) AS "views_text", now() AS "at""#,
    );
    assert_eq!(args, vec![Value::I32(0), Value::I32(1)]);
}

#[test]
fn delete_returning_qualified_columns_and_a_star() {
    let q = psql::delete((
        delete::from(quote("comments")).as_("c"),
        delete::where_(quote(("c", "user_id")).is_null()),
        delete::returning((quote(("c", "id")), quote(("c", "body")).as_("removed"))),
    ));
    check(
        &q,
        r#"DELETE FROM "comments" AS "c" WHERE ("c"."user_id" IS NULL)
           RETURNING "c"."id", "c"."body" AS "removed""#,
    );
}

// ---------------------------------------------------------------------------
// UPDATE
//
//   UPDATE [ ONLY ] table_name [ * ] [ [ AS ] alias ]
//       SET { column_name = { expression | DEFAULT }
//           | ( column_name [, ...] ) = [ ROW ] ( { expression | DEFAULT } [, ...] )
//           | ( column_name [, ...] ) = ( sub-SELECT ) } [, ...]
//       [ FROM from_item [, ...] ] [ WHERE condition | WHERE CURRENT OF cursor ]
//       [ RETURNING … ]
// ---------------------------------------------------------------------------

/// `WHERE` is optional: with none, every row is updated.
#[test]
fn update_every_row_with_one_assignment() {
    let q = psql::update((
        update::table(quote("posts")),
        update::set_col("views").to_arg(0i32),
    ));
    let args = check(&q, r#"UPDATE "posts" SET "views" = $1"#);
    assert_eq!(args, vec![Value::I32(0)]);
}

/// The assignment list is comma-separated, and each value is an ordinary
/// expression — a computation on the column, a bound argument, a call.
#[test]
fn update_several_assignments_of_different_shapes() {
    let q = psql::update((
        update::table(quote("posts")),
        update::set_col("views").to(quote("views").plus(1i32)),
        update::set_col("status").to_arg("hot"),
        update::set_col("published_at").to(f("now", ())),
        update::where_(quote("id").eq(arg(7i32))),
    ));
    let args = check(
        &q,
        r#"UPDATE "posts"
           SET "views" = ("views" + 1), "status" = $1, "published_at" = now()
           WHERE ("id" = $2)"#,
    );
    assert_eq!(args, vec![Value::Text("hot".into()), Value::I32(7)]);
}

/// `column_name = DEFAULT` — the keyword is a value, reached with [`raw`] because it
/// is a keyword and not an expression.
#[test]
fn update_assigns_the_column_default() {
    let q = psql::update((
        update::table(quote("posts")),
        update::set_col("status").to(raw("DEFAULT")),
        update::where_(quote("id").eq(arg(1i32))),
    ));
    check(
        &q,
        r#"UPDATE "posts" SET "status" = DEFAULT WHERE ("id" = $1)"#,
    );
}

/// A scalar sub-query as an assignment's value, correlated with the updated row.
#[test]
fn update_assigns_from_a_correlated_subquery() {
    let name = psql::select((
        select::columns(quote("name")),
        select::from(quote("tags")),
        select::where_(quote(("tags", "id")).eq(quote(("posts", "id")))),
    ));
    let q = psql::update((
        update::table(quote("posts")),
        update::set_col("status").to(subquery(name)),
        update::where_(quote("views").gt(arg(0i32))),
    ));
    let args = check(
        &q,
        r#"UPDATE "posts"
           SET "status" = (SELECT "name" FROM "tags" WHERE ("tags"."id" = "posts"."id"))
           WHERE ("views" > $1)"#,
    );
    assert_eq!(args, vec![Value::I32(0)]);
}

/// `( column_name [, ...] ) = ( … )` — one assignment whose two sides are rows.
/// `Expr::binary` is the un-parenthesised infix form, so the only parentheses are
/// the two row constructors'.
#[test]
fn update_a_column_list_from_a_row_of_values() {
    let q = psql::update((
        update::table(quote("users")),
        update::set(Expr::binary(
            group((quote("name"), quote("email"))),
            "=",
            arg_group(["ada", "ada@example.com"]),
        )),
        update::where_(quote("id").eq(arg(1i32))),
    ));
    let args = check(
        &q,
        r#"UPDATE "users" SET ("name", "email") = ($1, $2) WHERE ("id" = $3)"#,
    );
    assert_eq!(
        args,
        vec![
            Value::Text("ada".into()),
            Value::Text("ada@example.com".into()),
            Value::I32(1),
        ]
    );
}

/// `= [ ROW ] ( … )` — the optional keyword has no mod of its own, so it is written
/// with [`raw`]; `Expr::join` puts the space in.
#[test]
fn update_a_column_list_with_the_explicit_row_keyword() {
    let q = psql::update((
        update::table(quote("users")),
        update::set(Expr::binary(
            group((quote("name"), quote("email"))),
            "=",
            Expr::join((raw("ROW"), arg_group(["ada", "ada@example.com"]))),
        )),
        update::where_(quote("id").eq(arg(1i32))),
    ));
    check(
        &q,
        r#"UPDATE "users" SET ("name", "email") = ROW ($1, $2) WHERE ("id" = $3)"#,
    );
}

/// `( column_name [, ...] ) = ( sub-SELECT )` — the sub-select supplies one row.
#[test]
fn update_a_column_list_from_a_subselect() {
    let source = psql::select((
        select::columns((quote("title"), quote("status"))),
        select::from(quote("posts")),
        select::where_(quote("id").eq(arg(2i32))),
    ));
    let q = psql::update((
        update::table(quote("posts")),
        update::set(Expr::binary(
            group((quote("title"), quote("status"))),
            "=",
            subquery(source),
        )),
        update::where_(quote("id").eq(arg(1i32))),
    ));
    let args = check(
        &q,
        r#"UPDATE "posts"
           SET ("title", "status") = (SELECT "title", "status" FROM "posts" WHERE ("id" = $1))
           WHERE ("id" = $2)"#,
    );
    assert_eq!(args, vec![Value::I32(2), Value::I32(1)]);
}

/// `UPDATE [ ONLY ] table_name [ [ AS ] alias ]` — both decorations at once, with
/// `ONLY` in front of the name and the alias after it.
#[test]
fn update_only_an_aliased_table() {
    let q = psql::update((
        update::table(quote("posts")).only().as_("p"),
        update::set_col("views").to_arg(0i32),
        update::where_(quote(("p", "id")).eq(arg(1i32))),
    ));
    check(
        &q,
        r#"UPDATE ONLY "posts" AS "p" SET "views" = $1 WHERE ("p"."id" = $2)"#,
    );
}

/// A raw `&str` is raw SQL wherever an expression goes, so an unquoted statement is
/// reachable without any identifier quoting at all.
#[test]
fn update_written_entirely_from_raw_fragments() {
    let q = psql::update((
        update::table("posts"),
        update::set(raw("views = views + 1")),
        update::where_(raw("id = 1")),
    ));
    assert!(check(&q, "UPDATE posts SET views = views + 1 WHERE id = 1").is_empty());
}

/// `FROM from_item [, ...]` — a second table to read from, joined by the `WHERE`.
/// The comma-separated list means what a `CROSS JOIN` means.
#[test]
fn update_from_two_items() {
    let q = psql::update((
        update::table(quote("posts")).as_("p"),
        update::set_col("status").to_arg("archived"),
        update::from(quote("users")).as_("u"),
        update::from_also(quote("comments")).as_("c"),
        update::where_(quote(("p", "user_id")).eq(quote(("u", "id")))),
        update::where_(quote(("c", "post_id")).eq(quote(("p", "id")))),
    ));
    let args = check(
        &q,
        r#"UPDATE "posts" AS "p" SET "status" = $1
           FROM "users" AS "u", "comments" AS "c"
           WHERE ("p"."user_id" = "u"."id") AND ("c"."post_id" = "p"."id")"#,
    );
    assert_eq!(args, vec![Value::Text("archived".into())]);
}

/// A `FROM` item may be a parenthesised sub-query, which PostgreSQL requires to be
/// aliased.
#[test]
fn update_from_an_aliased_subquery() {
    let counts = psql::select((
        select::columns((quote("post_id"), f("count", "*").as_("n"))),
        select::from(quote("comments")),
        select::group_by(quote("post_id")),
    ));
    let q = psql::update((
        update::table(quote("posts")).as_("p"),
        update::set_col("views").to(cast(quote(("c", "n")), "integer")),
        update::from(subquery(counts)).as_("c"),
        update::where_(quote(("c", "post_id")).eq(quote(("p", "id")))),
    ));
    check(
        &q,
        r#"UPDATE "posts" AS "p" SET "views" = CAST("c"."n" AS integer)
           FROM (SELECT "post_id", count(*) AS "n" FROM "comments" GROUP BY "post_id") AS "c"
           WHERE ("c"."post_id" = "p"."id")"#,
    );
}

/// The joins of an `UPDATE` hang off the `FROM` item, so an outer join there is
/// written exactly as it would be in a `SELECT`.
#[test]
fn update_from_a_left_joined_pair() {
    let q = psql::update((
        update::table(quote("posts")).as_("p"),
        update::set_col("status").to_arg("stale"),
        update::from(quote("users")).as_("u"),
        update::left_join(quote("comments"))
            .as_("c")
            .on_eq(quote(("c", "user_id")), quote(("u", "id"))),
        update::where_(quote(("p", "user_id")).eq(quote(("u", "id")))),
        update::where_(quote(("c", "id")).is_null()),
    ));
    check(
        &q,
        r#"UPDATE "posts" AS "p" SET "status" = $1
           FROM "users" AS "u" LEFT JOIN "comments" AS "c" ON ("c"."user_id" = "u"."id")
           WHERE ("p"."user_id" = "u"."id") AND ("c"."id" IS NULL)"#,
    );
}

/// A set-returning function is a from-item, and its alias may rename the column it
/// produces: `func_alias_clause` allows `AS alias ( column [, ...] )`.
///
/// The bounds are literals rather than `arg`s deliberately: `generate_series` is
/// overloaded over four types, so a bare `$1` leaves PostgreSQL nothing to resolve
/// the overload from and the engine tier refuses the statement.
#[test]
fn update_from_a_set_returning_function_with_a_renamed_column() {
    let q = psql::update((
        update::table(quote("posts")).as_("p"),
        update::set_col("views").to(quote(("g", "n"))),
        update::from_function([f("generate_series", (1i32, 3i32))])
            .as_("g")
            .columns(["n"]),
        update::where_(quote(("p", "id")).eq(quote(("g", "n")))),
    ));
    assert!(
        check(
            &q,
            r#"UPDATE "posts" AS "p" SET "views" = "g"."n"
               FROM generate_series(1, 3) AS "g" ("n")
               WHERE ("p"."id" = "g"."n")"#,
        )
        .is_empty()
    );
}

/// `WITH` on an `UPDATE`, and the CTE named in the `FROM`.
#[test]
fn update_from_a_cte() {
    let active = psql::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        select::where_(quote("is_active")),
    ));
    let q = psql::update((
        update::with("active", active),
        update::table(quote("posts")).as_("p"),
        update::set_col("status").to_arg("live"),
        update::from(quote("active")).as_("a"),
        update::where_(quote(("p", "user_id")).eq(quote(("a", "id")))),
        update::returning(quote(("p", "id"))),
    ));
    check(
        &q,
        r#"WITH "active" AS (SELECT "id" FROM "users" WHERE "is_active")
           UPDATE "posts" AS "p" SET "status" = $1 FROM "active" AS "a"
           WHERE ("p"."user_id" = "a"."id") RETURNING "p"."id""#,
    );
}

/// `WITH RECURSIVE` on an `UPDATE`: the walk is computed first and the statement
/// reads from it.
#[test]
fn update_from_a_recursive_cte() {
    let step = psql::select((
        select::columns(quote(("c", "id"))),
        select::from(quote("comments")).as_("c"),
        select::inner_join(quote("r")).on_eq(quote(("c", "id")), quote(("r", "id"))),
    ));
    let body = psql::select((
        select::columns(quote("id")),
        select::from(quote("comments")),
        select::where_(quote("id").eq(arg(1i32))),
        select::union_all(step),
    ));
    let q = psql::update((
        update::recursive(true),
        update::with("r", body).columns(["id"]),
        update::table(quote("comments")).as_("c"),
        update::set_col("body").to_arg("edited"),
        update::from(quote("r")),
        update::where_(quote(("c", "id")).eq(quote(("r", "id")))),
    ));
    let args = check(
        &q,
        r#"WITH RECURSIVE "r" ("id") AS (SELECT "id" FROM "comments" WHERE ("id" = $1)
             UNION ALL (SELECT "c"."id" FROM "comments" AS "c"
                        INNER JOIN "r" ON ("c"."id" = "r"."id")))
           UPDATE "comments" AS "c" SET "body" = $2 FROM "r"
           WHERE ("c"."id" = "r"."id")"#,
    );
    assert_eq!(args, vec![Value::I32(1), Value::Text("edited".into())]);
}

/// Several `where_` calls are `AND`-joined, and [`or`](psql::or) is how the other
/// connective is written — each with its own single pair of parentheses.
#[test]
fn update_with_a_subquery_predicate_and_an_or_group() {
    let drafts = psql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::where_(quote("status").eq(arg("draft"))),
    ));
    let q = psql::update((
        update::table(quote("comments")),
        update::set_col("body").to_arg("hidden"),
        update::where_(quote("post_id").in_(psql::query(drafts))),
        update::where_(psql::or((
            quote("user_id").is_null(),
            quote("user_id").eq(arg(1i32)),
        ))),
    ));
    let args = check(
        &q,
        r#"UPDATE "comments" SET "body" = $1
           WHERE ("post_id" IN (SELECT "id" FROM "posts" WHERE ("status" = $2)))
             AND (("user_id" IS NULL) OR ("user_id" = $3))"#,
    );
    assert_eq!(
        args,
        vec![
            Value::Text("hidden".into()),
            Value::Text("draft".into()),
            Value::I32(1),
        ]
    );
}

/// `WHERE CURRENT OF cursor_name` is an alternative to a condition, not an addition
/// to one, and it is followed by `RETURNING` like any other `WHERE`.
#[test]
fn update_where_current_of_a_cursor_with_returning() {
    let q = psql::update((
        update::table(quote("posts")),
        update::set_col("views").to_arg(1i32),
        update::where_current_of("posts_cursor"),
        update::returning(quote("id")),
    ));
    check(
        &q,
        r#"UPDATE "posts" SET "views" = $1 WHERE CURRENT OF "posts_cursor" RETURNING "id""#,
    );
}

/// A statement with a `FROM` may return columns of either table.
#[test]
fn update_returning_columns_of_the_target_and_of_the_from_item() {
    let q = psql::update((
        update::table(quote("posts")).as_("p"),
        update::set_col("status").to_arg("live"),
        update::from(quote("users")).as_("u"),
        update::where_(quote(("p", "user_id")).eq(quote(("u", "id")))),
        update::returning((quote(("p", "id")), quote(("u", "name")).as_("author"))),
    ));
    check(
        &q,
        r#"UPDATE "posts" AS "p" SET "status" = $1 FROM "users" AS "u"
           WHERE ("p"."user_id" = "u"."id")
           RETURNING "p"."id", "u"."name" AS "author""#,
    );
}

/// An `UPDATE` with no assignments is not a statement, so it is refused rather than
/// rendered into a syntax error.
#[test]
fn update_without_assignments_is_a_build_error() {
    let q = psql::update((
        update::table(quote("posts")),
        update::where_(quote("id").eq(arg(1i32))),
    ));
    let err = q.build().unwrap_err();
    // The substrings name the SQL concepts (an UPDATE's assignments), not the
    // message wording.
    assert!(
        matches!(&err, psql::Error::Incomplete(what)
            if what.contains("assignments") && what.contains("UPDATE")),
        "got: {err}"
    );
}

// ---------------------------------------------------------------------------
// DELETE
//
//   DELETE FROM [ ONLY ] table_name [ * ] [ [ AS ] alias ]
//       [ USING from_item [, ...] ] [ WHERE condition | WHERE CURRENT OF cursor ]
//       [ RETURNING … ]
// ---------------------------------------------------------------------------

/// Everything after the table is optional.
#[test]
fn delete_every_row() {
    let q = psql::delete(delete::from(quote("comments")));
    assert!(check(&q, r#"DELETE FROM "comments""#).is_empty());
}

#[test]
fn delete_with_a_bound_predicate() {
    let q = psql::delete((
        delete::from(quote("comments")),
        delete::where_(quote("post_id").eq(arg(3i32))),
    ));
    let args = check(&q, r#"DELETE FROM "comments" WHERE ("post_id" = $1)"#);
    assert_eq!(args, vec![Value::I32(3)]);
}

/// `DELETE FROM [ ONLY ] table_name [ [ AS ] alias ]` — as on `UPDATE`, `ONLY`
/// precedes the name and the alias follows it.
#[test]
fn delete_from_only_an_aliased_table() {
    let q = psql::delete((
        delete::from(quote("comments")).only().as_("c"),
        delete::where_(quote(("c", "user_id")).is_null()),
    ));
    check(
        &q,
        r#"DELETE FROM ONLY "comments" AS "c" WHERE ("c"."user_id" IS NULL)"#,
    );
}

/// A `USING` item may carry joins, exactly as a `FROM` item does.
#[test]
fn delete_using_a_joined_pair() {
    let q = psql::delete((
        delete::from(quote("post_tags")).as_("pt"),
        delete::using(quote("posts")).as_("p"),
        delete::inner_join(quote("users"))
            .as_("u")
            .on_eq(quote(("u", "id")), quote(("p", "user_id"))),
        delete::where_(quote(("pt", "post_id")).eq(quote(("p", "id")))),
        delete::where_(quote(("u", "is_active")).is_false()),
    ));
    check(
        &q,
        r#"DELETE FROM "post_tags" AS "pt"
           USING "posts" AS "p" INNER JOIN "users" AS "u" ON ("u"."id" = "p"."user_id")
           WHERE ("pt"."post_id" = "p"."id") AND ("u"."is_active" IS FALSE)"#,
    );
}

/// A comma in the `USING` list means what `CROSS JOIN` means, and the explicit
/// spelling is available too.
#[test]
fn delete_using_a_cross_joined_pair() {
    let q = psql::delete((
        delete::from(quote("post_tags")).as_("pt"),
        delete::using(quote("posts")).as_("p"),
        delete::cross_join(quote("tags")).as_("t"),
        delete::where_(quote(("pt", "post_id")).eq(quote(("p", "id")))),
        delete::where_(quote(("pt", "tag_id")).eq(quote(("t", "id")))),
    ));
    check(
        &q,
        r#"DELETE FROM "post_tags" AS "pt"
           USING "posts" AS "p" CROSS JOIN "tags" AS "t"
           WHERE ("pt"."post_id" = "p"."id") AND ("pt"."tag_id" = "t"."id")"#,
    );
}

#[test]
fn delete_using_an_aliased_subquery() {
    let unread = psql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::where_(quote("views").eq(arg(0i32))),
    ));
    let q = psql::delete((
        delete::from(quote("comments")).as_("c"),
        delete::using(subquery(unread)).as_("u"),
        delete::where_(quote(("c", "post_id")).eq(quote(("u", "id")))),
        delete::returning(quote(("c", "id"))),
    ));
    let args = check(
        &q,
        r#"DELETE FROM "comments" AS "c"
           USING (SELECT "id" FROM "posts" WHERE ("views" = $1)) AS "u"
           WHERE ("c"."post_id" = "u"."id") RETURNING "c"."id""#,
    );
    assert_eq!(args, vec![Value::I32(0)]);
}

/// Two or more from-functions become `ROWS FROM ( … )`, whose alias may rename one
/// column per function. Literal bounds again, for the overload-resolution reason
/// [`update_from_a_set_returning_function_with_a_renamed_column`] records.
#[test]
fn delete_using_rows_from_two_set_returning_functions() {
    let q = psql::delete((
        delete::from(quote("post_tags")),
        delete::using_function([
            f("generate_series", (1i32, 3i32)),
            f("generate_series", (4i32, 6i32)),
        ])
        .as_("g")
        .columns(["a", "b"]),
        delete::where_(quote(("post_tags", "post_id")).eq(quote(("g", "a")))),
        delete::where_(quote(("post_tags", "tag_id")).eq(quote(("g", "b")))),
    ));
    assert!(
        check(
            &q,
            r#"DELETE FROM "post_tags"
               USING ROWS FROM (generate_series(1, 3), generate_series(4, 6)) AS "g" ("a", "b")
               WHERE ("post_tags"."post_id" = "g"."a") AND ("post_tags"."tag_id" = "g"."b")"#,
        )
        .is_empty()
    );
}

/// `USING` takes a comma-separated list, and a third item is one more comma.
#[test]
fn delete_using_three_items() {
    let q = psql::delete((
        delete::from(quote("post_tags")).as_("pt"),
        delete::using(quote("posts")).as_("p"),
        delete::using_also(quote("tags")).as_("t"),
        delete::using_also(quote("users")).as_("u"),
        delete::where_(quote(("pt", "post_id")).eq(quote(("p", "id")))),
        delete::where_(quote(("pt", "tag_id")).eq(quote(("t", "id")))),
        delete::where_(quote(("p", "user_id")).eq(quote(("u", "id")))),
    ));
    check(
        &q,
        r#"DELETE FROM "post_tags" AS "pt"
           USING "posts" AS "p", "tags" AS "t", "users" AS "u"
           WHERE ("pt"."post_id" = "p"."id") AND ("pt"."tag_id" = "t"."id")
             AND ("p"."user_id" = "u"."id")"#,
    );
}

/// `NOT EXISTS ( sub-SELECT )` has no mod of its own; `Expr::join` puts the keyword
/// and the parenthesised query together with one space.
#[test]
fn delete_where_not_exists_a_correlated_subquery() {
    let commented = psql::select((
        select::columns(quote("id")),
        select::from(quote("comments")),
        select::where_(quote(("comments", "post_id")).eq(quote(("posts", "id")))),
    ));
    let q = psql::delete((
        delete::from(quote("posts")),
        delete::where_(Expr::join((raw("NOT EXISTS"), subquery(commented)))),
    ));
    check(
        &q,
        r#"DELETE FROM "posts" WHERE NOT EXISTS (SELECT "id" FROM "comments"
           WHERE ("comments"."post_id" = "posts"."id"))"#,
    );
}

#[test]
fn delete_with_two_ctes() {
    let stale = psql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::where_(quote("views").eq(arg(0i32))),
    ));
    let unnamed = psql::select((
        select::columns(quote("id")),
        select::from(quote("tags")),
        select::where_(quote("name").eq(arg(""))),
    ));
    let q = psql::delete((
        delete::with("stale", stale),
        delete::with("unnamed", unnamed),
        delete::from(quote("post_tags")).as_("pt"),
        delete::using(quote("stale")).as_("s"),
        delete::using_also(quote("unnamed")).as_("n"),
        delete::where_(quote(("pt", "post_id")).eq(quote(("s", "id")))),
        delete::where_(quote(("pt", "tag_id")).eq(quote(("n", "id")))),
    ));
    let args = check(
        &q,
        r#"WITH "stale" AS (SELECT "id" FROM "posts" WHERE ("views" = $1)),
                "unnamed" AS (SELECT "id" FROM "tags" WHERE ("name" = $2))
           DELETE FROM "post_tags" AS "pt" USING "stale" AS "s", "unnamed" AS "n"
           WHERE ("pt"."post_id" = "s"."id") AND ("pt"."tag_id" = "n"."id")"#,
    );
    assert_eq!(args, vec![Value::I32(0), Value::Text("".into())]);
}

/// `WITH RECURSIVE` on a `DELETE`, with a `MATERIALIZED` sibling CTE to pin that the
/// keyword belongs to the individual CTE and not to the `WITH`.
#[test]
fn delete_with_a_recursive_and_a_materialized_cte() {
    let step = psql::select((
        select::columns(quote(("p", "id"))),
        select::from(quote("posts")).as_("p"),
        select::inner_join(quote("r")).on_eq(quote(("p", "id")), quote(("r", "id"))),
    ));
    let body = psql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::where_(quote("id").eq(arg(1i32))),
        select::union_all(step),
    ));
    let keep = psql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::where_(quote("views").gt(arg(10i32))),
    ));
    let q = psql::delete((
        delete::recursive(true),
        delete::with("r", body).columns(["id"]),
        delete::with("keep", keep).materialized(),
        delete::from(quote("post_tags")),
        delete::where_(quote("post_id").in_(psql::query(psql::select((
            select::columns(quote("id")),
            select::from(quote("r")),
        ))))),
        delete::where_(quote("post_id").not_in(psql::query(psql::select((
            select::columns(quote("id")),
            select::from(quote("keep")),
        ))))),
    ));
    let args = check(
        &q,
        r#"WITH RECURSIVE "r" ("id") AS (SELECT "id" FROM "posts" WHERE ("id" = $1)
             UNION ALL (SELECT "p"."id" FROM "posts" AS "p"
                        INNER JOIN "r" ON ("p"."id" = "r"."id"))),
                "keep" AS MATERIALIZED (SELECT "id" FROM "posts" WHERE ("views" > $2))
           DELETE FROM "post_tags"
           WHERE ("post_id" IN (SELECT "id" FROM "r"))
             AND ("post_id" NOT IN (SELECT "id" FROM "keep"))"#,
    );
    assert_eq!(args, vec![Value::I32(1), Value::I32(10)]);
}

#[test]
fn delete_where_current_of_a_cursor_with_returning() {
    let q = psql::delete((
        delete::from(quote("comments")),
        delete::where_current_of("comments_cursor"),
        delete::returning("*"),
    ));
    check(
        &q,
        r#"DELETE FROM "comments" WHERE CURRENT OF "comments_cursor" RETURNING *"#,
    );
}

/// The raw path, all the way through: an unquoted table and a hand-written
/// predicate.
#[test]
fn delete_written_entirely_from_raw_fragments() {
    let q = psql::delete((
        delete::from("comments"),
        delete::where_(raw("user_id IS NULL")),
    ));
    assert!(check(&q, "DELETE FROM comments WHERE user_id IS NULL").is_empty());
}

// ---------------------------------------------------------------------------
// Data-modifying CTEs
//
// sql-select.html, WITH clause: "and with_query can be a SELECT, TABLE, VALUES,
// INSERT, UPDATE, DELETE, or MERGE statement" — a data-modifying statement in
// `WITH` must have RETURNING, and RETURNING is what the rest of the statement
// reads. One of PostgreSQL's signature features, and the reason `Cte::query` is
// an ordinary expression rather than something SELECT-shaped.
//
// PREPARE runs parse and analysis without executing, so the engine tier judges
// these without deleting anything.
// ---------------------------------------------------------------------------

/// `WITH x AS (DELETE … RETURNING …) SELECT …` — the outer query reads the rows
/// the CTE removed, through the CTE's RETURNING list.
#[test]
fn a_delete_cte_feeds_the_outer_select() {
    let purge = psql::delete((
        delete::from(quote("comments")),
        delete::where_(quote("post_id").eq(arg(1i32))),
        delete::returning((quote("id"), quote("body"))),
    ));
    let q = psql::select((
        select::with("purged", purge),
        select::columns((quote("id"), quote("body"))),
        select::from(quote("purged")),
    ));
    let args = check(
        &q,
        r#"WITH "purged" AS (DELETE FROM "comments" WHERE ("post_id" = $1)
           RETURNING "id", "body")
           SELECT "id", "body" FROM "purged""#,
    );
    assert_eq!(args, vec![Value::I32(1)]);
}

/// The INSERT variant. The CTE's RETURNING may return more than the outer query
/// reads.
#[test]
fn an_insert_cte_feeds_the_outer_select() {
    let add = psql::insert((
        insert::into(quote("tags")).columns(["id", "name"]),
        insert::values((arg(1i32), arg("rust"))),
        insert::returning((quote("id"), quote("name"))),
    ));
    let q = psql::select((
        select::with("added", add),
        select::columns(quote("id")),
        select::from(quote("added")),
    ));
    let args = check(
        &q,
        r#"WITH "added" AS (INSERT INTO "tags" ("id", "name") VALUES ($1, $2)
           RETURNING "id", "name")
           SELECT "id" FROM "added""#,
    );
    assert_eq!(args, vec![Value::I32(1), Value::Text("rust".into())]);
}

/// The UPDATE variant, returning the post-update values — RETURNING in a CTE sees
/// the new row, which is why reading it back is worth doing at all.
#[test]
fn an_update_cte_feeds_the_outer_select() {
    let bump = psql::update((
        update::table(quote("posts")),
        update::set_col("views").to(quote("views").plus(1i32)),
        update::where_(quote("user_id").eq(arg(1i32))),
        update::returning((quote("id"), quote("views"))),
    ));
    let q = psql::select((
        select::with("bumped", bump),
        select::columns((quote("id"), quote("views"))),
        select::from(quote("bumped")),
        select::order_by(quote("views")).desc(),
    ));
    let args = check(
        &q,
        r#"WITH "bumped" AS (UPDATE "posts" SET "views" = ("views" + 1)
           WHERE ("user_id" = $1) RETURNING "id", "views")
           SELECT "id", "views" FROM "bumped" ORDER BY "views" DESC"#,
    );
    assert_eq!(args, vec![Value::I32(1)]);
}

/// The archive pattern: a DELETE in the CTE and an INSERT reading its RETURNING
/// as the row source — move rows in one statement.
#[test]
fn an_insert_reads_its_rows_from_a_delete_cte() {
    let removed = psql::delete((
        delete::from(quote("post_tags")),
        delete::where_(quote("post_id").eq(arg(1i32))),
        delete::returning((quote("post_id"), quote("tag_id"))),
    ));
    let q = psql::insert((
        insert::with("removed", removed),
        insert::into(quote("post_tags")).columns(["post_id", "tag_id"]),
        insert::query(psql::select((
            select::columns((quote("post_id"), quote("tag_id"))),
            select::from(quote("removed")),
        ))),
    ));
    let args = check(
        &q,
        r#"WITH "removed" AS (DELETE FROM "post_tags" WHERE ("post_id" = $1)
           RETURNING "post_id", "tag_id")
           INSERT INTO "post_tags" ("post_id", "tag_id")
           SELECT "post_id", "tag_id" FROM "removed""#,
    );
    assert_eq!(args, vec![Value::I32(1)]);
}

/// An UPDATE in the CTE of a DELETE, read back through `IN` — RETURNING is the
/// only channel from a modifying CTE to the statement around it.
#[test]
fn a_delete_reads_an_update_ctes_returning_through_in() {
    let demoted = psql::update((
        update::table(quote("posts")),
        update::set_col("status").to_arg("archived"),
        update::where_(quote("views").lt(arg(10i32))),
        update::returning(quote("id")),
    ));
    let q = psql::delete((
        delete::with("demoted", demoted),
        delete::from(quote("comments")),
        delete::where_(quote("post_id").in_(psql::query(psql::select((
            select::columns(quote("id")),
            select::from(quote("demoted")),
        ))))),
    ));
    let args = check(
        &q,
        r#"WITH "demoted" AS (UPDATE "posts" SET "status" = $1 WHERE ("views" < $2)
           RETURNING "id")
           DELETE FROM "comments" WHERE ("post_id" IN (SELECT "id" FROM "demoted"))"#,
    );
    assert_eq!(args, vec![Value::Text("archived".into()), Value::I32(10)]);
}

/// Two CTEs, the second an ordinary SELECT over the first's RETURNING, and the
/// statement itself a third modification. Sub-statements cannot see each other's
/// effects on the tables; the RETURNING list is the only data that flows.
#[test]
fn a_select_cte_reads_a_delete_ctes_returning_for_an_update() {
    let removed = psql::delete((
        delete::from(quote("comments")),
        delete::where_(quote("post_id").eq(arg(1i32))),
        delete::returning((quote("id"), quote("user_id"))),
    ));
    let authors = psql::select((
        select::columns(quote("user_id")),
        select::from(quote("removed")),
        select::where_(quote("user_id").is_not_null()),
    ));
    let q = psql::update((
        update::with("removed", removed),
        update::with("authors", authors),
        update::table(quote("users")),
        update::set_col("is_active").to_arg(false),
        update::from(quote("authors")).as_("a"),
        update::where_(quote(("users", "id")).eq(quote(("a", "user_id")))),
        update::returning(quote(("users", "id"))),
    ));
    let args = check(
        &q,
        r#"WITH "removed" AS (DELETE FROM "comments" WHERE ("post_id" = $1)
           RETURNING "id", "user_id"),
                "authors" AS (SELECT "user_id" FROM "removed" WHERE ("user_id" IS NOT NULL))
           UPDATE "users" SET "is_active" = $2 FROM "authors" AS "a"
           WHERE ("users"."id" = "a"."user_id") RETURNING "users"."id""#,
    );
    assert_eq!(args, vec![Value::I32(1), Value::Bool(false)]);
}

// ---------------------------------------------------------------------------
// Where the builder and PostgreSQL part company
//
// Rendering is not validation. These pin the places a well-typed statement still
// does not parse, so a later reader does not mistake a known gap for a bug.
// ---------------------------------------------------------------------------

/// `gram.y`'s `insert_rest` offers `DEFAULT VALUES` only as a bare alternative:
///
/// ```text
/// insert_rest: SelectStmt
///            | OVERRIDING override_kind VALUE_P SelectStmt
///            | '(' insert_column_list ')' [OVERRIDING …] SelectStmt
///            | DEFAULT VALUES
/// ```
///
/// So neither a column list nor `OVERRIDING` may precede it. The clause layer has no
/// way to express that dependency — an empty `Values` is what `DEFAULT VALUES` *is*
/// — so both spellings render and libpg_query is what says no.
#[test]
fn default_values_admits_neither_a_column_list_nor_overriding() {
    let with_columns = psql::insert(insert::into(quote("tags")).columns(["id", "name"]));
    let (sql, _) = with_columns.build().expect("it renders");
    assert_eq!(sql, r#"INSERT INTO "tags" ("id", "name") DEFAULT VALUES"#);
    assert!(
        keelson_sqlcheck::check_psql(&sql).is_err(),
        "a column list before DEFAULT VALUES has no production: {sql}"
    );

    let with_overriding = psql::insert((insert::into(quote("tags")), insert::overriding_system()));
    let (sql, _) = with_overriding.build().expect("it renders");
    assert_eq!(
        sql,
        r#"INSERT INTO "tags" OVERRIDING SYSTEM VALUE DEFAULT VALUES"#
    );
    assert!(
        keelson_sqlcheck::check_psql(&sql).is_err(),
        "OVERRIDING before DEFAULT VALUES has no production: {sql}"
    );
}

/// PostgreSQL refuses a qualified assignment target in *both* `UPDATE` and
/// `ON CONFLICT DO UPDATE`: `gram.y` has `set_target: ColId opt_indirection`, so
/// `"tags"."name" = …` parses with `tags` as the column name and analysis then says
/// `column "tags" of relation "tags" does not exist`, hinting *"SET target columns
/// cannot be qualified with the relation name"*.
///
/// Verified against PostgreSQL 17 directly. `set_col` takes an
/// [`IntoIdent`](psql::IntoIdent) because MySQL's grammar does allow the qualified
/// form, so the builder cannot refuse it — which is why this is pinned here rather
/// than assumed.
#[test]
fn a_qualified_assignment_target_is_refused_by_postgresql() {
    let updated = psql::update((
        update::table(quote("tags")),
        update::set_col(("tags", "name")).to_arg("x"),
        update::where_(quote("id").eq(arg(1i32))),
    ));
    let (sql, _) = updated.build().expect("it renders");
    assert_eq!(
        sql,
        r#"UPDATE "tags" SET "tags"."name" = $1 WHERE ("id" = $2)"#
    );
    assert!(
        keelson_sqlcheck::check_psql(&sql).is_ok(),
        "the grammar accepts it — only analysis refuses: {sql}"
    );

    let upserted = psql::insert((
        insert::into(quote("tags")).columns(["id", "name"]),
        insert::values((arg(1i32), arg("rust"))),
        insert::on_conflict(quote("id")).do_update(insert::set_col(("tags", "name")).to_arg("x")),
    ));
    let (sql, _) = upserted.build().expect("it renders");
    assert_eq!(
        sql,
        r#"INSERT INTO "tags" ("id", "name") VALUES ($1, $2) ON CONFLICT ("id") DO UPDATE SET "tags"."name" = $3"#
    );
}

/// The dangerous shape: a from-item list whose *leading* entry is missing has
/// nothing for `from_also` to extend — dropping it silently used to render
/// valid SQL that updates every row. The missing leading item is
/// a `build()` error instead.
#[test]
fn an_extra_from_item_with_no_leading_one_is_a_build_error() {
    let q = psql::update((
        update::table(quote("posts")),
        update::set_col("status").to_arg("archived"),
        update::from_also(quote("users")).as_("u"),
    ));
    let err = q.build().unwrap_err();
    // The substring names the SQL concept (the missing leading FROM item), not
    // the message wording.
    assert!(
        matches!(&err, psql::Error::Incomplete(what) if what.contains("FROM")),
        "got: {err}"
    );
}

/// The same rule on a `DELETE`, whose list is spelled `USING`.
#[test]
fn an_extra_using_item_with_no_leading_one_is_a_build_error() {
    let q = psql::delete((
        delete::from(quote("comments")),
        delete::using_also(quote("posts")).as_("p"),
    ));
    let err = q.build().unwrap_err();
    assert!(
        matches!(&err, psql::Error::Incomplete(what) if what.contains("USING")),
        "got: {err}"
    );
}

/// `VALUES ()` is not a row in PostgreSQL, so an empty one is dropped rather than
/// written — which leaves the statement with no row source, and that is
/// `DEFAULT VALUES`.
#[test]
fn an_empty_row_leaves_the_statement_with_default_values() {
    let q = psql::insert((insert::into(quote("tags")), insert::values(())));
    assert!(check(&q, r#"INSERT INTO "tags" DEFAULT VALUES"#).is_empty());

    let empty_list: Vec<(Expr, Expr)> = Vec::new();
    let q = psql::insert((insert::into(quote("tags")), insert::rows(empty_list)));
    assert!(check(&q, r#"INSERT INTO "tags" DEFAULT VALUES"#).is_empty());
}

/// `set_excluded` skips a blank column name instead of writing `"" = EXCLUDED.""`.
#[test]
fn set_excluded_skips_a_blank_column_name() {
    let q = psql::insert((
        insert::into(quote("tags")).columns(["id", "name"]),
        insert::values((arg(1i32), arg("rust"))),
        insert::on_conflict(quote("id")).do_update(insert::set_excluded(["name", ""])),
    ));
    check(
        &q,
        r#"INSERT INTO "tags" ("id", "name") VALUES ($1, $2)
           ON CONFLICT ("id") DO UPDATE SET "name" = EXCLUDED."name""#,
    );
}

/// A statement has one conflict clause, so a second `on_conflict` replaces the
/// first rather than appending a second `ON CONFLICT`.
#[test]
fn the_last_on_conflict_mod_wins() {
    let q = psql::insert((
        insert::into(quote("tags")).columns(["id", "name"]),
        insert::values((arg(1i32), arg("rust"))),
        insert::on_conflict(quote("id")).do_nothing(),
        insert::on_conflict(quote("name")).do_update(insert::set_excluded(["name"])),
    ));
    check(
        &q,
        r#"INSERT INTO "tags" ("id", "name") VALUES ($1, $2)
           ON CONFLICT ("name") DO UPDATE SET "name" = EXCLUDED."name""#,
    );
}

/// `qualified_name` is `ColId indirection`, so a schema-qualified target is written
/// with [`quote`] taking a tuple — on all three statements.
#[test]
fn schema_qualified_target_tables() {
    let inserted = psql::insert((
        insert::into(quote(("public", "tags"))).columns(["id", "name"]),
        insert::values((arg(1i32), arg("rust"))),
    ));
    check(
        &inserted,
        r#"INSERT INTO "public"."tags" ("id", "name") VALUES ($1, $2)"#,
    );

    let updated = psql::update((
        update::table(quote(("public", "tags"))).as_("t"),
        update::set_col("name").to_arg("rust"),
        update::where_(quote(("t", "id")).eq(arg(1i32))),
    ));
    check(
        &updated,
        r#"UPDATE "public"."tags" AS "t" SET "name" = $1 WHERE ("t"."id" = $2)"#,
    );

    let deleted = psql::delete((
        delete::from(quote(("public", "tags"))),
        delete::where_(quote("id").eq(arg(1i32))),
    ));
    check(&deleted, r#"DELETE FROM "public"."tags" WHERE ("id" = $1)"#);
}

/// `INSERT`'s target reaches the same [`TableChain`](psql::shared::TableChain) a
/// `FROM` item does, so the from-item decorations are callable on it even though
/// `INSERT INTO` takes none of them. The type system does not separate the two, and
/// this is the consequence.
#[test]
fn a_from_item_decoration_on_an_insert_target_does_not_parse() {
    let q = psql::insert((
        insert::into(quote("tags")).only(),
        insert::values((arg(1i32), arg("rust"))),
    ));
    let (sql, _) = q.build().expect("it renders");
    assert_eq!(sql, r#"INSERT INTO ONLY "tags" VALUES ($1, $2)"#);
    assert!(
        keelson_sqlcheck::check_psql(&sql).is_err(),
        "INSERT INTO has no ONLY: {sql}"
    );
}
