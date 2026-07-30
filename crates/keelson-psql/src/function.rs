use std::borrow::Cow;

use keelson_core::clause::{HasOrderBy, OrderBy, Window};
use keelson_core::expr::{Expr, IntoExpr, IntoExprList};
use keelson_core::{Expression, Mod, SqlWriter};

/// A PostgreSQL function call, with every decoration the grammar hangs off one.
///
/// From PostgreSQL 17, `4.2.7 Aggregate Expressions` and
/// `4.2.8 Window Function Calls`:
///
/// ```text
/// name ( [ DISTINCT ] expression [, ...] [ ORDER BY … ] ) [ FILTER ( WHERE … ) ]
/// name ( expression [, ...] ) WITHIN GROUP ( ORDER BY … ) [ FILTER ( WHERE … ) ]
/// name ( … ) [ FILTER ( WHERE … ) ] OVER ( window_definition | window_name )
/// ```
///
/// plus the `FROM`-list form, where a set-returning function may name its result
/// columns:
///
/// ```text
/// function_name ( … ) [ AS ] [ alias ] ( column_definition [, ...] )
/// ```
///
/// [`Expr::Func`](keelson_core::expr::Expr::Func) carries only what all three
/// dialects share, so everything above lives here and reaches core through
/// [`Expr::Custom`](keelson_core::expr::Expr::Custom) — which is why this is a
/// `keelson-psql` type and not a core one.
///
/// ```
/// use keelson_psql::{f, quote, window};
///
/// // avg("views") OVER (PARTITION BY "user_id")
/// let e = f("avg", quote("views")).over(window::partition_by(quote("user_id")));
/// ```
#[derive(Debug, Clone, Default)]
pub struct Function {
    name: Cow<'static, str>,
    args: Vec<Expr>,
    distinct: bool,
    order_by: OrderBy,
    within_group: bool,
    filter: Vec<Expr>,
    over: Option<OverClause>,
}

/// What follows `OVER`.
///
/// The two forms are not interchangeable, and the difference is not cosmetic.
/// `OVER window_name` **references** a window from the statement's `WINDOW` clause;
/// `OVER ( … )` is a definition, and a definition that begins with an existing
/// window's name *copies* it — which PostgreSQL refuses when that window has a frame
/// clause:
///
/// ```text
/// ERROR:  cannot copy window "w" because it has a frame clause
/// HINT:   Omit the parentheses in this OVER clause.
/// ```
///
/// bob only ever writes the parenthesised form, so a named window with a frame is
/// unreachable there. [`Function::over_name`] is the other one.
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

    /// Add a sort key to the aggregate's own `ORDER BY`.
    ///
    /// Rendered inside the argument list — `array_agg(x ORDER BY y)` — unless
    /// [`within_group`](Self::within_group) moved it out.
    #[must_use]
    pub fn order_by(mut self, order: impl IntoExpr) -> Function {
        self.order_by.append_order(order);
        self
    }

    /// Write the `ORDER BY` as `WITHIN GROUP (ORDER BY …)`, which is what an
    /// ordered-set aggregate such as `percentile_cont` requires.
    #[must_use]
    pub fn within_group(mut self) -> Function {
        self.within_group = true;
        self
    }

    /// Add a condition to `FILTER (WHERE …)`. Several are `AND`-joined.
    #[must_use]
    pub fn filter(mut self, condition: impl IntoExpr) -> Function {
        self.filter.push(condition.into_expr());
        self
    }

    /// The alias of a set-returning function used as a from-item: `f() AS "t"`,
    /// or — with [`columns`](TableFunction::columns) — `f() AS "t" ("a" int)`.
    ///
    /// Not the select-list alias — that is [`as_`](Self::as_). The two must not
    /// meet: `gram.y`'s `func_alias_clause` shares one `AS` between the alias
    /// and the column definitions, so a second alias would be a second `AS`,
    /// which is a syntax error. That is why this returns a [`TableFunction`],
    /// on which `as_` — and the rest of the expression-position decorations —
    /// does not exist.
    #[must_use]
    pub fn as_table(self, alias: impl Into<Cow<'static, str>>) -> TableFunction {
        TableFunction::from(self).as_table(alias)
    }

    /// Name and type the columns a set-returning function returns:
    /// `json_to_recordset($1) AS ("a" int, "b" text)`.
    ///
    /// The name is quoted; the type is written verbatim, so `int`, `text[]` and
    /// `numeric(10, 2)` all work.
    ///
    /// Returns a [`TableFunction`] for the same reason
    /// [`as_table`](Self::as_table) does: the column definitions spend the one
    /// `AS` the `func_alias_clause` production has, so the select-list
    /// [`as_`](Self::as_) cannot be allowed to write another.
    #[must_use]
    pub fn columns<N, T>(self, columns: impl IntoIterator<Item = (N, T)>) -> TableFunction
    where
        N: Into<Cow<'static, str>>,
        T: Into<Cow<'static, str>>,
    {
        TableFunction::from(self).columns(columns)
    }

    /// Attach `OVER (…)`, built from window mods — `psql::window::*` and
    /// `psql::frame::*`.
    ///
    /// Ends the builder, because `OVER` is the last thing in the grammar: it is
    /// written after `FILTER`, and nothing may follow it. `over(())` gives the legal
    /// `OVER ()`, which means the whole partition.
    ///
    /// To *reference* a window declared in the statement's `WINDOW` clause, use
    /// [`over_name`](Self::over_name) — not `over(window::based_on(..))`, which is
    /// the copying form and cannot copy a frame.
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
    /// Unparenthesised, which is what makes it a reference rather than a copy: the
    /// parenthesised form is refused outright when the named window has a frame.
    #[must_use]
    pub fn over_name(mut self, name: impl Into<Cow<'static, str>>) -> Expr {
        self.over = Some(OverClause::Name(name.into()));
        self.into_expr()
    }

    /// `f(…) AS "alias"` — the select-list alias.
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
        if !self.within_group {
            // `array_agg(x ORDER BY y)`: the separator is only needed when there
            // is an argument in front of it, and `f(ORDER BY x)` is not a thing.
            w.write_if(
                !self.order_by.is_empty() && !self.args.is_empty(),
                " ",
                &self.order_by,
                "",
            );
        }
        w.push_str(")");

        if self.within_group {
            w.write_if(
                !self.order_by.is_empty(),
                " WITHIN GROUP (",
                &self.order_by,
                ")",
            );
        }

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

/// A [`Function`] committed to the from-item form: the call plus `gram.y`'s
/// `func_alias_clause`, `[ AS ] [ alias ] ( column_definition [, ...] )`.
///
/// [`Function::as_table`] and [`Function::columns`] return this instead of
/// `Function`, and the expression-position enders — [`Function::as_`],
/// [`Function::over`], [`Function::over_name`] — do not exist here. That is the
/// point: `func_alias_clause` has exactly one `AS` shared between the alias and
/// the column definitions, so `f() AS ("a" int) AS "r"` — the column form plus
/// the select-list alias — is a syntax error, and this type makes it
/// unwritable rather than an error to render. The from-item alias *is* the
/// [`as_table`](Self::as_table) alias.
///
/// [`from_functions`](crate::shared::from_functions) accepts a plain
/// `Function` and a `TableFunction` alike, so `f(..)` and
/// `f(..).columns(..)` both go straight in.
#[derive(Debug, Clone)]
pub struct TableFunction {
    function: Function,
    alias: Option<Cow<'static, str>>,
    columns: Vec<ColumnDef>,
}

impl TableFunction {
    /// The alias in front of the column definitions: `f() AS "t" ("a" int)`.
    #[must_use]
    pub fn as_table(mut self, alias: impl Into<Cow<'static, str>>) -> TableFunction {
        self.alias = Some(alias.into());
        self
    }

    /// Add column definitions. Several calls accumulate into the one list.
    #[must_use]
    pub fn columns<N, T>(mut self, columns: impl IntoIterator<Item = (N, T)>) -> TableFunction
    where
        N: Into<Cow<'static, str>>,
        T: Into<Cow<'static, str>>,
    {
        self.columns.extend(
            columns
                .into_iter()
                .map(|(name, ty)| ColumnDef::new(name, ty)),
        );
        self
    }
}

impl From<Function> for TableFunction {
    fn from(function: Function) -> TableFunction {
        TableFunction {
            function,
            alias: None,
            columns: Vec::new(),
        }
    }
}

impl Expression for TableFunction {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        self.function.write_sql(w);

        // `AS` introduces the column-definition list, and the alias — when there
        // is one — sits between them: `f() AS "t" ("a" int)`.
        if self.alias.is_some() || !self.columns.is_empty() {
            w.push_str(" AS");
            if let Some(alias) = &self.alias {
                w.push_str(" ");
                w.push_quoted(&[alias]);
            }
            w.write_slice(&self.columns, " (", ", ", ")");
        }
    }
}

impl IntoExpr for TableFunction {
    fn into_expr(self) -> Expr {
        Expr::custom(self)
    }
}

impl IntoExprList for TableFunction {
    fn into_expr_list(self) -> Vec<Expr> {
        vec![self.into_expr()]
    }
}

/// One entry of a set-returning function's column-definition list: `"a" int`.
#[derive(Debug, Clone)]
pub struct ColumnDef {
    /// The column name, quoted on output.
    pub name: Cow<'static, str>,
    /// The type, written verbatim.
    pub data_type: Cow<'static, str>,
}

impl ColumnDef {
    /// A named, typed column.
    pub fn new(
        name: impl Into<Cow<'static, str>>,
        data_type: impl Into<Cow<'static, str>>,
    ) -> ColumnDef {
        ColumnDef {
            name: name.into(),
            data_type: data_type.into(),
        }
    }
}

impl Expression for ColumnDef {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.push_quoted(&[&self.name]);
        w.push_str(" ");
        w.push_str(&self.data_type);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Psql, arg, f, frame, quote, window};
    use keelson_core::build;

    fn sql(e: impl Expression) -> String {
        build(&Psql, &e).expect("render").0
    }

    #[test]
    fn a_plain_call_is_just_a_call() {
        assert_eq!(sql(f("now", ())), "now()");
        assert_eq!(sql(f("count", "*")), "count(*)");
    }

    /// PostgreSQL 17, 4.2.7: `DISTINCT` and the aggregate's own `ORDER BY` are
    /// both inside the argument parentheses.
    #[test]
    fn distinct_and_order_by_stay_inside_the_argument_list() {
        assert_eq!(
            sql(f("count", quote("id")).distinct()),
            r#"count(DISTINCT "id")"#
        );
        assert_eq!(
            sql(f("array_agg", quote("id")).order_by(quote("name"))),
            r#"array_agg("id" ORDER BY "name")"#
        );
    }

    /// PostgreSQL 17, 4.2.7: an ordered-set aggregate puts its `ORDER BY` in a
    /// `WITHIN GROUP` of its own, after the argument list.
    #[test]
    fn within_group_moves_the_order_by_out() {
        assert_eq!(
            sql(f("percentile_cont", arg(0.5f64))
                .within_group()
                .order_by(quote("views"))),
            r#"percentile_cont($1) WITHIN GROUP (ORDER BY "views")"#
        );
    }

    #[test]
    fn filter_conditions_are_and_joined_inside_one_where() {
        assert_eq!(
            sql(f("count", "*").filter(quote("a")).filter(quote("b"))),
            r#"count(*) FILTER (WHERE "a" AND "b")"#
        );
    }

    /// PostgreSQL 17 `sql-select.html`, `from_item`:
    /// `function_name ( … ) [ AS ] [ alias ] ( column_definition [, ...] )`.
    #[test]
    fn column_definitions_follow_as_with_the_alias_between() {
        assert_eq!(
            sql(f("json_to_recordset", arg("[]")).columns([("a", "int"), ("b", "text")])),
            r#"json_to_recordset($1) AS ("a" int, "b" text)"#
        );
        assert_eq!(
            sql(f("json_to_recordset", arg("[]"))
                .as_table("t")
                .columns([("a", "int")])),
            r#"json_to_recordset($1) AS "t" ("a" int)"#
        );
    }

    #[test]
    fn over_takes_a_definition_a_name_or_nothing() {
        assert_eq!(sql(f("row_number", ()).over(())), "row_number() OVER ()");
        // The reference form has no parentheses; the copy form does, and copying is
        // what PostgreSQL refuses when the named window has a frame.
        assert_eq!(
            sql(f("avg", quote("views")).over_name("w")),
            r#"avg("views") OVER "w""#
        );
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
    fn filter_is_written_before_over() {
        // 4.2.8: `[ FILTER ( WHERE filter_clause ) ] OVER ( … )`.
        assert_eq!(
            sql(f("count", "*")
                .filter(quote("a"))
                .over(window::partition_by(quote("b")))),
            r#"count(*) FILTER (WHERE "a") OVER (PARTITION BY "b")"#
        );
    }

    #[test]
    fn an_unnamed_call_is_a_recorded_failure() {
        let err = build(&Psql, &Function::default()).unwrap_err();
        // The substring names the SQL concept (a function's name), not the
        // message wording.
        assert!(
            matches!(&err, keelson_core::Error::Incomplete(what) if what.contains("function")),
            "got: {err}"
        );
    }
}
