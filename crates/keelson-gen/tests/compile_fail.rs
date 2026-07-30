//! The compile-error half of the override contract: a `[[types.override]]`
//! whose type cannot bind is a *compile* error at the emitted `assert_bind`
//! line. `config_effects.rs` pins that the generator emits exactly that
//! line; this pins that the line fails for a non-binding type.

#[test]
fn a_non_binding_override_fails_to_compile_at_the_named_line() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/nonbinding_override.rs");
}
