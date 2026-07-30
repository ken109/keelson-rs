//! Function mods: what goes after a function's argument list.

use keelson_core::clause::Window;
use keelson_core::{Mod, mod_fn};

use crate::function::Function;
use crate::into_expr::Exprs;
use crate::mods::OrderMod;

/// `f(DISTINCT a)`.
pub fn distinct() -> impl Mod<Function> {
    mod_fn(move |f: &mut Function| f.distinct = true)
}

/// One `ORDER BY` term of an aggregate.
pub fn order_by(expression: impl crate::into_expr::IntoExpr) -> OrderMod {
    OrderMod::new(expression.into_expr())
}

/// `f(a) WITHIN GROUP (ORDER BY b)`: order outside the parentheses instead of
/// inside them.
pub fn within_group() -> impl Mod<Function> {
    mod_fn(move |f: &mut Function| f.within_group = true)
}

/// `f(a) FILTER (WHERE …)`.
pub fn filter(conditions: impl Exprs) -> impl Mod<Function> {
    let conditions = conditions.into_exprs();
    mod_fn(move |f: &mut Function| f.filter.extend(conditions))
}

/// `f(a) OVER (…)`.
///
/// `fm::over(())` is an empty window, which is valid and renders as `OVER ()`.
pub fn over(window_mods: impl Mod<Window>) -> impl Mod<Function> {
    let mut window = Window::default();
    window_mods.apply(&mut window);
    mod_fn(move |f: &mut Function| f.set_window(window))
}

/// The alias written before a column definition list: `f() AS alias (a INTEGER)`.
pub fn as_(alias: impl Into<String>) -> impl Mod<Function> {
    let alias = alias.into();
    mod_fn(move |f: &mut Function| f.alias = alias)
}

/// One entry of the column definition list: `f() AS (a INTEGER, b TEXT)`.
pub fn columns(name: impl Into<String>, data_type: impl Into<String>) -> impl Mod<Function> {
    let name = name.into();
    let data_type = data_type.into();
    mod_fn(move |f: &mut Function| f.append_column(name, data_type))
}
