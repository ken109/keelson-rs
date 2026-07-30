//! Core primitives for keelson.
//!
//! Everything else — the three dialect crates, the generated models, the backend
//! adapters — is built out of what is defined here:
//!
//! - [`Value`], the bound argument. keelson carries its own value enum instead of
//!   being generic over a driver's parameter type, which keeps [`Expression`] free
//!   of type parameters and makes a built query's arguments inspectable.
//! - [`Dialect`], the per-database syntax decisions, and nothing else.
//! - [`Expression`], a fragment that can render itself, and [`SqlWriter`], which
//!   owns the SQL buffer, the argument list and the placeholder counter together
//!   so that nesting re-indexes for free.
//! - [`Mod`], the composition unit: a tuple of mods is a mod, and so is
//!   `Option<M>`, `Vec<M>` and `[M; N]`.
//! - [`Query`] and [`QueryType`], the little a runnable statement owes the layers
//!   above.
//!
//! Two properties are worth stating up front because the rest of the design leans
//! on them. Rendering is **infallible** — `write_sql` returns nothing, and the one
//! genuine failure (a named argument asked of a dialect without them) is recorded
//! on the writer and surfaced once, by [`build`]. And **no public type carries a
//! lifetime parameter**: identifiers and raw SQL are stored as
//! `Cow<'static, str>`, so a query type is `SelectQuery`, never `SelectQuery<'a>`.
//! The only lifetime in this crate is the transient one on [`SqlWriter`], which
//! borrows the dialect for the duration of a single build.
//!
//! Building is entirely synchronous and driver-independent: it produces a `String`
//! and a `Vec<Value>` and nothing more.
//!
//! ```
//! # use keelson_core::{Dialect, Expression, SqlWriter, Value, build};
//! #[derive(Debug)]
//! struct AgeAtLeast(i32);
//!
//! impl Expression for AgeAtLeast {
//!     fn write_sql(&self, w: &mut SqlWriter<'_>) {
//!         w.push_str("(");
//!         w.push_quoted(&["age"]);
//!         w.push_str(" >= ");
//!         w.push_arg(self.0);
//!         w.push_str(")");
//!     }
//! }
//!
//! # #[derive(Debug)]
//! # struct Psql;
//! # impl Dialect for Psql {
//! #     fn write_arg(&self, w: &mut SqlWriter<'_>, position: usize) {
//! #         w.push_str("$");
//! #         w.push_str(&position.to_string());
//! #     }
//! #     fn write_quoted(&self, w: &mut SqlWriter<'_>, s: &str) {
//! #         w.push_str("\"");
//! #         w.push_str(s);
//! #         w.push_str("\"");
//! #     }
//! # }
//! let (sql, args) = build(&Psql, &AgeAtLeast(21))?;
//! assert_eq!(sql, r#"("age" >= $1)"#);
//! assert_eq!(args, vec![Value::I32(21)]);
//! # Ok::<_, keelson_core::Error>(())
//! ```

#![warn(missing_docs)]

pub mod clause;
mod dialect;
mod error;
pub mod expr;
mod mods;
mod query;
mod value;
mod writer;

pub use dialect::Dialect;
pub use error::{Error, Result};
pub use mods::{BuildMod, Mod, ModFn, mod_fn};
pub use query::{Query, QueryExtensions, QueryType};
pub use value::{CustomValue, FromValue, ToValue, Value, from_value_array};

/// The derive macros, re-exported behind the `macros` feature.
///
/// [`Bind`](macro@Bind) writes the [`ToValue`]/[`FromValue`] pair for a
/// newtype — the bound a keelson-gen column override must satisfy.
/// [`FromRow`](macro@FromRow) maps a result row onto a struct; the trait it
/// implements lives in keelson-exec, which any user of it already depends on.
/// Both are documented in the keelson-macros crate.
///
/// `keelson_core::Bind` is the derive, `keelson_exec::Bind` the trait: two
/// namespaces, so importing both is fine.
#[cfg(feature = "macros")]
pub use keelson_macros::{Bind, FromRow};
pub use writer::{DynExpr, ExprFn, Expression, SqlWriter, build, build_from, dyn_expr, expr_fn};

/// Stand-in dialects for tests. See [`dialect::testing`].
#[cfg(any(test, feature = "testing"))]
pub use dialect::testing;
