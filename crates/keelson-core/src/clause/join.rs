use std::borrow::Cow;

use crate::expr::{Expr, IntoExpr};
use crate::writer::{Expression, SqlWriter};

use super::from::TableRef;
use super::{MaybeAbsent, write_quoted_list};

/// `[NATURAL] <kind> <table> [ON a AND b] [USING (cols)]`
///
/// From PostgreSQL 17's `from_item`:
///
/// ```text
/// from_item [ NATURAL ] join_type from_item
///     [ ON join_condition | USING ( join_column [, ...] ) ]
/// ```
///
/// `ON` and `USING` are alternatives, and `NATURAL` excludes both; nothing here
/// enforces that, because the check belongs to the mods that build a join — a
/// dialect exposes `join::on(..)` and `join::using(..)` as separate mods and the
/// caller picks one.
#[derive(Debug, Clone, Default)]
pub struct Join {
    /// Which join.
    pub kind: JoinKind,
    /// What is being joined to, with all of its own decorations.
    pub to: TableRef,
    /// `NATURAL`, which derives the join columns from the two items' names.
    pub natural: bool,
    /// `ON` conditions, `AND`-joined.
    pub on: Vec<Expr>,
    /// `USING` columns, quoted on output.
    pub using: Vec<Cow<'static, str>>,
}

impl Join {
    /// A join of `kind` onto `to`, with no condition yet.
    pub fn new(kind: JoinKind, to: TableRef) -> Self {
        Join {
            kind,
            to,
            ..Join::default()
        }
    }

    /// Append an `ON` condition.
    pub fn append_on(&mut self, condition: impl IntoExpr) {
        self.on.push(condition.into_expr());
    }

    /// Append `USING` columns.
    pub fn append_using(
        &mut self,
        columns: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) {
        self.using.extend(columns.into_iter().map(Into::into));
    }

    /// Whether there is nothing to join to, so that nothing will be written.
    pub fn is_empty(&self) -> bool {
        self.to.is_empty()
    }
}

impl Expression for Join {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        if self.to.is_empty() {
            // A join keyword with no table is not a fragment of anything.
            return;
        }

        if self.natural {
            w.push_str("NATURAL ");
        }
        w.push_str(self.kind.as_str());
        w.push_str(" ");
        w.write_expr(&self.to);

        w.write_slice(&self.on, " ON ", " AND ", "");
        write_quoted_list(w, &self.using, " USING (", ", ", ")");
    }
}

/// Anything joins can be appended to: a [`TableRef`], or a statement that keeps
/// its joins beside its table rather than on it.
pub trait HasJoins {
    /// The join list to modify.
    fn joins_mut(&mut self) -> &mut Vec<Join>;
}

impl HasJoins for TableRef {
    fn joins_mut(&mut self) -> &mut Vec<Join> {
        &mut self.joins
    }
}

impl HasJoins for Vec<Join> {
    fn joins_mut(&mut self) -> &mut Vec<Join> {
        self
    }
}

/// The `join_type` of a join.
///
/// Closed in the SQL standard, and left open at one point because MySQL's
/// `STRAIGHT_JOIN` sits in exactly this slot without being a standard join type.
/// The `OUTER` in `LEFT OUTER JOIN` is noise — the standard makes it optional and
/// means the same thing — so it is not spelled out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum JoinKind {
    /// `INNER JOIN`. The default, matching SQL's own default for a bare `JOIN`.
    #[default]
    Inner,
    /// `LEFT JOIN`.
    Left,
    /// `RIGHT JOIN`.
    Right,
    /// `FULL JOIN`.
    Full,
    /// `CROSS JOIN`, which takes neither `ON` nor `USING`.
    Cross,
    /// A dialect's own join keyword, written verbatim — MySQL's `STRAIGHT_JOIN`.
    Custom(Cow<'static, str>),
}

impl JoinKind {
    /// The keyword, as written.
    pub fn as_str(&self) -> &str {
        match self {
            JoinKind::Inner => "INNER JOIN",
            JoinKind::Left => "LEFT JOIN",
            JoinKind::Right => "RIGHT JOIN",
            JoinKind::Full => "FULL JOIN",
            JoinKind::Cross => "CROSS JOIN",
            JoinKind::Custom(kind) => kind,
        }
    }
}

impl MaybeAbsent for Join {
    fn is_absent(&self) -> bool {
        self.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use keelson_sqlcheck::testing::assert_frag_sql;

    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::expr::{Chain, arg, quote};
    use crate::value::Value;
    use crate::writer::build;

    /// A join is a fragment of a `FROM`, so this is the statement it extends.
    const FRAME: &str = "SELECT * FROM users {}";

    fn to(table: &'static str) -> TableRef {
        TableRef::new(quote(table))
    }

    fn sql(j: &impl Expression) -> String {
        build(&Numbered, j).expect("render").0
    }

    #[test]
    fn a_join_with_nothing_to_join_to_writes_nothing() {
        assert_frag_sql(FRAME, &sql(&Join::default()), "");
        assert!(Join::default().is_empty());
    }

    #[test]
    fn conditions_are_and_separated_after_one_on() {
        // PostgreSQL 17: `ON join_condition` takes a single boolean expression, so
        // several appended conditions become one conjunction rather than several
        // ON clauses.
        let mut j = Join::new(JoinKind::Inner, to("posts"));
        j.append_on(quote(("users", "id")).eq(quote(("posts", "user_id"))));
        j.append_on(quote(("posts", "status")).eq(arg("published")));

        let (rendered, args) = build(&Numbered, &j).unwrap();
        assert_frag_sql(
            FRAME,
            &rendered,
            r#"INNER JOIN "posts" ON ("users"."id" = "posts"."user_id") AND ("posts"."status" = $1)"#,
        );
        assert_eq!(args, vec![Value::Text("published".into())]);
    }

    #[test]
    fn using_columns_are_quoted_and_parenthesised() {
        // `users` and `tags` share both `id` and `name`, which is what a
        // two-column USING needs to resolve.
        let mut j = Join::new(JoinKind::Left, to("tags"));
        j.append_using(["id", "name"]);
        assert_frag_sql(FRAME, &sql(&j), r#"LEFT JOIN "tags" USING ("id", "name")"#);
    }

    #[test]
    fn a_cross_join_carries_neither_on_nor_using() {
        assert_frag_sql(
            FRAME,
            &sql(&Join::new(JoinKind::Cross, to("tags"))),
            r#"CROSS JOIN "tags""#,
        );
    }

    #[test]
    fn natural_precedes_the_join_kind() {
        // PostgreSQL 17: `from_item [ NATURAL ] join_type from_item`.
        let j = Join {
            natural: true,
            ..Join::new(JoinKind::Full, to("posts"))
        };
        assert_frag_sql(FRAME, &sql(&j), r#"NATURAL FULL JOIN "posts""#);
    }

    #[test]
    fn every_kind_has_its_standard_spelling() {
        // The frame supplies the `ON`, because every one of these kinds requires a
        // join condition — `CROSS JOIN` is the exception and has its own case.
        for (kind, keyword) in [
            (JoinKind::Inner, "INNER JOIN"),
            (JoinKind::Left, "LEFT JOIN"),
            (JoinKind::Right, "RIGHT JOIN"),
            (JoinKind::Full, "FULL JOIN"),
        ] {
            assert_frag_sql(
                "SELECT * FROM users {} ON true",
                &sql(&Join::new(kind, to("posts"))),
                &format!(r#"{keyword} "posts""#),
            );
        }

        // MySQL's `STRAIGHT_JOIN` takes the place of the keyword entirely. Not
        // framed: PostgreSQL has no such join type, so the psql judge would reject
        // valid SQL. Its own crate checks it against MySQL.
        assert_eq!(
            build(
                &Numbered,
                &Join::new(JoinKind::Custom("STRAIGHT_JOIN".into()), to("posts"))
            )
            .unwrap()
            .0,
            r#"STRAIGHT_JOIN "posts""#
        );
        assert_eq!(JoinKind::default(), JoinKind::Inner);
    }

    #[test]
    fn a_join_carries_the_whole_table_ref_including_its_own_joins() {
        // The recursion in PostgreSQL's grammar — a from_item may itself be a join
        // — is what lets `a JOIN b JOIN c` be expressed at all. Note where the
        // conditions land: joins nest to the *left*, so the inner join's ON binds
        // it to `posts`, and the outer INNER JOIN needs its own ON as well. A
        // single ON would leave the outer join without one, which is a syntax
        // error rather than a default.
        let mut inner = to("posts");
        let mut inner_join = Join::new(JoinKind::Left, to("comments"));
        inner_join.append_on(quote(("comments", "post_id")).eq(quote(("p", "id"))));
        inner.append_join(inner_join);

        let mut outer = Join::new(JoinKind::Inner, inner);
        outer.to.set_alias("p");
        outer.append_on("true");

        assert_frag_sql(
            FRAME,
            &sql(&outer),
            r#"INNER JOIN "posts" AS "p" LEFT JOIN "comments" ON ("comments"."post_id" = "p"."id") ON true"#,
        );
    }

    #[test]
    fn a_joined_sub_select_shares_the_placeholder_run() {
        let sub = Expr::group(Expr::join((
            Expr::raw(r#"SELECT "id" FROM posts WHERE "user_id" ="#),
            arg(7i32),
        )));
        let mut j = Join::new(JoinKind::Inner, TableRef::new(sub));
        j.to.set_alias("p");
        j.append_on(quote(("p", "id")).eq(arg(8i32)));

        let (rendered, args) = build(&Numbered, &j).unwrap();
        assert_frag_sql(
            FRAME,
            &rendered,
            r#"INNER JOIN (SELECT "id" FROM posts WHERE "user_id" = $1) AS "p" ON ("p"."id" = $2)"#,
        );
        assert_eq!(args, vec![Value::I32(7), Value::I32(8)]);
    }
}
