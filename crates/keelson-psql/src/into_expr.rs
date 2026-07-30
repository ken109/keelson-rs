//! The three conversion traits that stand in for Go's `...any`.
//!
//! bob's mods take `any` and its function args take `...any`, and `bob.Express`
//! sorts out at runtime whether a value is an expression, a string or something
//! to `fmt.Sprint`. keelson does the same sorting at compile time:
//!
//! - [`IntoExpr`] is one expression, and everything that renders itself is one —
//!   `&'static str`, `String`, numbers, [`Expr`](crate::Expr), a sub-query, a
//!   generated column.
//! - [`Exprs`] is a *list* of them. Go's variadic call becomes a tuple, which is
//!   the same choice the mod API makes: `psql::f("generate_series", (1, 3))`.
//!   Arrays and `Vec`s work too, `()` is the empty list, and the handful of
//!   single values that appear on their own often enough to matter are accepted
//!   bare.
//! - [`Names`] is a list of identifiers or keywords, which are never expressions
//!   — column aliases, `USING` columns, locked table names.

use keelson_core::{DynExpr, Expression, dyn_expr};

use crate::expr::Expr;
use crate::function::Function;
use crate::query::Query;

/// Anything that can be used where one expression is expected.
pub trait IntoExpr {
    /// Erase this into a [`DynExpr`].
    fn into_expr(self) -> DynExpr;
}

impl<E: Expression + 'static> IntoExpr for E {
    fn into_expr(self) -> DynExpr {
        dyn_expr(self)
    }
}

/// A list of expressions: a tuple, an array, a `Vec`, `()`, or a single value.
pub trait Exprs {
    /// Erase this into the list a clause stores.
    fn into_exprs(self) -> Vec<DynExpr>;
}

/// No expressions at all — `psql::f("NOW", ())`.
impl Exprs for () {
    fn into_exprs(self) -> Vec<DynExpr> {
        Vec::new()
    }
}

impl<T: IntoExpr> Exprs for Vec<T> {
    fn into_exprs(self) -> Vec<DynExpr> {
        self.into_iter().map(IntoExpr::into_expr).collect()
    }
}

impl<T: IntoExpr, const N: usize> Exprs for [T; N] {
    fn into_exprs(self) -> Vec<DynExpr> {
        self.into_iter().map(IntoExpr::into_expr).collect()
    }
}

macro_rules! exprs_for_tuple {
    ($($name:ident),+) => {
        impl<$($name: IntoExpr),+> Exprs for ($($name,)+) {
            fn into_exprs(self) -> Vec<DynExpr> {
                #[allow(non_snake_case)]
                let ($($name,)+) = self;
                vec![$($name.into_expr()),+]
            }
        }
    };
}

exprs_for_tuple!(A);
exprs_for_tuple!(A, B);
exprs_for_tuple!(A, B, C);
exprs_for_tuple!(A, B, C, D);
exprs_for_tuple!(A, B, C, D, E);
exprs_for_tuple!(A, B, C, D, E, F);
exprs_for_tuple!(A, B, C, D, E, F, G);
exprs_for_tuple!(A, B, C, D, E, F, G, H);
exprs_for_tuple!(A, B, C, D, E, F, G, H, I);
exprs_for_tuple!(A, B, C, D, E, F, G, H, I, J);
exprs_for_tuple!(A, B, C, D, E, F, G, H, I, J, K);
exprs_for_tuple!(A, B, C, D, E, F, G, H, I, J, K, L);

/// A single value read as a one-element list.
///
/// This cannot be a blanket impl over [`IntoExpr`] — a tuple could itself be an
/// expression as far as coherence is concerned — so it is spelled out for the
/// types that actually turn up alone: `psql::quote("id").in_(psql::args(..))`
/// and `sm::columns("id")` both go through here. Any other single expression
/// needs the one-element tuple, `(e,)`.
macro_rules! exprs_for_single {
    ($($t:ty),+) => {
        $(
            impl Exprs for $t {
                fn into_exprs(self) -> Vec<DynExpr> {
                    vec![self.into_expr()]
                }
            }
        )+
    };
}

exprs_for_single!(&'static str, String, DynExpr, Expr, Function);

impl<Q: Expression + 'static> Exprs for Query<Q> {
    fn into_exprs(self) -> Vec<DynExpr> {
        vec![self.into_expr()]
    }
}

/// A list of identifiers or fixed keywords, written verbatim or dialect-quoted
/// by whichever clause holds them.
pub trait Names {
    /// The names, in order.
    fn into_names(self) -> Vec<String>;
}

/// No names — `sm::with("c", ())`.
impl Names for () {
    fn into_names(self) -> Vec<String> {
        Vec::new()
    }
}

impl Names for &str {
    fn into_names(self) -> Vec<String> {
        vec![self.to_owned()]
    }
}

impl Names for String {
    fn into_names(self) -> Vec<String> {
        vec![self]
    }
}

impl Names for &String {
    fn into_names(self) -> Vec<String> {
        vec![self.clone()]
    }
}

impl<S: Into<String>> Names for Vec<S> {
    fn into_names(self) -> Vec<String> {
        self.into_iter().map(Into::into).collect()
    }
}

impl<S: Into<String> + Clone, const N: usize> Names for [S; N] {
    fn into_names(self) -> Vec<String> {
        self.into_iter().map(Into::into).collect()
    }
}

impl Names for &[&str] {
    fn into_names(self) -> Vec<String> {
        self.iter().map(|s| (*s).to_owned()).collect()
    }
}

macro_rules! names_for_tuple {
    ($($name:ident),+) => {
        impl<$($name: Into<String>),+> Names for ($($name,)+) {
            fn into_names(self) -> Vec<String> {
                #[allow(non_snake_case)]
                let ($($name,)+) = self;
                vec![$($name.into()),+]
            }
        }
    };
}

names_for_tuple!(A);
names_for_tuple!(A, B);
names_for_tuple!(A, B, C);
names_for_tuple!(A, B, C, D);
names_for_tuple!(A, B, C, D, E);
names_for_tuple!(A, B, C, D, E, F);
