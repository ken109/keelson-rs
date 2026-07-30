//! A walk through <https://dev.mysql.com/doc/refman/8.4/en/delete.html>. See
//! `tests/common/mod.rs` for what each assertion runs and where the expected strings
//! come from.

mod common;

use common::{check, check_shape_only, check_without_engine, check_without_grammar};
use keelson_mysql as mysql;
use keelson_mysql::{Chain, Query, Value, arg, delete, quote, select};

// ---------------------------------------------------------------------------
// The single-table form
// ---------------------------------------------------------------------------

/// bob renders the same construct as ``DELETE FROM films WHERE (`kind` = ?)``.
#[test]
fn delete_from_one_table_with_a_condition() {
    let q = mysql::delete((
        delete::from(quote("comments")),
        delete::where_(quote("id").eq(arg(1i32))),
    ));
    let args = check(&q, "DELETE FROM `comments` WHERE (`id` = ?)");
    assert_eq!(args, vec![Value::I32(1)]);
}

#[test]
fn the_target_may_be_aliased() {
    let q = mysql::delete((
        delete::from(quote("comments")).as_("c"),
        delete::where_(quote(("c", "id")).eq(arg(1i32))),
    ));
    check(&q, "DELETE FROM `comments` AS `c` WHERE (`c`.`id` = ?)");
}

/// `[WHERE …] [ORDER BY …] [LIMIT row_count]` — bob renders the same shape as
/// ``DELETE FROM films WHERE (`kind` = ?) ORDER BY producer DESC LIMIT 10``.
#[test]
fn order_by_and_limit_bound_the_rows_removed() {
    let q = mysql::delete((
        delete::from(quote("comments")),
        delete::where_(quote("user_id").is_null()),
        delete::order_by(quote("id")).desc(),
        delete::limit(10),
    ));
    check(
        &q,
        "DELETE FROM `comments` WHERE (`user_id` IS NULL) ORDER BY `id` DESC LIMIT 10",
    );
}

#[test]
fn a_bound_limit_is_prepared_like_any_other_argument() {
    let q = mysql::delete((
        delete::from(quote("comments")),
        delete::order_by(quote("id")),
        delete::limit(arg(3i64)),
    ));
    let args = check(&q, "DELETE FROM `comments` ORDER BY `id` LIMIT ?");
    assert_eq!(args, vec![Value::I64(3)]);
}

// ---------------------------------------------------------------------------
// Modifiers and hints
// ---------------------------------------------------------------------------

/// `DELETE [LOW_PRIORITY] [QUICK] [IGNORE] FROM …` — three modifiers in that order,
/// whichever order the mods were written in.
///
/// `sqlparser` stops at `QUICK`; the manual and the server both take it.
#[test]
fn low_priority_quick_and_ignore_in_grammar_order() {
    let q = mysql::delete((
        delete::ignore(),
        delete::quick(),
        delete::low_priority(),
        delete::from(quote("comments")),
        delete::where_(quote("id").eq(arg(1i32))),
    ));
    check_without_grammar(
        &q,
        "DELETE LOW_PRIORITY QUICK IGNORE FROM `comments` WHERE (`id` = ?)",
        "the DELETE … QUICK modifier",
    );
}

/// On its own `QUICK` parses; it is only in combination that `sqlparser` loses the
/// thread, which is another way of saying the backend is not MySQL's grammar.
#[test]
fn quick_on_its_own() {
    let q = mysql::delete((
        delete::quick(),
        delete::from(quote("comments")),
        delete::where_(quote("id").eq(arg(1i32))),
    ));
    check(&q, "DELETE QUICK FROM `comments` WHERE (`id` = ?)");
}

#[test]
fn an_optimizer_hint_sits_between_delete_and_from() {
    let q = mysql::delete((
        delete::qb_name("d"),
        delete::from(quote("comments")),
        delete::where_(quote("id").eq(arg(1i32))),
    ));
    check(
        &q,
        "DELETE /*+ QB_NAME(d) */ FROM `comments` WHERE (`id` = ?)",
    );
}

// ---------------------------------------------------------------------------
// PARTITION, which DELETE puts in an unusual place
// ---------------------------------------------------------------------------

/// `DELETE FROM tbl_name [[AS] tbl_alias] [PARTITION (…)]` — this is the one
/// statement where MySQL writes `PARTITION` *after* the alias, and once for the whole
/// statement rather than per table reference. bob carries a separate `Partitions`
/// field on its `DeleteQuery` for exactly this reason.
#[test]
fn a_partition_list_follows_the_delete_target() {
    let q = mysql::delete((
        delete::from(quote("comments")).partition(["p0"]),
        delete::where_(quote("id").eq(arg(1i32))),
    ));
    check_without_engine(
        &q,
        "DELETE FROM `comments` PARTITION (`p0`) WHERE (`id` = ?)",
        "the shared schema has no partitioned table and cannot have one",
    );
}

/// The alias-then-partition ordering, which is the whole point of the separate slot.
#[test]
fn a_partition_list_follows_the_delete_alias_too() {
    let q = mysql::delete((
        delete::from(quote("comments")).as_("c").partition(["p0"]),
        delete::where_(quote(("c", "id")).eq(arg(1i32))),
    ));
    check_shape_only(
        &q,
        "DELETE FROM `comments` AS `c` PARTITION (`p0`) WHERE (`c`.`id` = ?)",
        "PARTITION after an alias on DELETE",
        "the shared schema has no partitioned table and cannot have one",
    );
}

// ---------------------------------------------------------------------------
// The multiple-table form
// ---------------------------------------------------------------------------

/// `DELETE FROM t1, t2 USING table_references` — the spelling keelson builds, because
/// it is also the single-table form with the `USING` left out. bob's expected SQL for
/// the same shape is `DELETE FROM films, actors USING films INNER JOIN …`.
#[test]
fn two_tables_deleted_from_through_from_and_using() {
    let q = mysql::delete((
        delete::from(quote("comments")),
        delete::from(quote("post_tags")),
        delete::using(quote("comments")),
        delete::inner_join(quote("posts"))
            .on_eq(quote(("posts", "id")), quote(("comments", "post_id"))),
        delete::inner_join(quote("post_tags"))
            .on_eq(quote(("post_tags", "post_id")), quote(("posts", "id"))),
        delete::where_(quote(("posts", "status")).eq(arg("draft"))),
    ));
    let args = check(
        &q,
        "DELETE FROM `comments`, `post_tags` USING `comments` \
         INNER JOIN `posts` ON (`posts`.`id` = `comments`.`post_id`) \
         INNER JOIN `post_tags` ON (`post_tags`.`post_id` = `posts`.`id`) \
         WHERE (`posts`.`status` = ?)",
    );
    assert_eq!(args, vec![Value::Text("draft".into())]);
}

/// Every table named after `FROM` must also appear in the `USING` list: `USING` *is*
/// the statement's whole `table_references`, and `FROM` only selects which of them
/// lose rows.
///
/// bob's `with using` case for MySQL is `DELETE FROM employees USING accounts …`,
/// which omits `employees` from the `USING` list — real MySQL answers
/// *ERROR 1109 (42S02): Unknown table 'employees' in MULTI DELETE*. bob's judge is an
/// ANTLR grammar, which cannot see that. This case is the corrected shape, and it is
/// the clearest thing the engine tier caught here.
#[test]
fn every_table_deleted_from_must_appear_in_the_using_list() {
    let q = mysql::delete((
        delete::from(quote("comments")),
        delete::using(quote("comments")),
        delete::using_also(quote("posts")),
        delete::where_(quote(("comments", "post_id")).eq(quote(("posts", "id")))),
        delete::where_(quote(("posts", "status")).eq(arg("draft"))),
    ));
    check(
        &q,
        "DELETE FROM `comments` USING `comments`, `posts` \
         WHERE (`comments`.`post_id` = `posts`.`id`) AND (`posts`.`status` = ?)",
    );
}

#[test]
fn several_using_items_are_comma_separated() {
    let q = mysql::delete((
        delete::from(quote("post_tags")),
        delete::using(quote("post_tags")),
        delete::using_also(quote("posts")),
        delete::where_(quote(("post_tags", "post_id")).eq(quote(("posts", "id")))),
        delete::where_(quote(("posts", "views")).lt(arg(5i32))),
    ));
    check(
        &q,
        "DELETE FROM `post_tags` USING `post_tags`, `posts` \
         WHERE (`post_tags`.`post_id` = `posts`.`id`) AND (`posts`.`views` < ?)",
    );
}

#[test]
fn a_left_join_in_the_using_list_finds_the_orphans() {
    let q = mysql::delete((
        delete::from(quote("comments")),
        delete::using(quote("comments")),
        delete::left_join(quote("users"))
            .on_eq(quote(("users", "id")), quote(("comments", "user_id"))),
        delete::where_(quote(("users", "id")).is_null()),
    ));
    check(
        &q,
        "DELETE FROM `comments` USING `comments` \
         LEFT JOIN `users` ON (`users`.`id` = `comments`.`user_id`) \
         WHERE (`users`.`id` IS NULL)",
    );
}

#[test]
fn a_cross_join_in_the_using_list() {
    let q = mysql::delete((
        delete::from(quote("post_tags")),
        delete::using(quote("post_tags")),
        delete::cross_join(quote("tags"))
            .on_eq(quote(("tags", "id")), quote(("post_tags", "tag_id"))),
        delete::where_(quote(("tags", "name")).eq(arg("obsolete"))),
    ));
    check(
        &q,
        "DELETE FROM `post_tags` USING `post_tags` \
         CROSS JOIN `tags` ON (`tags`.`id` = `post_tags`.`tag_id`) \
         WHERE (`tags`.`name` = ?)",
    );
}

#[test]
fn a_straight_join_in_the_using_list() {
    let q = mysql::delete((
        delete::from(quote("comments")),
        delete::using(quote("comments")),
        delete::straight_join(quote("posts"))
            .on_eq(quote(("posts", "id")), quote(("comments", "post_id"))),
        delete::where_(quote(("posts", "views")).eq(arg(0i32))),
    ));
    check(
        &q,
        "DELETE FROM `comments` USING `comments` \
         STRAIGHT_JOIN `posts` ON (`posts`.`id` = `comments`.`post_id`) \
         WHERE (`posts`.`views` = ?)",
    );
}

#[test]
fn an_index_hint_on_a_using_item() {
    let q = mysql::delete((
        delete::from(quote("comments")),
        delete::using(quote("comments")).use_index(["PRIMARY"]),
        delete::where_(quote(("comments", "id")).eq(arg(1i32))),
    ));
    check(
        &q,
        "DELETE FROM `comments` USING `comments` USE INDEX (`PRIMARY`) \
         WHERE (`comments`.`id` = ?)",
    );
}

// ---------------------------------------------------------------------------
// WITH and sub-queries
// ---------------------------------------------------------------------------

/// MySQL permits a `WITH` at the beginning of a `DELETE`, unlike `INSERT`.
#[test]
fn a_cte_in_front_of_a_delete() {
    let stale = mysql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::where_(quote("views").lt(arg(5i32))),
    ));
    let ids = mysql::select((select::columns(quote("id")), select::from(quote("stale"))));
    let q = mysql::delete((
        delete::with("stale", stale),
        delete::from(quote("comments")),
        delete::where_(quote("post_id").in_(ids)),
    ));
    let args = check(
        &q,
        "WITH `stale` AS (SELECT `id` FROM `posts` WHERE (`views` < ?)) \
         DELETE FROM `comments` WHERE (`post_id` IN (SELECT `id` FROM `stale`))",
    );
    assert_eq!(args, vec![Value::I32(5)]);
}

#[test]
fn a_recursive_cte_in_front_of_a_delete() {
    use keelson_mysql::raw;

    let ids = mysql::select((select::columns(quote("n")), select::from(quote("counter"))));
    let q = mysql::delete((
        delete::recursive(true),
        delete::with(
            "counter",
            raw("SELECT 1 AS `n` UNION ALL SELECT `n` + 1 FROM `counter` WHERE `n` < 3"),
        )
        .columns(["n"]),
        delete::from(quote("comments")),
        delete::where_(quote("id").in_(ids)),
    ));
    check(
        &q,
        "WITH RECURSIVE `counter` (`n`) AS \
         (SELECT 1 AS `n` UNION ALL SELECT `n` + 1 FROM `counter` WHERE `n` < 3) \
         DELETE FROM `comments` WHERE (`id` IN (SELECT `n` FROM `counter`))",
    );
}

#[test]
fn a_subquery_in_the_where_clause() {
    let inner = mysql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::where_(quote("status").eq(arg("draft"))),
    ));
    let q = mysql::delete((
        delete::from(quote("comments")),
        delete::where_(quote("post_id").in_(inner)),
        delete::where_(quote("user_id").is_null()),
    ));
    let args = check(
        &q,
        "DELETE FROM `comments` \
         WHERE (`post_id` IN (SELECT `id` FROM `posts` WHERE (`status` = ?))) \
         AND (`user_id` IS NULL)",
    );
    assert_eq!(args, vec![Value::Text("draft".into())]);
}

// ---------------------------------------------------------------------------
// The incomplete statement
// ---------------------------------------------------------------------------

#[test]
fn a_delete_with_no_table_is_a_recorded_failure() {
    let err = mysql::delete(delete::where_(quote("id").eq(arg(1i32))))
        .build()
        .unwrap_err();
    assert_eq!(err.to_string(), "query is missing the table of a DELETE");
}

/// A `USING` list with no `FROM` table is not a repair of anything either.
#[test]
fn a_delete_with_only_a_using_list_is_a_recorded_failure() {
    let err = mysql::delete(delete::using(quote("posts")))
        .build()
        .unwrap_err();
    assert_eq!(err.to_string(), "query is missing the table of a DELETE");
}
