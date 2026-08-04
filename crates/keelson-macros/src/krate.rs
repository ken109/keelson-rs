//! Where the emitted paths point.
//!
//! The derives name absolute crate paths, and which name resolves depends on
//! how the *caller* depends on keelson:
//!
//! - directly on `keelson-core` / `keelson-exec` — `::keelson_exec` resolves;
//! - only on the `keelson` facade — it does **not**. A facade-only dependant
//!   has neither inner crate in its extern prelude, because a transitive
//!   dependency is not one. It reaches them as `keelson::core` and
//!   `keelson::exec`, which are the re-exports the facade exists to provide.
//!
//! Emitting a fixed `::keelson_exec` therefore breaks exactly the dependency
//! line the facade advertises. No test in this workspace caught that, because
//! every compilation context here — the macro crate's own tests, the examples
//! crate, even an integration test inside the facade — has the inner crates
//! available for its own reasons. `tests/facade-consumer` exists to be the one
//! that does not.
//!
//! `proc_macro_crate::crate_name` reads the *caller's* manifest, so the choice
//! is made per compilation rather than guessed, and a renamed dependency
//! (`keelson = { package = "keelson", ... }` under another name) still works.

use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};

/// The path to `keelson-core`, from wherever the derive was invoked.
pub(crate) fn core() -> TokenStream {
    resolve("keelson-core", "core")
}

/// The path to `keelson-exec`, from wherever the derive was invoked.
pub(crate) fn exec() -> TokenStream {
    resolve("keelson-exec", "exec")
}

fn resolve(direct: &str, facade_module: &str) -> TokenStream {
    // A direct dependency wins: it is the shorter path, it is what generated
    // code names, and it is what a crate depending on one layer alone has.
    match crate_name(direct) {
        Ok(FoundCrate::Itself) => return quote!(crate),
        Ok(FoundCrate::Name(name)) => {
            let ident = format_ident!("{}", name);
            return quote!(::#ident);
        }
        Err(_) => {}
    }

    // Otherwise reach it through the facade, under the name the caller gave it.
    if let Ok(found) = crate_name("keelson") {
        let facade = match found {
            FoundCrate::Itself => format_ident!("keelson"),
            FoundCrate::Name(name) => format_ident!("{}", name),
        };
        let module = format_ident!("{}", facade_module);
        return quote!(::#facade::#module);
    }

    // Neither is a dependency. Emit the plain name so that the compiler's error
    // names the crate that is actually missing, rather than something the
    // caller never wrote.
    let ident = format_ident!("{}", direct.replace('-', "_"));
    quote!(::#ident)
}
