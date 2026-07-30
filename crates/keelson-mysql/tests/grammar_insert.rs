//! A walk through <https://dev.mysql.com/doc/refman/8.4/en/insert.html> and
//! <https://dev.mysql.com/doc/refman/8.4/en/replace.html>. See `tests/common/mod.rs`
//! for what each assertion runs and where the expected strings come from.

mod common;

use common::{check, check_shape_only, check_without_engine};
use keelson_mysql as mysql;
use keelson_mysql::{
    Chain, Query, Value, arg, args, insert, quote, raw, replace, s, select, values_of,
};

// ---------------------------------------------------------------------------
// The row sources
// ---------------------------------------------------------------------------

#[test]
fn insert_into_with_a_column_list_and_one_row() {
    let q = mysql::insert((
        insert::into(quote("tags")).columns(["id", "name"]),
        insert::values((arg(1i32), arg("rust"))),
    ));
    let args = check(&q, "INSERT INTO `tags` (`id`, `name`) VALUES (?, ?)");
    assert_eq!(args, vec![Value::I32(1), Value::Text("rust".into())]);
}

/// bob renders the same shape as `INSERT INTO films VALUES (?, ?), (?, ?)`: one
/// `VALUES` keyword and a comma between the rows.
#[test]
fn several_values_mods_become_several_rows_under_one_keyword() {
    let q = mysql::insert((
        insert::into(quote("tags")).columns(["id", "name"]),
        insert::values((arg(1i32), arg("rust"))),
        insert::values((arg(2i32), arg("sql"))),
    ));
    let args = check(
        &q,
        "INSERT INTO `tags` (`id`, `name`) VALUES (?, ?), (?, ?)",
    );
    assert_eq!(args.len(), 4);
    assert_eq!(args[2], Value::I32(2));
}

#[test]
fn rows_adds_the_same_rows_in_one_mod() {
    let q = mysql::insert((
        insert::into(quote("tags")).columns(["id", "name"]),
        insert::rows([(arg(1i32), arg("a")), (arg(2i32), arg("b"))]),
    ));
    let args = check(
        &q,
        "INSERT INTO `tags` (`id`, `name`) VALUES (?, ?), (?, ?)",
    );
    assert_eq!(args.len(), 4);
}

#[test]
fn args_fills_a_whole_row_from_one_iterator() {
    let q = mysql::insert((
        insert::into(quote("tags")).columns(["id", "name"]),
        insert::values(args([1i32, 2])),
    ));
    let args = check(&q, "INSERT INTO `tags` (`id`, `name`) VALUES (?, ?)");
    assert_eq!(args, vec![Value::I32(1), Value::I32(2)]);
}

/// A cell may be `DEFAULT`, which is raw SQL rather than a value.
#[test]
fn a_default_cell_is_raw_sql() {
    let q = mysql::insert((
        insert::into(quote("posts")).columns(["id", "user_id", "title", "views"]),
        insert::values((arg(1i32), arg(2i32), arg("hello"), raw("DEFAULT"))),
    ));
    let args = check(
        &q,
        "INSERT INTO `posts` (`id`, `user_id`, `title`, `views`) VALUES (?, ?, ?, DEFAULT)",
    );
    assert_eq!(args.len(), 3);
}

/// With no row source at all MySQL's spelling of "take every default" is
/// `VALUES ()`, where PostgreSQL writes `DEFAULT VALUES`. The `INSERT` writes it
/// itself, because an absent [`Values`] has to render nothing.
#[test]
fn an_insert_with_no_row_source_takes_every_default() {
    let q = mysql::insert(insert::into(quote("users")));
    assert!(check(&q, "INSERT INTO `users` VALUES ()").is_empty());
}

/// `INSERT … SET assignment_list` is the third row source.
#[test]
fn the_set_row_source() {
    let q = mysql::insert((
        insert::into(quote("tags")),
        insert::set_col("id").to_arg(1i32),
        insert::set_col("name").to_arg("rust"),
    ));
    let args = check(&q, "INSERT INTO `tags` SET `id` = ?, `name` = ?");
    assert_eq!(args, vec![Value::I32(1), Value::Text("rust".into())]);
}

/// `SET` and `VALUES` are alternatives in the grammar, so one has to win. `SET`
/// does — a statement is never rendered half one and half the other.
///
/// The column list goes with the discarded `VALUES`: `INSERT INTO t (a) SET b = 1`
/// is a syntax error, because the `SET` production has no `(col_name, …)`. Real
/// MySQL is what caught that here.
#[test]
fn set_wins_over_values_and_takes_the_column_list_with_it() {
    let q = mysql::insert((
        insert::into(quote("tags")).columns(["id"]),
        insert::values(arg(1i32)),
        insert::set_col("name").to_arg("rust"),
    ));
    let args = check(&q, "INSERT INTO `tags` SET `name` = ?");
    // The discarded row's argument is discarded with it.
    assert_eq!(args, vec![Value::Text("rust".into())]);
}

#[test]
fn insert_from_a_select() {
    let source = mysql::select((
        select::columns((quote("id"), quote("title"))),
        select::from(quote("posts")),
        select::where_(quote("views").gt(arg(100i32))),
    ));
    let q = mysql::insert((
        insert::into(quote("tags")).columns(["id", "name"]),
        insert::query(source),
    ));
    let args = check(
        &q,
        "INSERT INTO `tags` (`id`, `name`) \
         SELECT `id`, `title` FROM `posts` WHERE (`views` > ?)",
    );
    assert_eq!(args, vec![Value::I32(100)]);
}

/// MySQL permits a CTE only *immediately before the `SELECT`* of an
/// `INSERT … SELECT` (*15.2.20*), never in front of the `INSERT` — which is why
/// there is no `insert::with` and the `WITH` goes on the sub-query.
#[test]
fn a_cte_for_an_insert_select_belongs_to_the_select() {
    let source = mysql::select((
        select::with(
            "popular",
            mysql::select((
                select::columns((quote("id"), quote("title"))),
                select::from(quote("posts")),
                select::where_(quote("views").gt(arg(100i32))),
            )),
        ),
        select::columns((quote("id"), quote("title"))),
        select::from(quote("popular")),
    ));
    let q = mysql::insert((
        insert::into(quote("tags")).columns(["id", "name"]),
        insert::query(source),
    ));
    check(
        &q,
        "INSERT INTO `tags` (`id`, `name`) \
         WITH `popular` AS (SELECT `id`, `title` FROM `posts` WHERE (`views` > ?)) \
         SELECT `id`, `title` FROM `popular`",
    );
}

#[test]
fn a_query_row_source_replaces_any_rows_already_added() {
    let source = mysql::select((select::columns(quote("id")), select::from(quote("users"))));
    let q = mysql::insert((
        insert::into(quote("tags")).columns(["id"]),
        insert::values(arg(1i32)),
        insert::query(source),
    ));
    let args = check(&q, "INSERT INTO `tags` (`id`) SELECT `id` FROM `users`");
    assert!(args.is_empty(), "the replaced row's argument went with it");
}

// ---------------------------------------------------------------------------
// Modifiers and hints
// ---------------------------------------------------------------------------

/// `INSERT [LOW_PRIORITY | DELAYED | HIGH_PRIORITY] [IGNORE]` — the priority
/// keyword precedes `IGNORE` whichever order the mods were written in.
#[test]
fn insert_ignore_follows_the_priority_keyword() {
    let q = mysql::insert((
        insert::ignore(),
        insert::high_priority(),
        insert::into(quote("tags")).columns(["id", "name"]),
        insert::values((arg(1i32), arg("rust"))),
    ));
    check(
        &q,
        "INSERT HIGH_PRIORITY IGNORE INTO `tags` (`id`, `name`) VALUES (?, ?)",
    );
}

#[test]
fn insert_ignore_on_its_own() {
    let q = mysql::insert((
        insert::ignore(),
        insert::into(quote("tags")).columns(["id", "name"]),
        insert::values((arg(1i32), arg("rust"))),
    ));
    check(&q, "INSERT IGNORE INTO `tags` (`id`, `name`) VALUES (?, ?)");
}

#[test]
fn insert_low_priority() {
    let q = mysql::insert((
        insert::low_priority(),
        insert::into(quote("tags")).columns(["id"]),
        insert::values(arg(1i32)),
    ));
    check(&q, "INSERT LOW_PRIORITY INTO `tags` (`id`) VALUES (?)");
}

/// `DELAYED` is accepted for backward compatibility and warns; it still parses and
/// prepares, which is what the judge asks about.
#[test]
fn insert_delayed_is_still_in_the_grammar() {
    let q = mysql::insert((
        insert::delayed(),
        insert::into(quote("tags")).columns(["id"]),
        insert::values(arg(1i32)),
    ));
    check(&q, "INSERT DELAYED INTO `tags` (`id`) VALUES (?)");
}

/// bob renders the same construct as `INSERT /*+ … */ INTO films …`: the hint
/// comment sits between `INSERT` and `INTO`.
#[test]
fn an_optimizer_hint_sits_between_insert_and_into() {
    let q = mysql::insert((
        insert::max_execution_time(1000),
        insert::set_var("cte_max_recursion_depth = 1M"),
        insert::into(quote("tags")).columns(["id"]),
        insert::values(arg(1i32)),
    ));
    check(
        &q,
        "INSERT /*+ MAX_EXECUTION_TIME(1000) SET_VAR(cte_max_recursion_depth = 1M) */ \
         INTO `tags` (`id`) VALUES (?)",
    );
}

/// `INSERT [INTO] tbl_name [PARTITION (…)]` — the partition list follows the table.
///
/// The engine cannot be asked: MySQL refuses `PARTITION` on a non-partitioned table,
/// and refuses to partition a table that takes part in a foreign key, which every
/// table in the shared schema does.
#[test]
fn a_partition_list_follows_the_insert_target() {
    let q = mysql::insert((
        insert::into(quote("tags")).partition(["p0"]),
        insert::values((arg(1i32), arg("rust"))),
    ));
    check_without_engine(
        &q,
        "INSERT INTO `tags` PARTITION (`p0`) VALUES (?, ?)",
        "the shared schema has no partitioned table and cannot have one",
    );
}

/// `INSERT [INTO] tbl_name [PARTITION (…)] [(col_name, …)]` — and the partition list
/// precedes the column list, which is the ordering worth pinning.
#[test]
fn a_partition_list_precedes_the_insert_column_list() {
    let q = mysql::insert((
        insert::into(quote("tags"))
            .partition(["p0"])
            .columns(["id"]),
        insert::values(arg(1i32)),
    ));
    check_shape_only(
        &q,
        "INSERT INTO `tags` PARTITION (`p0`) (`id`) VALUES (?)",
        "PARTITION followed by a column list on INSERT",
        "the shared schema has no partitioned table and cannot have one",
    );
}

// ---------------------------------------------------------------------------
// ON DUPLICATE KEY UPDATE
// ---------------------------------------------------------------------------

/// The pre-8.0.19 upsert. bob renders it as
/// ``ON DUPLICATE KEY UPDATE `did` = VALUES(`did`), `dbname` = VALUES(`dbname`)``.
#[test]
fn on_duplicate_key_update_with_the_values_function() {
    let q = mysql::insert((
        insert::into(quote("tags")).columns(["id", "name"]),
        insert::values((arg(1i32), arg("rust"))),
        insert::on_duplicate_key_update(insert::set_values(["name"])),
    ));
    let args = check(
        &q,
        "INSERT INTO `tags` (`id`, `name`) VALUES (?, ?) \
         ON DUPLICATE KEY UPDATE `name` = VALUES(`name`)",
    );
    assert_eq!(args.len(), 2);
}

/// The 8.0.19 upsert: `AS row_alias` after the rows, then the alias in the
/// assignments. bob renders the same as
/// ``AS new ON DUPLICATE KEY UPDATE `did` = `new`.`did` ``, quoting all four names.
#[test]
fn on_duplicate_key_update_through_a_row_alias() {
    let q = mysql::insert((
        insert::into(quote("tags")).columns(["id", "name"]),
        insert::values((arg(1i32), arg("rust"))),
        insert::as_("new"),
        insert::on_duplicate_key_update(insert::set_row("new", ["name"])),
    ));
    check(
        &q,
        "INSERT INTO `tags` (`id`, `name`) VALUES (?, ?) AS `new` \
         ON DUPLICATE KEY UPDATE `name` = `new`.`name`",
    );
}

#[test]
fn a_row_alias_may_rename_the_columns_too() {
    let q = mysql::insert((
        insert::into(quote("tags")).columns(["id", "name"]),
        insert::values((arg(1i32), arg("rust"))),
        insert::as_("new").columns(["new_id", "new_name"]),
        insert::on_duplicate_key_update(insert::set_col("name").to(quote(("new", "new_name")))),
    ));
    check(
        &q,
        "INSERT INTO `tags` (`id`, `name`) VALUES (?, ?) AS `new` (`new_id`, `new_name`) \
         ON DUPLICATE KEY UPDATE `name` = `new`.`new_name`",
    );
}

#[test]
fn an_upsert_assignment_may_be_any_expression() {
    let q = mysql::insert((
        insert::into(quote("posts")).columns(["id", "user_id", "title", "views"]),
        insert::values((arg(1i32), arg(2i32), arg("hello"), arg(0i32))),
        insert::on_duplicate_key_update((
            insert::set_col("views").to(quote("views").plus(values_of("views"))),
            insert::set_col("title").to_arg("updated"),
        )),
    ));
    let args = check(
        &q,
        "INSERT INTO `posts` (`id`, `user_id`, `title`, `views`) VALUES (?, ?, ?, ?) \
         ON DUPLICATE KEY UPDATE `views` = (`views` + VALUES(`views`)), `title` = ?",
    );
    assert_eq!(args.len(), 5);
    assert_eq!(args[4], Value::Text("updated".into()));
}

#[test]
fn an_upsert_on_top_of_an_insert_select() {
    let source = mysql::select((
        select::columns((quote("id"), quote("title"))),
        select::from(quote("posts")),
    ));
    let q = mysql::insert((
        insert::into(quote("tags")).columns(["id", "name"]),
        insert::query(source),
        insert::on_duplicate_key_update(insert::set_col("name").to(s("clash"))),
    ));
    check(
        &q,
        "INSERT INTO `tags` (`id`, `name`) SELECT `id`, `title` FROM `posts` \
         ON DUPLICATE KEY UPDATE `name` = 'clash'",
    );
}

// ---------------------------------------------------------------------------
// The incomplete statements
// ---------------------------------------------------------------------------

#[test]
fn an_insert_with_no_table_is_a_recorded_failure() {
    let err = mysql::insert(insert::values(arg(1i32)))
        .build()
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "query is missing the target table of an INSERT"
    );
}

// ---------------------------------------------------------------------------
// REPLACE
// ---------------------------------------------------------------------------

#[test]
fn replace_into_with_a_column_list_and_rows() {
    let q = mysql::replace((
        replace::into(quote("tags")).columns(["id", "name"]),
        replace::values((arg(1i32), arg("rust"))),
        replace::values((arg(2i32), arg("sql"))),
    ));
    let args = check(
        &q,
        "REPLACE INTO `tags` (`id`, `name`) VALUES (?, ?), (?, ?)",
    );
    assert_eq!(args.len(), 4);
}

/// `REPLACE [LOW_PRIORITY | DELAYED] [INTO] …` — the only two modifiers it has.
#[test]
fn replace_low_priority_with_the_set_row_source() {
    let q = mysql::replace((
        replace::low_priority(),
        replace::into(quote("tags")),
        replace::set_col("id").to_arg(1i32),
        replace::set_col("name").to_arg("rust"),
    ));
    check(
        &q,
        "REPLACE LOW_PRIORITY INTO `tags` SET `id` = ?, `name` = ?",
    );
}

#[test]
fn replace_delayed() {
    let q = mysql::replace((
        replace::delayed(),
        replace::into(quote("tags")).columns(["id"]),
        replace::values(arg(1i32)),
    ));
    check(&q, "REPLACE DELAYED INTO `tags` (`id`) VALUES (?)");
}

#[test]
fn replace_from_a_select() {
    let source = mysql::select((
        select::columns((quote("id"), quote("title"))),
        select::from(quote("posts")),
    ));
    let q = mysql::replace((
        replace::into(quote("tags")).columns(["id", "name"]),
        replace::query(source),
    ));
    check(
        &q,
        "REPLACE INTO `tags` (`id`, `name`) SELECT `id`, `title` FROM `posts`",
    );
}

#[test]
fn replace_with_an_optimizer_hint() {
    let q = mysql::replace((
        replace::optimizer_hint("SET_VAR(foreign_key_checks = OFF)"),
        replace::into(quote("tags")).columns(["id"]),
        replace::values(arg(1i32)),
    ));
    check(
        &q,
        "REPLACE /*+ SET_VAR(foreign_key_checks = OFF) */ INTO `tags` (`id`) VALUES (?)",
    );
}

#[test]
fn a_replace_with_no_row_source_takes_every_default() {
    let q = mysql::replace(replace::into(quote("users")));
    check(&q, "REPLACE INTO `users` VALUES ()");
}

#[test]
fn a_replace_with_no_table_is_a_recorded_failure() {
    let err = mysql::replace(replace::values(arg(1i32)))
        .build()
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "query is missing the target table of a REPLACE"
    );
}
