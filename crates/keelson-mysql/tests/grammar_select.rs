//! A walk through <https://dev.mysql.com/doc/refman/8.4/en/select.html>, clause by
//! clause. See `tests/common/mod.rs` for what each assertion runs and where the
//! expected strings come from.

mod common;

use common::{check, check_without_engine, check_without_grammar};
use keelson_mysql as mysql;
use keelson_mysql::{
    Chain, IntoExpr, MysqlOps, Value, arg, arg_group, args, case_, cast, f, frame, group,
    match_against, match_against_mode, not, or, quote, raw, s, select, subquery, template, window,
};

// ---------------------------------------------------------------------------
// The projection and the modifiers
// ---------------------------------------------------------------------------

#[test]
fn select_columns_from_where_with_a_bound_argument() {
    let q = mysql::select((
        select::columns((quote("id"), quote("name"))),
        select::from(quote("users")),
        select::where_(quote("age").gte(arg(21i32))),
    ));
    let args = check(&q, "SELECT `id`, `name` FROM `users` WHERE (`age` >= ?)");
    assert_eq!(args, vec![Value::I32(21)]);
}

#[test]
fn select_with_no_mods_at_all_is_a_star_projection() {
    // `SelectList` is the one clause whose absent rendering is not empty.
    let q = mysql::select(select::from(quote("users")));
    assert!(check(&q, "SELECT * FROM `users`").is_empty());
}

#[test]
fn a_qualified_star_and_a_qualified_column_sit_side_by_side() {
    // `users.*` is raw SQL: `*` is not an identifier and must not be quoted.
    let q = mysql::select((
        select::columns((raw("`users`.*"), quote(("posts", "title")))),
        select::from(quote("users")),
        select::inner_join(quote("posts"))
            .on_eq(quote(("posts", "user_id")), quote(("users", "id"))),
    ));
    check(
        &q,
        "SELECT `users`.*, `posts`.`title` FROM `users` \
         INNER JOIN `posts` ON (`posts`.`user_id` = `users`.`id`)",
    );
}

/// `SELECT [ALL | DISTINCT | DISTINCTROW] …` — `ALL` is the default and is not
/// representable.
#[test]
fn select_distinct_and_its_synonym() {
    let q = mysql::select((
        select::distinct(),
        select::columns(quote("status")),
        select::from(quote("posts")),
    ));
    check(&q, "SELECT DISTINCT `status` FROM `posts`");

    let q = mysql::select((
        select::distinct_row(),
        select::columns(quote("status")),
        select::from(quote("posts")),
    ));
    check(&q, "SELECT DISTINCTROW `status` FROM `posts`");
}

/// The grammar fixes the modifier order, and `Modifiers` sorts by it — so the three
/// mods below come out in the manual's sequence rather than the caller's.
#[test]
fn modifiers_are_written_in_grammar_order_not_call_order() {
    let q = mysql::select((
        select::sql_calc_found_rows(),
        select::high_priority(),
        select::distinct(),
        select::columns(quote("id")),
        select::from(quote("users")),
        select::limit(5),
    ));
    check(
        &q,
        "SELECT DISTINCT HIGH_PRIORITY SQL_CALC_FOUND_ROWS `id` FROM `users` LIMIT 5",
    );
}

/// The `STRAIGHT_JOIN` *modifier*: join every table in the order written. Not the
/// join operator of the same name.
#[test]
fn the_straight_join_modifier_precedes_the_select_list() {
    let q = mysql::select((
        select::straight(),
        select::columns(quote(("users", "id"))),
        select::from(quote("users")),
        select::from_also(quote("posts")),
    ));
    check(
        &q,
        "SELECT STRAIGHT_JOIN `users`.`id` FROM `users`, `posts`",
    );
}

#[test]
fn the_result_set_hints_are_modifiers_too() {
    let q = mysql::select((
        select::sql_small_result(),
        select::sql_big_result(),
        select::sql_buffer_result(),
        select::sql_no_cache(),
        select::columns(quote("status")),
        select::from(quote("posts")),
        select::group_by(quote("status")),
    ));
    check(
        &q,
        "SELECT SQL_SMALL_RESULT SQL_BIG_RESULT SQL_BUFFER_RESULT SQL_NO_CACHE `status` \
         FROM `posts` GROUP BY `status`",
    );
}

// ---------------------------------------------------------------------------
// Optimizer hints
// ---------------------------------------------------------------------------

/// *10.9.2*: the hint comment goes immediately after the statement's first keyword,
/// in front of the modifiers.
#[test]
fn an_optimizer_hint_comes_between_select_and_the_modifiers() {
    let q = mysql::select((
        select::max_execution_time(1000),
        select::distinct(),
        select::columns(quote("id")),
        select::from(quote("users")),
    ));
    check(
        &q,
        "SELECT /*+ MAX_EXECUTION_TIME(1000) */ DISTINCT `id` FROM `users`",
    );
}

#[test]
fn several_hints_share_one_comment() {
    let q = mysql::select((
        select::qb_name("outer"),
        select::set_var("sort_buffer_size = 16M"),
        select::optimizer_hint("NO_INDEX_MERGE(`users`)"),
        select::columns(quote("id")),
        select::from(quote("users")),
    ));
    check(
        &q,
        "SELECT /*+ QB_NAME(outer) SET_VAR(sort_buffer_size = 16M) NO_INDEX_MERGE(`users`) */ \
         `id` FROM `users`",
    );
}

// ---------------------------------------------------------------------------
// table_references
// ---------------------------------------------------------------------------

#[test]
fn a_from_item_takes_an_alias() {
    let q = mysql::select((
        select::columns(quote(("u", "name"))),
        select::from(quote("users")).as_("u"),
    ));
    check(&q, "SELECT `u`.`name` FROM `users` AS `u`");
}

/// A comma in `table_references` means the same thing as `CROSS JOIN`.
#[test]
fn several_from_items_are_comma_separated() {
    let q = mysql::select((
        select::columns((quote(("u", "id")), quote(("t", "name")))),
        select::from(quote("users")).as_("u"),
        select::from_also(quote("tags")).as_("t"),
        select::limit(1),
    ));
    check(
        &q,
        "SELECT `u`.`id`, `t`.`name` FROM `users` AS `u`, `tags` AS `t` LIMIT 1",
    );
}

/// A join hanging off a *non-leading* comma entry. *15.2.15.2*'s
/// `table_references` is `escaped_table_reference [, escaped_table_reference]…`
/// and each `table_reference` may be a `joined_table`; the comma has lower
/// precedence than the join keywords, so the join's left operand is the entry
/// it is written after, not the whole list.
#[test]
fn a_non_leading_from_entry_takes_its_own_joins() {
    let q = mysql::select((
        select::columns((quote(("u", "id")), quote(("c", "body")))),
        select::from(quote("users")).as_("u"),
        select::from_also(quote("posts")).as_("p").join(
            select::inner_join(quote("comments"))
                .as_("c")
                .on_eq(quote(("c", "post_id")), quote(("p", "id"))),
        ),
    ));
    check(
        &q,
        "SELECT `u`.`id`, `c`.`body` FROM `users` AS `u`, \
         `posts` AS `p` INNER JOIN `comments` AS `c` ON (`c`.`post_id` = `p`.`id`)",
    );

    // Several joins chain onto the one entry, and STRAIGHT_JOIN — the
    // narrower chain — attaches the same way.
    let q = mysql::select((
        select::columns(quote(("u", "id"))),
        select::from(quote("users")).as_("u"),
        select::from_also(quote("posts"))
            .as_("p")
            .join(
                select::inner_join(quote("comments"))
                    .as_("c")
                    .on_eq(quote(("c", "post_id")), quote(("p", "id"))),
            )
            .join(
                select::straight_join(quote("post_tags"))
                    .as_("pt")
                    .on_eq(quote(("pt", "post_id")), quote(("p", "id"))),
            ),
    ));
    check(
        &q,
        "SELECT `u`.`id` FROM `users` AS `u`, \
         `posts` AS `p` INNER JOIN `comments` AS `c` ON (`c`.`post_id` = `p`.`id`) \
         STRAIGHT_JOIN `post_tags` AS `pt` ON (`pt`.`post_id` = `p`.`id`)",
    );
}

#[test]
fn inner_left_and_right_joins_each_carry_their_own_condition_shape() {
    let q = mysql::select((
        select::columns(quote(("u", "id"))),
        select::from(quote("users")).as_("u"),
        select::inner_join(quote("posts"))
            .as_("p")
            .on_eq(quote(("p", "user_id")), quote(("u", "id"))),
        select::left_join(quote("comments"))
            .as_("c")
            .on(quote(("c", "post_id")).eq(quote(("p", "id")))),
        select::right_join(quote("post_tags"))
            .as_("pt")
            .using(["post_id"]),
    ));
    check(
        &q,
        "SELECT `u`.`id` FROM `users` AS `u` \
         INNER JOIN `posts` AS `p` ON (`p`.`user_id` = `u`.`id`) \
         LEFT JOIN `comments` AS `c` ON (`c`.`post_id` = `p`.`id`) \
         RIGHT JOIN `post_tags` AS `pt` USING (`post_id`)",
    );
}

/// *15.2.13.2*: "`JOIN`, `CROSS JOIN` and `INNER JOIN` are syntactic equivalents",
/// so unlike PostgreSQL a MySQL `CROSS JOIN` takes an `ON`.
#[test]
fn a_cross_join_may_carry_a_condition_in_mysql() {
    let q = mysql::select((
        select::columns(quote(("p", "title"))),
        select::from(quote("posts")).as_("p"),
        select::cross_join(quote("users"))
            .as_("u")
            .on_eq(quote(("u", "id")), quote(("p", "user_id"))),
    ));
    check(
        &q,
        "SELECT `p`.`title` FROM `posts` AS `p` \
         CROSS JOIN `users` AS `u` ON (`u`.`id` = `p`.`user_id`)",
    );
}

#[test]
fn a_cross_join_without_a_condition_is_a_plain_product() {
    let q = mysql::select((
        select::columns(quote(("t", "name"))),
        select::from(quote("tags")).as_("t"),
        select::cross_join(quote("users")).as_("u"),
        select::limit(1),
    ));
    check(
        &q,
        "SELECT `t`.`name` FROM `tags` AS `t` CROSS JOIN `users` AS `u` LIMIT 1",
    );
}

/// The join *operator* spelled `STRAIGHT_JOIN`, which forbids the optimizer from
/// reading the right-hand table first.
#[test]
fn a_straight_join_is_an_inner_join_with_a_fixed_order() {
    let q = mysql::select((
        select::columns(quote(("u", "id"))),
        select::from(quote("users")).as_("u"),
        select::straight_join(quote("posts"))
            .as_("p")
            .on_eq(quote(("p", "user_id")), quote(("u", "id"))),
    ));
    check(
        &q,
        "SELECT `u`.`id` FROM `users` AS `u` \
         STRAIGHT_JOIN `posts` AS `p` ON (`p`.`user_id` = `u`.`id`)",
    );
}

/// `NATURAL [INNER | {LEFT|RIGHT} [OUTER]] JOIN` is its own production, which is why
/// `natural()` is on `JoinChain` and not on the cross/straight chain.
#[test]
fn natural_joins_take_no_condition_at_all() {
    let q = mysql::select((
        select::columns(quote("post_id")),
        select::from(quote("post_tags")),
        select::inner_join(quote("posts")).natural(),
    ));
    check(
        &q,
        "SELECT `post_id` FROM `post_tags` NATURAL INNER JOIN `posts`",
    );

    let q = mysql::select((
        select::columns(quote("post_id")),
        select::from(quote("post_tags")),
        select::left_join(quote("tags")).natural(),
    ));
    check(
        &q,
        "SELECT `post_id` FROM `post_tags` NATURAL LEFT JOIN `tags`",
    );
}

#[test]
fn a_derived_table_is_a_parenthesised_query_with_an_alias() {
    let inner = mysql::select((
        select::columns((quote("id"), quote("user_id"))),
        select::from(quote("posts")),
        select::where_(quote("views").gt(arg(10i32))),
    ));
    let q = mysql::select((
        select::columns(quote(("p", "id"))),
        select::from(subquery(inner)).as_("p"),
    ));
    let args = check(
        &q,
        "SELECT `p`.`id` FROM (SELECT `id`, `user_id` FROM `posts` WHERE (`views` > ?)) AS `p`",
    );
    assert_eq!(args, vec![Value::I32(10)]);
}

/// MySQL 8.0.19 added the column-alias list on a derived table.
#[test]
fn a_derived_table_may_rename_its_columns() {
    let inner = mysql::select((select::columns(quote("id")), select::from(quote("users"))));
    let q = mysql::select((
        select::columns(quote(("x", "a"))),
        select::from(subquery(inner)).as_("x").columns(["a"]),
    ));
    check(
        &q,
        "SELECT `x`.`a` FROM (SELECT `id` FROM `users`) AS `x` (`a`)",
    );
}

/// MySQL 8.0.14 added `LATERAL`, which is what lets the derived table see `u`.
#[test]
fn a_lateral_derived_table_sees_the_items_before_it() {
    let inner = mysql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::where_(quote(("posts", "user_id")).eq(quote(("u", "id")))),
    ));
    let q = mysql::select((
        select::columns(quote(("u", "id"))),
        select::from(quote("users")).as_("u"),
        select::from_also(subquery(inner)).lateral().as_("p"),
    ));
    check(
        &q,
        "SELECT `u`.`id` FROM `users` AS `u`, \
         LATERAL (SELECT `id` FROM `posts` WHERE (`posts`.`user_id` = `u`.`id`)) AS `p`",
    );
}

/// *15.2.15.9 Lateral Derived Tables*: `LATERAL` is grammatical only in front
/// of a derived table — ``FROM LATERAL `posts` `` is a syntax error with
/// nothing to mean, since a base table cannot see the items before it anyway.
/// `.lateral()` on a bare table or CTE name records the error at the call and
/// `build()` refuses — the same judgment keelson-psql applies to
/// PostgreSQL's grammar.
#[test]
fn lateral_on_a_bare_table_is_a_build_error() {
    use keelson_mysql::Query as _;

    let q = mysql::select((
        select::from(quote("users")),
        select::inner_join(quote("posts")).lateral().on(raw("TRUE")),
    ));
    assert_eq!(
        q.build().unwrap_err().to_string(),
        "LATERAL is set on a bare table or CTE name, but LATERAL can precede only a derived table"
    );

    // The comma-list path is judged the same way.
    let q = mysql::select((
        select::from(quote("users")),
        select::from_also(quote("posts")).lateral(),
    ));
    assert!(q.build().is_err(), "a comma-listed bare name is no better");

    // A raw fragment stays trusted — progressive enhancement means
    // hand-written SQL is never judged — and a derived table is exactly what
    // the keyword is for (`a_lateral_derived_table_sees_the_items_before_it`).
    let q = mysql::select((
        select::columns(quote(("d", "one"))),
        select::from(quote("users")).as_("u"),
        select::from_also(raw("(SELECT 1 AS `one`)"))
            .lateral()
            .as_("d"),
    ));
    check(
        &q,
        "SELECT `d`.`one` FROM `users` AS `u`, LATERAL (SELECT 1 AS `one`) AS `d`",
    );
}

// ---------------------------------------------------------------------------
// Index hints
// ---------------------------------------------------------------------------

/// *10.9.4*: `index_hint_list` follows the alias, and each hint brings its own
/// parentheses.
#[test]
fn an_index_hint_follows_the_alias() {
    let q = mysql::select((
        select::columns(quote(("u", "id"))),
        select::from(quote("users")).as_("u").use_index(["PRIMARY"]),
        select::where_(quote(("u", "id")).eq(arg(1i32))),
    ));
    check(
        &q,
        "SELECT `u`.`id` FROM `users` AS `u` USE INDEX (`PRIMARY`) WHERE (`u`.`id` = ?)",
    );
}

#[test]
fn a_hint_scope_attaches_to_the_hint_just_added() {
    let q = mysql::select((
        select::columns(quote("id")),
        select::from(quote("users"))
            .ignore_index(["PRIMARY"])
            .for_order_by()
            .force_index(["PRIMARY"])
            .for_join(),
        select::order_by(quote("id")),
    ));
    check(
        &q,
        "SELECT `id` FROM `users` IGNORE INDEX FOR ORDER BY (`PRIMARY`) \
         FORCE INDEX FOR JOIN (`PRIMARY`) ORDER BY `id`",
    );
}

/// `USE INDEX ()` is meaningful — it tells MySQL to use no index — so the
/// parentheses are unconditional even for an empty list.
#[test]
fn use_index_with_no_indexes_still_has_its_parentheses() {
    let q = mysql::select((
        select::columns(quote("id")),
        select::from(quote("users")).use_index(Vec::<&'static str>::new()),
    ));
    check(&q, "SELECT `id` FROM `users` USE INDEX ()");
}

#[test]
fn an_index_hint_on_a_joined_table_lands_on_that_table() {
    let q = mysql::select((
        select::columns(quote(("p", "id"))),
        select::from(quote("users")).as_("u"),
        select::inner_join(quote("posts"))
            .as_("p")
            .use_index(["PRIMARY"])
            .for_join()
            .on_eq(quote(("p", "user_id")), quote(("u", "id"))),
    ));
    check(
        &q,
        "SELECT `p`.`id` FROM `users` AS `u` \
         INNER JOIN `posts` AS `p` USE INDEX FOR JOIN (`PRIMARY`) \
         ON (`p`.`user_id` = `u`.`id`)",
    );
}

/// *10.9.4*: `index_hint: USE {INDEX|KEY} [FOR {JOIN|ORDER BY|GROUP BY}] …` —
/// `FOR GROUP BY` is the third scope, restricting the hint to resolving the
/// `GROUP BY`. The manual has it; nothing had tested it.
#[test]
fn a_group_by_hint_scope_on_the_from_table() {
    let q = mysql::select((
        select::columns((quote("user_id"), f("COUNT", "*").as_("n"))),
        select::from(quote("posts"))
            .use_index(["PRIMARY"])
            .for_group_by(),
        select::group_by(quote("user_id")),
    ));
    check(
        &q,
        "SELECT `user_id`, COUNT(*) AS `n` FROM `posts` \
         USE INDEX FOR GROUP BY (`PRIMARY`) GROUP BY `user_id`",
    );
}

/// The same scope through a join's hint — [`JoinChain`] carries the other
/// `for_group_by`, and the hint stays with the joined table.
#[test]
fn a_group_by_hint_scope_on_a_joined_table() {
    let q = mysql::select((
        select::columns((quote(("p", "user_id")), f("COUNT", "*"))),
        select::from(quote("users")).as_("u"),
        select::inner_join(quote("posts"))
            .as_("p")
            .force_index(["PRIMARY"])
            .for_group_by()
            .on_eq(quote(("p", "user_id")), quote(("u", "id"))),
        select::group_by(quote(("p", "user_id"))),
    ));
    check(
        &q,
        "SELECT `p`.`user_id`, COUNT(*) FROM `users` AS `u` \
         INNER JOIN `posts` AS `p` FORCE INDEX FOR GROUP BY (`PRIMARY`) \
         ON (`p`.`user_id` = `u`.`id`) GROUP BY `p`.`user_id`",
    );
}

/// `PARTITION` precedes the alias everywhere except `DELETE`.
///
/// The engine tier cannot answer here: MySQL refuses `PARTITION` on a
/// non-partitioned table, and it also refuses to partition a table that takes part
/// in a foreign key — which every table in the shared schema does.
#[test]
fn a_partition_list_precedes_the_alias() {
    let q = mysql::select((
        select::columns(quote(("u", "id"))),
        select::from(quote("users"))
            .partition(["p0", "p1"])
            .as_("u"),
    ));
    check_without_engine(
        &q,
        "SELECT `u`.`id` FROM `users` PARTITION (`p0`, `p1`) AS `u`",
        "the shared schema has no partitioned table and cannot have one",
    );
}

// ---------------------------------------------------------------------------
// WHERE, GROUP BY, HAVING
// ---------------------------------------------------------------------------

#[test]
fn several_where_mods_are_and_joined() {
    let q = mysql::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        select::where_(quote("age").gte(arg(18i32))),
        select::where_(quote("is_active").eq(arg(true))),
    ));
    let args = check(
        &q,
        "SELECT `id` FROM `users` WHERE (`age` >= ?) AND (`is_active` = ?)",
    );
    assert_eq!(args, vec![Value::I32(18), Value::Bool(true)]);
}

#[test]
fn or_and_not_bring_their_own_parentheses() {
    let q = mysql::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        select::where_(or((
            quote("email").is_null(),
            not(quote("is_active").eq(arg(true))),
        ))),
    ));
    check(
        &q,
        "SELECT `id` FROM `users` WHERE ((`email` IS NULL) OR NOT (`is_active` = ?))",
    );
}

#[test]
fn group_by_and_having_with_an_aggregate() {
    let q = mysql::select((
        select::columns((quote("status"), f("COUNT", "*").as_("n"))),
        select::from(quote("posts")),
        select::group_by(quote("status")),
        select::having(f("COUNT", "*").into_expr().gt(arg(1i32))),
    ));
    let args = check(
        &q,
        "SELECT `status`, COUNT(*) AS `n` FROM `posts` GROUP BY `status` HAVING (COUNT(*) > ?)",
    );
    assert_eq!(args, vec![Value::I32(1)]);
}

/// `GROUP BY … [WITH ROLLUP]` is MySQL's only super-aggregate: there is no
/// `ROLLUP(…)` grouping element, no `CUBE` and no `GROUPING SETS`.
#[test]
fn group_by_with_rollup() {
    let q = mysql::select((
        select::columns((quote("status"), f("SUM", quote("views")))),
        select::from(quote("posts")),
        select::group_by(quote("status")),
        select::with_rollup(),
    ));
    check_without_grammar(
        &q,
        "SELECT `status`, SUM(`views`) FROM `posts` GROUP BY `status` WITH ROLLUP",
        "GROUP BY … WITH ROLLUP",
    );
}

#[test]
fn group_by_several_expressions() {
    let q = mysql::select((
        select::columns((quote("user_id"), quote("status"), f("COUNT", "*"))),
        select::from(quote("posts")),
        select::group_by(quote("user_id")),
        select::group_by(quote("status")),
    ));
    check(
        &q,
        "SELECT `user_id`, `status`, COUNT(*) FROM `posts` GROUP BY `user_id`, `status`",
    );
}

// ---------------------------------------------------------------------------
// ORDER BY, LIMIT, OFFSET
// ---------------------------------------------------------------------------

#[test]
fn order_by_takes_a_direction_and_nothing_else() {
    // No `NULLS FIRST`/`NULLS LAST` and no `USING operator`: MySQL has neither.
    let q = mysql::select((
        select::columns((quote("id"), quote("name"))),
        select::from(quote("users")),
        select::order_by(quote("name")).asc(),
        select::order_by(quote("id")).desc(),
    ));
    check(
        &q,
        "SELECT `id`, `name` FROM `users` ORDER BY `name` ASC, `id` DESC",
    );
}

/// bob renders the same construct as ``ORDER BY name COLLATE `utf8mb4_bg_0900_as_cs`
/// ASC``: the collation name is quoted as an identifier and sits between the
/// expression and the direction.
#[test]
fn order_by_collate_sits_between_the_expression_and_the_direction() {
    let q = mysql::select((
        select::columns(quote("name")),
        select::from(quote("users")),
        select::order_by(quote("name"))
            .collate("utf8mb4_bg_0900_as_cs")
            .asc(),
    ));
    check(
        &q,
        "SELECT `name` FROM `users` ORDER BY `name` COLLATE `utf8mb4_bg_0900_as_cs` ASC",
    );
}

/// `LIMIT {[offset,] row_count | row_count OFFSET offset}` — the second spelling.
/// A number is a literal because [`IntoExpr`](keelson_mysql::IntoExpr) makes one.
#[test]
fn limit_and_offset_are_literals_by_default() {
    let q = mysql::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        select::limit(10),
        select::offset(20),
    ));
    assert!(check(&q, "SELECT `id` FROM `users` LIMIT 10 OFFSET 20").is_empty());
}

#[test]
fn limit_and_offset_can_be_bound_instead() {
    let q = mysql::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        select::limit(arg(10i64)),
        select::offset(arg(20i64)),
    ));
    let args = check(&q, "SELECT `id` FROM `users` LIMIT ? OFFSET ?");
    assert_eq!(args, vec![Value::I64(10), Value::I64(20)]);
}

// ---------------------------------------------------------------------------
// Locking
// ---------------------------------------------------------------------------

/// `FOR {UPDATE | SHARE} [OF tbl_name [, …]] [NOWAIT | SKIP LOCKED]`. MySQL has two
/// strengths, not PostgreSQL's four.
#[test]
fn for_update_scoped_to_a_table_and_refusing_to_wait() {
    let q = mysql::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        select::for_update().of(["users"]).no_wait(),
    ));
    check(&q, "SELECT `id` FROM `users` FOR UPDATE OF `users` NOWAIT");
}

#[test]
fn for_share_skipping_locked_rows() {
    let q = mysql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::for_share().skip_locked(),
    ));
    check(&q, "SELECT `id` FROM `posts` FOR SHARE SKIP LOCKED");
}

#[test]
fn two_locking_clauses_each_scoped_to_one_table() {
    let q = mysql::select((
        select::columns(quote(("u", "id"))),
        select::from(quote("users")).as_("u"),
        select::cross_join(quote("posts")).as_("p"),
        select::for_update().of(["u"]),
        select::for_share().of(["p"]).skip_locked(),
    ));
    check(
        &q,
        "SELECT `u`.`id` FROM `users` AS `u` CROSS JOIN `posts` AS `p` \
         FOR UPDATE OF `u` FOR SHARE OF `p` SKIP LOCKED",
    );
}

/// The pre-8.0 spelling, a production of its own with no `OF` list and no wait
/// option.
#[test]
fn lock_in_share_mode_is_the_other_alternative() {
    let q = mysql::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        select::lock_in_share_mode(),
    ));
    check_without_grammar(
        &q,
        "SELECT `id` FROM `users` LOCK IN SHARE MODE",
        "LOCK IN SHARE MODE",
    );
}

// ---------------------------------------------------------------------------
// Set operations
// ---------------------------------------------------------------------------

#[test]
fn union_parenthesises_each_operand() {
    let other = mysql::select((
        select::columns(quote("user_id")),
        select::from(quote("posts")),
    ));
    let q = mysql::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        select::union(other),
    ));
    check(
        &q,
        "SELECT `id` FROM `users` UNION (SELECT `user_id` FROM `posts`)",
    );
}

#[test]
fn union_all_keeps_the_duplicates() {
    let other = mysql::select((
        select::columns(quote("user_id")),
        select::from(quote("comments")),
    ));
    let q = mysql::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        select::union_all(other),
    ));
    check(
        &q,
        "SELECT `id` FROM `users` UNION ALL (SELECT `user_id` FROM `comments`)",
    );
}

/// `INTERSECT` and `EXCEPT` arrived in MySQL 8.0.31.
#[test]
fn intersect_and_except_are_available_in_mysql_8() {
    let posts = || {
        mysql::select((
            select::columns(quote("user_id")),
            select::from(quote("posts")),
        ))
    };
    let q = mysql::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        select::intersect(posts()),
    ));
    check(
        &q,
        "SELECT `id` FROM `users` INTERSECT (SELECT `user_id` FROM `posts`)",
    );

    let q = mysql::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        select::except(posts()),
    ));
    check(
        &q,
        "SELECT `id` FROM `users` EXCEPT (SELECT `user_id` FROM `posts`)",
    );
}

/// The leading query is parenthesised exactly when a set operation is present *and*
/// it has a tail clause of its own — otherwise `ORDER BY` would be taken to apply to
/// the union. bob renders this case identically.
#[test]
fn a_leading_query_with_its_own_tail_clauses_is_wrapped() {
    let other = mysql::select((
        select::columns(quote("user_id")),
        select::from(quote("posts")),
        select::order_by(quote("user_id")),
        select::limit(10),
    ));
    let q = mysql::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        select::order_by(quote("id")),
        select::limit(100),
        select::union(other),
        select::order_by_combined(quote("id")).desc(),
        select::limit_combined(1000),
        select::offset_combined(5),
    ));
    check(
        &q,
        "(SELECT `id` FROM `users` ORDER BY `id` LIMIT 100) \
         UNION (SELECT `user_id` FROM `posts` ORDER BY `user_id` LIMIT 10) \
         ORDER BY `id` DESC LIMIT 1000 OFFSET 5",
    );
}

#[test]
fn a_combination_with_no_tail_clause_on_the_leading_query_needs_no_wrapping() {
    let other = mysql::select((
        select::columns(quote("user_id")),
        select::from(quote("posts")),
    ));
    let q = mysql::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        select::union(other),
        select::order_by_combined(quote("id")),
        select::limit_combined(3),
    ));
    check(
        &q,
        "SELECT `id` FROM `users` UNION (SELECT `user_id` FROM `posts`) ORDER BY `id` LIMIT 3",
    );
}

// ---------------------------------------------------------------------------
// WITH
// ---------------------------------------------------------------------------

#[test]
fn a_cte_names_its_columns() {
    let body = mysql::select((
        select::columns((quote("id"), quote("title"))),
        select::from(quote("posts")),
        select::where_(quote("views").gt(arg(100i32))),
    ));
    let q = mysql::select((
        select::with("popular", body).columns(["post_id", "post_title"]),
        select::columns(quote("post_title")),
        select::from(quote("popular")),
    ));
    let args = check(
        &q,
        "WITH `popular` (`post_id`, `post_title`) AS \
         (SELECT `id`, `title` FROM `posts` WHERE (`views` > ?)) \
         SELECT `post_title` FROM `popular`",
    );
    assert_eq!(args, vec![Value::I32(100)]);
}

/// `WITH RECURSIVE` is a property of the whole list. The body is written raw here
/// because a recursive CTE refers to itself, which no builder can express as a
/// dependency.
#[test]
fn a_recursive_cte_is_marked_on_the_with_not_the_entry() {
    let q = mysql::select((
        select::recursive(true),
        select::with(
            "counter",
            raw("SELECT 1 AS `n` UNION ALL SELECT `n` + 1 FROM `counter` WHERE `n` < 5"),
        )
        .columns(["n"]),
        select::columns(quote("n")),
        select::from(quote("counter")),
    ));
    check(
        &q,
        "WITH RECURSIVE `counter` (`n`) AS \
         (SELECT 1 AS `n` UNION ALL SELECT `n` + 1 FROM `counter` WHERE `n` < 5) \
         SELECT `n` FROM `counter`",
    );
}

#[test]
fn two_ctes_are_comma_separated_under_one_with() {
    let a = mysql::select((select::columns(quote("id")), select::from(quote("users"))));
    let b = mysql::select((select::columns(quote("id")), select::from(quote("posts"))));
    let q = mysql::select((
        select::with("u", a),
        select::with("p", b),
        select::columns(quote(("u", "id"))),
        select::from(quote("u")),
        select::cross_join(quote("p")).on_eq(quote(("p", "id")), quote(("u", "id"))),
    ));
    check(
        &q,
        "WITH `u` AS (SELECT `id` FROM `users`), `p` AS (SELECT `id` FROM `posts`) \
         SELECT `u`.`id` FROM `u` CROSS JOIN `p` ON (`p`.`id` = `u`.`id`)",
    );
}

// ---------------------------------------------------------------------------
// Window functions
// ---------------------------------------------------------------------------

/// `window_spec: [window_name] [partition_clause] [order_clause] [frame_clause]`.
#[test]
fn a_window_definition_in_an_over_clause() {
    let q = mysql::select((
        select::columns(
            f("SUM", quote("views"))
                .over((
                    window::partition_by(quote("user_id")),
                    window::order_by(quote("id")),
                    frame::rows(),
                    frame::from_preceding(3),
                    frame::to_current_row(),
                ))
                .as_("running"),
        ),
        select::from(quote("posts")),
    ));
    check(
        &q,
        "SELECT SUM(`views`) OVER (PARTITION BY `user_id` ORDER BY `id` \
         ROWS BETWEEN 3 PRECEDING AND CURRENT ROW) AS `running` FROM `posts`",
    );
}

/// `frame_start` defaults to `UNBOUNDED PRECEDING` and `BETWEEN` appears exactly
/// when there is an end bound, so a lone `ROWS` is a complete frame.
#[test]
fn a_frame_with_only_a_mode_relies_on_the_grammars_defaults() {
    let q = mysql::select((
        select::columns(f("COUNT", "*").over(frame::rows())),
        select::from(quote("posts")),
    ));
    check(
        &q,
        "SELECT COUNT(*) OVER (ROWS UNBOUNDED PRECEDING) FROM `posts`",
    );
}

#[test]
fn a_range_frame_between_two_offsets() {
    let q = mysql::select((
        select::columns(f("AVG", quote("views")).over((
            window::order_by(quote("id")),
            frame::range(),
            frame::from_preceding(arg(1i32)),
            frame::to_following(arg(1i32)),
        ))),
        select::from(quote("posts")),
    ));
    let args = check(
        &q,
        "SELECT AVG(`views`) OVER (ORDER BY `id` RANGE BETWEEN ? PRECEDING AND ? FOLLOWING) \
         FROM `posts`",
    );
    assert_eq!(args, vec![Value::I32(1), Value::I32(1)]);
}

/// `OVER \`w\`` references a `WINDOW`-clause entry; the parenthesised form would
/// copy it, which MySQL refuses once the named window has a frame.
#[test]
fn a_named_window_is_referenced_without_parentheses() {
    let q = mysql::select((
        select::columns(f("ROW_NUMBER", ()).over_name("w").as_("rn")),
        select::from(quote("posts")),
        select::window(
            "w",
            (
                window::partition_by(quote("user_id")),
                window::order_by(quote("id")).desc(),
            ),
        ),
    ));
    check(
        &q,
        "SELECT ROW_NUMBER() OVER `w` AS `rn` FROM `posts` \
         WINDOW `w` AS (PARTITION BY `user_id` ORDER BY `id` DESC)",
    );
}

#[test]
fn one_window_may_be_based_on_another() {
    let q = mysql::select((
        select::columns(f("SUM", quote("views")).over_name("w2")),
        select::from(quote("posts")),
        select::window("w1", window::partition_by(quote("user_id"))),
        select::window(
            "w2",
            (window::based_on("w1"), window::order_by(quote("id"))),
        ),
    ));
    check(
        &q,
        "SELECT SUM(`views`) OVER `w2` FROM `posts` \
         WINDOW `w1` AS (PARTITION BY `user_id`), `w2` AS (`w1` ORDER BY `id`)",
    );
}

#[test]
fn over_with_no_definition_at_all_is_the_whole_partition() {
    let q = mysql::select((
        select::columns(f("COUNT", "*").over(())),
        select::from(quote("posts")),
    ));
    check(&q, "SELECT COUNT(*) OVER () FROM `posts`");
}

// ---------------------------------------------------------------------------
// Expressions
// ---------------------------------------------------------------------------

#[test]
fn an_aggregate_with_distinct_and_group_concat_with_a_separator() {
    let q = mysql::select((
        select::columns((
            f("COUNT", quote("user_id")).distinct(),
            f("GROUP_CONCAT", quote("title"))
                .order_by(quote("id"))
                .separator(", ")
                .as_("titles"),
        )),
        select::from(quote("posts")),
    ));
    check(
        &q,
        "SELECT COUNT(DISTINCT `user_id`), \
         GROUP_CONCAT(`title` ORDER BY `id` SEPARATOR ', ') AS `titles` FROM `posts`",
    );
}

#[test]
fn case_with_and_without_an_else_branch() {
    let q = mysql::select((
        select::columns((
            case_()
                .when(quote("views").gt(arg(100i32)), s("hot"))
                .else_(s("cold"))
                .as_("temp"),
            case_()
                .when(quote("status").eq(s("draft")), arg(1i32))
                .end()
                .as_("d"),
        )),
        select::from(quote("posts")),
    ));
    let args = check(
        &q,
        "SELECT (CASE WHEN (`views` > ?) THEN 'hot' ELSE 'cold' END) AS `temp`, \
         (CASE WHEN (`status` = 'draft') THEN ? END) AS `d` FROM `posts`",
    );
    assert_eq!(args, vec![Value::I32(100), Value::I32(1)]);
}

/// MySQL has no `::`, so `CAST` is the only spelling — and it is already
/// self-delimiting, so keelson adds no parentheses of its own.
#[test]
fn cast_is_written_out_and_not_wrapped() {
    let q = mysql::select((
        select::columns(cast(quote("age"), "CHAR").as_("age_text")),
        select::from(quote("users")),
    ));
    check(&q, "SELECT CAST(`age` AS CHAR) AS `age_text` FROM `users`");
}

#[test]
fn a_row_constructor_compared_against_a_list_of_rows() {
    let q = mysql::select((
        select::columns(quote("post_id")),
        select::from(quote("post_tags")),
        select::where_(
            group((quote("post_id"), quote("tag_id")))
                .in_((arg_group([1i32, 2]), arg_group([3i32, 4]))),
        ),
    ));
    let args = check(
        &q,
        "SELECT `post_id` FROM `post_tags` \
         WHERE ((`post_id`, `tag_id`) IN ((?, ?), (?, ?)))",
    );
    assert_eq!(
        args,
        vec![Value::I32(1), Value::I32(2), Value::I32(3), Value::I32(4)]
    );
}

#[test]
fn a_scalar_subquery_in_the_where_clause_continues_the_argument_order() {
    let inner = mysql::select((
        select::columns(quote("user_id")),
        select::from(quote("posts")),
        select::where_(quote("views").gt(arg(50i32))),
    ));
    let q = mysql::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        select::where_(quote("age").gte(arg(18i32))),
        select::where_(quote("id").in_(inner)),
        select::where_(quote("is_active").eq(arg(true))),
    ));
    let args = check(
        &q,
        "SELECT `id` FROM `users` WHERE (`age` >= ?) \
         AND (`id` IN (SELECT `user_id` FROM `posts` WHERE (`views` > ?))) \
         AND (`is_active` = ?)",
    );
    // The whole point of a shared counter: the sub-query's argument lands between
    // its siblings, which is invisible in MySQL's SQL and visible here.
    assert_eq!(
        args,
        vec![Value::I32(18), Value::I32(50), Value::Bool(true)]
    );
}

#[test]
fn a_quantified_comparison_against_a_subquery() {
    let inner = mysql::select((
        select::columns(quote("user_id")),
        select::from(quote("posts")),
    ));
    let q = mysql::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        select::where_(quote("id").eq_any(inner)),
    ));
    check(
        &q,
        "SELECT `id` FROM `users` \
         WHERE (`id` = ANY (SELECT `user_id` FROM `posts`))",
    );
}

#[test]
fn the_pattern_operators_mysql_adds() {
    let q = mysql::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        select::where_(quote("name").regexp(arg("^a"))),
        select::where_(quote("email").not_like(arg("%@example.com"))),
        select::where_(quote("name").sounds_like(arg("robert"))),
    ));
    let args = check_without_grammar(
        &q,
        "SELECT `id` FROM `users` WHERE (`name` REGEXP ?) \
         AND (`email` NOT LIKE ?) AND (`name` SOUNDS LIKE ?)",
        "SOUNDS LIKE",
    );
    assert_eq!(args.len(), 3);
}

#[test]
fn the_null_safe_equality_operator_is_what_mysql_has_instead_of_is_distinct_from() {
    let q = mysql::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        select::where_(quote("email").null_safe_eq(arg("a@b.c"))),
    ));
    check(&q, "SELECT `id` FROM `users` WHERE (`email` <=> ?)");
}

#[test]
fn integer_division_and_modulo_are_keywords() {
    let q = mysql::select((
        select::columns((
            quote("age").div(2i32).as_("halves"),
            quote("age").modulo(3i32).as_("rem"),
        )),
        select::from(quote("users")),
    ));
    check_without_grammar(
        &q,
        "SELECT (`age` DIV 2) AS `halves`, (`age` MOD 3) AS `rem` FROM `users`",
        "the DIV and MOD keyword operators",
    );
}

#[test]
fn the_bitwise_operators_and_xor() {
    let q = mysql::select((
        select::columns((
            quote("age").bit_and(3i32).as_("masked"),
            quote("age").shift_left(1i32).as_("doubled"),
        )),
        select::from(quote("users")),
        select::where_(quote("is_active").xor(arg(true))),
    ));
    check(
        &q,
        "SELECT (`age` & 3) AS `masked`, (`age` << 1) AS `doubled` \
         FROM `users` WHERE (`is_active` XOR ?)",
    );
}

#[test]
fn the_json_arrow_operators() {
    let q = mysql::select((
        select::columns((
            quote("body").json_get(s("$.a")).as_("a"),
            quote("body").json_get_text(s("$.b")).as_("b"),
        )),
        select::from(quote("comments")),
    ));
    check(
        &q,
        "SELECT (`body` -> '$.a') AS `a`, (`body` ->> '$.b') AS `b` FROM `comments`",
    );
}

/// `MEMBER OF` arrived in MySQL 8.0.17 and parenthesises its right operand.
#[test]
fn member_of_tests_a_json_array() {
    let q = mysql::select((
        select::columns(quote("id")),
        select::from(quote("comments")),
        select::where_(arg(3i32).member_of(quote("body"))),
    ));
    check(
        &q,
        "SELECT `id` FROM `comments` WHERE (? MEMBER OF (`body`))",
    );
}

#[test]
fn the_three_valued_logic_predicates() {
    let q = mysql::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        select::where_(quote("is_active").is_true()),
        select::where_(quote("email").is_not_unknown()),
    ));
    check(
        &q,
        "SELECT `id` FROM `users` WHERE (`is_active` IS TRUE) AND (`email` IS NOT UNKNOWN)",
    );
}

#[test]
fn collate_and_binary_decorate_an_expression() {
    let q = mysql::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        select::where_(quote("name").collate("utf8mb4_bin").eq(arg("Ada"))),
        select::where_(quote("email").binary().ne(arg("x"))),
    ));
    check(
        &q,
        "SELECT `id` FROM `users` \
         WHERE ((`name` COLLATE `utf8mb4_bin`) = ?) AND ((BINARY `email`) <> ?)",
    );
}

/// *14.9*: `MATCH (cols) AGAINST (expr [search_modifier])`. Needs a `FULLTEXT`
/// index to run, but not to prepare.
#[test]
fn match_against_in_both_of_its_modes() {
    let q = mysql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::where_(match_against(quote("title"), s("rust"))),
    ));
    check(
        &q,
        "SELECT `id` FROM `posts` WHERE MATCH (`title`) AGAINST ('rust')",
    );

    let q = mysql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::where_(match_against_mode(
            quote("title"),
            s("+rust -go"),
            "IN BOOLEAN MODE",
        )),
    ));
    check(
        &q,
        "SELECT `id` FROM `posts` WHERE MATCH (`title`) AGAINST ('+rust -go' IN BOOLEAN MODE)",
    );
}

#[test]
fn a_template_counts_its_holes_and_binds_in_order() {
    use keelson_mysql::RawArg;

    let q = mysql::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        select::where_(template(
            "`age` BETWEEN ? AND ?",
            [RawArg::value(18i32), RawArg::value(65i32)],
        )),
    ));
    let args = check(&q, "SELECT `id` FROM `users` WHERE `age` BETWEEN ? AND ?");
    assert_eq!(args, vec![Value::I32(18), Value::I32(65)]);
}

#[test]
fn args_expands_to_one_placeholder_per_value() {
    let q = mysql::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        select::where_(quote("id").in_(args([1i32, 2, 3]))),
    ));
    let args = check(&q, "SELECT `id` FROM `users` WHERE (`id` IN (?, ?, ?))");
    assert_eq!(args, vec![Value::I32(1), Value::I32(2), Value::I32(3)]);
}

/// A `&'static str` in an expression slot is raw SQL, so an unquoted table name is
/// what `from("users")` gives — the progressive-enhancement rule.
#[test]
fn a_bare_string_is_raw_sql_and_is_not_quoted() {
    let q = mysql::select((select::columns("id, name"), select::from("users")));
    check(&q, "SELECT id, name FROM users");
}

#[test]
fn a_conditional_mod_is_an_option() {
    let admin = false;
    let q = mysql::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        (!admin).then(|| select::where_(quote("is_active").eq(arg(true)))),
    ));
    check(&q, "SELECT `id` FROM `users` WHERE (`is_active` = ?)");

    let admin = true;
    let q = mysql::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        (!admin).then(|| select::where_(quote("is_active").eq(arg(true)))),
    ));
    check(&q, "SELECT `id` FROM `users`");
}

// ---------------------------------------------------------------------------
// The constructs the coverage gate found unexercised (docs/testing-tiers.md)
// ---------------------------------------------------------------------------

/// *10.9.2*: `RESOURCE_GROUP` is the one fixed-shape hint the walk above
/// misses. The group need not exist for `PREPARE` — an unresolvable hint is a
/// warning, which is the whole point of hints.
#[test]
fn the_resource_group_hint() {
    let q = mysql::select((
        select::resource_group("batch"),
        select::columns(quote("id")),
        select::from(quote("users")),
    ));
    check(&q, "SELECT /*+ RESOURCE_GROUP(batch) */ `id` FROM `users`");
}

/// *15.2.14 / 15.2.4*: the `ALL` spellings of `INTERSECT` and `EXCEPT`
/// (8.0.31, same release as the operators themselves).
#[test]
fn intersect_all_and_except_all_keep_duplicates() {
    let posts = || {
        mysql::select((
            select::columns(quote("user_id")),
            select::from(quote("posts")),
        ))
    };
    let q = mysql::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        select::intersect_all(posts()),
    ));
    check(
        &q,
        "SELECT `id` FROM `users` INTERSECT ALL (SELECT `user_id` FROM `posts`)",
    );

    let q = mysql::select((
        select::columns(quote("id")),
        select::from(quote("users")),
        select::except_all(posts()),
    ));
    check(
        &q,
        "SELECT `id` FROM `users` EXCEPT ALL (SELECT `user_id` FROM `posts`)",
    );
}

/// *14.20.2*: a frame that runs from the current row to the partition's end —
/// the one bound the frame walk above never reaches.
#[test]
fn a_frame_may_run_to_unbounded_following() {
    let q = mysql::select((
        select::columns(f("SUM", quote("views")).over((
            window::order_by(quote("id")),
            frame::rows(),
            frame::from_current_row(),
            frame::to_unbounded_following(),
        ))),
        select::from(quote("posts")),
    ));
    check(
        &q,
        "SELECT SUM(`views`) OVER (ORDER BY `id` \
         ROWS BETWEEN CURRENT ROW AND UNBOUNDED FOLLOWING) FROM `posts`",
    );
}

/// The negated and less-travelled comparison predicates (*14.4.2*, *14.8.2*).
/// Each is a one-line spelling of its production; what the engine tier adds is
/// that every one of them still resolves against real columns.
#[test]
fn every_remaining_comparison_predicate() {
    let predicates: [(mysql::Expr, &str); 8] = [
        (
            quote("id").not_in((arg(1i32), arg(2i32))),
            "(`id` NOT IN (?, ?))",
        ),
        (quote("email").is_not_null(), "(`email` IS NOT NULL)"),
        (
            quote("age").not_between(arg(1i32), arg(9i32)),
            "(`age` NOT BETWEEN ? AND ?)",
        ),
        (
            quote("name").like_escape(arg("a!_%"), s("!")),
            "(`name` LIKE ? ESCAPE '!')",
        ),
        (quote("name").not_regexp(arg("^a")), "(`name` NOT REGEXP ?)"),
        (quote("name").rlike(arg("^a")), "(`name` RLIKE ?)"),
        (quote("age").bang_eq(arg(21i32)), "(`age` != ?)"),
        (
            quote("id").ne_all(mysql::query(mysql::select((
                select::columns(quote("user_id")),
                select::from(quote("posts")),
            )))),
            "(`id` <> ALL (SELECT `user_id` FROM `posts`))",
        ),
    ];
    for (predicate, rendered) in predicates {
        check(
            &mysql::select((
                select::columns(quote("id")),
                select::from(quote("users")),
                select::where_(predicate),
            )),
            &format!("SELECT `id` FROM `users` WHERE {rendered}"),
        );
    }
}

/// *14.3.2*: the three-valued tests the boolean walk above misses. MySQL has
/// no boolean type — `is_active` is `tinyint(1)` — and `IS TRUE` is defined on
/// numbers, which is exactly what makes these engine-checkable here.
#[test]
fn the_remaining_boolean_tests() {
    let predicates: [(mysql::Expr, &str); 4] = [
        (
            quote("is_active").is_not_true(),
            "(`is_active` IS NOT TRUE)",
        ),
        (quote("is_active").is_false(), "(`is_active` IS FALSE)"),
        (
            quote("is_active").is_not_false(),
            "(`is_active` IS NOT FALSE)",
        ),
        (quote("is_active").is_unknown(), "(`is_active` IS UNKNOWN)"),
    ];
    for (predicate, rendered) in predicates {
        check(
            &mysql::select((
                select::columns(quote("id")),
                select::from(quote("users")),
                select::where_(predicate),
            )),
            &format!("SELECT `id` FROM `users` WHERE {rendered}"),
        );
    }
}

/// *14.12*: the bit operators the arithmetic walk above misses. Values, not
/// predicates, so they stand in the select list.
#[test]
fn the_remaining_bit_operators() {
    let values: [(mysql::Expr, &str); 3] = [
        (quote("views").bit_or(arg(8i32)), "(`views` | ?)"),
        (quote("views").bit_xor(arg(8i32)), "(`views` ^ ?)"),
        (quote("views").shift_right(arg(2i32)), "(`views` >> ?)"),
    ];
    for (value, rendered) in values {
        check(
            &mysql::select((select::columns(value), select::from(quote("posts")))),
            &format!("SELECT {rendered} FROM `posts`"),
        );
    }
}
