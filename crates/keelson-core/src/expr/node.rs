use std::borrow::Cow;

use crate::value::{ToValue, Value};
use crate::writer::{DynExpr, Expression, SqlWriter, dyn_expr};

use super::convert::{IntoExpr, IntoExprList, IntoIdent};
use super::raw::{RawArg, write_template};

/// A SQL expression, as data.
///
/// Where bob threads `bob.Expression` — a Go interface — through every slot and
/// gives each shape its own unexported struct, keelson has one algebraic type.
/// The reasons, in the order they matter:
///
/// 1. **Expressions stay inspectable.** Layer 4 rewrites a parsed query into
///    clause-reconstruction code, and a rewriter needs to *look at* what it has.
///    A `Box<dyn Expression>` can only be rendered.
/// 2. **No dynamic dispatch on the hot path**, and `Clone` is a memcpy plus a
///    couple of refcount bumps.
/// 3. **One `match`** renders everything, so the spacing and separator decisions
///    that bob spreads over a dozen files sit in one screen where they can be
///    compared against the grammar.
///
/// The escape hatch is [`Expr::Custom`], which holds an erased
/// [`Expression`]. Dialect-specific shapes — PostgreSQL's `ROWS FROM`, MySQL's
/// index hints — live in their own crate as an ordinary `Expression` and travel
/// through core as `Custom`, so `keelson-core` never learns about them.
///
/// This enum is deliberately *not* `#[non_exhaustive]`: exhaustive matching is
/// the point, and a downstream rewriter that stops compiling when a variant is
/// added is being told something true.
///
/// # Strings
///
/// Identifiers, raw SQL and type names are `Cow<'static, str>`: a literal borrows
/// and costs nothing, a computed name is owned, and no lifetime parameter escapes
/// into any public type. Operators and separators are `&'static str` — they are
/// always literals, in core and in a dialect crate alike.
#[derive(Debug, Clone)]
pub enum Expr {
    /// SQL written out verbatim. `?` is *not* rewritten — use
    /// [`Expr::Template`] for that.
    ///
    /// This is bob's "progressive enhancement" in the enum: a hand-written
    /// fragment is a first-class expression, and keyword fragments like `AND` or
    /// `IS NULL` are nothing more than this.
    Raw(Cow<'static, str>),

    /// A single-quoted SQL string literal — bob's `S()`. Renders `'abc'`.
    ///
    /// Nothing is escaped, exactly as in bob. This is for keywords, enum labels
    /// and other SQL the program itself wrote; user input belongs in
    /// [`Expr::Arg`], where it is bound rather than interpolated.
    Literal(Cow<'static, str>),

    /// A dot-joined quoted identifier: `["users", "id"]` renders `"users"."id"`.
    ///
    /// Empty parts are skipped, so an unset qualifier needs no branch at the call
    /// site, and an entirely empty list renders nothing at all.
    Ident(Vec<Cow<'static, str>>),

    /// One bound argument, rendered as the dialect's placeholder.
    Arg(Value),

    /// Several bound arguments, comma-separated: `$1, $2, $3`.
    ///
    /// Not parenthesised — it is usually written into a slot that brings its own
    /// parentheses, such as `VALUES (..)`. Wrap it in [`Expr::Group`] when the
    /// parentheses are wanted; that is what [`super::arg_group`] does. An empty
    /// list renders `NULL`, matching bob.
    Args(Vec<Value>),

    /// A named argument placeholder, for preparing a statement whose values
    /// arrive at bind time.
    ///
    /// Binds nothing and consumes no positional slot. On a dialect with no named
    /// arguments this records [`Error::NoNamedArgs`](crate::Error::NoNamedArgs)
    /// on the writer, which [`build`](crate::build) then surfaces.
    NamedArg(Cow<'static, str>),

    /// Raw SQL whose `?` placeholders are rewritten to the dialect's own syntax,
    /// with `args` interleaved. See [`RawArg`] and [`super::template`].
    Template {
        /// The SQL, using `?` for every hole and `\?` for a literal question
        /// mark.
        sql: Cow<'static, str>,
        /// One replacement per `?`, in order.
        args: Vec<RawArg>,
    },

    /// A parenthesised, comma-separated list: `(a, b, c)`.
    ///
    /// One element is how a plain parenthesised expression is written. Empty
    /// renders `(NULL)`, matching bob — a row constructor with no columns is
    /// still a value.
    Group(Vec<Expr>),

    /// An infix operator: `lhs op rhs`, one space either side.
    Binary {
        /// Left operand.
        lhs: Box<Expr>,
        /// The operator, written verbatim between the operands.
        op: &'static str,
        /// Right operand.
        rhs: Box<Expr>,
    },

    /// A prefix operator: `op operand`. `NOT x`, `-x`.
    Prefix {
        /// The operator.
        op: &'static str,
        /// The operand.
        operand: Box<Expr>,
    },

    /// A postfix operator: `operand op`. `x IS NULL`, `x DESC`.
    Postfix {
        /// The operand.
        operand: Box<Expr>,
        /// The operator.
        op: &'static str,
    },

    /// A separator-joined sequence — bob's `Join`. Renders nothing when empty.
    ///
    /// This is the general-purpose "several fragments in a row" node: `AND`/`OR`
    /// chains, `BETWEEN a AND b`, a clause built out of keyword fragments.
    Join {
        /// The parts, in order.
        exprs: Vec<Expr>,
        /// Written between consecutive parts, verbatim. Use `" "` for bob's
        /// default; see [`Expr::join`].
        sep: &'static str,
    },

    /// A function call, optionally with an `OVER` window: `avg(x) OVER (w)`.
    ///
    /// Core keeps only what every dialect has. `DISTINCT`, `FILTER (WHERE ..)`
    /// and `WITHIN GROUP` are per-dialect and belong to a dialect's own function
    /// builder, reaching core through [`Expr::Custom`].
    Func {
        /// The function name, written verbatim — not quoted.
        name: Cow<'static, str>,
        /// The arguments, comma-separated.
        args: Vec<Expr>,
        /// The window definition or window name, rendered inside `OVER (..)`.
        /// An empty expression is meaningful: `OVER ()`.
        over: Option<Box<Expr>>,
    },

    /// `CASE WHEN c THEN t .. [ELSE e] END`.
    ///
    /// At least one `WHEN` is required; with none this records
    /// [`Error::Incomplete`](crate::Error::Incomplete) and writes nothing, which
    /// is bob's error turned into the recorded-failure form.
    Case {
        /// The `WHEN condition THEN result` pairs, in order.
        whens: Vec<(Expr, Expr)>,
        /// The `ELSE` branch.
        else_: Option<Box<Expr>>,
    },

    /// `CAST(expr AS type_name)`.
    Cast {
        /// The expression being cast.
        expr: Box<Expr>,
        /// The target type, written verbatim — `int`, `numeric(10, 2)`.
        type_name: Cow<'static, str>,
    },

    /// A dialect-specific expression core knows nothing about.
    ///
    /// The one place dynamic dispatch survives, and the reason core never needs a
    /// variant for `ROWS FROM`, `MATCH .. AGAINST` or anything else that belongs
    /// to exactly one grammar.
    Custom(DynExpr),
}

impl Expr {
    /// Raw SQL, verbatim. `?` is left alone.
    pub fn raw(sql: impl Into<Cow<'static, str>>) -> Expr {
        Expr::Raw(sql.into())
    }

    /// Raw SQL with `?` placeholders and their replacements.
    pub fn template(
        sql: impl Into<Cow<'static, str>>,
        args: impl IntoIterator<Item = RawArg>,
    ) -> Expr {
        Expr::Template {
            sql: sql.into(),
            args: args.into_iter().collect(),
        }
    }

    /// A single-quoted string literal — bob's `S()`.
    pub fn literal(s: impl Into<Cow<'static, str>>) -> Expr {
        Expr::Literal(s.into())
    }

    /// A quoted identifier. `ident("age")` and `ident(("users", "id"))` both
    /// work; see [`IntoIdent`].
    ///
    /// Empty parts are dropped here rather than at render time, so the stored
    /// node is exactly what will be written.
    pub fn ident(parts: impl IntoIdent) -> Expr {
        let mut parts = parts.into_ident_parts();
        parts.retain(|p| !p.is_empty());
        Expr::Ident(parts)
    }

    /// One bound argument.
    pub fn arg(v: impl ToValue) -> Expr {
        Expr::Arg(v.to_value())
    }

    /// A comma-separated list of bound arguments.
    pub fn args<V: ToValue>(vals: impl IntoIterator<Item = V>) -> Expr {
        Expr::Args(vals.into_iter().map(ToValue::to_value).collect())
    }

    /// `n` unbound placeholders — bob's `Placeholder(n)`.
    ///
    /// Each one binds `NULL`, so the shape of the statement is right and the
    /// values are supplied by whatever rebinds it.
    pub fn placeholders(n: usize) -> Expr {
        Expr::Args(vec![Value::Null; n])
    }

    /// A named argument placeholder.
    pub fn named_arg(name: impl Into<Cow<'static, str>>) -> Expr {
        Expr::NamedArg(name.into())
    }

    /// A parenthesised list. A single expression gives plain parentheses.
    pub fn group(items: impl IntoExprList) -> Expr {
        Expr::Group(items.into_expr_list())
    }

    /// An infix operator applied to two operands.
    pub fn binary(lhs: impl IntoExpr, op: &'static str, rhs: impl IntoExpr) -> Expr {
        Expr::Binary {
            lhs: Box::new(lhs.into_expr()),
            op,
            rhs: Box::new(rhs.into_expr()),
        }
    }

    /// A prefix operator.
    pub fn prefix(op: &'static str, operand: impl IntoExpr) -> Expr {
        Expr::Prefix {
            op,
            operand: Box::new(operand.into_expr()),
        }
    }

    /// A postfix operator.
    pub fn postfix(operand: impl IntoExpr, op: &'static str) -> Expr {
        Expr::Postfix {
            operand: Box::new(operand.into_expr()),
            op,
        }
    }

    /// Space-separated parts — bob's `Join` with its default separator.
    pub fn join(items: impl IntoExprList) -> Expr {
        Expr::join_with(" ", items)
    }

    /// Parts joined by `sep`, written verbatim.
    ///
    /// Unlike bob, an empty separator means an empty separator. bob silently
    /// substitutes a space for `Sep: ""`, which is a trap in a language where the
    /// zero value is what you get by leaving a field out; here the separator is
    /// always passed explicitly, and [`Expr::join`] is the space-separated form.
    pub fn join_with(sep: &'static str, items: impl IntoExprList) -> Expr {
        Expr::Join {
            exprs: items.into_expr_list(),
            sep,
        }
    }

    /// A function call with no window.
    pub fn func(name: impl Into<Cow<'static, str>>, args: impl IntoExprList) -> Expr {
        Expr::Func {
            name: name.into(),
            args: args.into_expr_list(),
            over: None,
        }
    }

    /// `CAST(expr AS type_name)`.
    pub fn cast(expr: impl IntoExpr, type_name: impl Into<Cow<'static, str>>) -> Expr {
        Expr::Cast {
            expr: Box::new(expr.into_expr()),
            type_name: type_name.into(),
        }
    }

    /// Wrap an arbitrary [`Expression`] so it can travel through core.
    pub fn custom(e: impl Expression + 'static) -> Expr {
        Expr::Custom(dyn_expr(e))
    }

    /// Whether this expression renders as a self-delimiting fragment, so that
    /// parentheses around it would add nothing.
    ///
    /// # The parenthesisation rule
    ///
    /// This predicate and [`grouped`](Self::grouped) are bob's `expr.X` — the
    /// single most output-visible decision in the whole library. Every operator
    /// in bob's chain wraps its result in parentheses *unless* the result is one
    /// of a small set of shapes, and that is precisely why bob emits
    /// `("id" = $1)` for an equality but `"users"."id"` for a column.
    ///
    /// Atomic, and so never wrapped:
    ///
    /// - [`Raw`](Expr::Raw) and [`Template`](Expr::Template) — the author wrote
    ///   the SQL and gets it back unedited.
    /// - [`Literal`](Expr::Literal) — `'abc'` is one token.
    /// - [`Ident`](Expr::Ident) — `"users"."id"` is one token.
    /// - [`Arg`](Expr::Arg), [`Args`](Expr::Args), [`NamedArg`](Expr::NamedArg) —
    ///   a placeholder list is normally written into a slot that supplies its own
    ///   parentheses, such as `VALUES (..)`.
    /// - [`Group`](Expr::Group) — already parenthesised.
    ///
    /// Everything else is wrapped, including [`Custom`](Expr::Custom): core
    /// cannot see inside it, and bob's fallback for an unrecognised expression is
    /// to wrap.
    ///
    /// bob has a further arm — an expression that is *already* a built chain
    /// value is returned unchanged — which has no counterpart here and needs
    /// none. Every chain step applies this rule to its own result, so a chain
    /// value is always `Group` or atomic by construction, and re-applying the
    /// rule is a no-op. That invariant is what makes `NOT ("a" = $1)` come out
    /// with one set of parentheses rather than two.
    pub fn is_atomic(&self) -> bool {
        matches!(
            self,
            Expr::Raw(_)
                | Expr::Template { .. }
                | Expr::Literal(_)
                | Expr::Ident(_)
                | Expr::Arg(_)
                | Expr::Args(_)
                | Expr::NamedArg(_)
                | Expr::Group(_)
        )
    }

    /// Parenthesise this expression unless it [`is_atomic`](Self::is_atomic).
    ///
    /// This is bob's `expr.X`, and every operator in [`Chain`](super::Chain)
    /// finishes with it.
    #[must_use]
    pub fn grouped(self) -> Expr {
        if self.is_atomic() {
            self
        } else {
            Expr::Group(vec![self])
        }
    }
}

impl Expression for Expr {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        match self {
            Expr::Raw(sql) => w.push_str(sql),

            Expr::Literal(s) => {
                w.push_str("'");
                w.push_str(s);
                w.push_str("'");
            }

            Expr::Ident(parts) => w.push_quoted(parts),

            Expr::Arg(v) => w.push_arg(v.clone()),

            // An empty list still has to render as a value; bob writes NULL and
            // so must we, or `VALUES ()` comes out as a syntax error.
            Expr::Args(vals) => {
                if vals.is_empty() {
                    w.push_str("NULL");
                }
                for (i, v) in vals.iter().enumerate() {
                    if i > 0 {
                        w.push_str(", ");
                    }
                    w.push_arg(v.clone());
                }
            }

            Expr::NamedArg(name) => w.push_named_arg(name),

            Expr::Template { sql, args } => write_template(w, sql, args),

            Expr::Group(items) => {
                if items.is_empty() {
                    w.push_str("(NULL)");
                } else {
                    w.write_slice(items, "(", ", ", ")");
                }
            }

            Expr::Binary { lhs, op, rhs } => {
                w.write_expr(&**lhs);
                w.push_str(" ");
                w.push_str(op);
                w.push_str(" ");
                w.write_expr(&**rhs);
            }

            Expr::Prefix { op, operand } => {
                w.push_str(op);
                w.push_str(" ");
                w.write_expr(&**operand);
            }

            Expr::Postfix { operand, op } => {
                w.write_expr(&**operand);
                w.push_str(" ");
                w.push_str(op);
            }

            Expr::Join { exprs, sep } => w.write_slice(exprs, "", sep, ""),

            Expr::Func { name, args, over } => {
                w.push_str(name);
                w.push_str("(");
                w.write_slice(args, "", ", ", "");
                w.push_str(")");
                // bob writes `avg(x)OVER (w)` with no space, which is legal but
                // reads like a typo. The space is free: the golden comparison
                // normalises whitespace next to parentheses, so both forms clean
                // to the same string.
                w.write_if_some(over.as_deref(), " OVER (", ")");
            }

            Expr::Case { whens, else_ } => {
                if whens.is_empty() {
                    // bob returns an error here and writes nothing. Rendering is
                    // infallible for us, so the failure is recorded and the
                    // fragment omitted rather than half-written.
                    w.record_error(crate::Error::Incomplete("a CASE WHEN branch"));
                    return;
                }
                w.push_str("CASE");
                for (cond, then) in whens {
                    w.push_str(" WHEN ");
                    w.write_expr(cond);
                    w.push_str(" THEN ");
                    w.write_expr(then);
                }
                w.write_if_some(else_.as_deref(), " ELSE ", "");
                w.push_str(" END");
            }

            Expr::Cast { expr, type_name } => {
                w.push_str("CAST(");
                w.write_expr(&**expr);
                w.push_str(" AS ");
                w.push_str(type_name);
                w.push_str(")");
            }

            Expr::Custom(e) => w.write_expr(&**e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::{Numbered, TestDialect};
    use crate::writer::build;

    fn sql(e: &Expr) -> String {
        build(&TestDialect, e).expect("render").0
    }

    // --- the parenthesisation rule, arm by arm -------------------------------

    #[test]
    fn raw_sql_is_never_parenthesised() {
        // The author wrote it; handing it back with parentheses added would be
        // editing it.
        let e = Expr::raw("a = 1");
        assert!(e.is_atomic());
        assert_eq!(sql(&e.clone().grouped()), "a = 1");
        assert_eq!(sql(&e), "a = 1");
    }

    #[test]
    fn a_template_is_never_parenthesised() {
        let e = Expr::template("a = ?", [RawArg::value(1i32)]);
        assert!(e.is_atomic());
        assert_eq!(sql(&e.grouped()), "a = ?1");
    }

    #[test]
    fn a_string_literal_is_never_parenthesised() {
        let e = Expr::literal("A");
        assert!(e.is_atomic());
        assert_eq!(sql(&e.grouped()), "'A'");
    }

    #[test]
    fn a_quoted_identifier_is_never_parenthesised() {
        let e = Expr::ident(("users", "id"));
        assert!(e.is_atomic());
        assert_eq!(sql(&e.grouped()), r#""users"."id""#);
    }

    #[test]
    fn placeholders_are_never_parenthesised() {
        // They land in slots that bring their own parentheses, such as VALUES.
        for e in [
            Expr::arg(1i32),
            Expr::args([1i32, 2]),
            Expr::named_arg("n"),
        ] {
            assert!(e.is_atomic(), "{e:?}");
        }
        assert_eq!(sql(&Expr::args([1i32, 2]).grouped()), "?1, ?2");
    }

    #[test]
    fn a_group_is_not_parenthesised_twice() {
        let e = Expr::group(Expr::binary(Expr::ident("a"), "=", Expr::arg(1i32)));
        assert!(e.is_atomic());
        assert_eq!(sql(&e.grouped()), r#"("a" = ?1)"#);
    }

    #[test]
    fn every_operator_shape_is_parenthesised() {
        let cases: Vec<(Expr, &str)> = vec![
            (
                Expr::binary(Expr::ident("a"), "=", Expr::arg(1i32)),
                r#"("a" = ?1)"#,
            ),
            (Expr::prefix("NOT", Expr::ident("a")), r#"(NOT "a")"#),
            (
                Expr::postfix(Expr::ident("a"), "IS NULL"),
                r#"("a" IS NULL)"#,
            ),
            (
                Expr::join([Expr::ident("a"), Expr::raw("DESC")]),
                r#"("a" DESC)"#,
            ),
            (Expr::func("NOW", ()), "(NOW())"),
            (
                Expr::cast(Expr::ident("a"), "int"),
                r#"(CAST("a" AS int))"#,
            ),
            (
                Expr::Case {
                    whens: vec![(Expr::raw("a"), Expr::literal("x"))],
                    else_: None,
                },
                "(CASE WHEN a THEN 'x' END)",
            ),
        ];
        for (e, expected) in cases {
            assert!(!e.is_atomic(), "{e:?} should not be atomic");
            assert_eq!(sql(&e.grouped()), expected);
        }
    }

    #[test]
    fn a_custom_expression_is_parenthesised_because_core_cannot_see_inside_it() {
        #[derive(Debug)]
        struct Opaque;
        impl Expression for Opaque {
            fn write_sql(&self, w: &mut SqlWriter<'_>) {
                w.push_str("x @> y");
            }
        }
        let e = Expr::custom(Opaque);
        assert!(!e.is_atomic());
        assert_eq!(sql(&e.grouped()), "(x @> y)");
    }

    #[test]
    fn grouping_is_idempotent_which_is_what_keeps_operators_from_nesting_parens() {
        let once = Expr::binary(Expr::ident("a"), "=", Expr::arg(1i32)).grouped();
        let twice = once.clone().grouped();
        assert_eq!(sql(&once), sql(&twice));
    }

    // --- rendering -----------------------------------------------------------

    #[test]
    fn an_empty_identifier_renders_nothing_and_empty_parts_are_dropped() {
        assert_eq!(sql(&Expr::ident(Vec::<String>::new())), "");
        assert_eq!(sql(&Expr::ident(["", "id"])), r#""id""#);
        assert!(matches!(Expr::ident(["", "id"]), Expr::Ident(p) if p.len() == 1));
    }

    #[test]
    fn an_empty_argument_list_renders_null() {
        assert_eq!(sql(&Expr::args(Vec::<i32>::new())), "NULL");
    }

    #[test]
    fn an_empty_group_renders_a_null_row() {
        assert_eq!(sql(&Expr::Group(vec![])), "(NULL)");
    }

    #[test]
    fn placeholders_bind_null_and_keep_their_positions() {
        let (s, args) = build(&Numbered, &Expr::placeholders(3)).unwrap();
        assert_eq!(s, "$1, $2, $3");
        assert!(args.iter().all(Value::is_null));
    }

    #[test]
    fn a_named_argument_binds_nothing_and_fails_where_unsupported() {
        let (s, args) = build(&TestDialect, &Expr::named_arg("name")).unwrap();
        assert_eq!(s, ":name");
        assert!(args.is_empty());
        assert!(matches!(
            build(&Numbered, &Expr::named_arg("name")),
            Err(crate::Error::NoNamedArgs)
        ));
    }

    #[test]
    fn a_function_call_renders_its_arguments_and_window() {
        assert_eq!(sql(&Expr::func("NOW", ())), "NOW()");
        assert_eq!(
            sql(&Expr::func("LEAD", ("created_date", 1, Expr::func("NOW", ())))),
            "LEAD(created_date, 1, NOW())"
        );
        assert_eq!(
            sql(&Expr::Func {
                name: "row_number".into(),
                args: vec![],
                over: Some(Box::new(Expr::raw(""))),
            }),
            "row_number() OVER ()"
        );
    }

    #[test]
    fn case_renders_both_with_and_without_an_else() {
        let with_else = Expr::Case {
            whens: vec![(
                Expr::binary(Expr::ident("id"), "=", Expr::literal("1")).grouped(),
                Expr::literal("A"),
            )],
            else_: Some(Box::new(Expr::literal("B"))),
        };
        assert_eq!(
            sql(&with_else),
            r#"CASE WHEN ("id" = '1') THEN 'A' ELSE 'B' END"#
        );

        let without = Expr::Case {
            whens: vec![(Expr::raw("a"), Expr::literal("A"))],
            else_: None,
        };
        assert_eq!(sql(&without), "CASE WHEN a THEN 'A' END");
    }

    #[test]
    fn a_case_with_no_branches_is_a_recorded_failure_not_a_broken_fragment() {
        let empty = Expr::Case {
            whens: vec![],
            else_: None,
        };
        let err = build(&TestDialect, &empty).unwrap_err();
        assert_eq!(err.to_string(), "query is missing a CASE WHEN branch");
    }

    #[test]
    fn join_uses_its_separator_verbatim() {
        let parts = [Expr::raw("a"), Expr::raw("b")];
        assert_eq!(sql(&Expr::join(parts.clone())), "a b");
        assert_eq!(sql(&Expr::join_with(" || ", parts.clone())), "a || b");
        assert_eq!(sql(&Expr::join_with("", parts)), "ab");
    }

    #[test]
    fn an_empty_join_renders_nothing_which_is_how_a_clause_omits_itself() {
        assert_eq!(sql(&Expr::join(Vec::<Expr>::new())), "");
    }

    #[test]
    fn nested_arguments_are_numbered_in_write_order() {
        let e = Expr::binary(
            Expr::group(Expr::args([1i32, 2])),
            "IN",
            Expr::group([Expr::group(Expr::args([3i32, 4])), Expr::arg(5i32)]),
        );
        let (s, args) = build(&Numbered, &e).unwrap();
        assert_eq!(s, "($1, $2) IN (($3, $4), $5)");
        assert_eq!(args.len(), 5);
    }
}
