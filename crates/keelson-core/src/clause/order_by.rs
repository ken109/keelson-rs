use crate::error::Result;
use crate::writer::{DynExpr, Expression, SqlWriter};

/// `ORDER BY a, b DESC`
#[derive(Debug, Clone, Default)]
pub struct OrderBy {
    pub expressions: Vec<DynExpr>,
}

impl OrderBy {
    pub fn append_order(&mut self, order: DynExpr) {
        self.expressions.push(order);
    }

    pub fn clear_order_by(&mut self) {
        self.expressions.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.expressions.is_empty()
    }
}

impl Expression for OrderBy {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        w.write_slice(&self.expressions, "ORDER BY ", ", ", "")
    }
}

/// A query — or a [`Window`](super::Window) — with an `ORDER BY`.
pub trait HasOrderBy {
    fn order_by_mut(&mut self) -> &mut OrderBy;
}

impl HasOrderBy for OrderBy {
    fn order_by_mut(&mut self) -> &mut OrderBy {
        self
    }
}

/// One sort key: `expr [COLLATE c] [ASC | DESC | USING op] [NULLS FIRST|LAST]`.
///
/// `direction` is free text because PostgreSQL allows `USING <operator>`, which
/// no closed set of variants could hold.
#[derive(Debug, Clone, Default)]
pub struct OrderDef {
    pub expression: Option<DynExpr>,
    /// `ASC`, `DESC` or `USING <operator>`.
    pub direction: String,
    /// `FIRST` or `LAST`.
    pub nulls: String,
    /// A collation name, quoted on output.
    pub collation: String,
}

impl OrderDef {
    pub fn new(expression: DynExpr) -> Self {
        OrderDef {
            expression: Some(expression),
            ..OrderDef::default()
        }
    }
}

impl Expression for OrderDef {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        if let Some(e) = &self.expression {
            w.write_expr(e)?;
        }

        if !self.collation.is_empty() {
            w.push_str(" COLLATE ");
            w.push_quoted(&[&self.collation]);
        }

        if !self.direction.is_empty() {
            w.push_str(" ");
            w.push_str(&self.direction);
        }

        if !self.nulls.is_empty() {
            w.push_str(" NULLS ");
            w.push_str(&self.nulls);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::writer::{build, dyn_expr};

    #[test]
    fn an_empty_order_by_writes_nothing() {
        assert_eq!(build(&Numbered, &OrderBy::default()).unwrap().0, "");
    }

    #[test]
    fn a_bare_order_def_is_just_the_expression() {
        let o = OrderDef::new(dyn_expr("name"));
        assert_eq!(build(&Numbered, &o).unwrap().0, "name");
    }

    #[test]
    fn collation_comes_before_direction_and_nulls_last() {
        let o = OrderDef {
            collation: "NOCASE".into(),
            direction: "ASC".into(),
            nulls: "LAST".into(),
            ..OrderDef::new(dyn_expr("name"))
        };
        assert_eq!(
            build(&Numbered, &o).unwrap().0,
            r#"name COLLATE "NOCASE" ASC NULLS LAST"#
        );
    }

    #[test]
    fn direction_can_be_a_using_operator() {
        let o = OrderDef {
            direction: "USING >".into(),
            ..OrderDef::new(dyn_expr("name"))
        };
        assert_eq!(build(&Numbered, &o).unwrap().0, "name USING >");
    }

    #[test]
    fn keys_are_comma_separated() {
        let mut ob = OrderBy::default();
        ob.append_order(dyn_expr(OrderDef::new(dyn_expr("a"))));
        ob.append_order(dyn_expr(OrderDef {
            direction: "DESC".into(),
            ..OrderDef::new(dyn_expr("b"))
        }));
        assert_eq!(build(&Numbered, &ob).unwrap().0, "ORDER BY a, b DESC");

        ob.clear_order_by();
        assert_eq!(build(&Numbered, &ob).unwrap().0, "");
    }
}
