//! Proves the primitives compose the way the later layers assume they do.
//!
//! A miniature dialect crate: one query struct made of named clause fields, a
//! `Has*` accessor trait of the kind `#[derive(Clauses)]` will generate, and
//! clause mods generic over it. If this file stops compiling, the shape of the
//! public API has changed under the dialect crates.

use std::sync::Arc;

use keelson_core::{
    BuildMod, Dialect, DynExpr, Error, Expression, Mod, QueryType, Result, SqlWriter, Value, build,
    dyn_expr, mod_fn,
};

#[derive(Debug)]
struct Psql;

impl Dialect for Psql {
    fn write_arg(&self, w: &mut String, position: usize) {
        w.push('$');
        w.push_str(&position.to_string());
    }

    fn write_quoted(&self, w: &mut String, s: &str) {
        w.push('"');
        w.push_str(s);
        w.push('"');
    }
}

#[derive(Debug, Default, Clone)]
struct Where {
    conditions: Vec<DynExpr>,
}

impl Expression for Where {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        w.write_slice(&self.conditions, " WHERE ", " AND ", "")
    }
}

#[derive(Debug, Default, Clone)]
struct Limit {
    count: Option<i64>,
}

impl Expression for Limit {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        if let Some(n) = self.count {
            w.push_str(" LIMIT ");
            w.push_arg(n);
        }
        Ok(())
    }
}

#[derive(Debug, Default, Clone)]
struct SelectQuery {
    table: String,
    where_: Where,
    limit: Limit,
    build_mods: Vec<Arc<dyn BuildMod<SelectQuery>>>,
}

// What `#[derive(Clauses)]` will emit.
trait HasWhere {
    fn where_mut(&mut self) -> &mut Where;
}

trait HasLimit {
    fn limit_mut(&mut self) -> &mut Limit;
}

impl HasWhere for SelectQuery {
    fn where_mut(&mut self) -> &mut Where {
        &mut self.where_
    }
}

impl HasLimit for SelectQuery {
    fn limit_mut(&mut self) -> &mut Limit {
        &mut self.limit
    }
}

impl Expression for SelectQuery {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        // Build mods run against a clone so that rendering stays `&self`.
        if !self.build_mods.is_empty() {
            let mut applied = self.clone();
            applied.build_mods.clear();
            for m in &self.build_mods {
                m.apply(&mut applied)?;
            }
            return applied.write_sql(w);
        }

        w.push_str("SELECT * FROM ");
        w.push_quoted(&[&self.table]);
        w.write_expr(&self.where_)?;
        w.write_expr(&self.limit)
    }
}

impl SelectQuery {
    fn build(&self) -> Result<(String, Vec<Value>)> {
        build(&Psql, self)
    }

    fn apply<M: Mod<Self>>(&mut self, m: M) {
        m.apply(self);
    }

    fn query_type(&self) -> QueryType {
        QueryType::Select
    }
}

fn select<M: Mod<SelectQuery>>(mods: M) -> SelectQuery {
    let mut q = SelectQuery::default();
    mods.apply(&mut q);
    q
}

// The generic clause mods that make the `Has*` traits worth having.
fn from<Q>(table: impl Into<String>) -> impl Mod<Q>
where
    Q: AsMut<String>,
{
    let table = table.into();
    mod_fn(move |q: &mut Q| *q.as_mut() = table)
}

impl AsMut<String> for SelectQuery {
    fn as_mut(&mut self) -> &mut String {
        &mut self.table
    }
}

fn where_<Q: HasWhere>(e: impl Expression + 'static) -> impl Mod<Q> {
    let e = dyn_expr(e);
    mod_fn(move |q: &mut Q| q.where_mut().conditions.push(e))
}

fn limit<Q: HasLimit>(n: i64) -> impl Mod<Q> {
    mod_fn(move |q: &mut Q| q.limit_mut().count = Some(n))
}

#[derive(Debug)]
struct Gte(&'static str, i32);

impl Expression for Gte {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        w.push_str("(");
        w.push_quoted(&[self.0]);
        w.push_str(" >= ");
        w.push_arg(self.1);
        w.push_str(")");
        Ok(())
    }
}

#[test]
fn a_tuple_of_mods_builds_a_query() {
    let q = select((from("users"), where_(Gte("age", 21))));
    let (sql, args) = q.build().unwrap();
    assert_eq!(sql, r#"SELECT * FROM "users" WHERE ("age" >= $1)"#);
    assert_eq!(args, vec![Value::I32(21)]);
    assert_eq!(q.query_type(), QueryType::Select);
}

#[test]
fn a_single_mod_needs_no_tuple_and_no_mods_is_unit() {
    assert_eq!(
        select(from("projects")).build().unwrap().0,
        r#"SELECT * FROM "projects""#
    );
    assert_eq!(select(()).build().unwrap().0, "SELECT * FROM ");
}

#[test]
fn a_raw_string_works_where_an_expression_is_expected() {
    let q = select((from("users"), where_("id = 1")));
    assert_eq!(
        q.build().unwrap().0,
        r#"SELECT * FROM "users" WHERE id = 1"#
    );
}

#[test]
fn conditional_mods_are_written_as_options() {
    fn scoped(is_admin: bool) -> String {
        select((
            from("projects"),
            (!is_admin).then(|| where_(Gte("user_id", 3))),
        ))
        .build()
        .unwrap()
        .0
    }

    assert_eq!(
        scoped(false),
        r#"SELECT * FROM "projects" WHERE ("user_id" >= $1)"#
    );
    assert_eq!(scoped(true), r#"SELECT * FROM "projects""#);
}

#[test]
fn mods_can_also_be_applied_after_the_fact() {
    let mut q = select(from("users"));
    q.apply(where_(Gte("age", 18)));
    q.apply(vec![where_(Gte("id", 1))]);
    q.apply(limit(20));
    let (sql, args) = q.build().unwrap();
    assert_eq!(
        sql,
        r#"SELECT * FROM "users" WHERE ("age" >= $1) AND ("id" >= $2) LIMIT $3"#
    );
    assert_eq!(args, vec![Value::I32(18), Value::I32(1), Value::I64(20)]);
}

#[test]
fn a_query_nests_as_a_subquery_and_the_placeholders_continue() {
    #[derive(Debug)]
    struct In(&'static str, SelectQuery);

    impl Expression for In {
        fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
            w.push_quoted(&[self.0]);
            w.push_str(" IN (");
            w.write_expr(&self.1)?;
            w.push_str(")");
            Ok(())
        }
    }

    let inner = select((from("admins"), where_(Gte("level", 9))));
    let outer = select((
        from("users"),
        where_(Gte("age", 21)),
        where_(In("id", inner)),
        limit(5),
    ));

    let (sql, args) = outer.build().unwrap();
    assert_eq!(
        sql,
        r#"SELECT * FROM "users" WHERE ("age" >= $1) AND "id" IN (SELECT * FROM "admins" WHERE ("level" >= $2)) LIMIT $3"#
    );
    assert_eq!(args, vec![Value::I32(21), Value::I32(9), Value::I64(5)]);
}

#[test]
fn build_mods_run_at_build_time_and_do_not_mutate_the_query() {
    #[derive(Debug)]
    struct AlwaysLimit(i64);

    impl BuildMod<SelectQuery> for AlwaysLimit {
        fn apply(&self, q: &mut SelectQuery) -> Result<()> {
            q.limit.count = Some(self.0);
            Ok(())
        }
    }

    let mut q = select(from("users"));
    q.build_mods.push(Arc::new(AlwaysLimit(7)));

    assert_eq!(q.build().unwrap().0, r#"SELECT * FROM "users" LIMIT $1"#);
    assert!(q.limit.count.is_none(), "the original is untouched");
    // Idempotent: a second build sees the same query.
    assert_eq!(q.build().unwrap().0, r#"SELECT * FROM "users" LIMIT $1"#);
}

#[test]
fn a_failing_build_mod_aborts_the_build() {
    #[derive(Debug)]
    struct NeedsSchema;

    impl BuildMod<SelectQuery> for NeedsSchema {
        fn apply(&self, _q: &mut SelectQuery) -> Result<()> {
            Err(Error::Incomplete("a schema"))
        }
    }

    let mut q = select(from("users"));
    q.build_mods.push(Arc::new(NeedsSchema));
    assert!(matches!(q.build(), Err(Error::Incomplete("a schema"))));
}

#[test]
fn a_query_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>(_: &T) {}
    assert_send_sync(&select(from("users")));
    let e: DynExpr = dyn_expr(Gte("age", 1));
    assert_send_sync(&e);
}
