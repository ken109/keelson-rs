use std::fmt;
use std::sync::Arc;

use crate::dialect::Dialect;
use crate::error::Result;
use crate::value::{ToValue, Value};

/// A fragment of SQL that can render itself.
///
/// The `Debug + Send + Sync` bounds are deliberate. Queries store erased
/// expressions, and a query must be printable while debugging and holdable
/// across an `.await` in the async execution layer, so the bounds have to sit
/// here rather than at every use site.
pub trait Expression: fmt::Debug + Send + Sync {
    /// Append this fragment to `w`.
    ///
    /// Every bound argument must go through [`SqlWriter::push_arg`]; that is the
    /// only thing that advances the placeholder counter, which is what makes
    /// nesting re-index correctly for free.
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()>;
}

/// The erased expression that clauses store.
///
/// `Arc` rather than `Box` because query structs derive `Clone` — build-time
/// mods are applied to a clone of the query so that building stays `&self`.
pub type DynExpr = Arc<dyn Expression>;

/// Erase an expression into a [`DynExpr`].
pub fn dyn_expr(e: impl Expression + 'static) -> DynExpr {
    Arc::new(e)
}

/// A raw string is rendered verbatim.
///
/// This is bob's "progressive enhancement": anywhere an expression is accepted,
/// a hand-written string works too.
impl Expression for str {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        w.push_str(self);
        Ok(())
    }
}

impl Expression for String {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        w.push_str(self);
        Ok(())
    }
}

/// Numbers render as SQL literals.
///
/// bob's `Express` has a default arm that writes anything which is not an
/// expression with `fmt.Sprint`, and that is what makes `sm::limit(20)` come out
/// as `LIMIT 20` rather than as a placeholder. Where a bound argument is wanted
/// instead, the call is [`push_arg`](SqlWriter::push_arg) or an `arg(..)`
/// expression.
macro_rules! impl_expression_for_number {
    ($($t:ty),+) => {
        $(
            impl Expression for $t {
                fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
                    w.push_str(&self.to_string());
                    Ok(())
                }
            }
        )+
    };
}

impl_expression_for_number!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize, f32, f64);

impl<T: Expression + ?Sized> Expression for &T {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        (**self).write_sql(w)
    }
}

impl<T: Expression + ?Sized> Expression for Box<T> {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        (**self).write_sql(w)
    }
}

impl<T: Expression + ?Sized> Expression for Arc<T> {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        (**self).write_sql(w)
    }
}

/// An expression from a closure, for cases with no natural struct — notably
/// generated code.
pub struct ExprFn<F>(F);

/// Wrap a closure as an [`Expression`].
pub fn expr_fn<F>(f: F) -> ExprFn<F>
where
    F: Fn(&mut SqlWriter<'_>) -> Result<()> + Send + Sync,
{
    ExprFn(f)
}

impl<F> fmt::Debug for ExprFn<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ExprFn")
    }
}

impl<F> Expression for ExprFn<F>
where
    F: Fn(&mut SqlWriter<'_>) -> Result<()> + Send + Sync,
{
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        (self.0)(w)
    }
}

/// The SQL buffer, the bound arguments and the placeholder counter, together.
///
/// bob passes `start int` down the tree and every caller has to add
/// `len(args)` by hand before recursing — `SelectQuery.WriteSQL` does it more
/// than fifteen times. Here the counter lives next to the arguments and only
/// [`push_arg`](Self::push_arg) touches it, so sub-queries and nested
/// expressions re-index correctly without any bookkeeping at the call site.
#[derive(Debug)]
pub struct SqlWriter<'d> {
    sql: String,
    args: Vec<Value>,
    dialect: &'d dyn Dialect,
    next_arg: usize,
}

impl<'d> SqlWriter<'d> {
    /// A writer numbering placeholders from 1.
    pub fn new(dialect: &'d dyn Dialect) -> Self {
        Self::with_start(dialect, 1)
    }

    /// A writer numbering placeholders from `start`.
    ///
    /// Used to splice a query into one that already has arguments — bob's
    /// `BuildN`.
    ///
    /// # Panics
    /// If `start` is 0. Placeholders are 1-based in every supported dialect, and
    /// bob panics here too.
    pub fn with_start(dialect: &'d dyn Dialect, start: usize) -> Self {
        assert!(start > 0, "placeholder positions are 1-based, got {start}");
        SqlWriter {
            sql: String::new(),
            args: Vec::new(),
            dialect,
            next_arg: start,
        }
    }

    /// The dialect this writer renders for.
    pub fn dialect(&self) -> &'d dyn Dialect {
        self.dialect
    }

    /// The SQL written so far.
    pub fn sql(&self) -> &str {
        &self.sql
    }

    /// The arguments bound so far, in placeholder order.
    pub fn args(&self) -> &[Value] {
        &self.args
    }

    /// The position the next [`push_arg`](Self::push_arg) will use (1-based).
    pub fn arg_position(&self) -> usize {
        self.next_arg
    }

    /// Append raw SQL.
    pub fn push_str(&mut self, s: &str) {
        self.sql.push_str(s);
    }

    /// Bind `v` and write its placeholder.
    ///
    /// The single point where the placeholder counter advances.
    pub fn push_arg(&mut self, v: impl ToValue) {
        self.dialect.write_arg(&mut self.sql, self.next_arg);
        self.args.push(v.to_value());
        self.next_arg += 1;
    }

    /// Bind a named argument and write its placeholder.
    ///
    /// Named arguments exist to prepare statements, so nothing is added to the
    /// argument list — the caller supplies the values at bind time.
    ///
    /// # Errors
    /// [`Error::NoNamedArgs`](crate::Error::NoNamedArgs) if the dialect has no
    /// named-argument syntax.
    pub fn push_named_arg(&mut self, name: &str) -> Result<()> {
        self.dialect.write_named_arg(&mut self.sql, name)
    }

    /// Write a dotted, quoted identifier: `["users", "id"]` becomes
    /// `"users"."id"`.
    ///
    /// Empty parts are skipped, so a caller can pass an unset qualifier without
    /// branching.
    pub fn push_quoted(&mut self, parts: &[&str]) {
        let mut written = 0;
        for part in parts {
            if part.is_empty() {
                continue;
            }
            if written > 0 {
                self.sql.push('.');
            }
            self.dialect.write_quoted(&mut self.sql, part);
            written += 1;
        }
    }

    /// Render a nested expression. Replaces bob's `Express`.
    pub fn write_expr<E: Expression + ?Sized>(&mut self, e: &E) -> Result<()> {
        e.write_sql(self)
    }

    /// Render `prefix`, the expression, then `suffix` — but only if `cond`.
    /// Replaces bob's `ExpressIf`.
    pub fn write_if<E: Expression + ?Sized>(
        &mut self,
        cond: bool,
        prefix: &str,
        e: &E,
        suffix: &str,
    ) -> Result<()> {
        if !cond {
            return Ok(());
        }
        self.push_str(prefix);
        self.write_expr(e)?;
        self.push_str(suffix);
        Ok(())
    }

    /// Render a slice joined by `sep` and wrapped in `prefix`/`suffix`, writing
    /// nothing at all when the slice is empty. Replaces bob's `ExpressSlice`.
    pub fn write_slice<E: Expression>(
        &mut self,
        items: &[E],
        prefix: &str,
        sep: &str,
        suffix: &str,
    ) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        self.push_str(prefix);
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                self.push_str(sep);
            }
            self.write_expr(item)?;
        }
        self.push_str(suffix);
        Ok(())
    }

    /// [`write_slice`](Self::write_slice) for anything iterable, so a clause can
    /// map over its own storage without collecting into a `Vec` first.
    pub fn write_iter<E, I>(
        &mut self,
        items: I,
        prefix: &str,
        sep: &str,
        suffix: &str,
    ) -> Result<()>
    where
        E: Expression,
        I: IntoIterator<Item = E>,
    {
        let mut it = items.into_iter().peekable();
        if it.peek().is_none() {
            return Ok(());
        }
        self.push_str(prefix);
        for (i, item) in it.enumerate() {
            if i > 0 {
                self.push_str(sep);
            }
            self.write_expr(&item)?;
        }
        self.push_str(suffix);
        Ok(())
    }

    /// Render a nested expression under a different dialect, keeping one shared
    /// argument list and placeholder counter.
    ///
    /// This is how a sub-query built for one dialect embeds in a query built for
    /// another — bob's `BaseQuery.WriteSQL` ignores the dialect handed to it and
    /// uses its own.
    pub fn write_with_dialect<E: Expression + ?Sized>(
        &mut self,
        dialect: &dyn Dialect,
        e: &E,
    ) -> Result<()> {
        // The borrowed dialect may outlive nothing in particular, so the nested
        // writer gets its own (shorter) lifetime and the buffers are moved
        // through it.
        let mut nested = SqlWriter {
            sql: std::mem::take(&mut self.sql),
            args: std::mem::take(&mut self.args),
            dialect,
            next_arg: self.next_arg,
        };
        let result = e.write_sql(&mut nested);
        self.sql = nested.sql;
        self.args = nested.args;
        self.next_arg = nested.next_arg;
        result
    }

    /// Consume the writer, yielding the SQL and its arguments.
    pub fn finish(self) -> (String, Vec<Value>) {
        (self.sql, self.args)
    }
}

impl fmt::Write for SqlWriter<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.sql.push_str(s);
        Ok(())
    }
}

/// Render an expression to SQL and arguments, numbering placeholders from 1.
///
/// This is what `query.build()` calls.
pub fn build<E: Expression + ?Sized>(dialect: &dyn Dialect, e: &E) -> Result<(String, Vec<Value>)> {
    build_from(dialect, 1, e)
}

/// [`build`] with a different first placeholder position — bob's `BuildN`.
///
/// # Panics
/// If `start` is 0.
pub fn build_from<E: Expression + ?Sized>(
    dialect: &dyn Dialect,
    start: usize,
    e: &E,
) -> Result<(String, Vec<Value>)> {
    let mut w = SqlWriter::with_start(dialect, start);
    e.write_sql(&mut w)?;
    Ok(w.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::{Named, Numbered, Positional};

    /// `col = $n`
    #[derive(Debug)]
    struct Eq(&'static str, i32);

    impl Expression for Eq {
        fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
            w.push_quoted(&[self.0]);
            w.push_str(" = ");
            w.push_arg(self.1);
            Ok(())
        }
    }

    /// A sub-select, to prove nesting re-indexes.
    #[derive(Debug)]
    struct Sub(Vec<Eq>);

    impl Expression for Sub {
        fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
            w.push_str("(SELECT 1 WHERE ");
            w.write_slice(&self.0, "", " AND ", "")?;
            w.push_str(")");
            Ok(())
        }
    }

    #[test]
    fn placeholders_are_numbered_in_write_order() {
        let (sql, args) =
            build(&Numbered, &Sub(vec![Eq("a", 10), Eq("b", 20), Eq("c", 30)])).unwrap();
        assert_eq!(
            sql,
            r#"(SELECT 1 WHERE "a" = $1 AND "b" = $2 AND "c" = $3)"#
        );
        assert_eq!(args, vec![Value::I32(10), Value::I32(20), Value::I32(30)]);
    }

    #[test]
    fn nesting_continues_the_outer_numbering() {
        #[derive(Debug)]
        struct Outer;

        impl Expression for Outer {
            fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
                w.write_expr(&Eq("x", 1))?;
                w.push_str(" AND ");
                w.write_expr(&Sub(vec![Eq("a", 2), Eq("b", 3)]))?;
                w.push_str(" AND ");
                w.write_expr(&Eq("y", 4))
            }
        }

        let (sql, args) = build(&Numbered, &Outer).unwrap();
        assert_eq!(
            sql,
            r#""x" = $1 AND (SELECT 1 WHERE "a" = $2 AND "b" = $3) AND "y" = $4"#
        );
        assert_eq!(args.len(), 4);
        assert_eq!(args[3], Value::I32(4));
    }

    #[test]
    fn build_from_offsets_the_first_placeholder() {
        let (sql, args) = build_from(&Numbered, 3, &Sub(vec![Eq("a", 1), Eq("b", 2)])).unwrap();
        assert_eq!(sql, r#"(SELECT 1 WHERE "a" = $3 AND "b" = $4)"#);
        assert_eq!(args.len(), 2, "args are still returned from the start");
    }

    #[test]
    #[should_panic(expected = "1-based")]
    fn start_zero_is_rejected() {
        let _ = build_from(&Numbered, 0, &Eq("a", 1));
    }

    #[test]
    fn positional_dialects_ignore_the_index_but_still_order_args() {
        let (sql, args) = build(&Positional, &Sub(vec![Eq("a", 7), Eq("b", 8)])).unwrap();
        assert_eq!(sql, "(SELECT 1 WHERE `a` = ? AND `b` = ?)");
        assert_eq!(args, vec![Value::I32(7), Value::I32(8)]);
    }

    #[test]
    fn a_raw_string_is_an_expression() {
        let (sql, args) = build(&Numbered, "id = 1").unwrap();
        assert_eq!(sql, "id = 1");
        assert!(args.is_empty());

        let (sql, _) = build(&Numbered, &String::from("id = 2")).unwrap();
        assert_eq!(sql, "id = 2");

        let boxed: Box<dyn Expression> = Box::new(Eq("a", 1));
        let (sql, _) = build(&Numbered, &boxed).unwrap();
        assert_eq!(sql, r#""a" = $1"#);

        let shared: DynExpr = dyn_expr(Eq("b", 2));
        let (sql, _) = build(&Numbered, &shared).unwrap();
        assert_eq!(sql, r#""b" = $1"#);
    }

    #[test]
    fn expr_fn_wraps_a_closure() {
        let e = expr_fn(|w: &mut SqlWriter<'_>| {
            w.push_str("LIMIT ");
            w.push_arg(5i64);
            Ok(())
        });
        let (sql, args) = build(&Numbered, &e).unwrap();
        assert_eq!(sql, "LIMIT $1");
        assert_eq!(args, vec![Value::I64(5)]);
    }

    #[test]
    fn write_if_skips_everything_including_the_affixes() {
        let mut w = SqlWriter::new(&Numbered);
        w.write_if(false, " WHERE ", &Eq("a", 1), "!").unwrap();
        assert_eq!(w.arg_position(), 1, "a skipped arg must not advance");
        w.write_if(true, " WHERE ", &Eq("a", 1), "!").unwrap();
        let (sql, args) = w.finish();
        assert_eq!(sql, r#" WHERE "a" = $1!"#);
        assert_eq!(args.len(), 1);
    }

    #[test]
    fn write_slice_is_a_no_op_when_empty() {
        let mut w = SqlWriter::new(&Numbered);
        w.write_slice::<Eq>(&[], " WHERE ", " AND ", ";").unwrap();
        assert_eq!(w.sql(), "");

        w.write_iter(Vec::<String>::new(), "(", ", ", ")").unwrap();
        assert_eq!(w.sql(), "");

        w.write_iter(vec!["a", "b"], "(", ", ", ")").unwrap();
        assert_eq!(w.sql(), "(a, b)");
    }

    #[test]
    fn push_quoted_joins_with_dots_and_drops_empty_parts() {
        let mut w = SqlWriter::new(&Numbered);
        w.push_quoted(&["users", "id"]);
        w.push_str(" ");
        w.push_quoted(&["", "id"]);
        w.push_str(" ");
        w.push_quoted(&[]);
        assert_eq!(w.sql(), r#""users"."id" "id" "#);
    }

    #[test]
    fn named_args_do_not_consume_an_arg_slot() {
        let mut w = SqlWriter::new(&Named);
        w.push_arg(1i32);
        w.push_str(", ");
        w.push_named_arg("name").unwrap();
        w.push_str(", ");
        w.push_arg(2i32);
        let (sql, args) = w.finish();
        assert_eq!(sql, "?1, :name, ?2");
        assert_eq!(args, vec![Value::I32(1), Value::I32(2)]);
    }

    #[test]
    fn named_args_fail_on_dialects_without_them() {
        let mut w = SqlWriter::new(&Numbered);
        assert!(w.push_named_arg("name").is_err());
    }

    #[test]
    fn a_nested_dialect_shares_the_arg_list_and_counter() {
        #[derive(Debug)]
        struct Mixed;

        impl Expression for Mixed {
            fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
                w.write_expr(&Eq("a", 1))?;
                w.push_str(" AND ");
                w.write_with_dialect(&Positional, &Eq("b", 2))?;
                w.push_str(" AND ");
                w.write_expr(&Eq("c", 3))
            }
        }

        let (sql, args) = build(&Numbered, &Mixed).unwrap();
        assert_eq!(sql, r#""a" = $1 AND `b` = ? AND "c" = $3"#);
        assert_eq!(
            args.len(),
            3,
            "the counter advanced through the nested part"
        );
    }

    #[test]
    fn errors_propagate_out_of_the_helpers() {
        #[derive(Debug)]
        struct Bad;

        impl Expression for Bad {
            fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
                w.push_named_arg("nope")
            }
        }

        assert!(build(&Numbered, &Bad).is_err());
        let mut w = SqlWriter::new(&Numbered);
        assert!(w.write_slice(&[Bad], "(", ", ", ")").is_err());
    }
}
