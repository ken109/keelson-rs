use std::borrow::Cow;

use crate::expr::{Expr, IntoExpr, IntoExprList};
use crate::writer::{Expression, SqlWriter};

use super::join::Join;
use super::{MaybeAbsent, write_present, write_quoted_list};

/// A `from_item`: a table, a sub-select, a function call or a CTE name, plus every
/// decoration the three dialects hang off one, plus its joins.
///
/// ```text
/// [ONLY] [LATERAL] expr [WITH ORDINALITY] [PARTITION (…)] [[AS] alias [(cols)]]
///        [index hints] [INDEXED BY … | NOT INDEXED] [joins]
/// ```
///
/// The order is PostgreSQL's `from_item` production
/// (<https://www.postgresql.org/docs/17/sql-select.html>) with MySQL's
/// `table_reference` (<https://dev.mysql.com/doc/refman/8.4/en/join.html>) and
/// SQLite's `qualified-table-name` slotted into the positions those grammars put
/// them in.
///
/// Every dialect's decorations live on this one struct rather than in three
/// near-identical copies, because a shared [`Join`] has to be able to hold a
/// `TableRef` whatever dialect it came from. Which of them a dialect *permits* is
/// decided by which mods that dialect exports — not by this shape. The
/// dialect-specific fields are labelled below.
///
/// The same struct is also an `INSERT`'s target, where
/// [`columns`](Self::columns) is the insert column list, and an `UPDATE`'s or
/// `DELETE`'s table.
#[derive(Debug, Clone, Default)]
pub struct TableRef {
    /// The table, sub-select, function call or CTE name.
    pub expression: Option<Expr>,

    /// The table alias, quoted on output.
    pub alias: Option<Cow<'static, str>>,
    /// Column aliases — or, for an `INSERT`, the insert column list. Quoted.
    pub columns: Vec<Cow<'static, str>>,

    /// PostgreSQL `ONLY`: do not include descendant tables.
    pub only: bool,
    /// PostgreSQL and MySQL `LATERAL`.
    pub lateral: bool,
    /// PostgreSQL `WITH ORDINALITY`, for a set-returning function.
    pub with_ordinality: bool,
    /// MySQL `PARTITION (…)`. Quoted.
    pub partitions: Vec<Cow<'static, str>>,
    /// MySQL `USE`/`FORCE`/`IGNORE INDEX`.
    pub index_hints: Vec<IndexHint>,
    /// SQLite `INDEXED BY …` / `NOT INDEXED`.
    pub indexed_by: Option<IndexedBy>,

    /// Joins hanging off this item, rendered after all of its decorations.
    pub joins: Vec<Join>,
}

impl TableRef {
    /// A plain table reference with no decorations.
    pub fn new(table: impl IntoExpr) -> Self {
        TableRef {
            expression: Some(table.into_expr()),
            ..TableRef::default()
        }
    }

    /// Replace the table.
    pub fn set_table(&mut self, table: impl IntoExpr) {
        self.expression = Some(table.into_expr());
    }

    /// Set the alias. Column aliases are [`set_columns`](Self::set_columns), so
    /// that neither has to be named to set the other.
    pub fn set_alias(&mut self, alias: impl Into<Cow<'static, str>>) {
        self.alias = Some(alias.into());
    }

    /// Set the column aliases — or, for an `INSERT`, the insert column list.
    pub fn set_columns(&mut self, columns: impl IntoIterator<Item = impl Into<Cow<'static, str>>>) {
        self.columns = columns.into_iter().map(Into::into).collect();
    }

    /// Append a join.
    pub fn append_join(&mut self, join: Join) {
        self.joins.push(join);
    }

    /// Append MySQL partition names.
    pub fn append_partition(
        &mut self,
        names: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) {
        self.partitions.extend(names.into_iter().map(Into::into));
    }

    /// Append a MySQL index hint.
    pub fn append_index_hint(&mut self, hint: IndexHint) {
        self.index_hints.push(hint);
    }

    /// Whether there is no table at all — what a query tests before writing
    /// `FROM`. A `TableRef` that has joins but no table of its own is still
    /// unwritable, so only the table is consulted.
    pub fn is_empty(&self) -> bool {
        self.expression.is_none()
    }
}

impl Expression for TableRef {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        let Some(expression) = &self.expression else {
            // No table: nothing can be written, not even the decorations, and the
            // enclosing statement is the one that knows whether that is legal
            // (`DELETE` needs a table, `SELECT 1` does not).
            return;
        };

        if self.only {
            w.push_str("ONLY ");
        }
        if self.lateral {
            w.push_str("LATERAL ");
        }

        w.write_expr(expression);

        if self.with_ordinality {
            w.push_str(" WITH ORDINALITY");
        }

        write_quoted_list(w, &self.partitions, " PARTITION (", ", ", ")");

        if let Some(alias) = &self.alias {
            w.push_str(" AS ");
            w.push_quoted(&[alias]);
        }
        // Column aliases can appear without a table alias — `t (a, b)` — and an
        // INSERT's column list always does.
        write_quoted_list(w, &self.columns, " (", ", ", ")");

        write_present(w, &self.index_hints, " ", " ", "");

        match &self.indexed_by {
            None => {}
            Some(IndexedBy::NotIndexed) => w.push_str(" NOT INDEXED"),
            Some(IndexedBy::Index(name)) => {
                w.push_str(" INDEXED BY ");
                w.push_quoted(&[name]);
            }
        }

        write_present(w, &self.joins, " ", " ", "");
    }
}

/// Anything with a table reference: the `FROM` of a `SELECT`, the target of an
/// `INSERT`/`UPDATE`/`DELETE`, the `USING` of a `DELETE`.
pub trait HasTableRef {
    /// The table reference to modify.
    fn table_ref_mut(&mut self) -> &mut TableRef;
}

impl HasTableRef for TableRef {
    fn table_ref_mut(&mut self) -> &mut TableRef {
        self
    }
}

/// SQLite's index directive.
///
/// Three states rather than bob's `*string` with `""` standing for `NOT INDEXED`:
/// an empty index name is not a thing SQLite has a syntax for, so it should not be
/// representable.
#[derive(Debug, Clone)]
pub enum IndexedBy {
    /// `NOT INDEXED` — refuse the index the planner would have chosen.
    NotIndexed,
    /// `INDEXED BY <name>`.
    Index(Cow<'static, str>),
}

/// MySQL's `USE | FORCE | IGNORE INDEX [FOR …] (indexes)`.
///
/// Never contributes a bound argument: index names are identifiers.
#[derive(Debug, Clone, Default)]
pub struct IndexHint {
    /// Which hint. `None` is how a default-constructed hint stays absent.
    pub kind: Option<IndexHintKind>,
    /// The index names, quoted. May be empty — `USE INDEX ()` is how MySQL is
    /// told to use no index at all, so the parentheses are unconditional.
    pub indexes: Vec<Cow<'static, str>>,
    /// Restrict the hint to one phase of planning.
    pub for_: Option<IndexHintScope>,
}

impl IndexHint {
    /// A hint of `kind` over `indexes`, applying to the whole query.
    pub fn new(
        kind: IndexHintKind,
        indexes: impl IntoIterator<Item = impl Into<Cow<'static, str>>>,
    ) -> Self {
        IndexHint {
            kind: Some(kind),
            indexes: indexes.into_iter().map(Into::into).collect(),
            for_: None,
        }
    }

    /// Whether the hint is absent.
    pub fn is_empty(&self) -> bool {
        self.kind.is_none()
    }
}

impl Expression for IndexHint {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        let Some(kind) = &self.kind else {
            return;
        };
        w.push_str(kind.as_str());
        w.push_str(" INDEX");
        if let Some(for_) = &self.for_ {
            w.push_str(" FOR ");
            w.push_str(for_.as_str());
        }
        // Unconditional, empty list included: see `indexes`.
        w.push_str(" (");
        write_quoted_list(w, &self.indexes, "", ", ", "");
        w.push_str(")");
    }
}

/// Which way a MySQL index hint leans.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexHintKind {
    /// `USE INDEX` — prefer these.
    Use,
    /// `IGNORE INDEX` — do not consider these.
    Ignore,
    /// `FORCE INDEX` — a table scan is not acceptable.
    Force,
}

impl IndexHintKind {
    /// The keyword, as written.
    pub fn as_str(self) -> &'static str {
        match self {
            IndexHintKind::Use => "USE",
            IndexHintKind::Ignore => "IGNORE",
            IndexHintKind::Force => "FORCE",
        }
    }
}

/// What a MySQL index hint applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexHintScope {
    /// `FOR JOIN`.
    Join,
    /// `FOR ORDER BY`.
    OrderBy,
    /// `FOR GROUP BY`.
    GroupBy,
}

impl IndexHintScope {
    /// The keyword, as written.
    pub fn as_str(self) -> &'static str {
        match self {
            IndexHintScope::Join => "JOIN",
            IndexHintScope::OrderBy => "ORDER BY",
            IndexHintScope::GroupBy => "GROUP BY",
        }
    }
}

/// A set of set-returning functions in a `FROM`, which PostgreSQL spells
/// `ROWS FROM (…)` once there is more than one.
///
/// ```text
/// [LATERAL] function_name ( … ) [WITH ORDINALITY] …
/// [LATERAL] ROWS FROM ( function_name ( … ) [AS (coldefs)] [, …] ) [WITH ORDINALITY] …
/// ```
///
/// The rule worth keeping is that the wrapper appears **only** for a set: one
/// function is written plainly, because `ROWS FROM (f())` and `f()` mean the same
/// thing and the shorter form is what a person writes. Put the result in
/// [`TableRef::expression`]; the column-definition lists belong to the individual
/// function expressions, which is a dialect's own function builder reaching core
/// through [`Expr::Custom`](crate::expr::Expr::Custom).
#[derive(Debug, Clone, Default)]
pub struct TableFunctions {
    /// The function calls, in order.
    pub functions: Vec<Expr>,
}

impl TableFunctions {
    /// A set from any expression list.
    pub fn new(functions: impl IntoExprList) -> Self {
        TableFunctions {
            functions: functions.into_expr_list(),
        }
    }

    /// Whether there are no functions at all.
    pub fn is_empty(&self) -> bool {
        self.functions.is_empty()
    }
}

impl Expression for TableFunctions {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        if self.functions.len() > 1 {
            w.write_slice(&self.functions, "ROWS FROM (", ", ", ")");
        } else {
            w.write_slice(&self.functions, "", ", ", "");
        }
    }
}

impl MaybeAbsent for IndexHint {
    fn is_absent(&self) -> bool {
        self.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use keelson_sqlcheck::testing::assert_frag_sql;

    use super::*;
    use crate::dialect::testing::{Numbered, Positional, TestDialect};
    use crate::expr::{arg, quote};
    use crate::value::Value;
    use crate::writer::build;
    use crate::{clause::JoinKind, expr::Chain};

    /// A table reference is a fragment of a `FROM`.
    const FRAME: &str = "SELECT * FROM {}";

    fn users() -> TableRef {
        TableRef::new(quote("users"))
    }

    fn sql(e: &impl Expression) -> String {
        build(&Numbered, e).expect("render").0
    }

    #[test]
    fn an_empty_table_ref_writes_nothing() {
        // Not framed: `SELECT * FROM` is not a statement, so there is nothing to
        // judge — which is precisely why a query type has to omit the keyword
        // along with the reference. `SELECT * FROM users {}` would judge the frame
        // and say nothing about the fragment.
        assert_eq!(build(&Numbered, &TableRef::default()).unwrap().0, "");
        assert!(TableRef::default().is_empty());
    }

    #[test]
    fn a_table_ref_with_only_decorations_still_writes_nothing() {
        // No table means nothing is writable — ONLY on its own is not SQL.
        let t = TableRef {
            only: true,
            lateral: true,
            alias: Some("u".into()),
            ..TableRef::default()
        };
        assert_eq!(build(&Numbered, &t).unwrap().0, "");
    }

    #[test]
    fn a_bare_table_is_just_its_expression() {
        assert_frag_sql(FRAME, &sql(&users()), r#""users""#);
    }

    #[test]
    fn the_alias_and_its_columns_are_quoted() {
        // PostgreSQL 17 from_item: `table_name [ [ AS ] alias [ ( column_alias
        // [, ...] ) ] ]` — fewer column aliases than columns renames a prefix.
        let mut t = users();
        t.set_alias("u");
        t.set_columns(["a", "b"]);
        assert_frag_sql(FRAME, &sql(&t), r#""users" AS "u" ("a", "b")"#);
    }

    #[test]
    fn column_aliases_without_an_alias_still_render() {
        // This is the INSERT column-list shape: `INSERT INTO users ("id", "name")`.
        let mut t = users();
        t.columns = vec!["id".into(), "name".into()];
        assert_frag_sql(
            "INSERT INTO {} VALUES (1, 'kubo')",
            &sql(&t),
            r#""users" ("id", "name")"#,
        );
    }

    #[test]
    fn postgres_decorations_bracket_the_expression() {
        // PostgreSQL 17 from_item:
        //   [ ONLY ] table_name … | [ LATERAL ] function_name ( … )
        //   [ WITH ORDINALITY ] [ [ AS ] alias … ]
        // ONLY and LATERAL precede the item; WITH ORDINALITY follows it and
        // precedes the alias. ONLY qualifies a table and LATERAL a function, so
        // they are judged on the item each one is legal on rather than together.
        let only = TableRef {
            only: true,
            alias: Some("u".into()),
            ..users()
        };
        assert_frag_sql(FRAME, &sql(&only), r#"ONLY "users" AS "u""#);

        let lateral = TableRef {
            lateral: true,
            with_ordinality: true,
            alias: Some("x".into()),
            ..TableRef::new(Expr::func("generate_series", (1i32, 3i32)))
        };
        assert_frag_sql(
            "SELECT * FROM users, {}",
            &sql(&lateral),
            r#"LATERAL generate_series(1, 3) WITH ORDINALITY AS "x""#,
        );
    }

    #[test]
    fn a_sub_select_in_the_from_keeps_the_outer_numbering() {
        let sub = Expr::group(Expr::join((
            Expr::raw(r#"SELECT "id" FROM posts WHERE "user_id" ="#),
            arg(3i32),
        )));
        let mut t = TableRef::new(sub);
        t.set_alias("p");
        let (rendered, args) = build(&Numbered, &t).unwrap();
        assert_frag_sql(
            FRAME,
            &rendered,
            r#"(SELECT "id" FROM posts WHERE "user_id" = $1) AS "p""#,
        );
        assert_eq!(args, vec![Value::I32(3)]);
    }

    #[test]
    fn mysql_partitions_come_before_the_alias() {
        // MySQL 8.4 table_reference:
        //   tbl_name [PARTITION (partition_names)] [[AS] alias] [index_hint_list]
        //
        // Not framed: this is MySQL syntax rendered by the MySQL-shaped stand-in
        // dialect, and the judge reachable from here is PostgreSQL's — which has
        // neither backticks nor PARTITION. `keelson-mysql` checks the same shape
        // against a real MySQL.
        let mut t = TableRef::new(Expr::ident("users"));
        t.append_partition(["p0", "p1"]);
        t.set_alias("u");
        assert_eq!(
            build(&Positional, &t).unwrap().0,
            "`users` PARTITION (`p0`, `p1`) AS `u`"
        );
    }

    #[test]
    fn index_hints_follow_the_alias_and_are_space_separated() {
        // MySQL 8.4: index_hint_list follows the alias, and each hint always
        // brings its parentheses — `USE INDEX ()` is meaningful. MySQL-only, so
        // unframed for the same reason as the case above.
        let mut t = users();
        t.set_alias("u");
        t.append_index_hint(IndexHint::new(IndexHintKind::Use, ["a"]));
        t.append_index_hint(IndexHint {
            for_: Some(IndexHintScope::OrderBy),
            ..IndexHint::new(IndexHintKind::Ignore, ["b", "c"])
        });
        t.append_index_hint(IndexHint::new(
            IndexHintKind::Force,
            Vec::<&'static str>::new(),
        ));

        let (sql, args) = build(&Positional, &t).unwrap();
        assert_eq!(
            sql,
            "`users` AS `u` USE INDEX (`a`) IGNORE INDEX FOR ORDER BY (`b`, `c`) FORCE INDEX ()"
        );
        assert!(
            args.is_empty(),
            "index names are identifiers, not arguments"
        );
    }

    #[test]
    fn an_absent_hint_or_join_leaves_no_separator_behind() {
        let mut t = users();
        t.append_index_hint(IndexHint::default());
        t.append_join(Join::default());
        assert!(IndexHint::default().is_empty());
        // Not even the space that would precede them: an absent item is absent
        // separator and all.
        assert_frag_sql(FRAME, &sql(&t), r#""users""#);

        t.append_join(Join::new(JoinKind::Cross, TableRef::new(quote("tags"))));
        assert_frag_sql(FRAME, &sql(&t), r#""users" CROSS JOIN "tags""#);
    }

    #[test]
    fn sqlite_indexed_by_has_three_states() {
        // SQLite's own `INDEXED BY` / `NOT INDEXED`. Not framed: PostgreSQL has no
        // such syntax, so the judge reachable from here would reject valid SQL.
        // `keelson-sqlite` checks it against a real SQLite.
        let mut t = users();
        assert_eq!(build(&TestDialect, &t).unwrap().0, r#""users""#);

        t.indexed_by = Some(IndexedBy::NotIndexed);
        assert_eq!(build(&TestDialect, &t).unwrap().0, r#""users" NOT INDEXED"#);

        t.indexed_by = Some(IndexedBy::Index("users_pkey".into()));
        assert_eq!(
            build(&TestDialect, &t).unwrap().0,
            r#""users" INDEXED BY "users_pkey""#
        );
    }

    #[test]
    fn joins_come_last_and_are_space_separated() {
        let mut t = users();
        t.set_alias("u");
        t.append_join(Join {
            kind: JoinKind::Inner,
            to: TableRef::new(quote("posts")),
            on: vec![quote(("u", "id")).eq(quote(("posts", "user_id")))],
            ..Join::default()
        });
        t.append_join(Join {
            kind: JoinKind::Cross,
            to: TableRef::new(quote("tags")),
            ..Join::default()
        });

        assert_frag_sql(
            FRAME,
            &sql(&t),
            r#""users" AS "u" INNER JOIN "posts" ON ("u"."id" = "posts"."user_id") CROSS JOIN "tags""#,
        );
    }

    #[test]
    fn one_function_is_written_plainly_and_several_get_rows_from() {
        // PostgreSQL 17 from_item: the ROWS FROM( … ) form exists to hold a *list*
        // of function calls; a single call is a from_item on its own.
        assert_eq!(build(&Numbered, &TableFunctions::default()).unwrap().0, "");

        let one = TableFunctions::new(Expr::func("generate_series", (1i32, 3i32)));
        assert_frag_sql(FRAME, &sql(&one), "generate_series(1, 3)");

        let many = TableFunctions::new((
            Expr::func("generate_series", (1i32, 3i32)),
            Expr::func("unnest", "ARRAY['a', 'b']"),
        ));
        assert_frag_sql(
            FRAME,
            &sql(&many),
            "ROWS FROM (generate_series(1, 3), unnest(ARRAY['a', 'b']))",
        );
    }

    #[test]
    fn a_rows_from_set_is_a_table_ref_expression() {
        let mut t = TableRef::new(Expr::custom(TableFunctions::new((
            Expr::func("generate_series", (1i32, 2i32)),
            Expr::func("generate_series", (3i32, 4i32)),
        ))));
        t.with_ordinality = true;
        t.set_alias("x");
        t.set_columns(["p", "q"]);
        assert_frag_sql(
            FRAME,
            &sql(&t),
            concat!(
                r#"ROWS FROM (generate_series(1, 2), generate_series(3, 4))"#,
                r#" WITH ORDINALITY AS "x" ("p", "q")"#
            ),
        );
    }
}
