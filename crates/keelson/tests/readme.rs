//! The README is this crate's documentation, by symlink — so check the link.
//!
//! `crates/keelson/README.md` is a symlink to the repository's `README.md`, and
//! `lib.rs` pulls it in with `#![doc = include_str!("../README.md")]`. That is
//! what makes the README's worked example a doctest rather than a snippet that
//! rots, and what puts the same text on docs.rs and crates.io.
//!
//! It has one failure mode, and it is silent: a checkout without symlink
//! support (Git for Windows leaves `core.symlinks` off by default) writes the
//! *link target path* into the file. Everything still compiles — the crate
//! documentation becomes the string `../../README.md`, and the doctest simply
//! ceases to exist. Nothing fails, and the example stops being checked.
//!
//! So assert what the include actually got.

const README: &str = include_str!("../README.md");

#[test]
fn the_readme_symlink_resolved_to_the_readme() {
    assert!(
        README.starts_with("# keelson\n"),
        "crates/keelson/README.md did not resolve to the repository README — \
         it begins {:?}. If this is a checkout without symlink support, enable \
         it (`git config core.symlinks true` and re-checkout); the crate \
         documentation and the README's doctest both come through that link.",
        &README[..README.len().min(40)]
    );
}

#[test]
fn the_worked_example_is_still_in_it() {
    // The one code block that is compiled and run as a doctest. If it is
    // renamed, retagged (```rust -> ```text) or deleted, the README stops
    // being verified — which is the whole reason it lives here.
    assert!(
        README.contains("```rust\nuse keelson::exec::"),
        "the README's worked example is no longer a compiled ```rust block; \
         a README example that is not a doctest cannot be trusted to compile"
    );
}
