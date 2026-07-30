//! Core primitives for keelson.
//!
//! Everything else — the three dialect crates, the generated models, the backend
//! adapters — is built out of five things defined here:
//!
//! - [`Value`], the bound argument. keelson carries its own value enum instead of
//!   being generic over a driver's parameter type, which keeps [`Expression`]
//!   free of type parameters and makes a built query's arguments inspectable.
//! - [`Dialect`], the three per-database syntax decisions.
//! - [`Expression`], a fragment that can render itself, and [`SqlWriter`], which
//!   owns the SQL buffer, the argument list and the placeholder counter together
//!   so that nesting re-indexes for free.
//! - [`Mod`], the composition unit: a tuple of mods is a mod.
//! - [`QueryType`], for the execution layer.
//!
//! Building is entirely synchronous and driver-independent — it produces a
//! `String` and a `Vec<Value>` and nothing more.
//!
//! ```
//! # use keelson_core::{Dialect, Expression, Result, SqlWriter, Value, build};
//! #[derive(Debug)]
//! struct AgeAtLeast(i32);
//!
//! impl Expression for AgeAtLeast {
//!     fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
//!         w.push_str("(");
//!         w.push_quoted(&["age"]);
//!         w.push_str(" >= ");
//!         w.push_arg(self.0);
//!         w.push_str(")");
//!         Ok(())
//!     }
//! }
//!
//! # #[derive(Debug)]
//! # struct Psql;
//! # impl Dialect for Psql {
//! #     fn write_arg(&self, w: &mut String, position: usize) {
//! #         w.push('$');
//! #         w.push_str(&position.to_string());
//! #     }
//! #     fn write_quoted(&self, w: &mut String, s: &str) {
//! #         w.push('"');
//! #         w.push_str(s);
//! #         w.push('"');
//! #     }
//! # }
//! let (sql, args) = build(&Psql, &AgeAtLeast(21))?;
//! assert_eq!(sql, r#"("age" >= $1)"#);
//! assert_eq!(args, vec![Value::I32(21)]);
//! # Ok::<_, keelson_core::Error>(())
//! ```

pub mod clause;
pub mod expr;

mod dialect;
mod error;
mod mods;
mod query;
mod value;
mod writer;

pub use dialect::Dialect;
pub use error::{Error, Result};
pub use mods::{BuildMod, Mod, ModFn, mod_fn};
pub use query::QueryType;
pub use value::{CustomValue, FromValue, ToValue, Value, from_value_array};
pub use writer::{DynExpr, ExprFn, Expression, SqlWriter, build, build_from, dyn_expr, expr_fn};
