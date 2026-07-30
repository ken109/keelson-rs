use std::fmt;
use std::marker::PhantomData;

use keelson_core::clause::HasWhere;
use keelson_core::expr::{Chain, Expr, IntoExpr, IntoExprList};
use keelson_core::{Mod, ToValue};

/// A model's column: the **one** entry point for everything a column is used
/// for.
///
/// bob splits a column across four generated surfaces (`ColumnNames`,
/// `Columns`, `SelectWhere`, `Preload`); keelson deliberately unifies them.
/// `users::age()` is
///
/// - the column *expression* — [`IntoExpr`] renders the qualified, quoted
///   identifier, so it drops into any Layer 1 slot (`select::columns`,
///   `select::order_by`, a join condition);
/// - the *filter origin* — [`gte`](Column::gte) and friends take the column's
///   Rust type, so `age().gte(21)` compiles and `age().gte("x")` does not;
/// - the *alias carrier* — [`aliased_as`](Column::aliased_as) re-qualifies it
///   when the query aliases the table.
///
/// The type parameter is the column's Rust type from
/// `docs/type-mappings.md` (the base type; nullability lives on the row
/// struct's `Option`, not here — a comparison against `NULL` is never what
/// `= $1` means in SQL, which is why [`is_null`](Column::is_null) is its own
/// method rather than `eq(None)`).
pub struct Column<T> {
    table: &'static str,
    name: &'static str,
    _type: PhantomData<fn() -> T>,
}

impl<T> fmt::Debug for Column<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Column")
            .field("table", &self.table)
            .field("name", &self.name)
            .finish()
    }
}

impl<T> Clone for Column<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Column<T> {}

impl<T> Column<T> {
    /// The column `table`.`name`. Generated code calls this; nothing checks
    /// the names — the schema they came from is the generator's authority.
    pub const fn new(table: &'static str, name: &'static str) -> Column<T> {
        Column {
            table,
            name,
            _type: PhantomData,
        }
    }

    /// The same column under a table alias: when the query says
    /// `FROM "users" AS "u"`, `users::age().aliased_as("u")` renders
    /// `"u"."age"`. The type is carried along — an aliased column is exactly
    /// as typed as the original.
    pub const fn aliased_as(self, alias: &'static str) -> Column<T> {
        Column {
            table: alias,
            ..self
        }
    }

    /// The table (or alias) this column renders under.
    pub const fn table(&self) -> &'static str {
        self.table
    }

    /// The bare column name — what the result-set column is called, and what
    /// `FromRow` reads it back by.
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// The qualified, quoted identifier expression: `"users"."age"`.
    pub fn expr(self) -> Expr {
        Expr::ident((self.table, self.name))
    }

    /// `self IS NULL`. On any column: nullability is the schema's fact, and a
    /// filter on a `NOT NULL` column is merely always false.
    pub fn is_null(self) -> Filter {
        Filter::from_expr(self.expr()).is_null()
    }

    /// `self IS NOT NULL`.
    pub fn is_not_null(self) -> Filter {
        Filter::from_expr(self.expr()).is_not_null()
    }
}

impl<T: ToValue> Column<T> {
    fn cmp(self, op: &'static str, rhs: T) -> Filter {
        Filter::from_expr(self.expr()).op(op, Expr::arg(rhs))
    }

    /// `self = $n`, binding the value.
    ///
    /// The comparison is typed by the column: the value must convert into the
    /// column's Rust type, so a mistyped literal fails to compile —
    ///
    /// ```compile_fail
    /// let age: keelson_models::Column<i32> = keelson_models::Column::new("users", "age");
    /// age.gte("x"); // a &str is not an i32: compile error
    /// ```
    pub fn eq(self, value: impl Into<T>) -> Filter {
        self.cmp("=", value.into())
    }

    /// `self <> $n`.
    pub fn ne(self, value: impl Into<T>) -> Filter {
        self.cmp("<>", value.into())
    }

    /// `self < $n`.
    pub fn lt(self, value: impl Into<T>) -> Filter {
        self.cmp("<", value.into())
    }

    /// `self <= $n`.
    pub fn lte(self, value: impl Into<T>) -> Filter {
        self.cmp("<=", value.into())
    }

    /// `self > $n`.
    pub fn gt(self, value: impl Into<T>) -> Filter {
        self.cmp(">", value.into())
    }

    /// `self >= $n`.
    pub fn gte(self, value: impl Into<T>) -> Filter {
        self.cmp(">=", value.into())
    }

    /// `self IN ($1, $2, …)`, every element bound.
    pub fn in_(self, values: impl IntoIterator<Item = impl Into<T>>) -> Filter {
        let vals: Vec<Expr> = values.into_iter().map(|v| Expr::arg(v.into())).collect();
        Filter::from_expr(self.expr()).in_(vals)
    }

    /// `self NOT IN ($1, $2, …)`.
    pub fn not_in(self, values: impl IntoIterator<Item = impl Into<T>>) -> Filter {
        let vals: Vec<Expr> = values.into_iter().map(|v| Expr::arg(v.into())).collect();
        Filter::from_expr(self.expr()).not_in(vals)
    }

    /// `self BETWEEN $1 AND $2`.
    pub fn between(self, low: impl Into<T>, high: impl Into<T>) -> Filter {
        Filter::from_expr(self.expr()).between(Expr::arg(low.into()), Expr::arg(high.into()))
    }
}

impl Column<String> {
    /// `self LIKE $n`. Text columns only — a pattern match against a number
    /// is a type error here rather than an engine surprise.
    pub fn like(self, pattern: impl Into<String>) -> Filter {
        Filter::from_expr(self.expr()).like(Expr::arg(pattern.into()))
    }
}

impl<T> IntoExpr for Column<T> {
    fn into_expr(self) -> Expr {
        self.expr()
    }
}

impl<T> IntoExprList for Column<T> {
    fn into_expr_list(self) -> Vec<Expr> {
        vec![self.expr()]
    }
}

/// A typed condition on its way to a `WHERE`.
///
/// What a [`Column`] comparison produces. It is three things at once, which is
/// what lets typed filters mix with Layer 1 anywhere:
///
/// - a [`Mod`] on anything with a `WHERE` ([`HasWhere`]) — so it sits directly
///   in `users::table().query((users::age().gte(21), …))`, and equally in a
///   raw dialect statement's mod tuple;
/// - an [`IntoExpr`] — so it drops into any expression slot (`select::where_`,
///   a join's `on`, a `CASE` arm);
/// - a [`Chain`] — so `.and(…)`, `.or(…)` and every other Layer 1 operator
///   keep working after the typed comparison started the chain.
#[derive(Debug, Clone)]
pub struct Filter(Expr);

impl Filter {
    /// Wrap any expression — a raw `&str` fragment included — as a filter, so
    /// hand-written SQL rides the same `WHERE` path the typed comparisons use.
    pub fn new(condition: impl IntoExpr) -> Filter {
        Filter(condition.into_expr())
    }
}

impl IntoExpr for Filter {
    fn into_expr(self) -> Expr {
        self.0
    }
}

impl IntoExprList for Filter {
    fn into_expr_list(self) -> Vec<Expr> {
        vec![self.0]
    }
}

impl Chain for Filter {
    fn from_expr(e: Expr) -> Filter {
        Filter(e)
    }
}

/// Appends to the `WHERE` clause; several filters `AND` together, matching
/// `Where`'s own contract.
impl<Q: HasWhere> Mod<Q> for Filter {
    fn apply(self, q: &mut Q) {
        q.where_mut().append_where(self.0);
    }
}

#[cfg(test)]
mod tests {
    use keelson_core::Value;
    use keelson_core::clause::Where;
    use keelson_sqlcheck::testing::{assert_frag, render};

    use super::*;

    const COND: &str = r#"SELECT "id" FROM users WHERE {}"#;

    fn age() -> Column<i32> {
        Column::new("users", "age")
    }

    fn name() -> Column<String> {
        Column::new("users", "name")
    }

    #[test]
    fn a_column_is_its_qualified_quoted_identifier() {
        assert_frag(r#"SELECT {} FROM users"#, &age().expr(), r#""users"."age""#);
    }

    #[test]
    fn typed_comparisons_bind_the_column_type() {
        let args = assert_frag(COND, &age().gte(21).into_expr(), r#"("users"."age" >= $1)"#);
        assert_eq!(args, vec![Value::I32(21)]);

        // Into<T> does the lifting: a &str lands in a String column.
        let args = assert_frag(
            COND,
            &name().eq("ada").into_expr(),
            r#"("users"."name" = $1)"#,
        );
        assert_eq!(args, vec![Value::Text("ada".into())]);
    }

    #[test]
    fn in_binds_every_element() {
        let args = assert_frag(
            COND,
            &age().in_([1, 2, 3]).into_expr(),
            r#"("users"."age" IN ($1, $2, $3))"#,
        );
        assert_eq!(args.len(), 3);
    }

    #[test]
    fn null_tests_and_like_have_their_sql_shapes() {
        assert_frag(
            COND,
            &age().is_null().into_expr(),
            r#"("users"."age" IS NULL)"#,
        );
        assert_frag(
            COND,
            &name().like("a%").into_expr(),
            r#"("users"."name" LIKE $1)"#,
        );
        assert_frag(
            COND,
            &age().between(1, 9).into_expr(),
            r#"("users"."age" BETWEEN $1 AND $2)"#,
        );
    }

    #[test]
    fn a_filter_chains_on_with_layer_1_operators() {
        // The typed comparison starts the chain; Layer 1's Chain continues it.
        let f = age().gte(21).and(name().like("a%"));
        assert_frag(
            COND,
            &f.into_expr(),
            r#"(("users"."age" >= $1) AND ("users"."name" LIKE $2))"#,
        );
    }

    #[test]
    fn aliased_as_requalifies_and_keeps_the_type() {
        let f = age().aliased_as("u").gte(21);
        assert_frag(
            r#"SELECT "id" FROM users AS u WHERE {}"#,
            &f.into_expr(),
            r#"("u"."age" >= $1)"#,
        );
        assert_eq!(age().aliased_as("u").name(), "age");
    }

    #[test]
    fn a_filter_is_a_mod_on_anything_with_a_where() {
        let mut w = Where::default();
        age().gte(21).apply(&mut w);
        name().eq("ada").apply(&mut w);
        let (sql, args) = render(&w);
        assert_eq!(
            sql,
            r#"WHERE ("users"."age" >= $1) AND ("users"."name" = $2)"#
        );
        assert_eq!(args.len(), 2);
    }

    #[test]
    fn filter_new_wraps_a_raw_fragment() {
        assert_frag(COND, &Filter::new("age > 21").into_expr(), "age > 21");
    }
}
