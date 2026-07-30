//! The core API exercised from outside the crate, the way a dialect crate will
//! use it: a hand-written dialect, clauses that store `Cow<'static, str>` and
//! erased expressions, mods shared through a `Has*` trait, and a query that knows
//! its own dialect.
//!
//! This is a contract test. If it stops compiling, a later phase's code stops
//! compiling too.

use std::borrow::Cow;

use keelson_core::{
    BuildMod, DynExpr, Error, Expression, Mod, Query, QueryType, Result, SqlWriter, Value, build,
    dyn_expr, mod_fn,
};

/// A dialect crate's dialect: `$N`, `"id"`, no named arguments.
#[derive(Debug, Clone, Copy)]
struct Psql;

impl keelson_core::Dialect for Psql {
    fn write_arg(&self, w: &mut SqlWriter<'_>, position: usize) {
        w.push_str("$");
        w.push_str(&position.to_string());
    }

    fn write_quoted(&self, w: &mut SqlWriter<'_>, s: &str) {
        w.push_str("\"");
        w.push_str(s);
        w.push_str("\"");
    }
}

/// A quoted identifier, stored the way clauses store one: owned when computed,
/// free when a literal, and with no lifetime on the type.
#[derive(Debug, Clone)]
struct Quoted(Vec<Cow<'static, str>>);

impl Expression for Quoted {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.push_quoted(&self.0);
    }
}

/// A bound argument as an expression.
#[derive(Debug, Clone)]
struct Arg(Value);

impl Expression for Arg {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.push_arg(self.0.clone());
    }
}

/// `lhs >= rhs`, to show two erased operands composing.
#[derive(Debug, Clone)]
struct Gte(DynExpr, DynExpr);

impl Expression for Gte {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.push_str("(");
        w.write_expr(&self.0);
        w.push_str(" >= ");
        w.write_expr(&self.1);
        w.push_str(")");
    }
}

#[derive(Debug, Clone, Default)]
struct Where(Vec<DynExpr>);

#[derive(Debug, Clone, Default)]
struct SelectQuery {
    from: Option<Cow<'static, str>>,
    where_: Where,
}

/// The shared-clause trait the `where_` mod is written against: a statement
/// without a `WHERE` simply does not implement it.
trait HasWhere {
    fn where_mut(&mut self) -> &mut Where;
}

impl HasWhere for SelectQuery {
    fn where_mut(&mut self) -> &mut Where {
        &mut self.where_
    }
}

/// Written once, usable on every query that has a `WHERE`.
fn where_<Q: HasWhere>(e: impl Expression + 'static) -> impl Mod<Q> {
    let e = dyn_expr(e);
    mod_fn(move |q: &mut Q| q.where_mut().0.push(e))
}

fn from(table: impl Into<Cow<'static, str>>) -> impl Mod<SelectQuery> {
    let table = table.into();
    mod_fn(move |q: &mut SelectQuery| q.from = Some(table))
}

impl Expression for SelectQuery {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.push_str("SELECT *");
        w.write_if_some(self.from.as_ref(), " FROM ", "");
        w.write_slice(&self.where_.0, " WHERE ", " AND ", "");
    }
}

impl Query for SelectQuery {
    fn query_type(&self) -> QueryType {
        QueryType::Select
    }

    fn dialect(&self) -> &dyn keelson_core::Dialect {
        &Psql
    }
}

fn select(mods: impl Mod<SelectQuery>) -> SelectQuery {
    let mut q = SelectQuery::default();
    mods.apply(&mut q);
    q
}

fn quote(parts: impl IntoIterator<Item = &'static str>) -> Quoted {
    Quoted(parts.into_iter().map(Cow::Borrowed).collect())
}

fn arg(v: impl keelson_core::ToValue) -> Arg {
    Arg(v.to_value())
}

fn gte(lhs: impl Expression + 'static, rhs: impl Expression + 'static) -> Gte {
    Gte(dyn_expr(lhs), dyn_expr(rhs))
}

#[test]
fn a_query_assembled_from_mods_builds_itself() {
    let q = select((
        from("users"),
        where_(gte(quote(["users", "age"]), arg(21i32))),
    ));

    let (sql, args) = q.build().unwrap();
    assert_eq!(sql, r#"SELECT * FROM users WHERE ("users"."age" >= $1)"#);
    assert_eq!(args, vec![Value::I32(21)]);
    assert_eq!(q.query_type(), QueryType::Select);
}

#[test]
fn conditional_mods_need_no_if_statement() {
    let admin = false;
    let scoped = select((
        from("projects"),
        (!admin).then(|| where_(gte(quote(["user_id"]), arg(7i32)))),
    ));
    assert_eq!(
        scoped.build().unwrap().0,
        r#"SELECT * FROM projects WHERE ("user_id" >= $1)"#
    );

    let admin = true;
    let unscoped = select((
        from("projects"),
        (!admin).then(|| where_(gte(quote(["user_id"]), arg(7i32)))),
    ));
    assert_eq!(unscoped.build().unwrap().0, "SELECT * FROM projects");
}

#[test]
fn a_raw_fragment_goes_wherever_an_expression_goes() {
    let q = select((from("users"), where_("id = 1")));
    assert_eq!(q.build().unwrap().0, "SELECT * FROM users WHERE id = 1");
}

#[test]
fn a_subquery_continues_the_outer_placeholder_numbering() {
    #[derive(Debug)]
    struct In(DynExpr, SelectQuery);

    impl Expression for In {
        fn write_sql(&self, w: &mut SqlWriter<'_>) {
            w.write_expr(&self.0);
            w.push_str(" IN (");
            w.write_expr(&self.1);
            w.push_str(")");
        }
    }

    let inner = select((from("adults"), where_(gte(quote(["age"]), arg(21i32)))));
    let outer = select((
        from("users"),
        where_(gte(quote(["rank"]), arg(3i32))),
        where_(In(dyn_expr(quote(["id"])), inner)),
        where_(gte(quote(["score"]), arg(9i32))),
    ));

    let (sql, args) = outer.build().unwrap();
    assert_eq!(
        sql,
        r#"SELECT * FROM users WHERE ("rank" >= $1) AND "id" IN (SELECT * FROM adults WHERE ("age" >= $2)) AND ("score" >= $3)"#
    );
    assert_eq!(args, vec![Value::I32(3), Value::I32(21), Value::I32(9)]);

    // And splicing the whole thing into an existing statement shifts every
    // placeholder, arguments unchanged.
    let (sql, args) = outer.build_from(5).unwrap();
    assert!(sql.contains("$5") && sql.contains("$6") && sql.contains("$7"));
    assert_eq!(args.len(), 3);
}

#[test]
fn args_serialise_as_the_plain_json_the_golden_harness_compares() {
    let q = select((
        from("users"),
        where_(gte(quote(["name"]), arg("Stephen"))),
        where_(gte(quote(["age"]), arg(100i32))),
    ));
    let (_, args) = q.build().unwrap();
    let json: Vec<serde_json::Value> = args
        .iter()
        .map(|a| serde_json::to_value(a).unwrap())
        .collect();
    assert_eq!(
        json,
        vec![serde_json::json!("Stephen"), serde_json::json!(100)]
    );
}

#[test]
fn a_build_mod_runs_against_a_clone_on_every_build() {
    #[derive(Debug)]
    struct Schema(&'static str);

    impl BuildMod<SelectQuery> for Schema {
        fn apply(&self, q: &mut SelectQuery) -> Result<()> {
            match q.from.take() {
                Some(t) => {
                    q.from = Some(Cow::Owned(format!("{}.{t}", self.0)));
                    Ok(())
                }
                None => Err(Error::Incomplete("a table")),
            }
        }
    }

    let base = select(from("users"));
    for _ in 0..2 {
        let mut q = base.clone();
        Schema("public").apply(&mut q).unwrap();
        assert_eq!(q.build().unwrap().0, "SELECT * FROM public.users");
    }
    assert_eq!(base.build().unwrap().0, "SELECT * FROM users");

    let mut empty = SelectQuery::default();
    assert!(Schema("public").apply(&mut empty).is_err());
}

#[test]
fn asking_for_a_named_arg_a_dialect_lacks_fails_the_build_and_nothing_else() {
    #[derive(Debug)]
    struct Named(&'static str);

    impl Expression for Named {
        fn write_sql(&self, w: &mut SqlWriter<'_>) {
            w.push_named_arg(self.0);
        }
    }

    let q = select((from("users"), where_(Named("id"))));
    assert!(matches!(q.build(), Err(Error::NoNamedArgs)));

    // The same expression under a dialect that has them renders fine.
    #[derive(Debug)]
    struct Sqlite;

    impl keelson_core::Dialect for Sqlite {
        fn write_arg(&self, w: &mut SqlWriter<'_>, position: usize) {
            w.push_str("?");
            w.push_str(&position.to_string());
        }

        fn write_quoted(&self, w: &mut SqlWriter<'_>, s: &str) {
            w.push_str("\"");
            w.push_str(s);
            w.push_str("\"");
        }

        fn write_named_arg(&self, w: &mut SqlWriter<'_>, name: &str) {
            w.push_str(":");
            w.push_str(name);
        }
    }

    let (sql, args) = build(&Sqlite, &Named("id")).unwrap();
    assert_eq!(sql, ":id");
    assert!(args.is_empty(), "a named arg binds nothing positionally");
}
