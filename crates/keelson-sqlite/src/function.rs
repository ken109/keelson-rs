use std::borrow::Cow;

use keelson_core::clause::{HasOrderBy, OrderBy, Window};
use keelson_core::expr::{Expr, IntoExpr, IntoExprList};
use keelson_core::{Expression, Mod, SqlWriter};

/// A SQLite function call, with the decorations SQLite's grammar hangs off one.
///
/// From <https://www.sqlite.org/syntax/aggregate-function-invocation.html> and
/// <https://www.sqlite.org/syntax/window-function-invocation.html>:
///
/// ```text
/// name ( [ DISTINCT ] expr [, ...] [ ORDER BY ordering-term [, ...] ] | * )
///     [ FILTER ( WHERE expr ) ]
/// name ( expr [, ...] | * ) [ FILTER ( WHERE expr ) ]
///     OVER ( window-defn | window-name )
/// ```
///
/// That is the whole list, and it is shorter than PostgreSQL's. SQLite has **no**
/// `WITHIN GROUP (ORDER BY …)` — an ordered-set aggregate is not a thing it has —
/// and **no** column-definition list on a table-valued function, so neither
/// appears here. The aggregate `ORDER BY` inside the argument list needs SQLite
/// 3.44 or later.
///
/// ```
/// use keelson_sqlite::{f, quote, window};
///
/// // count(*) OVER (PARTITION BY "user_id")
/// let e = f("count", "*").over(window::partition_by(quote("user_id")));
/// ```
#[derive(Debug, Clone, Default)]
pub struct Function {
    name: Cow<'static, str>,
    args: Vec<Expr>,
    distinct: bool,
    order_by: OrderBy,
    filter: Vec<Expr>,
    over: Option<OverClause>,
}

/// What follows `OVER`.
///
/// The two forms are not interchangeable. `OVER window-name` **references** an
/// entry of the statement's `WINDOW` clause; `OVER ( … )` is a definition, and a
/// definition that starts with a base window name *copies* it — which SQLite
/// refuses when the base window has a frame specification. See
/// [`Function::over_name`].
#[derive(Debug, Clone)]
enum OverClause {
    /// `OVER "w"`.
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

    /// Add a sort key to the aggregate's own `ORDER BY`, inside the argument list:
    /// `group_concat("name" ORDER BY "id")`.
    ///
    /// SQLite 3.44 and later.
    #[must_use]
    pub fn order_by(mut self, order: impl IntoExpr) -> Function {
        self.order_by.append_order(order);
        self
    }

    /// Add a condition to `FILTER (WHERE …)`. Several are `AND`-joined.
    #[must_use]
    pub fn filter(mut self, condition: impl IntoExpr) -> Function {
        self.filter.push(condition.into_expr());
        self
    }

    /// Attach `OVER (…)`, built from [`window`](crate::window) and
    /// [`frame`](crate::frame) mods.
    ///
    /// Ends the builder, because `OVER` is the last thing in the grammar. `over(())`
    /// gives the legal `OVER ()`, which means the whole partition.
    ///
    /// To *reference* a window declared in the statement's `WINDOW` clause use
    /// [`over_name`](Self::over_name), not `over(window::based_on(..))` — the
    /// parenthesised form is the copying one.
    #[must_use]
    pub fn over(mut self, mods: impl Mod<Window>) -> Expr {
        let mut w = Window::default();
        mods.apply(&mut w);
        self.over = Some(OverClause::Definition(w));
        self.into_expr()
    }

    /// Attach `OVER "w"` — a reference to a window in the statement's `WINDOW`
    /// clause.
    ///
    /// Unparenthesised, which is what makes it a reference rather than a copy.
    #[must_use]
    pub fn over_name(mut self, name: impl Into<Cow<'static, str>>) -> Expr {
        self.over = Some(OverClause::Name(name.into()));
        self.into_expr()
    }

    /// `f(…) AS "alias"` — the result-column alias.
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

keelson_core::impl_clause_accessors!(Function {
    HasOrderBy => order_by_mut: OrderBy = order_by,
});

impl Expression for Function {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        if self.name.is_empty() {
            // A call with no name is not a fragment of anything.
            w.record_error(keelson_core::Error::Incomplete("the name of a function"));
            return;
        }

        w.push_str(&self.name);
        w.push_str("(");
        if self.distinct {
            w.push_str("DISTINCT ");
        }
        w.write_slice(&self.args, "", ", ", "");
        // `group_concat(x ORDER BY y)`: the separator is only needed when there is
        // an argument in front of it, and `f(ORDER BY x)` is not a thing.
        w.write_if(
            !self.order_by.is_empty() && !self.args.is_empty(),
            " ",
            &self.order_by,
            "",
        );
        w.push_str(")");

        w.write_slice(&self.filter, " FILTER (WHERE ", " AND ", ")");

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
    use crate::{Sqlite, arg, f, frame, quote, window};
    use keelson_core::build;

    fn sql(e: impl Expression) -> String {
        build(&Sqlite, &e).expect("render").0
    }

    #[test]
    fn a_plain_call_is_just_a_call() {
        assert_eq!(sql(f("date", ())), "date()");
        assert_eq!(sql(f("count", "*")), "count(*)");
    }

    /// <https://www.sqlite.org/syntax/aggregate-function-invocation.html>:
    /// `DISTINCT` and the aggregate's `ORDER BY` are both inside the parentheses.
    #[test]
    fn distinct_and_order_by_stay_inside_the_argument_list() {
        assert_eq!(
            sql(f("count", quote("id")).distinct()),
            r#"count(DISTINCT "id")"#
        );
        assert_eq!(
            sql(f("group_concat", quote("name")).order_by(quote("id"))),
            r#"group_concat("name" ORDER BY "id")"#
        );
    }

    #[test]
    fn filter_conditions_are_and_joined_inside_one_where() {
        assert_eq!(
            sql(f("count", "*").filter(quote("a")).filter(quote("b"))),
            r#"count(*) FILTER (WHERE "a" AND "b")"#
        );
    }

    /// <https://www.sqlite.org/syntax/window-function-invocation.html>:
    /// `FILTER` precedes `OVER`.
    #[test]
    fn filter_is_written_before_over() {
        assert_eq!(
            sql(f("count", "*")
                .filter(quote("a"))
                .over(window::partition_by(quote("b")))),
            r#"count(*) FILTER (WHERE "a") OVER (PARTITION BY "b")"#
        );
    }

    #[test]
    fn over_takes_a_definition_a_name_or_nothing() {
        assert_eq!(sql(f("row_number", ()).over(())), "row_number() OVER ()");
        assert_eq!(
            sql(f("avg", quote("views")).over_name("w")),
            r#"avg("views") OVER "w""#
        );
        // The copying form: legal, and refused by SQLite when "w" has a frame.
        assert_eq!(
            sql(f("avg", quote("views")).over(window::based_on("w"))),
            r#"avg("views") OVER ("w")"#
        );
        assert_eq!(
            sql(f("sum", quote("views")).over((
                window::partition_by(quote("user_id")),
                window::order_by(quote("id")),
                frame::rows(),
                frame::from_current_row(),
                frame::to_unbounded_following(),
            ))),
            r#"sum("views") OVER (PARTITION BY "user_id" ORDER BY "id" ROWS BETWEEN CURRENT ROW AND UNBOUNDED FOLLOWING)"#
        );
    }

    #[test]
    fn arguments_are_numbered_across_the_whole_call() {
        let (sql, args) = build(
            &Sqlite,
            &f("max", (arg(1i32), arg(2i32))).filter(quote("a")).over(()),
        )
        .unwrap();
        assert_eq!(sql, r#"max(?1, ?2) FILTER (WHERE "a") OVER ()"#);
        assert_eq!(args.len(), 2);
    }

    #[test]
    fn an_unnamed_call_is_a_recorded_failure() {
        let err = build(&Sqlite, &Function::default()).unwrap_err();
        // The substring names the SQL concept (a function's name), not the
        // message wording.
        assert!(
            matches!(&err, keelson_core::Error::Incomplete(what) if what.contains("function")),
            "got: {err}"
        );
    }
}
