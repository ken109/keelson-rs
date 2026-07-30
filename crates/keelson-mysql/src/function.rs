use std::borrow::Cow;

use keelson_core::clause::{HasOrderBy, OrderBy, Window};
use keelson_core::expr::{Expr, IntoExpr, IntoExprList};
use keelson_core::{Expression, Mod, SqlWriter};

/// A MySQL function call, with every decoration the grammar hangs off one.
///
/// From *14.19 Aggregate Functions* and *14.20 Window Functions*:
///
/// ```text
/// name([DISTINCT] expr [, expr] ... [ORDER BY …])
/// GROUP_CONCAT([DISTINCT] expr [, expr] ... [ORDER BY …] [SEPARATOR str_val])
/// name(…) OVER (window_spec) | name(…) OVER window_name
/// ```
///
/// **MySQL has no `FILTER (WHERE …)` and no `WITHIN GROUP`.** Both are on
/// PostgreSQL's function builder and neither is here.
///
/// [`Expr::Func`](keelson_core::expr::Expr::Func) carries only what all three
/// dialects share, so everything above lives here and reaches core through
/// [`Expr::Custom`](keelson_core::expr::Expr::Custom).
///
/// ```
/// use keelson_mysql::{f, quote, window};
///
/// // AVG(`views`) OVER (PARTITION BY `user_id`)
/// let e = f("AVG", quote("views")).over(window::partition_by(quote("user_id")));
/// ```
#[derive(Debug, Clone, Default)]
pub struct Function {
    name: Cow<'static, str>,
    args: Vec<Expr>,
    distinct: bool,
    order_by: OrderBy,
    separator: Option<Cow<'static, str>>,
    over: Option<OverClause>,
}

/// What follows `OVER`.
///
/// The two forms are different productions. `OVER window_name` **references** a
/// window from the statement's `WINDOW` clause; `OVER ( … )` is a definition, and a
/// definition that begins with an existing window's name *copies* it, which MySQL
/// refuses when that window has a frame clause:
///
/// ```text
/// ERROR 3581 (HY000): A window which depends on another cannot define partitioning.
/// ```
///
/// bob only ever writes the parenthesised form, so a named framed window is
/// unreachable there. [`Function::over_name`] is the other one.
#[derive(Debug, Clone)]
enum OverClause {
    /// `OVER \`w\``.
    Name(Cow<'static, str>),
    /// `OVER ( … )`.
    Definition(Window),
}

impl Function {
    /// A call to `name` with `args`.
    pub fn new(name: impl Into<Cow<'static, str>>, args: impl IntoExprList) -> Function {
        Function {
            name: name.into(),
            args: args.into_expr_list(),
            ..Function::default()
        }
    }

    /// `DISTINCT`, for an aggregate that should see each distinct input once.
    #[must_use]
    pub fn distinct(mut self) -> Function {
        self.distinct = true;
        self
    }

    /// Add a sort key to the aggregate's own `ORDER BY`, rendered inside the
    /// argument list: `GROUP_CONCAT(x ORDER BY y)`.
    #[must_use]
    pub fn order_by(mut self, order: impl IntoExpr) -> Function {
        self.order_by.append_order(order);
        self
    }

    /// `SEPARATOR 'str'`, which only `GROUP_CONCAT` takes.
    ///
    /// Written as a single-quoted literal with nothing escaped, exactly like
    /// [`s`](crate::s): this is for a separator the program itself chose.
    #[must_use]
    pub fn separator(mut self, separator: impl Into<Cow<'static, str>>) -> Function {
        self.separator = Some(separator.into());
        self
    }

    /// Attach `OVER (…)`, built from window mods — `mysql::window::*` and
    /// `mysql::frame::*`.
    ///
    /// Ends the builder, because `OVER` is the last thing in the grammar and
    /// nothing may follow it. `over(())` gives the legal `OVER ()`, which means the
    /// whole partition.
    #[must_use]
    pub fn over(mut self, mods: impl Mod<Window>) -> Expr {
        let mut w = Window::default();
        mods.apply(&mut w);
        self.over = Some(OverClause::Definition(w));
        self.into_expr()
    }

    /// Attach `OVER \`w\`` — a reference to a window in the statement's `WINDOW`
    /// clause.
    ///
    /// Unparenthesised, which is what makes it a reference rather than a copy.
    #[must_use]
    pub fn over_name(mut self, name: impl Into<Cow<'static, str>>) -> Expr {
        self.over = Some(OverClause::Name(name.into()));
        self.into_expr()
    }

    /// `f(…) AS \`alias\`` — the select-list alias.
    ///
    /// Ends the builder for the same reason
    /// [`Chain::as_`](keelson_core::expr::Chain::as_) does: an alias is not an
    /// operand.
    #[must_use]
    pub fn as_(self, alias: impl Into<Cow<'static, str>>) -> Expr {
        use keelson_core::expr::Chain as _;
        self.into_expr().as_(alias.into())
    }
}

impl HasOrderBy for Function {
    fn order_by_mut(&mut self) -> &mut OrderBy {
        &mut self.order_by
    }
}

impl Expression for Function {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        if self.name.is_empty() {
            // A call with no name is not a fragment of anything, and there is no
            // rendering of it that parses.
            w.record_error(keelson_core::Error::Incomplete("the name of a function"));
            return;
        }

        w.push_str(&self.name);
        w.push_str("(");
        if self.distinct {
            w.push_str("DISTINCT ");
        }
        w.write_slice(&self.args, "", ", ", "");
        // `GROUP_CONCAT(x ORDER BY y)`: the separator is only needed when there is
        // an argument in front of it, and `f(ORDER BY x)` is not a thing.
        w.write_if(
            !self.order_by.is_empty() && !self.args.is_empty(),
            " ",
            &self.order_by,
            "",
        );
        if let Some(separator) = &self.separator {
            w.push_str(" SEPARATOR '");
            w.push_str(separator);
            w.push_str("'");
        }
        w.push_str(")");

        match &self.over {
            None => {}
            Some(OverClause::Name(name)) => {
                w.push_str(" OVER ");
                w.push_quoted(&[name]);
            }
            Some(OverClause::Definition(window)) => {
                w.push_str(" OVER (");
                w.write_expr(window);
                w.push_str(")");
            }
        }
    }
}

impl IntoExpr for Function {
    fn into_expr(self) -> Expr {
        Expr::custom(self)
    }
}

impl IntoExprList for Function {
    fn into_expr_list(self) -> Vec<Expr> {
        vec![self.into_expr()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Mysql, arg, f, frame, quote, window};
    use keelson_core::build;

    fn sql(e: impl Expression) -> String {
        build(&Mysql, &e).expect("render").0
    }

    #[test]
    fn a_plain_call_is_just_a_call() {
        assert_eq!(sql(f("NOW", ())), "NOW()");
        assert_eq!(sql(f("COUNT", "*")), "COUNT(*)");
    }

    /// *14.19.1*: `DISTINCT` and the aggregate's own `ORDER BY` are both inside the
    /// argument parentheses, and `SEPARATOR` follows the `ORDER BY`.
    #[test]
    fn distinct_order_by_and_separator_all_stay_inside_the_argument_list() {
        assert_eq!(
            sql(f("COUNT", quote("id")).distinct()),
            "COUNT(DISTINCT `id`)"
        );
        assert_eq!(
            sql(f("GROUP_CONCAT", quote("name")).order_by(quote("id"))),
            "GROUP_CONCAT(`name` ORDER BY `id`)"
        );
        assert_eq!(
            sql(f("GROUP_CONCAT", quote("name"))
                .distinct()
                .order_by(quote("id"))
                .separator(", ")),
            "GROUP_CONCAT(DISTINCT `name` ORDER BY `id` SEPARATOR ', ')"
        );
    }

    #[test]
    fn over_takes_a_definition_a_name_or_nothing() {
        assert_eq!(sql(f("ROW_NUMBER", ()).over(())), "ROW_NUMBER() OVER ()");
        // The reference form has no parentheses; the copy form does.
        assert_eq!(
            sql(f("AVG", quote("views")).over_name("w")),
            "AVG(`views`) OVER `w`"
        );
        assert_eq!(
            sql(f("AVG", quote("views")).over(window::based_on("w"))),
            "AVG(`views`) OVER (`w`)"
        );
        assert_eq!(
            sql(f("SUM", quote("views")).over((
                window::partition_by(quote("user_id")),
                window::order_by(quote("id")),
                frame::rows(),
                frame::from_current_row(),
                frame::to_unbounded_following(),
            ))),
            "SUM(`views`) OVER (PARTITION BY `user_id` ORDER BY `id` \
             ROWS BETWEEN CURRENT ROW AND UNBOUNDED FOLLOWING)"
        );
    }

    #[test]
    fn an_alias_ends_the_builder_and_is_not_parenthesised() {
        assert_eq!(sql(f("COUNT", arg(1i32)).as_("n")), "COUNT(?) AS `n`");
    }

    #[test]
    fn an_unnamed_call_is_a_recorded_failure() {
        let err = build(&Mysql, &Function::default()).unwrap_err();
        // The substring names the SQL concept (a function's name), not the
        // message wording.
        assert!(
            matches!(&err, keelson_core::Error::Incomplete(what) if what.contains("function")),
            "got: {err}"
        );
    }
}
