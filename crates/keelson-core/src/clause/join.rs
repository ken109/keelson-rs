use crate::error::Result;
use crate::writer::{DynExpr, Expression, SqlWriter};

use super::from::TableRef;

pub const INNER_JOIN: &str = "INNER JOIN";
pub const LEFT_JOIN: &str = "LEFT JOIN";
pub const RIGHT_JOIN: &str = "RIGHT JOIN";
pub const FULL_JOIN: &str = "FULL JOIN";
pub const CROSS_JOIN: &str = "CROSS JOIN";
/// MySQL only.
pub const STRAIGHT_JOIN: &str = "STRAIGHT_JOIN";

/// `[NATURAL] <kind> <table> [ON …] [USING(…)]`
///
/// `kind` is a plain string rather than an enum: MySQL adds `STRAIGHT_JOIN`,
/// generated code composes join keywords, and there is no behaviour attached to
/// the choice. The constants above are the vocabulary the dialect crates use.
#[derive(Debug, Clone, Default)]
pub struct Join {
    pub kind: String,
    pub to: TableRef,

    pub natural: bool,
    pub on: Vec<DynExpr>,
    /// Column names, quoted on output.
    pub using: Vec<String>,
}

impl Expression for Join {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        if self.natural {
            w.push_str("NATURAL ");
        }

        w.push_str(&self.kind);
        w.push_str(" ");

        w.write_expr(&self.to)?;
        w.write_slice(&self.on, " ON ", " AND ", "")?;

        // `USING` closes with a trailing space, which bob emits and the recorded
        // fixtures contain.
        if !self.using.is_empty() {
            w.push_str(" USING(");
            for (i, col) in self.using.iter().enumerate() {
                if i > 0 {
                    w.push_str(", ");
                }
                w.push_quoted(&[col]);
            }
            w.push_str(") ");
        }

        Ok(())
    }
}

/// A query — or a [`TableRef`] — that joins can be appended to.
pub trait HasJoins {
    fn joins_mut(&mut self) -> &mut Vec<Join>;
}

impl HasJoins for TableRef {
    fn joins_mut(&mut self) -> &mut Vec<Join> {
        &mut self.joins
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::writer::{build, dyn_expr, expr_fn};

    fn to(table: &'static str) -> TableRef {
        TableRef::new(dyn_expr(table))
    }

    #[test]
    fn conditions_are_and_separated_after_on() {
        let j = Join {
            kind: INNER_JOIN.into(),
            to: to("pilots"),
            on: vec![dyn_expr("a = b"), dyn_expr("c = d")],
            ..Join::default()
        };
        assert_eq!(
            build(&Numbered, &j).unwrap().0,
            "INNER JOIN pilots ON a = b AND c = d"
        );
    }

    #[test]
    fn a_cross_join_has_neither_on_nor_using() {
        let j = Join {
            kind: CROSS_JOIN.into(),
            to: to("jets"),
            ..Join::default()
        };
        assert_eq!(build(&Numbered, &j).unwrap().0, "CROSS JOIN jets");
    }

    #[test]
    fn using_columns_are_quoted_and_leave_a_trailing_space() {
        let j = Join {
            kind: LEFT_JOIN.into(),
            to: to("test2"),
            using: vec!["id".into(), "kind".into()],
            ..Join::default()
        };
        assert_eq!(
            build(&Numbered, &j).unwrap().0,
            r#"LEFT JOIN test2 USING("id", "kind") "#
        );
    }

    #[test]
    fn natural_precedes_the_join_kind() {
        let j = Join {
            kind: LEFT_JOIN.into(),
            to: to("test2"),
            natural: true,
            ..Join::default()
        };
        assert_eq!(build(&Numbered, &j).unwrap().0, "NATURAL LEFT JOIN test2");
    }

    #[test]
    fn a_joined_sub_select_shares_the_placeholder_run() {
        let j = Join {
            kind: CROSS_JOIN.into(),
            to: TableRef {
                alias: "clients".into(),
                ..TableRef::new(dyn_expr(expr_fn(|w: &mut SqlWriter<'_>| {
                    w.push_str("(SELECT id FROM clients WHERE client_id = ");
                    w.push_arg(7i32);
                    w.push_str(")");
                    Ok(())
                })))
            },
            on: vec![dyn_expr(expr_fn(|w: &mut SqlWriter<'_>| {
                w.push_str("x = ");
                w.push_arg(8i32);
                Ok(())
            }))],
            ..Join::default()
        };
        let (sql, args) = build(&Numbered, &j).unwrap();
        assert_eq!(
            sql,
            r#"CROSS JOIN (SELECT id FROM clients WHERE client_id = $1) AS "clients" ON x = $2"#
        );
        assert_eq!(args.len(), 2);
    }
}
