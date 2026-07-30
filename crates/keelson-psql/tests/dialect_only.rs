//! The parts of the port no recorded fixture reaches.
//!
//! bob's `select_test.go` exercises twenty queries; the PostgreSQL-only
//! operators, the frame bounds, the lock and CTE modifiers and the combined
//! `OFFSET`/`FETCH` are not among them. The expectations here are read off bob's
//! own clause writers rather than off a fixture, so they pin the shape of the
//! port without pretending to be golden.

use keelson_core::Value;
use keelson_psql::{self as psql, fm, sm, wm};

#[test]
fn postgres_only_operators() {
    let q = psql::select((
        sm::from("t"),
        sm::where_(psql::quote("name").ilike(psql::arg("a%"))),
        sm::where_(psql::quote("n").between_symmetric(psql::arg(1), psql::arg(9))),
    ));

    let (sql, args) = q.build().unwrap();
    assert_eq!(
        sql,
        "SELECT \n*\nFROM t\nWHERE (\"name\" ILIKE $1) AND (\"n\" BETWEEN SYMMETRIC $2 AND $3)\n"
    );
    assert_eq!(
        args,
        vec![Value::Text("a%".into()), Value::I32(1), Value::I32(9)]
    );
}

#[test]
fn a_frame_bound_can_bind_an_argument() {
    let q = psql::select((
        sm::columns(
            psql::f("sum", ("x",))
                .apply(fm::over((
                    wm::rows(),
                    wm::from_preceding(psql::arg(3)),
                    wm::to_following(1),
                    wm::exclude_ties(),
                )))
                .expr(),
        ),
        sm::from("t"),
    ));

    let (sql, args) = q.build().unwrap();
    assert_eq!(
        sql,
        "SELECT \nsum(x)OVER ( ROWS BETWEEN $1 PRECEDING AND 1 FOLLOWING EXCLUDE TIES)\nFROM t\n"
    );
    // bob passes a stale `start` when writing a frame, and would number this
    // wrong; keelson's writer owns the counter, so it cannot.
    assert_eq!(args, vec![Value::I32(3)]);
}

#[test]
fn fetch_and_locks() {
    let q = psql::select((
        sm::from("t"),
        sm::fetch(10, true),
        sm::for_share(("a", "b")).no_wait(),
    ));

    let (sql, _) = q.build().unwrap();
    assert_eq!(
        sql,
        "SELECT \n*\nFROM t\nFETCH NEXT 10 ROWS WITH TIES\nFOR SHARE OF a, b NOWAIT\n"
    );
}

#[test]
fn a_recursive_cte_with_every_modifier() {
    let q = psql::select((
        sm::recursive(true),
        sm::with("c", "id")
            .as_(psql::select(sm::from("t")))
            .not_materialized()
            .search_depth("ord", ("id",))
            .cycle("is_cycle", "path", ("id",)),
        sm::from("c"),
    ));

    let (sql, _) = q.build().unwrap();
    // The missing separator between the `WITH` clause and `SELECT` is bob's.
    assert_eq!(
        sql,
        "\nWITH RECURSIVE\nc(id) AS NOT MATERIALIZED (SELECT \n*\nFROM t\n)\nSEARCH DEPTH FIRST BY id SET ord\nCYCLE id SET is_cycle USING pathSELECT \n*\nFROM c\n"
    );
}

#[test]
fn grouping_joins_and_combined_tail_clauses() {
    let q = psql::select((
        sm::from("t"),
        sm::inner_join("u").on_eq(psql::quote("t.id"), psql::quote("u.id")),
        sm::group_by("a"),
        sm::group_by_distinct(true),
        sm::having(psql::quote("a").gt(1)),
        sm::offset_combined(5),
        sm::fetch_combined(2, false),
    ));

    let (sql, _) = q.build().unwrap();
    assert_eq!(
        sql,
        "SELECT \n*\nFROM t\nINNER JOIN u ON (\"t.id\" = \"u.id\")\nGROUP BY DISTINCT a\nHAVING (\"a\" > 1)\nOFFSET 5\nFETCH NEXT 2 ROWS ONLY\n"
    );
}

/// The two ways the design doc writes a conditional clause.
#[test]
fn mods_apply_declaratively_and_after_the_fact() {
    let admin = false;

    let declarative = psql::select((
        sm::from("projects"),
        (!admin).then(|| sm::where_(psql::quote("user_id").eq(psql::arg(7)))),
    ));

    let mut after = psql::select(sm::from("projects"));
    if !admin {
        after.apply(sm::where_(psql::quote("user_id").eq(psql::arg(7))));
    }

    assert_eq!(declarative.build().unwrap(), after.build().unwrap());
    assert_eq!(
        declarative.build().unwrap().0,
        "SELECT \n*\nFROM projects\nWHERE (\"user_id\" = $1)\n"
    );
}

/// A raw string is an expression, so hand-written SQL goes anywhere.
#[test]
fn a_string_is_a_condition() {
    let q = psql::select((sm::from("t"), sm::where_("id = 1")));
    assert_eq!(q.build().unwrap().0, "SELECT \n*\nFROM t\nWHERE id = 1\n");
}

/// A sub-query is parenthesised; a CTE body and a `UNION` operand are not,
/// because those clauses supply the parentheses themselves.
#[test]
fn a_sub_query_in_an_expression_is_parenthesised() {
    let inner = psql::select((sm::columns("id"), sm::from("admins")));
    let q = psql::select((sm::from("users"), sm::where_(psql::quote("id").in_(inner))));

    assert_eq!(
        q.build().unwrap().0,
        "SELECT \n*\nFROM users\nWHERE (\"id\" IN ((SELECT \nid\nFROM admins\n)))\n"
    );
}

/// Build mods run on every build, against a clone of the query.
#[test]
fn build_mods_run_at_build_time() {
    use std::sync::Arc;

    use keelson_core::{BuildMod, Result};
    use keelson_psql::SelectQuery;

    #[derive(Debug)]
    struct UseSchema(&'static str);

    impl BuildMod<SelectQuery> for UseSchema {
        fn apply(&self, q: &mut SelectQuery) -> Result<()> {
            q.from.expression = Some(keelson_core::dyn_expr(psql::quote(self.0).eq("users")));
            Ok(())
        }
    }

    let q = psql::select((
        sm::from("users"),
        sm::build_mod(Arc::new(UseSchema("public"))),
    ));

    // Twice, to prove the mod did not consume itself.
    assert_eq!(q.build().unwrap().0, q.build().unwrap().0);
    assert_eq!(
        q.build().unwrap().0,
        "SELECT \n*\nFROM (\"public\" = users)\n"
    );
}
