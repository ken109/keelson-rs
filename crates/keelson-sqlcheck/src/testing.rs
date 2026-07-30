//! A stand-in dialect, so a crate with no dialect of its own can still be judged.
//!
//! Every dialect crate depends on `keelson-core`, so `keelson-core` cannot depend
//! on one back — not even as a dev-dependency. Without a dialect there is nothing
//! to render with, and without rendered SQL there is nothing for
//! [`assert_sql`](crate::assert_sql) to judge, which is how `keelson-core`'s tests
//! came to assert string equality and nothing else.
//!
//! [`PgLike`] closes that gap. It is a two-method `keelson_core::Dialect` that
//! writes `$1` and `"id"` — deliberately PostgreSQL's spelling, so the SQL it
//! produces can be handed to the psql judges: libpg_query, and a real PostgreSQL
//! 17 under `live-docker`. This crate depends on nothing else in the workspace, so
//! `keelson-core [dev] -> keelson-sqlcheck -> keelson-core` is a cycle only
//! through a dev-dependency, which is the one kind Cargo allows.
//!
//! # What can and cannot be judged
//!
//! A parser judges statements. A bare expression or a lone clause is a fragment,
//! and no amount of tooling can decide whether `("age" >= $1)` is correct SQL,
//! because on its own it is not SQL at all. So a fragment has to be put where a
//! statement can hold it — that is what [`assert_frag`] is for, and the frame it
//! is given is part of what the test asserts.
//!
//! Two consequences worth knowing before writing the frame:
//!
//! - **Name only the shared schema.** A real engine resolves names, so
//!   `tests/schema/psql.sql` (`users`, `posts`, `comments`, `tags`, `post_tags`)
//!   is the vocabulary. `SELECT "nope" FROM users` passes the grammar and fails
//!   the engine.
//! - **Watch where a placeholder has no type to take.** PostgreSQL resolves a
//!   placeholder it has nothing else to go on to `text`, so `SELECT $1 FROM users`
//!   is accepted. What it cannot do is resolve an *operator* whose every operand
//!   is a placeholder: `coalesce($1, $2)` and a `CASE` whose every result is one
//!   both fail with "No operator matches the given name and argument types",
//!   comparison against a typed column included. Put such a fragment inside a
//!   `CAST(… AS …)` in the frame.
//!
//! # Two tiers of assertion, and why
//!
//! [`assert_stmt`] and [`assert_frag`] take the expression and render it here, so
//! a test cannot render with one dialect and judge as another. They are what a
//! *dialect crate* and `keelson-core`'s `tests/*.rs` should use.
//!
//! `keelson-core`'s in-module `#[cfg(test)]` tests cannot: the unit-test target
//! compiles `keelson-core` a second time, so the `Expression` those tests hold is a
//! different type from the one this crate links, and the bound does not hold. They
//! render with `keelson_core::testing::Numbered` — byte-identical to [`PgLike`],
//! pinned by `numbered_and_pg_like_render_the_same_sql` in
//! `keelson-core/tests/grammar_raw.rs` — and judge the resulting *string* with
//! [`assert_stmt_sql`] or [`assert_frag_sql`].
//!
//! # Not a substitute for the real dialects
//!
//! [`PgLike`] quotes without doubling an embedded `"`, has no named arguments and
//! knows no operators. It is the minimum that makes rendering possible;
//! `keelson-psql` is what the psql *dialect* is tested through.

use keelson_core::{Dialect as CoreDialect, Expression, SqlWriter, Value, build};

use crate::Dialect;

/// The grammar and engine that judge [`PgLike`]'s output.
///
/// A test should not have to know this — the assertions below pin it — but a test
/// that reaches for [`crate::assert_sql`] directly needs to pass the same thing.
pub const JUDGE: Dialect = Dialect::Psql;

/// `$N` placeholders and `"` quoting, matching PostgreSQL closely enough to be
/// judged by it.
///
/// Numbered rather than positional on purpose: under `?` every off-by-one in the
/// placeholder counter renders the same string, so the bug is invisible in the SQL
/// and only the argument list gives it away.
#[derive(Debug, Clone, Copy, Default)]
pub struct PgLike;

impl CoreDialect for PgLike {
    fn write_arg(&self, w: &mut SqlWriter<'_>, position: usize) {
        w.push_str("$");
        w.push_str(&position.to_string());
    }

    fn write_quoted(&self, w: &mut SqlWriter<'_>, s: &str) {
        // No doubling of an embedded `"`. Real psql escapes it; core delegates the
        // decision, so what is pinned here is the delegation, not the escaping.
        w.push_str("\"");
        w.push_str(s);
        w.push_str("\"");
    }

    // `write_named_arg` is left at its default, which records `NoNamedArgs`:
    // PostgreSQL has no named placeholders either.
}

/// Render `e` under [`PgLike`], numbering placeholders from 1.
///
/// # Panics
/// If the build records an error. A test about a *recorded* failure should call
/// `keelson_core::build` itself and inspect the `Err`.
#[track_caller]
pub fn render<E: Expression + ?Sized>(e: &E) -> (String, Vec<Value>) {
    match build(&PgLike, e) {
        Ok(parts) => parts,
        Err(err) => panic!("rendering under PgLike failed: {err}"),
    }
}

/// Render a whole statement and judge it: grammar, engine, then `expected`.
///
/// Returns the bound arguments, since the argument list is half of what a
/// placeholder test is about and no parser can check it.
///
/// # Panics
/// Through [`assert_sql`](crate::assert_sql), naming which check failed.
#[track_caller]
pub fn assert_stmt<E: Expression + ?Sized>(e: &E, expected: &str) -> Vec<Value> {
    let (sql, args) = render(e);
    crate::assert_sql(JUDGE, &sql, expected);
    args
}

/// Judge a fragment by the statement it belongs to.
///
/// `frame` is a complete statement with `{}` marking the fragment's place;
/// `expected` is what the fragment itself should render as. Both sides of the
/// comparison get the same frame, so what the test reads as asserting is still the
/// fragment — the frame is what makes the assertion judgeable at all.
///
/// ```ignore
/// let args = assert_frag(
///     r#"SELECT "id" FROM users {}"#,
///     &where_clause,
///     r#"WHERE ("age" >= $1)"#,
/// );
/// ```
///
/// Returns the fragment's bound arguments.
///
/// # Panics
/// If `frame` does not contain exactly one `{}`, or through
/// [`assert_sql`](crate::assert_sql).
#[track_caller]
pub fn assert_frag<E: Expression + ?Sized>(frame: &str, e: &E, expected: &str) -> Vec<Value> {
    let (sql, args) = render(e);
    assert_frag_sql(frame, &sql, expected);
    args
}

/// [`assert_stmt`] for a caller that has already rendered.
///
/// Only for `keelson-core`'s in-module tests, which cannot name this crate's
/// `Expression` — see the module documentation. Render with
/// `keelson_core::testing::Numbered`, which writes what [`PgLike`] writes.
///
/// # Panics
/// Through [`assert_sql`](crate::assert_sql), naming which check failed.
#[track_caller]
pub fn assert_stmt_sql(produced: &str, expected: &str) {
    crate::assert_sql(JUDGE, produced, expected);
}

/// [`assert_frag`] for a caller that has already rendered.
///
/// # Panics
/// If `frame` does not contain exactly one `{}`, or through
/// [`assert_sql`](crate::assert_sql).
#[track_caller]
pub fn assert_frag_sql(frame: &str, produced: &str, expected: &str) {
    assert_eq!(
        frame.matches("{}").count(),
        1,
        "a frame needs exactly one {{}} to put the fragment in: {frame}"
    );
    crate::assert_sql(
        JUDGE,
        &frame.replace("{}", produced),
        &frame.replace("{}", expected),
    );
}

#[cfg(test)]
mod tests {
    use keelson_core::{Error, expr::Expr};

    use super::*;

    #[test]
    fn the_dialect_writes_postgresqls_spelling() {
        let (sql, args) = render(&Expr::join((
            Expr::ident(("users", "age")),
            Expr::raw("="),
            Expr::arg(21i32),
        )));
        assert_eq!(sql, r#""users"."age" = $1"#);
        assert_eq!(args, vec![Value::I32(21)]);
    }

    #[test]
    fn positions_run_past_nine_as_single_tokens() {
        let (sql, _) = render(&Expr::args((1..=11i32).collect::<Vec<_>>()));
        assert_eq!(sql, "$1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11");
    }

    #[test]
    fn there_are_no_named_arguments() {
        assert!(matches!(
            build(&PgLike, &Expr::named_arg("cutoff")),
            Err(Error::NoNamedArgs)
        ));
    }

    #[test]
    fn a_framed_fragment_is_judged_as_the_statement_it_completes() {
        let args = assert_frag(
            r#"SELECT "id" FROM users {}"#,
            &Expr::join((
                Expr::raw("WHERE"),
                Expr::group(Expr::join((
                    Expr::ident("age"),
                    Expr::raw(">="),
                    Expr::arg(21i32),
                ))),
            )),
            r#"WHERE ("age" >= $1)"#,
        );
        assert_eq!(args, vec![Value::I32(21)]);
    }

    /// The judge has to be reachable through the frame, or `assert_frag` would be
    /// a string comparison wearing a costume.
    #[test]
    #[should_panic(expected = "rejected the generated SQL")]
    fn a_fragment_that_ruins_the_statement_is_caught() {
        // Valid on its own as a select-list item; a syntax error where the frame
        // puts it.
        assert_frag(
            r#"SELECT "id" FROM users WHERE {}"#,
            &Expr::raw("ORDER BY"),
            "ORDER BY",
        );
    }

    #[test]
    #[should_panic(expected = "not what was expected")]
    fn a_valid_fragment_that_is_not_the_intended_one_is_caught() {
        assert_frag(
            r#"SELECT "id" FROM users {}"#,
            &Expr::raw(r#"WHERE "age" IS NULL"#),
            r#"WHERE "age" IS NOT NULL"#,
        );
    }

    #[test]
    #[should_panic(expected = "exactly one {}")]
    fn a_frame_without_a_hole_is_a_test_bug_not_a_pass() {
        assert_frag(
            "SELECT 1 FROM users",
            &Expr::raw("WHERE true"),
            "WHERE true",
        );
    }
}
