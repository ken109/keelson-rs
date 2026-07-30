//! A walk through <https://dev.mysql.com/doc/refman/8.4/en/update.html>. See
//! `tests/common/mod.rs` for what each assertion runs and where the expected strings
//! come from.

mod common;

use common::{check, check_without_engine, check_without_grammar};
use keelson_mysql as mysql;
use keelson_mysql::{Chain, MysqlOps, Query, Value, arg, quote, s, select, update};

// ---------------------------------------------------------------------------
// The single-table form
// ---------------------------------------------------------------------------

/// bob renders the same construct as ``UPDATE films SET `kind` = ? WHERE (`kind` =
/// ?)``.
#[test]
fn update_one_column_with_a_bound_value() {
    let q = mysql::update((
        update::table(quote("users")),
        update::set_col("age").to_arg(30i32),
        update::where_(quote("id").eq(arg(1i32))),
    ));
    let args = check(&q, "UPDATE `users` SET `age` = ? WHERE (`id` = ?)");
    assert_eq!(args, vec![Value::I32(30), Value::I32(1)]);
}

#[test]
fn several_assignments_are_comma_separated() {
    let q = mysql::update((
        update::table(quote("posts")),
        update::set_col("status").to_arg("published"),
        update::set_col("views").to(quote("views").plus(1i32)),
        update::where_(quote("id").eq(arg(1i32))),
    ));
    let args = check(
        &q,
        "UPDATE `posts` SET `status` = ?, `views` = (`views` + 1) WHERE (`id` = ?)",
    );
    assert_eq!(args.len(), 2);
}

/// [`update::set`] takes the assignment already written out, for the shapes the
/// column/value pair cannot reach.
///
/// Note what it must *not* be handed: `set(quote("a").eq(s("b")))` renders
/// ``SET (`a` = 'b')``, because every chain operator parenthesises its own result and
/// an assignment list has no room for parentheses. [`update::set_col`] is the route
/// for a plain assignment, and this mod is for raw SQL and for expressions built with
/// [`Expr::binary`](keelson_mysql::Expr::binary), which does not group.
#[test]
fn set_takes_a_written_out_assignment() {
    use keelson_mysql::{Expr, raw};

    let q = mysql::update((
        update::table(quote("posts")).as_("p"),
        update::set(Expr::binary(quote(("p", "status")), "=", s("draft"))),
        update::set(raw("`p`.`views` = `p`.`views` + 1")),
        update::where_(quote(("p", "views")).lt(arg(10i32))),
    ));
    check(
        &q,
        "UPDATE `posts` AS `p` SET `p`.`status` = 'draft', `p`.`views` = `p`.`views` + 1 \
         WHERE (`p`.`views` < ?)",
    );
}

#[test]
fn the_table_may_be_aliased() {
    let q = mysql::update((
        update::table(quote("users")).as_("u"),
        update::set_col(("u", "age")).to_arg(31i32),
        update::where_(quote(("u", "id")).eq(arg(1i32))),
    ));
    check(
        &q,
        "UPDATE `users` AS `u` SET `u`.`age` = ? WHERE (`u`.`id` = ?)",
    );
}

/// `[WHERE …] [ORDER BY …] [LIMIT row_count]` — the single-table form only, and the
/// `LIMIT` takes a row count with no `OFFSET`.
#[test]
fn order_by_and_limit_bound_the_rows_touched() {
    let q = mysql::update((
        update::table(quote("users")),
        update::set_col("is_active").to_arg(false),
        update::where_(quote("age").lt(arg(18i32))),
        update::order_by(quote("id")).desc(),
        update::limit(5),
    ));
    let args = check(
        &q,
        "UPDATE `users` SET `is_active` = ? WHERE (`age` < ?) ORDER BY `id` DESC LIMIT 5",
    );
    assert_eq!(args, vec![Value::Bool(false), Value::I32(18)]);
}

#[test]
fn a_bound_limit_is_prepared_like_any_other_argument() {
    let q = mysql::update((
        update::table(quote("users")),
        update::set_col("age").to_arg(30i32),
        update::order_by(quote("id")),
        update::limit(arg(3i64)),
    ));
    let args = check(&q, "UPDATE `users` SET `age` = ? ORDER BY `id` LIMIT ?");
    assert_eq!(args, vec![Value::I32(30), Value::I64(3)]);
}

// ---------------------------------------------------------------------------
// Modifiers and hints
// ---------------------------------------------------------------------------

/// `UPDATE [LOW_PRIORITY] [IGNORE] …`.
///
/// `sqlparser` stops at `IGNORE`, which the manual puts right there; the server
/// accepts it.
#[test]
fn low_priority_and_ignore_in_grammar_order() {
    let q = mysql::update((
        update::ignore(),
        update::low_priority(),
        update::table(quote("users")),
        update::set_col("age").to_arg(30i32),
    ));
    check_without_grammar(
        &q,
        "UPDATE LOW_PRIORITY IGNORE `users` SET `age` = ?",
        "the UPDATE … IGNORE modifier",
    );
}

#[test]
fn an_optimizer_hint_sits_between_update_and_the_table() {
    let q = mysql::update((
        update::max_execution_time(1000),
        update::table(quote("users")),
        update::set_col("age").to_arg(30i32),
    ));
    check(
        &q,
        "UPDATE /*+ MAX_EXECUTION_TIME(1000) */ `users` SET `age` = ?",
    );
}

/// `UPDATE tbl_name [PARTITION (…)] [[AS] alias]` — the partition list precedes the
/// alias here, unlike in `DELETE`. The engine cannot be asked: MySQL refuses
/// `PARTITION` on a non-partitioned table, and refuses to partition a table that
/// takes part in a foreign key.
#[test]
fn a_partition_list_precedes_the_update_alias() {
    let q = mysql::update((
        update::table(quote("users")).partition(["p0"]).as_("u"),
        update::set_col(("u", "age")).to_arg(30i32),
    ));
    check_without_engine(
        &q,
        "UPDATE `users` PARTITION (`p0`) AS `u` SET `u`.`age` = ?",
        "the shared schema has no partitioned table and cannot have one",
    );
}

// ---------------------------------------------------------------------------
// The multiple-table form
// ---------------------------------------------------------------------------

/// MySQL has no `UPDATE … FROM`: the target *is* a `table_references`, so a comma
/// list is how two tables are updated at once. bob reaches the same SQL by passing
/// the whole list as one raw string; [`update::table_also`] is the structured way.
///
/// This is the construct the task brief names as `sqlparser`'s worst false negative
/// — it insists on `SET` immediately after the first table.
#[test]
fn two_tables_updated_through_a_comma_list() {
    let q = mysql::update((
        update::table(quote("users")).as_("u"),
        update::table_also(quote("posts")).as_("p"),
        update::set_col(("p", "views")).to_arg(0i32),
        update::where_(quote(("u", "id")).eq(quote(("p", "user_id")))),
    ));
    let args = check_without_grammar(
        &q,
        "UPDATE `users` AS `u`, `posts` AS `p` SET `p`.`views` = ? \
         WHERE (`u`.`id` = `p`.`user_id`)",
        "multiple-table UPDATE through a comma list",
    );
    assert_eq!(args, vec![Value::I32(0)]);
}

/// The same statement written as a join. `HasJoins` reaches the *target's* joins,
/// because in MySQL there is nowhere else for them to go. bob's expected SQL for
/// this shape is
/// ``UPDATE `table1` AS `T1` LEFT JOIN `table2` AS `T2` ON (…) SET …``.
#[test]
fn two_tables_updated_through_a_join() {
    let q = mysql::update((
        update::table(quote("posts")).as_("p"),
        update::inner_join(quote("users"))
            .as_("u")
            .on_eq(quote(("u", "id")), quote(("p", "user_id"))),
        update::set_col(("p", "views")).to_arg(0i32),
        update::where_(quote(("u", "is_active")).eq(arg(true))),
    ));
    let args = check(
        &q,
        "UPDATE `posts` AS `p` INNER JOIN `users` AS `u` ON (`u`.`id` = `p`.`user_id`) \
         SET `p`.`views` = ? WHERE (`u`.`is_active` = ?)",
    );
    assert_eq!(args, vec![Value::I32(0), Value::Bool(true)]);
}

#[test]
fn a_left_join_target_updates_only_the_matched_side() {
    let q = mysql::update((
        update::table(quote("users")).as_("u"),
        update::left_join(quote("posts"))
            .as_("p")
            .on_eq(quote(("p", "user_id")), quote(("u", "id"))),
        update::set_col(("u", "is_active")).to_arg(false),
        update::where_(quote(("p", "id")).is_null()),
    ));
    check(
        &q,
        "UPDATE `users` AS `u` LEFT JOIN `posts` AS `p` ON (`p`.`user_id` = `u`.`id`) \
         SET `u`.`is_active` = ? WHERE (`p`.`id` IS NULL)",
    );
}

#[test]
fn a_straight_join_target_fixes_the_read_order() {
    let q = mysql::update((
        update::table(quote("users")).as_("u"),
        update::straight_join(quote("posts"))
            .as_("p")
            .on_eq(quote(("p", "user_id")), quote(("u", "id"))),
        update::set_col(("p", "status")).to_arg("stale"),
    ));
    check(
        &q,
        "UPDATE `users` AS `u` STRAIGHT_JOIN `posts` AS `p` ON (`p`.`user_id` = `u`.`id`) \
         SET `p`.`status` = ?",
    );
}

#[test]
fn a_cross_join_target_may_carry_a_condition() {
    let q = mysql::update((
        update::table(quote("users")).as_("u"),
        update::cross_join(quote("posts"))
            .as_("p")
            .on_eq(quote(("p", "user_id")), quote(("u", "id"))),
        update::set_col(("u", "age")).to_arg(40i32),
    ));
    check(
        &q,
        "UPDATE `users` AS `u` CROSS JOIN `posts` AS `p` ON (`p`.`user_id` = `u`.`id`) \
         SET `u`.`age` = ?",
    );
}

#[test]
fn an_index_hint_on_the_updated_table() {
    let q = mysql::update((
        update::table(quote("users"))
            .as_("u")
            .force_index(["PRIMARY"]),
        update::set_col(("u", "age")).to_arg(30i32),
        update::where_(quote(("u", "id")).eq(arg(1i32))),
    ));
    check(
        &q,
        "UPDATE `users` AS `u` FORCE INDEX (`PRIMARY`) SET `u`.`age` = ? WHERE (`u`.`id` = ?)",
    );
}

// ---------------------------------------------------------------------------
// WITH, sub-queries and operators
// ---------------------------------------------------------------------------

/// MySQL permits a `WITH` at the beginning of an `UPDATE` — unlike `INSERT`, where it
/// may only precede the sub-`SELECT`.
#[test]
fn a_cte_in_front_of_an_update() {
    let popular = mysql::select((
        select::columns(quote("id")),
        select::from(quote("posts")),
        select::where_(quote("views").gt(arg(100i32))),
    ));
    let ids = mysql::select((select::columns(quote("id")), select::from(quote("popular"))));
    let q = mysql::update((
        update::with("popular", popular),
        update::table(quote("posts")),
        update::set_col("status").to_arg("hot"),
        update::where_(quote("id").in_(ids)),
    ));
    let args = check(
        &q,
        "WITH `popular` AS (SELECT `id` FROM `posts` WHERE (`views` > ?)) \
         UPDATE `posts` SET `status` = ? WHERE (`id` IN (SELECT `id` FROM `popular`))",
    );
    assert_eq!(
        args,
        vec![Value::I32(100), Value::Text("hot".into())],
        "the CTE's argument is bound before the SET's, as it is written"
    );
}

#[test]
fn a_recursive_cte_in_front_of_an_update() {
    use keelson_mysql::raw;

    let ids = mysql::select((select::columns(quote("n")), select::from(quote("counter"))));
    let q = mysql::update((
        update::recursive(true),
        update::with(
            "counter",
            raw("SELECT 1 AS `n` UNION ALL SELECT `n` + 1 FROM `counter` WHERE `n` < 3"),
        )
        .columns(["n"]),
        update::table(quote("posts")),
        update::set_col("status").to_arg("seeded"),
        update::where_(quote("id").in_(ids)),
    ));
    check(
        &q,
        "WITH RECURSIVE `counter` (`n`) AS \
         (SELECT 1 AS `n` UNION ALL SELECT `n` + 1 FROM `counter` WHERE `n` < 3) \
         UPDATE `posts` SET `status` = ? WHERE (`id` IN (SELECT `n` FROM `counter`))",
    );
}

/// bob's `with sub-select` case for MySQL is this shape:
/// ``UPDATE employees SET `sales_count` = sales_count + 1 WHERE (`id` = (SELECT …))``.
#[test]
fn a_scalar_subquery_on_the_right_of_a_comparison() {
    let newest = mysql::select((
        select::columns(quote("user_id")),
        select::from(quote("posts")),
        select::order_by(quote("id")).desc(),
        select::limit(1),
    ));
    let q = mysql::update((
        update::table(quote("users")),
        update::set_col("is_active").to_arg(true),
        update::where_(quote("id").eq(mysql::subquery(newest))),
    ));
    check(
        &q,
        "UPDATE `users` SET `is_active` = ? \
         WHERE (`id` = (SELECT `user_id` FROM `posts` ORDER BY `id` DESC LIMIT 1))",
    );
}

/// The same `DIV` that `sqlparser` refuses inside a `SELECT` list it happily parses
/// here — the backend is inconsistent as well as wrong, which is the point of not
/// trusting it for this dialect.
#[test]
fn an_assignment_may_use_a_mysql_only_operator() {
    let q = mysql::update((
        update::table(quote("users")),
        update::set_col("age").to(quote("age").div(2i32)),
        update::where_(quote("name").regexp(arg("^a"))),
    ));
    check(
        &q,
        "UPDATE `users` SET `age` = (`age` DIV 2) WHERE (`name` REGEXP ?)",
    );
}

#[test]
fn setting_a_column_to_null_and_to_default() {
    use keelson_mysql::raw;

    let q = mysql::update((
        update::table(quote("users")),
        update::set_col("email").to(raw("NULL")),
        update::set_col("is_active").to(raw("DEFAULT")),
        update::where_(quote("id").eq(arg(1i32))),
    ));
    check(
        &q,
        "UPDATE `users` SET `email` = NULL, `is_active` = DEFAULT WHERE (`id` = ?)",
    );
}

// ---------------------------------------------------------------------------
// The incomplete statements
// ---------------------------------------------------------------------------

#[test]
fn an_update_with_no_table_is_a_recorded_failure() {
    let err = mysql::update(update::set_col("age").to_arg(1i32))
        .build()
        .unwrap_err();
    // The substring names the SQL concept (an UPDATE's table), not the message
    // wording.
    assert!(
        matches!(&err, mysql::Error::Incomplete(what) if what.contains("UPDATE")),
        "got: {err}"
    );
}

/// `UPDATE t` with no `SET` is not a statement, so an empty assignment list is a
/// recorded failure rather than a clause that renders nothing.
#[test]
fn an_update_with_no_assignments_is_a_recorded_failure() {
    let err = mysql::update(update::table(quote("users")))
        .build()
        .unwrap_err();
    // The substrings name the SQL concepts (an UPDATE's assignments), not the
    // message wording.
    assert!(
        matches!(&err, mysql::Error::Incomplete(what)
            if what.contains("assignments") && what.contains("UPDATE")),
        "got: {err}"
    );
}
