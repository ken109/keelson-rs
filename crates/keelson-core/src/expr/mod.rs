//! Expression building blocks.
//!
//! Everything that goes *inside* a clause: bound arguments, quoted identifiers,
//! raw fragments with `?` placeholders, groups, operator chains, `CASE`, casts
//! and column lists.
//!
//! # How the pieces fit
//!
//! The constructors here return small concrete types — [`Args`], [`Quoted`],
//! [`LeftRight`], [`Join`] — and each renders exactly one thing. What turns them
//! into a fluent API is [`x`] and [`Chain`]:
//!
//! - [`x`] wraps an expression as the dialect's own expression type, adding
//!   parentheses unless the expression already prints as a self-contained unit.
//!   Every operator method routes through it, which is why the parentheses in
//!   `WHERE ("age" >= $1)` appear without anybody asking for them, and why they
//!   do *not* appear around a quoted identifier or an argument list. The rule is
//!   spelled out at [`is_self_contained`].
//! - [`Chain`] carries the operator methods. A dialect defines its own
//!   expression type, implements [`ExprBuilder`] for it, dereferences to
//!   `Chain<Self>` to inherit `eq` / `in_` / `between` / …, and adds whatever
//!   operators only it has. See [`ExprBuilder`] for the pattern.
//!
//! Rust keywords force three renames: `where` → `where_`, `in` → `in_`,
//! `as` → `as_`.
//!
//! ```
//! # use std::ops::Deref;
//! # use keelson_core::expr::{Chain, ExprBuilder};
//! # use keelson_core::{Dialect, DynExpr, Expression, Result, SqlWriter, Value, build};
//! # #[derive(Debug, Clone)]
//! # struct Expr(Chain<Expr>);
//! # impl ExprBuilder for Expr { fn new(base: DynExpr) -> Self { Expr(Chain::new(base)) } }
//! # impl Expression for Expr {
//! #     fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> { w.write_expr(&self.0) }
//! # }
//! # impl Deref for Expr { type Target = Chain<Expr>; fn deref(&self) -> &Chain<Expr> { &self.0 } }
//! # #[derive(Debug)]
//! # struct Psql;
//! # impl Dialect for Psql {
//! #     fn write_arg(&self, w: &mut String, position: usize) { w.push('$'); w.push_str(&position.to_string()); }
//! #     fn write_quoted(&self, w: &mut String, s: &str) { w.push('"'); w.push_str(s); w.push('"'); }
//! # }
//! let e = Expr::quote(("users", "age")).gte(Expr::arg(21));
//! let (sql, args) = build(&Psql, &e)?;
//! assert_eq!(sql, r#"("users"."age" >= $1)"#);
//! assert_eq!(args, vec![Value::I32(21)]);
//! # Ok::<_, keelson_core::Error>(())
//! ```

mod arg;
mod builder;
mod case;
mod cast;
mod chain;
mod columns;
mod constants;
mod group;
mod operators;
mod quote;
mod raw;
mod string;

pub use arg::{Args, arg, arg_group, args, placeholders};
pub use builder::{ExprBuilder, is_self_contained, not, x, x_all, x_group, x_raw};
pub use case::{CaseChain, CaseExpr};
pub use cast::{Cast, cast};
pub use chain::Chain;
pub use columns::ColumnsExpr;
pub use constants::{
    AND, BETWEEN, IS_DISTINCT_FROM, IS_NOT_DISTINCT_FROM, IS_NOT_NULL, IS_NULL, NOT, NOT_BETWEEN,
    NULL,
};
pub use group::{Group, group};
pub use operators::{Join, LeftRight, join, join_with, op};
pub use quote::{QuoteParts, Quoted, quote};
pub use raw::{Clause, Raw, RawArg, clause};
pub use string::{RawString, s};
