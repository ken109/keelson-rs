use std::borrow::Cow;

use crate::value::Value;
use crate::writer::DynExpr;

use super::node::Expr;

/// Anything that can stand where an expression is expected.
///
/// This is bob's progressive enhancement, made a trait: a `&'static str` is raw
/// SQL, a number is a SQL literal, a [`Value`] is a bound argument, and an
/// [`Expr`] is itself. Every builder slot in keelson takes
/// `impl IntoExpr`, so the same call site accepts a hand-written fragment today
/// and a structured expression tomorrow with no change in shape.
///
/// # Which conversion each type gets, and why
///
/// | from | to | rationale |
/// |---|---|---|
/// | [`Expr`] | itself | |
/// | `&'static str`, `String`, `Cow<'static, str>` | [`Expr::Raw`] | bob writes a Go `string` verbatim |
/// | `i8`..`i64`, `u8`..`u64`, `f32`, `f64`, `bool` | [`Expr::Raw`] | bob's fallback formats any other value into the SQL, which is what makes `LIMIT 20` a literal |
/// | [`Value`] | [`Expr::Arg`] | a `Value` is data, and data is bound, never interpolated |
/// | [`DynExpr`] | [`Expr::Custom`] | the escape hatch for a dialect's own shape |
///
/// The split on the last two rows is the whole safety story: text that the
/// program wrote is SQL, and text that came from outside is a `Value`. There is
/// deliberately no impl turning `&str` into a bound string — that would make the
/// same call site mean two different things depending on the author's intent, and
/// bob does not do it either.
///
/// # Why `&'static str` and not `&str`
///
/// An [`Expr`] stores `Cow<'static, str>`, so that no public type needs a
/// lifetime parameter. A `&'static str` — which is what a literal is — borrows
/// for free; a shorter-lived `&str` would have to be copied on every conversion,
/// silently allocating for the overwhelmingly common literal case. Pass a
/// `String` when the text is computed.
pub trait IntoExpr {
    /// Perform the conversion.
    fn into_expr(self) -> Expr;
}

/// A list of expressions: a tuple, an array, a `Vec`, `()` for none, or a single
/// expression standing for a one-element list.
///
/// Function arguments, `IN (..)` operands, `AND` chains and row constructors all
/// take one of these, so `f("LEAD", ("created_date", 1, f("NOW", ())))` works
/// with a heterogeneous tuple while `in_(ids)` works with a `Vec`.
///
/// A tuple is how a *heterogeneous* list is written — Rust has no variadics, and
/// this is the same trick that makes a tuple of [`Mod`](crate::Mod)s a `Mod`.
pub trait IntoExprList {
    /// Perform the conversion.
    fn into_expr_list(self) -> Vec<Expr>;
}

/// The parts of a qualified identifier: `"age"`, or `("users", "id")`.
///
/// One entry point covers both, so a caller never has to decide between a
/// singular and a plural helper. Empty parts are dropped by
/// [`Expr::ident`](Expr::ident), which lets an unset table qualifier be passed
/// straight through.
pub trait IntoIdent {
    /// Perform the conversion.
    fn into_ident_parts(self) -> Vec<Cow<'static, str>>;
}

impl IntoExpr for Expr {
    fn into_expr(self) -> Expr {
        self
    }
}

impl IntoExprList for Expr {
    fn into_expr_list(self) -> Vec<Expr> {
        vec![self]
    }
}

/// No expressions at all — `f("NOW", ())`.
impl IntoExprList for () {
    fn into_expr_list(self) -> Vec<Expr> {
        Vec::new()
    }
}

impl IntoExpr for Value {
    fn into_expr(self) -> Expr {
        Expr::Arg(self)
    }
}

impl IntoExprList for Value {
    fn into_expr_list(self) -> Vec<Expr> {
        vec![Expr::Arg(self)]
    }
}

impl IntoExpr for DynExpr {
    fn into_expr(self) -> Expr {
        Expr::Custom(self)
    }
}

impl IntoExprList for DynExpr {
    fn into_expr_list(self) -> Vec<Expr> {
        vec![Expr::Custom(self)]
    }
}

/// Text is raw SQL, and doubles as a one-element list.
macro_rules! impl_from_text {
    ($($t:ty),+ $(,)?) => {
        $(
            impl IntoExpr for $t {
                fn into_expr(self) -> Expr {
                    Expr::Raw(self.into())
                }
            }

            impl IntoExprList for $t {
                fn into_expr_list(self) -> Vec<Expr> {
                    vec![Expr::Raw(self.into())]
                }
            }
        )+
    };
}

impl_from_text!(&'static str, String, Cow<'static, str>);

/// Numbers and booleans render as SQL literals, not as bound arguments — bob's
/// `fmt.Sprint` fallback, and the reason `limit(20)` is `LIMIT 20`. Wrap the
/// value in [`Expr::arg`] to bind it instead.
macro_rules! impl_from_scalar {
    ($($t:ty),+ $(,)?) => {
        $(
            impl IntoExpr for $t {
                fn into_expr(self) -> Expr {
                    Expr::Raw(Cow::Owned(self.to_string()))
                }
            }

            impl IntoExprList for $t {
                fn into_expr_list(self) -> Vec<Expr> {
                    vec![self.into_expr()]
                }
            }
        )+
    };
}

impl_from_scalar!(
    bool, i8, i16, i32, i64, isize, u8, u16, u32, u64, usize, f32, f64
);

impl<T: IntoExpr, const N: usize> IntoExprList for [T; N] {
    fn into_expr_list(self) -> Vec<Expr> {
        self.into_iter().map(IntoExpr::into_expr).collect()
    }
}

impl<T: IntoExpr> IntoExprList for Vec<T> {
    fn into_expr_list(self) -> Vec<Expr> {
        self.into_iter().map(IntoExpr::into_expr).collect()
    }
}

macro_rules! impl_expr_list_tuple {
    ($($name:ident),+) => {
        #[allow(non_snake_case)]
        impl<$($name: IntoExpr),+> IntoExprList for ($($name,)+) {
            fn into_expr_list(self) -> Vec<Expr> {
                let ($($name,)+) = self;
                vec![$($name.into_expr()),+]
            }
        }
    };
}

impl_expr_list_tuple!(A);
impl_expr_list_tuple!(A, B);
impl_expr_list_tuple!(A, B, C);
impl_expr_list_tuple!(A, B, C, D);
impl_expr_list_tuple!(A, B, C, D, E);
impl_expr_list_tuple!(A, B, C, D, E, F);
impl_expr_list_tuple!(A, B, C, D, E, F, G);
impl_expr_list_tuple!(A, B, C, D, E, F, G, H);
impl_expr_list_tuple!(A, B, C, D, E, F, G, H, I);
impl_expr_list_tuple!(A, B, C, D, E, F, G, H, I, J);
impl_expr_list_tuple!(A, B, C, D, E, F, G, H, I, J, K);
impl_expr_list_tuple!(A, B, C, D, E, F, G, H, I, J, K, L);
impl_expr_list_tuple!(A, B, C, D, E, F, G, H, I, J, K, L, M);
impl_expr_list_tuple!(A, B, C, D, E, F, G, H, I, J, K, L, M, N);
impl_expr_list_tuple!(A, B, C, D, E, F, G, H, I, J, K, L, M, N, O);
impl_expr_list_tuple!(A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P);

macro_rules! impl_ident_from_text {
    ($($t:ty),+ $(,)?) => {
        $(
            impl IntoIdent for $t {
                fn into_ident_parts(self) -> Vec<Cow<'static, str>> {
                    vec![self.into()]
                }
            }
        )+
    };
}

impl_ident_from_text!(&'static str, String, Cow<'static, str>);

impl<T: Into<Cow<'static, str>>, const N: usize> IntoIdent for [T; N] {
    fn into_ident_parts(self) -> Vec<Cow<'static, str>> {
        self.into_iter().map(Into::into).collect()
    }
}

impl<T: Into<Cow<'static, str>>> IntoIdent for Vec<T> {
    fn into_ident_parts(self) -> Vec<Cow<'static, str>> {
        self.into_iter().map(Into::into).collect()
    }
}

macro_rules! impl_ident_tuple {
    ($($name:ident),+) => {
        #[allow(non_snake_case)]
        impl<$($name: Into<Cow<'static, str>>),+> IntoIdent for ($($name,)+) {
            fn into_ident_parts(self) -> Vec<Cow<'static, str>> {
                let ($($name,)+) = self;
                vec![$($name.into()),+]
            }
        }
    };
}

// A schema, a table, a column and an attribute is as deep as SQL goes.
impl_ident_tuple!(A);
impl_ident_tuple!(A, B);
impl_ident_tuple!(A, B, C);
impl_ident_tuple!(A, B, C, D);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::writer::build;

    fn rendered(e: Expr) -> (String, Vec<Value>) {
        build(&Numbered, &e).expect("render")
    }

    #[test]
    fn text_becomes_raw_sql() {
        for e in [
            "id = 1".into_expr(),
            String::from("id = 1").into_expr(),
            Cow::Borrowed("id = 1").into_expr(),
        ] {
            assert!(matches!(e, Expr::Raw(_)));
            assert_eq!(rendered(e).0, "id = 1");
        }
    }

    #[test]
    fn numbers_become_literals_and_values_become_arguments() {
        let (sql, args) = rendered(20i64.into_expr());
        assert_eq!(sql, "20");
        assert!(args.is_empty(), "a literal binds nothing");

        let (sql, args) = rendered(Value::I64(20).into_expr());
        assert_eq!(sql, "$1");
        assert_eq!(args, vec![Value::I64(20)]);
    }

    #[test]
    fn booleans_and_floats_render_as_written() {
        assert_eq!(rendered(true.into_expr()).0, "true");
        assert_eq!(rendered(1.5f64.into_expr()).0, "1.5");
    }

    #[test]
    fn a_list_can_be_a_tuple_an_array_a_vec_or_nothing() {
        assert_eq!(().into_expr_list().len(), 0);
        assert_eq!(("a", 1, Value::I32(2)).into_expr_list().len(), 3);
        assert_eq!(["a", "b"].into_expr_list().len(), 2);
        assert_eq!(vec![1i32, 2, 3].into_expr_list().len(), 3);
        assert_eq!(Expr::raw("a").into_expr_list().len(), 1);
        assert_eq!("a".into_expr_list().len(), 1);
    }

    #[test]
    fn a_heterogeneous_tuple_keeps_each_conversion() {
        let list = ("created_date", 1i32, Value::Text("x".into())).into_expr_list();
        assert!(matches!(list[0], Expr::Raw(_)));
        assert!(matches!(list[1], Expr::Raw(_)));
        assert!(matches!(list[2], Expr::Arg(_)));
    }

    #[test]
    fn an_identifier_takes_one_part_or_several() {
        assert_eq!(rendered(Expr::ident("age")).0, r#""age""#);
        assert_eq!(rendered(Expr::ident(("users", "id"))).0, r#""users"."id""#);
        assert_eq!(
            rendered(Expr::ident(["public", "users", "id"])).0,
            r#""public"."users"."id""#
        );
        assert_eq!(
            rendered(Expr::ident(vec![String::from("a"), String::from("b")])).0,
            r#""a"."b""#
        );
        // An unset qualifier needs no branch at the call site.
        assert_eq!(rendered(Expr::ident(("", "id"))).0, r#""id""#);
    }

    #[test]
    fn an_erased_expression_arrives_as_custom() {
        let e: DynExpr = crate::writer::dyn_expr("raw bits");
        assert!(matches!(e.clone().into_expr(), Expr::Custom(_)));
        assert_eq!(rendered(e.into_expr()).0, "raw bits");
    }
}
