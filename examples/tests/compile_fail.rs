//! **What does not compile** — the half of the examples that cannot be a
//! program, because the whole point is that it fails to build.
//!
//! `examples/*.rs` show what keelson does. These show what it refuses, and
//! they are run as tests so the refusals are regressions if they ever stop
//! happening. Each case is a tiny program under `tests/compile_fail/`, next to
//! the exact error message it must produce.
//!
//! The list is keelson's compile-time safety, stated as a list:
//!
//! - a typed column will not compare against the wrong Rust type
//! - a column that is not in the schema has no function to call
//! - a `SELECT`-only model has no `insert`/`update`/`delete`
//! - an engine that cannot do something is missing the method entirely, not
//!   failing at run time (MySQL and `RETURNING`)
//! - a relation load path is typed by the child model, so a wrong path is a
//!   type error rather than a query that silently loads nothing
//! - a transaction closure cannot commit or roll back the transaction it was
//!   handed
//!
//! Reading the `.stderr` files is the fastest tour of what the type system is
//! carrying. Regenerate them after an intentional change with
//! `TRYBUILD=overwrite cargo test -p keelson-examples --test compile_fail`.

#[test]
fn the_mistakes_keelson_refuses_to_compile() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/*.rs");
}
