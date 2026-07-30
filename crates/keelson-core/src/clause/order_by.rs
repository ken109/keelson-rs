use std::borrow::Cow;

use crate::expr::{Expr, IntoExpr};
use crate::writer::{Expression, SqlWriter};

/// `ORDER BY a, b DESC`
#[derive(Debug, Clone, Default)]
pub struct OrderBy {
    /// The sort keys, in precedence order. Usually [`OrderDef`]s, but a bare
    /// expression is a sort key too.
    pub expressions: Vec<Expr>,
}

impl OrderBy {
    /// Append one sort key.
    pub fn append_order(&mut self, order: impl IntoExpr) {
        self.expressions.push(order.into_expr());
    }

    /// Drop every sort key. Needed because a mod may have to *replace* an
    /// inherited ordering rather than add to it.
    pub fn clear_order_by(&mut self) {
        self.expressions.clear();
    }

    /// Whether the clause is absent.
    pub fn is_empty(&self) -> bool {
        self.expressions.is_empty()
    }
}

impl Expression for OrderBy {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.write_slice(&self.expressions, "ORDER BY ", ", ", "");
    }
}

/// Anything with an `ORDER BY`: a statement, a [`Window`](super::Window)
/// definition, an aggregate's `ORDER BY` inside its own parentheses, or the
/// trailing ordering of a set operation ([`Combines`](super::Combines)).
pub trait HasOrderBy {
    /// The `ORDER BY` clause to modify.
    fn order_by_mut(&mut self) -> &mut OrderBy;
}

impl HasOrderBy for OrderBy {
    fn order_by_mut(&mut self) -> &mut OrderBy {
        self
    }
}

/// One sort key: `expr [COLLATE c] [ASC | DESC | USING op] [NULLS FIRST | LAST]`.
///
/// From PostgreSQL 17:
///
/// ```text
/// ORDER BY expression [ ASC | DESC | USING operator ] [ NULLS { FIRST | LAST } ]
/// ```
///
/// `COLLATE` is formally part of the expression rather than of the sort key, but it
/// has to be written between the expression and the direction, so it lives here.
#[derive(Debug, Clone, Default)]
pub struct OrderDef {
    /// What to sort by.
    pub expression: Option<Expr>,
    /// A collation name, quoted on output.
    pub collation: Option<Cow<'static, str>>,
    /// Ascending, descending, or by an operator.
    pub direction: Option<OrderDirection>,
    /// Where nulls sort. Defaults, in every dialect, to whichever end the
    /// direction puts last.
    pub nulls: Option<NullsPosition>,
}

impl OrderDef {
    /// A sort key with no modifiers.
    pub fn new(expression: impl IntoExpr) -> Self {
        OrderDef {
            expression: Some(expression.into_expr()),
            ..OrderDef::default()
        }
    }

    /// Whether there is nothing to sort by.
    pub fn is_empty(&self) -> bool {
        self.expression.is_none()
    }
}

impl Expression for OrderDef {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        let Some(expression) = &self.expression else {
            // A direction with nothing to apply it to is not a fragment.
            return;
        };
        w.write_expr(expression);

        if let Some(collation) = &self.collation {
            w.push_str(" COLLATE ");
            w.push_quoted(&[collation]);
        }

        match &self.direction {
            None => {}
            Some(OrderDirection::Asc) => w.push_str(" ASC"),
            Some(OrderDirection::Desc) => w.push_str(" DESC"),
            Some(OrderDirection::Using(op)) => {
                w.push_str(" USING ");
                w.push_str(op);
            }
        }

        if let Some(nulls) = &self.nulls {
            w.push_str(" NULLS ");
            w.push_str(nulls.as_str());
        }
    }
}

/// Which way a sort key sorts.
///
/// `Using` is why this is not a two-variant enum: PostgreSQL lets a sort key name
/// a `<` or `>` operator directly, which no closed set could hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrderDirection {
    /// `ASC`.
    Asc,
    /// `DESC`.
    Desc,
    /// PostgreSQL's `USING <operator>`, written verbatim after the keyword.
    Using(Cow<'static, str>),
}

/// Where nulls sort relative to everything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NullsPosition {
    /// `NULLS FIRST`.
    First,
    /// `NULLS LAST`.
    Last,
}

impl NullsPosition {
    /// The keyword, as written.
    pub fn as_str(self) -> &'static str {
        match self {
            NullsPosition::First => "FIRST",
            NullsPosition::Last => "LAST",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::expr::{arg, quote};
    use crate::value::Value;
    use crate::writer::build;

    #[test]
    fn an_empty_order_by_writes_nothing() {
        assert_eq!(build(&Numbered, &OrderBy::default()).unwrap().0, "");
        assert!(OrderBy::default().is_empty());
    }

    #[test]
    fn an_order_def_with_no_expression_writes_nothing() {
        let o = OrderDef {
            direction: Some(OrderDirection::Desc),
            ..OrderDef::default()
        };
        assert_eq!(build(&Numbered, &o).unwrap().0, "");
        assert!(o.is_empty());
    }

    #[test]
    fn a_bare_order_def_is_just_its_expression() {
        assert_eq!(
            build(&Numbered, &OrderDef::new(quote("name"))).unwrap().0,
            r#""name""#
        );
    }

    #[test]
    fn collation_precedes_the_direction_and_nulls_comes_last() {
        // PostgreSQL 17 sql-select: the sort key is
        //   expression [ COLLATE collation ] [ ASC | DESC ] [ NULLS … ]
        // and the collation is quoted because `bg-BG-x-icu` is not an identifier.
        let o = OrderDef {
            collation: Some("bg-BG-x-icu".into()),
            direction: Some(OrderDirection::Asc),
            nulls: Some(NullsPosition::Last),
            ..OrderDef::new(quote("name"))
        };
        assert_eq!(
            build(&Numbered, &o).unwrap().0,
            r#""name" COLLATE "bg-BG-x-icu" ASC NULLS LAST"#
        );
    }

    #[test]
    fn a_direction_can_be_an_operator() {
        let o = OrderDef {
            direction: Some(OrderDirection::Using(">".into())),
            nulls: Some(NullsPosition::First),
            ..OrderDef::new(quote("name"))
        };
        assert_eq!(
            build(&Numbered, &o).unwrap().0,
            r#""name" USING > NULLS FIRST"#
        );
    }

    #[test]
    fn keys_are_comma_separated_and_can_be_cleared() {
        let mut ob = OrderBy::default();
        ob.append_order(Expr::custom(OrderDef::new(quote("a"))));
        ob.append_order(Expr::custom(OrderDef {
            direction: Some(OrderDirection::Desc),
            ..OrderDef::new(quote("b"))
        }));
        // A sort key may also be an ordinal or any expression.
        ob.append_order("3");

        assert_eq!(
            build(&Numbered, &ob).unwrap().0,
            r#"ORDER BY "a", "b" DESC, 3"#
        );

        ob.clear_order_by();
        assert_eq!(build(&Numbered, &ob).unwrap().0, "");
    }

    #[test]
    fn a_sort_key_may_bind_an_argument() {
        let mut ob = OrderBy::default();
        ob.append_order(Expr::custom(OrderDef::new(Expr::func(
            "coalesce",
            (quote("a"), arg(0i32)),
        ))));
        let (sql, args) = build(&Numbered, &ob).unwrap();
        assert_eq!(sql, r#"ORDER BY coalesce("a", $1)"#);
        assert_eq!(args, vec![Value::I32(0)]);
    }
}
