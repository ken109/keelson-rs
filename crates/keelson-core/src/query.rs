use std::fmt;

use crate::dialect::Dialect;
use crate::error::Result;
use crate::value::Value;
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
}

impl fmt::Display for QueryType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            QueryType::Unknown => "UNKNOWN",
            QueryType::Select => "SELECT",
            QueryType::Insert => "INSERT",
            QueryType::Update => "UPDATE",
            QueryType::Delete => "DELETE",
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

#[cfg(test)]
mod tests {
    use keelson_sqlcheck::testing::assert_stmt_sql;

    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::writer::SqlWriter;

    #[test]
    fn display_matches_the_sql_keyword() {
        assert_eq!(QueryType::Select.to_string(), "SELECT");
        assert_eq!(QueryType::Insert.to_string(), "INSERT");
        assert_eq!(QueryType::Update.to_string(), "UPDATE");
        assert_eq!(QueryType::Delete.to_string(), "DELETE");
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
}
