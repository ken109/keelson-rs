//! The PostgreSQL dialect.
//!
//! ```
//! use keelson_psql::{self as psql, sm};
//!
//! let q = psql::select((
//!     sm::from("users"),
//!     sm::where_(psql::quote("age").gte(psql::arg(21))),
//! ));
//!
//! let (sql, args) = q.build()?;
//! assert_eq!(sql, "SELECT \n*\nFROM users\nWHERE (\"age\" >= $1)\n");
//! # Ok::<_, keelson_core::Error>(())
//! ```
//!
//! # How it is laid out
//!
//! - [`select`] and the other starters ([`arg`], [`quote`], [`f`], [`case_`], …)
//!   are the entry points, re-exported at the crate root.
//! - [`sm`] holds the `SELECT` mods, [`wm`] the window mods, [`fm`] the function
//!   mods. `psql::select((sm::from("users"), sm::limit(10)))`.
//! - [`SelectQuery`] is the statement: named clause fields, all public. Mods
//!   reach them through the `Has*` traits, so most mods are generic over a clause
//!   rather than over a statement kind.
//! - [`Query`] wraps a statement with its dialect and decides how it nests. A
//!   statement used as a sub-query is parenthesised; used as a CTE body or a
//!   `UNION` operand it is not, because those clauses write their own
//!   parentheses.
//! - [`Expr`] is the expression type every operator returns. The shared operators
//!   come from [`keelson_core::expr::Chain`] through `Deref`; the
//!   PostgreSQL-only ones — `ILIKE`, `BETWEEN SYMMETRIC` — are inherent.
//!
//! Whitespace inside the generated SQL is bob's, down to the newlines and the
//! occasional double space. It is meaningless to PostgreSQL and it is what makes
//! the golden fixtures comparable.

mod dialect;
mod expr;
mod function;
mod into_expr;
mod query;
mod select;
mod starters;

pub mod fm;
pub mod mods;
pub mod sm;
pub mod wm;

pub use dialect::{PSQL, Psql};
pub use expr::Expr;
pub use function::{ColumnDef, Function, Functions};
pub use into_expr::{Exprs, IntoExpr, Names};
pub use query::{Bare, Query};
pub use select::{
    Distinct, HasCombinedFetch, HasCombinedLimit, HasCombinedOffset, HasCombinedOrder, HasDistinct,
    SelectQuery,
};
pub use starters::*;
