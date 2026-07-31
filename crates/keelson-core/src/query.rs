use std::borrow::Cow;
use std::fmt;

use crate::dialect::Dialect;
use crate::error::Result;
use crate::expr::{IntoExpr, RawArg};
use crate::value::{ToValue, Value};
use crate::writer::{Expression, build, build_from};

/// Which statement a query renders.
///
/// Carried so the execution layer can decide whether to expect rows without
/// parsing the SQL back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum QueryType {
    /// Not one of the four — a raw statement, a DDL statement, a bare expression.
    #[default]
    Unknown,
    /// `SELECT`.
    Select,
    /// `INSERT`.
    Insert,
    /// `UPDATE`.
    Update,
    /// `DELETE`.
    Delete,
    /// `MERGE` — PostgreSQL's conditional insert/update/delete against a source.
    ///
    /// A fifth statement kind rather than a flavour of the four: whether it
    /// returns rows depends on its `RETURNING` clause, exactly as for the three
    /// mutations, but it is none of them.
    Merge,
}

impl fmt::Display for QueryType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            QueryType::Unknown => "UNKNOWN",
            QueryType::Select => "SELECT",
            QueryType::Insert => "INSERT",
            QueryType::Update => "UPDATE",
            QueryType::Delete => "DELETE",
            QueryType::Merge => "MERGE",
        })
    }
}

/// A complete, runnable statement.
///
/// An [`Expression`] is any fragment; a `Query` is a fragment that stands alone,
/// knows which dialect it is written in and therefore can be built without being
/// told anything. That is what lets `q.build()` take no arguments while a bare
/// expression still needs [`build(dialect, expr)`](build).
///
/// A query is also an expression, so it nests as a sub-select — and because the
/// placeholder counter belongs to the [`SqlWriter`](crate::SqlWriter) rather than
/// to the query, nesting re-indexes on its own.
pub trait Query: Expression {
    /// Which statement this renders.
    fn query_type(&self) -> QueryType;

    /// The dialect this query renders itself in.
    ///
    /// bob's `BaseQuery` carries its dialect the same way, and deliberately
    /// ignores the dialect handed to it when embedded in another query.
    fn dialect(&self) -> &dyn Dialect;

    /// Render to SQL and arguments, numbering placeholders from 1.
    ///
    /// The escape hatch that is always open: whatever layer produced the query,
    /// this hands back a `String` and a `Vec<Value>` and nothing else.
    fn build(&self) -> Result<(String, Vec<Value>)> {
        build(self.dialect(), self)
    }

    /// [`build`](Self::build) with a different first placeholder position, for
    /// splicing into a statement that already has arguments — bob's `BuildN`.
    fn build_from(&self, start: usize) -> Result<(String, Vec<Value>)> {
        build_from(self.dialect(), start, self)
    }
}

/// The execution layer's extension points, hung on the query rather than
/// discovered by downcasting.
///
/// bob type-asserts a query against `HookableQuery`, `Loadable` and
/// `MapperModder` at run time. Here the questions are trait methods, so they
/// resolve statically: every one is defaulted to "none", a Layer 1 query opts in
/// with an empty impl, and a generated Layer 2 query overrides only what it
/// actually carries.
///
/// The three payloads are type parameters because their concrete shapes depend on
/// the executor, which core knows nothing about — while the trait itself has to
/// live here, since neither a dialect crate nor a backend crate could implement
/// the other's trait for the other's type. Core fixes the mechanism; the
/// execution layer fills in the types.
pub trait QueryExtensions<Hook, Loader, MapperMod>: Query {
    /// Hooks to run before this query executes.
    fn hooks(&self) -> &[Hook] {
        &[]
    }

    /// Loaders to run after this query executes, for eagerly loaded relations.
    fn loaders(&self) -> &[Loader] {
        &[]
    }

    /// Adjustments to the row mapper, for relations loaded in the same query.
    fn mapper_mods(&self) -> &[MapperMod] {
        &[]
    }
}

/// A whole statement, written by hand.
///
/// The builder's counterpart to [`raw`](crate::expr::raw): that one is a
/// *fragment* an expression accepts, this one is a *statement* nothing built
/// it. It is an ordinary [`Query`], so the execution layer's verbs apply
/// unchanged — `fetch_all::<T>()` maps hand-written SQL onto a struct exactly
/// as it maps a built statement — and it is an [`Expression`], so it nests as
/// a sub-select in a built statement like any other query.
///
/// Construct one from the dialect it is written in: `psql::raw_query(…)`,
/// `mysql::raw_query(…)`, `sqlite::raw_query(…)`.
///
/// **Placeholders are `?`, on every dialect,** and are rewritten into that
/// dialect's own syntax as the statement renders — `$1` on PostgreSQL, `?1` on
/// SQLite, `?` on MySQL. Write `\?` for a literal question mark. The rule is
/// [`template`](crate::expr::template)'s, for the same reason: a hand-written
/// statement should not have to be respelled to move between engines, and a
/// value still never reaches the SQL text. A `?` count that disagrees with the
/// bound arguments is an error from [`build`](Query::build), not a silently
/// misbound statement.
///
/// keelson does not parse what you hand it. Everything the builder guarantees
/// — that the SQL is grammatical for the engine, that identifiers are quoted —
/// is yours here; what you keep is the binding, the placeholder rewriting, the
/// row mapping, the transaction, and the tracing span.
///
/// # Why this is untyped, and stays untyped
///
/// The row type is whatever you name in `fetch_all::<T>()`, and nothing checks
/// it against the schema. That is the escape hatch's job description, not a
/// gap waiting to be filled.
///
/// **The rejected alternative:** a proc macro that types the row and the
/// parameters at the call site, by running keelson-gen's own inference against
/// a committed schema snapshot at compile time. It would work — the analysis
/// is already a library, and the snapshot would need no database at build
/// time, which is more than sqlx's `query!` manages. It was rejected because
/// it adds **no guarantee keelson does not already offer**: the same analysis,
/// against the same schema, producing the same types, is Layer 4
/// (`keelson-gen`'s `.sql` files). The only difference is where the SQL lives.
/// Paying a build-time SQL parser (libpg_query, in C, for PostgreSQL), a new
/// artifact to keep fresh, and a second answer to "typed hand-written SQL"
/// buys locality and nothing else.
///
/// So the line is: **typed** SQL goes in a `.sql` file (Layer 4) or comes from
/// a generated model (Layer 3), and both are typed because they were derived
/// from the schema. **Untyped** SQL is this, and its counterpart for
/// fragments. Wanting the compiler to reject a malformed query outright is a
/// real want, and [diesel](https://diesel.rs) is the library that serves it.
#[derive(Debug, Clone)]
pub struct RawQuery<D> {
    sql: Cow<'static, str>,
    args: Vec<RawArg>,
    dialect: D,
    query_type: QueryType,
}

impl<D> RawQuery<D> {
    /// The statement, in `dialect`'s SQL. Dialect crates wrap this as
    /// `raw_query`, which is how a caller should reach it.
    pub fn new(dialect: D, sql: impl Into<Cow<'static, str>>) -> Self {
        RawQuery {
            sql: sql.into(),
            args: Vec::new(),
            dialect,
            query_type: QueryType::Unknown,
        }
    }

    /// Bind a value to the next `?`.
    #[must_use]
    pub fn bind(mut self, value: impl ToValue) -> Self {
        self.args.push(RawArg::value(value));
        self
    }

    /// Bind every value in `values`, in order — one `?` each.
    #[must_use]
    pub fn bind_all<V: ToValue>(mut self, values: impl IntoIterator<Item = V>) -> Self {
        self.args.extend(values.into_iter().map(RawArg::value));
        self
    }

    /// Splice an expression into the next `?` instead of binding a value.
    ///
    /// This is what makes `WHERE id IN (?)` work: the expression consumes as
    /// many placeholder positions as it binds arguments, and the counter keeps
    /// going from there. A quoted identifier, a sub-query and a built
    /// statement all go in this way.
    #[must_use]
    pub fn bind_expr(mut self, expression: impl IntoExpr) -> Self {
        self.args.push(RawArg::expr(expression));
        self
    }

    /// Declare which statement this is.
    ///
    /// It feeds the tracing span and nothing else — the execution layer never
    /// polices it against the SQL. The default is
    /// [`QueryType::Unknown`], which is the honest answer for text keelson has
    /// not read: guessing from the leading keyword would be wrong for
    /// `WITH … INSERT`, and keelson does not guess.
    #[must_use]
    pub fn kind(mut self, query_type: QueryType) -> Self {
        self.query_type = query_type;
        self
    }
}

impl<D: fmt::Debug + Send + Sync> Expression for RawQuery<D> {
    fn write_sql(&self, w: &mut crate::writer::SqlWriter<'_>) {
        crate::expr::template(self.sql.clone(), self.args.iter().cloned()).write_sql(w);
    }
}

impl<D: Dialect> Query for RawQuery<D> {
    fn query_type(&self) -> QueryType {
        self.query_type
    }

    fn dialect(&self) -> &dyn Dialect {
        &self.dialect
    }
}

// No hooks, no loaders, no mapper mods: a statement keelson did not build has
// nothing hung on it. The empty impl is what makes the execution layer's verbs
// available at all.
impl<D: Dialect, H, L, M> QueryExtensions<H, L, M> for RawQuery<D> {}

#[cfg(test)]
mod tests {
    use keelson_sqlcheck::testing::assert_stmt_sql;

    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::error::Error;
    use crate::writer::SqlWriter;

    #[test]
    fn display_matches_the_sql_keyword() {
        assert_eq!(QueryType::Select.to_string(), "SELECT");
        assert_eq!(QueryType::Insert.to_string(), "INSERT");
        assert_eq!(QueryType::Update.to_string(), "UPDATE");
        assert_eq!(QueryType::Delete.to_string(), "DELETE");
        assert_eq!(QueryType::Merge.to_string(), "MERGE");
        assert_eq!(QueryType::Unknown.to_string(), "UNKNOWN");
        assert_eq!(QueryType::default(), QueryType::Unknown);
    }

    /// The shape a dialect crate's query struct will have. It renders a whole
    /// statement, so the cases below go to the judge — and it names a table from
    /// `tests/schema/psql.sql` so that a real server can resolve it.
    #[derive(Debug)]
    struct Select {
        table: &'static str,
        min_age: i32,
    }

    impl Expression for Select {
        fn write_sql(&self, w: &mut SqlWriter<'_>) {
            w.push_str("SELECT * FROM ");
            w.push_quoted(&[self.table]);
            w.push_str(" WHERE ");
            w.push_quoted(&["age"]);
            w.push_str(" >= ");
            w.push_arg(self.min_age);
        }
    }

    impl Query for Select {
        fn query_type(&self) -> QueryType {
            QueryType::Select
        }

        fn dialect(&self) -> &dyn Dialect {
            &Numbered
        }
    }

    #[test]
    fn a_query_builds_itself_without_being_told_the_dialect() {
        let q = Select {
            table: "users",
            min_age: 21,
        };
        let (sql, args) = q.build().unwrap();
        assert_stmt_sql(&sql, r#"SELECT * FROM "users" WHERE "age" >= $1"#);
        assert_eq!(args, vec![Value::I32(21)]);
        assert_eq!(q.query_type(), QueryType::Select);

        // Not judged: a statement whose lowest placeholder is `$4` has no `$1`, and
        // a server refuses to prepare that. Which is the point of `build_from` —
        // the result is a fragment for splicing into a statement that already has
        // three arguments, not something to send on its own.
        let (sql, _) = q.build_from(4).unwrap();
        assert_eq!(sql, r#"SELECT * FROM "users" WHERE "age" >= $4"#);
    }

    #[test]
    fn a_query_is_usable_erased() {
        let q: Box<dyn Query> = Box::new(Select {
            table: "users",
            min_age: 1,
        });
        assert_eq!(q.query_type(), QueryType::Select);
        assert!(q.build().is_ok());
    }

    #[test]
    fn a_query_nested_in_another_shares_the_numbering() {
        #[derive(Debug)]
        struct Wrapper(Select);

        impl Expression for Wrapper {
            fn write_sql(&self, w: &mut SqlWriter<'_>) {
                w.push_str("SELECT * FROM (");
                w.write_expr(&self.0);
                // The alias is not decoration: PostgreSQL requires one on a
                // sub-query in a FROM.
                w.push_str(") AS \"u\" WHERE \"u\".\"id\" = ");
                w.push_arg(9i32);
            }
        }

        let (sql, args) = build(
            &Numbered,
            &Wrapper(Select {
                table: "users",
                min_age: 21,
            }),
        )
        .unwrap();
        assert_stmt_sql(
            &sql,
            concat!(
                r#"SELECT * FROM (SELECT * FROM "users" WHERE "age" >= $1) AS "u" "#,
                r#"WHERE "u"."id" = $2"#
            ),
        );
        assert_eq!(args, vec![Value::I32(21), Value::I32(9)]);
    }

    // A Layer 1 query opts in with an empty impl and answers "no extensions" for
    // whatever payload types the execution layer picks.
    impl<H, L, M> QueryExtensions<H, L, M> for Select {}

    #[test]
    fn extension_points_default_to_none_without_any_downcasting() {
        let q = Select {
            table: "users",
            min_age: 1,
        };
        let q: &dyn QueryExtensions<&'static str, u8, u8> = &q;
        assert!(q.hooks().is_empty());
        assert!(q.loaders().is_empty());
        assert!(q.mapper_mods().is_empty());
    }

    // ── RawQuery: a whole statement nobody built ──────────────────────────
    //
    // Judged like any other statement: what a caller hands in is what the
    // engine has to accept, and `?` rewriting is the only thing keelson does
    // to it.

    #[test]
    fn a_hand_written_statement_binds_and_rewrites_its_placeholders() {
        let q = RawQuery::new(Numbered, "SELECT * FROM \"users\" WHERE \"age\" >= ?")
            .bind(21)
            .kind(QueryType::Select);
        let (sql, args) = q.build().unwrap();
        assert_stmt_sql(&sql, r#"SELECT * FROM "users" WHERE "age" >= $1"#);
        assert_eq!(args, vec![Value::I32(21)]);
        assert_eq!(q.query_type(), QueryType::Select);
    }

    #[test]
    fn the_statement_kind_is_unknown_until_it_is_declared() {
        // keelson has not read the text, so it does not claim to know. Guessing
        // from the leading keyword would be wrong for `WITH … INSERT`.
        let q = RawQuery::new(Numbered, "SELECT 1");
        assert_eq!(q.query_type(), QueryType::Unknown);
    }

    #[test]
    fn binding_more_than_the_placeholders_is_an_error_not_a_misbound_statement() {
        let q = RawQuery::new(Numbered, "SELECT * FROM \"users\" WHERE \"age\" >= ?")
            .bind(21)
            .bind(22);
        let err = q.build().unwrap_err();
        assert!(matches!(err, Error::RawArgCount { .. }), "{err}");
    }

    #[test]
    fn an_expression_can_be_spliced_where_a_value_would_go() {
        // The `IN (?)` case: the spliced expression consumes as many
        // placeholder positions as it binds, and the counter carries on.
        let q = RawQuery::new(
            Numbered,
            "SELECT * FROM \"users\" WHERE \"id\" IN (?) AND \"age\" >= ?",
        )
        .bind_expr(crate::expr::args([1, 2, 3]))
        .bind(21);
        let (sql, args) = q.build().unwrap();
        assert_stmt_sql(
            &sql,
            r#"SELECT * FROM "users" WHERE "id" IN ($1, $2, $3) AND "age" >= $4"#,
        );
        assert_eq!(
            args,
            vec![Value::I32(1), Value::I32(2), Value::I32(3), Value::I32(21)]
        );
    }

    #[test]
    fn a_hand_written_statement_nests_in_a_built_one() {
        // It is an `Expression`, so it goes anywhere a query goes — and the
        // outer writer renumbers it, exactly as for a built sub-select.
        let (sql, args) = build(
            &Numbered,
            &Select {
                table: "users",
                min_age: 21,
            }
            .wrapped_around(
                RawQuery::new(Numbered, "SELECT \"id\" FROM \"posts\" WHERE \"views\" > ?")
                    .bind(100),
            ),
        )
        .unwrap();
        assert_stmt_sql(
            &sql,
            concat!(
                r#"SELECT * FROM "users" WHERE "age" >= $1 AND "id" IN "#,
                r#"(SELECT "id" FROM "posts" WHERE "views" > $2)"#
            ),
        );
        assert_eq!(args, vec![Value::I32(21), Value::I32(100)]);
    }

    /// `Select`, with a sub-query hung on its `WHERE` — just enough to prove
    /// the nesting, without a second statement type.
    #[derive(Debug)]
    struct Wrapped(Select, RawQuery<Numbered>);

    impl Select {
        fn wrapped_around(self, inner: RawQuery<Numbered>) -> Wrapped {
            Wrapped(self, inner)
        }
    }

    impl Expression for Wrapped {
        fn write_sql(&self, w: &mut SqlWriter<'_>) {
            self.0.write_sql(w);
            w.push_str(" AND ");
            w.push_quoted(&["id"]);
            w.push_str(" IN (");
            self.1.write_sql(w);
            w.push_str(")");
        }
    }
}
