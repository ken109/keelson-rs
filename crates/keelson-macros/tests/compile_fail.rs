//! The rejections, as compile errors.
//!
//! Error quality is the deliverable here, so the exact message and the exact
//! span are pinned rather than merely "this does not compile": the `.stderr`
//! files next to each case are the specification of what a user is told, and
//! `TRYBUILD=overwrite cargo test -p keelson-macros --test compile_fail`
//! rewrites them when a message is deliberately changed.
//!
//! trybuild rather than a hand-rolled `rustc` harness because it is already
//! this workspace's tool for the same job (`keelson-gen/tests/compile_fail.rs`
//! pins the override assertion), it normalises paths and rustc's own noise,
//! and one failed case prints the diff.

#[test]
fn every_rejection_names_the_offending_span_and_says_what_to_do() {
    let t = trybuild::TestCases::new();
    // #[derive(Bind)]
    t.compile_fail("tests/compile_fail/bind_shape.rs");
    t.compile_fail("tests/compile_fail/bind_options.rs");
    t.compile_fail("tests/compile_fail/bind_inner_does_not_bind.rs");
    // #[derive(FromRow)]
    t.compile_fail("tests/compile_fail/from_row_shape.rs");
    t.compile_fail("tests/compile_fail/from_row_options.rs");
    t.compile_fail("tests/compile_fail/from_row_duplicate_column.rs");
}
