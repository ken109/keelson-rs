//! One test per recorded `psql` `SELECT` case.
//!
//! Each builds the keelson equivalent of the query in bob's
//! `dialect/psql/select_test.go` and compares against what bob actually emitted.
//! The case names are bob's test-table keys, so a failure names the Go test it
//! came from.

use keelson_core::Expression;
use keelson_psql::{self as psql, Query, fm, sm, wm};

/// Build a query and assert it against its recorded case.
#[track_caller]
fn check<Q: Expression>(name: &str, query: Query<Q>) {
    let (sql, args) = query.build().expect("query should build");
    let args: Vec<serde_json::Value> = args
        .iter()
        .map(|v| serde_json::to_value(v).expect("a Value should serialise"))
        .collect();

    keelson_golden::assert_case("psql", name, &sql, &args);
}

#[test]
fn simple_select() {
    check(
        "simple select",
        psql::select((
            sm::columns(("id", "name")),
            sm::from("users"),
            sm::where_(psql::quote("id").in_(psql::args([100, 200, 300]))),
        )),
    );
}

#[test]
fn case_with_else() {
    check(
        "case with else",
        psql::select((
            sm::columns((
                "id",
                "name",
                psql::case_()
                    .when(psql::quote("id").eq(psql::s("1")), psql::s("A"))
                    .else_(psql::s("B"))
                    .as_("C"),
            )),
            sm::from("users"),
        )),
    );
}

#[test]
fn case_without_else() {
    check(
        "case without else",
        psql::select((
            sm::columns((
                "id",
                "name",
                psql::case_()
                    .when(psql::quote("id").eq(psql::s("1")), psql::s("A"))
                    .end()
                    .as_("C"),
            )),
            sm::from("users"),
        )),
    );
}

#[test]
fn select_distinct() {
    check(
        "select distinct",
        psql::select((
            sm::columns(("id", "name")),
            sm::distinct(()),
            sm::from("users"),
            sm::where_(psql::quote("id").in_(psql::args([100, 200, 300]))),
        )),
    );
}

#[test]
fn select_distinct_on() {
    check(
        "select distinct on",
        psql::select((
            sm::columns(("id", "name")),
            sm::distinct(("id",)),
            sm::from("users"),
            sm::where_(psql::quote("id").in_(psql::args([100, 200, 300]))),
        )),
    );
}

#[test]
fn select_from_function() {
    check(
        "select from function",
        psql::select(
            sm::from(psql::f("generate_series", (1, 3)))
                .as_("x")
                .columns(("p", "q", "s")),
        ),
    );
}

#[test]
fn with_rows_from() {
    check(
        "with rows from",
        psql::select((
            sm::from_function([
                psql::f(
                    "json_to_recordset",
                    (psql::arg(r#"[{"a":40,"b":"foo"},{"a":"100","b":"bar"}]"#),),
                )
                .apply((fm::columns("a", "INTEGER"), fm::columns("b", "TEXT"))),
                psql::f("generate_series", (1, 3)),
            ])
            .as_("x")
            .columns(("p", "q", "s")),
            sm::order_by("p"),
        )),
    );
}

#[test]
fn with_sub_select_and_window() {
    let difference = psql::f("LEAD", ("created_date", 1, psql::f("NOW", ())))
        .apply(fm::over((
            wm::partition_by("presale_id"),
            wm::order_by("created_date"),
        )))
        .expr()
        .minus(psql::quote("created_date"))
        .as_("difference");

    check(
        "with sub-select and window",
        psql::select((
            sm::columns(("status", psql::f("avg", ("difference",)))),
            sm::from(psql::select((
                sm::columns(("status", difference)),
                sm::from("presales_presalestatus"),
            )))
            .as_("differnce_by_status"),
            sm::where_(psql::quote("status").in_((psql::s("A"), psql::s("B"), psql::s("C")))),
            sm::group_by("status"),
        )),
    );
}

#[test]
fn select_with_grouped_in() {
    check(
        "select with grouped IN",
        psql::select((
            sm::columns(("id", "name")),
            sm::from("users"),
            sm::where_(
                psql::group((psql::quote("id"), psql::quote("employee_id")))
                    .in_((psql::arg_group([100, 200]), psql::arg_group([300, 400]))),
            ),
        )),
    );
}

#[test]
fn simple_limit_offset_arg() {
    check(
        "simple limit offset arg",
        psql::select((
            sm::columns(("id", "name")),
            sm::from("users"),
            sm::offset(psql::arg(15)),
            sm::limit(psql::arg(10)),
        )),
    );
}

#[test]
fn join_using() {
    check(
        "join using",
        psql::select((
            sm::columns("id"),
            sm::from("test1"),
            sm::left_join("test2").using("id"),
        )),
    );
}

#[test]
fn cte_with_column_aliases() {
    check(
        "CTE with column aliases",
        psql::select((
            sm::with("c", ("id", "data")).as_(psql::select((
                sm::columns("id"),
                sm::from("test1"),
                sm::left_join("test2").using("id"),
            ))),
            sm::from("c"),
        )),
    );
}

#[test]
fn window_function_over_empty_frame() {
    check(
        "Window function over empty frame",
        psql::select((
            sm::columns(psql::f("row_number", ()).apply(fm::over(()))),
            sm::from("c"),
        )),
    );
}

#[test]
fn window_function_over_window_name() {
    check(
        "Window function over window name",
        psql::select((
            sm::columns(psql::f("avg", ("salary",)).apply(fm::over(wm::based_on("w")))),
            sm::from("c"),
            sm::window("w", (wm::partition_by("depname"), wm::order_by("salary"))),
        )),
    );
}

#[test]
fn select_with_order_by_and_collate() {
    check(
        "select with order by and collate",
        psql::select((
            sm::columns(("id", "name")),
            sm::from("users"),
            sm::order_by("name").collate("bg-BG-x-icu").asc(),
        )),
    );
}

#[test]
fn with_cross_join() {
    check(
        "with cross join",
        psql::select((
            sm::columns(("id", "name", "type")),
            sm::from("users").as_("u"),
            sm::cross_join(psql::select((
                sm::columns(("id", "type")),
                sm::from("clients"),
                sm::where_(psql::quote("client_id").eq(psql::arg("123"))),
            )))
            .as_("clients"),
            sm::where_(psql::quote("id").eq(psql::arg(100))),
        )),
    );
}

#[test]
fn with_locking() {
    check(
        "with locking",
        psql::select((
            sm::columns(("id", "name")),
            sm::from("users"),
            sm::for_update("users").skip_locked(),
        )),
    );
}

#[test]
fn multiple_unions() {
    check(
        "Multiple Unions",
        psql::select((
            sm::columns(("id", "name")),
            sm::from("users"),
            sm::union(psql::select((
                sm::columns(("id", "name")),
                sm::from("admins"),
            ))),
            sm::union(psql::select((
                sm::columns(("id", "name")),
                sm::from("mods"),
            ))),
        )),
    );
}

#[test]
fn union_with_combined_args() {
    check(
        "Union with combined args",
        psql::select((
            sm::columns(("id", "name")),
            sm::from("users"),
            sm::limit(100),
            sm::order_by("id"),
            sm::union(psql::select((
                sm::columns(("id", "name")),
                sm::from("admins"),
                sm::limit(10),
                sm::order_by("id"),
            ))),
            sm::order_combined("id"),
            sm::limit_combined(1000),
        )),
    );
}

#[test]
fn union_with_uncombined_args() {
    check(
        "Union with uncombined args",
        psql::select((
            sm::columns(("id", "name")),
            sm::from("users"),
            sm::limit(1),
            sm::order_by("id"),
            sm::union(psql::select((
                sm::columns(("id", "name")),
                sm::from("admins"),
                sm::limit(1),
                sm::order_by("id"),
            ))),
        )),
    );
}
