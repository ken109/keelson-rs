use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;

use crate::dialect::Dialect;
use crate::error::{Error, Result};
use crate::value::{ToValue, Value};

/// A fragment of SQL that can render itself.
///
/// Rendering is infallible: appending to a `String` cannot fail, and neither can
/// anything else an expression does. The one genuine failure — asking a dialect
/// for a named argument it has no syntax for — is recorded on the writer with
/// [`SqlWriter::record_error`] and surfaced once, by [`build`]. bob checks an
/// error return more than fifteen times inside a single `SELECT`; none of that
/// bookkeeping exists here.
///
/// The `Debug + Send + Sync` bounds are deliberate. Clauses store erased
/// expressions, so a query must stay printable while debugging and holdable
/// across an `.await` in the async execution layer; the bounds have to sit here
/// rather than at every use site.
pub trait Expression: fmt::Debug + Send + Sync {
    /// Append this fragment to `w`.
    ///
    /// Every bound argument must go through [`SqlWriter::push_arg`]; that is the
    /// only thing that advances the placeholder counter, which is what makes
    /// nesting re-index correctly for free.
    fn write_sql(&self, w: &mut SqlWriter<'_>);
}

/// The erased expression that clauses store.
///
/// `Arc` rather than `Box` because query structs derive `Clone` — build-time mods
/// are applied to a clone of the query so that building stays `&self`.
pub type DynExpr = Arc<dyn Expression>;

/// Erase an expression into a [`DynExpr`].
pub fn dyn_expr(e: impl Expression + 'static) -> DynExpr {
    Arc::new(e)
}

/// A raw string is rendered verbatim.
///
/// This is bob's "progressive enhancement": anywhere an expression is accepted, a
/// hand-written fragment works too. Covers `&str` of any lifetime through the
/// blanket `&T` impl below, which includes the `&'static str` that clauses store.
impl Expression for str {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.push_str(self);
    }
}

impl Expression for String {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.push_str(self);
    }
}

/// The form identifiers and raw SQL are stored in: a literal costs nothing and a
/// computed string is owned, with no lifetime parameter leaking into query types.
impl Expression for Cow<'_, str> {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.push_str(self);
    }
}

/// Numbers render as SQL literals, not as bound arguments.
///
/// bob's `Express` has a default arm that writes any non-expression with
/// `fmt.Sprint`, and that is what makes `select::limit(20)` come out as
/// `LIMIT 20`. Where a bound argument is wanted instead, the call is
/// [`SqlWriter::push_arg`] or a dialect's `arg(..)` expression.
macro_rules! impl_expression_for_number {
    ($($t:ty),+) => {
        $(
            impl Expression for $t {
                fn write_sql(&self, w: &mut SqlWriter<'_>) {
                    w.push_str(&self.to_string());
                }
            }
        )+
    };
}

impl_expression_for_number!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize, f32, f64);

impl<T: Expression + ?Sized> Expression for &T {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        (**self).write_sql(w);
    }
}

impl<T: Expression + ?Sized> Expression for Box<T> {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        (**self).write_sql(w);
    }
}

/// Covers [`DynExpr`] — `Arc<dyn Expression>` — as well as `Arc<ConcreteExpr>`.
impl<T: Expression + ?Sized> Expression for Arc<T> {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        (**self).write_sql(w);
    }
}

/// An expression from a closure, for fragments with no natural struct — notably
/// generated code.
pub struct ExprFn<F>(F);

/// Wrap a closure as an [`Expression`].
pub fn expr_fn<F>(f: F) -> ExprFn<F>
where
    F: Fn(&mut SqlWriter<'_>) + Send + Sync,
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
    F: Fn(&mut SqlWriter<'_>) + Send + Sync,
{
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        (self.0)(w);
    }
}

/// The SQL buffer, the bound arguments, the placeholder counter and the dialect,
/// together.
///
/// bob passes `start int` down the tree and every caller adds `len(args)` by hand
/// before recursing — `SelectQuery.WriteSQL` does it more than fifteen times.
/// Here the counter lives next to the arguments and only
/// [`push_arg`](Self::push_arg) touches it, so sub-queries and nested expressions
/// re-index correctly with no bookkeeping at the call site.
#[derive(Debug)]
pub struct SqlWriter<'d> {
    sql: String,
    args: Vec<Value>,
    dialect: &'d dyn Dialect,
    next_arg: usize,
    /// The first recorded failure. Kept rather than returned so that
    /// [`Expression::write_sql`] can be infallible; [`finish`](Self::finish)
    /// surfaces it.
    error: Option<Error>,
}

/// What a writer starts with room for.
///
/// One writer renders one whole statement — nesting reuses it, because the
/// placeholder counter belongs to the writer — so these are sized for a
/// statement rather than a fragment. Starting empty meant a typical `SELECT`
/// grew its buffer from 8 bytes through six reallocations on the way to its
/// couple of hundred; an allocation of 256 bytes costs no more than one of 8,
/// so the only thing the old default bought was the copying.
///
/// A statement that outgrows either figure still grows, exactly as before.
const SQL_CAPACITY: usize = 256;
/// Room for the arguments of that same statement.
const ARG_CAPACITY: usize = 8;

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
    /// If `start` is 0. Placeholders are 1-based in every supported dialect.
    pub fn with_start(dialect: &'d dyn Dialect, start: usize) -> Self {
        assert!(start > 0, "placeholder positions are 1-based, got {start}");
        SqlWriter {
            sql: String::with_capacity(SQL_CAPACITY),
            args: Vec::with_capacity(ARG_CAPACITY),
            dialect,
            next_arg: start,
            error: None,
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

    /// The first recorded failure, if any.
    pub fn error(&self) -> Option<&Error> {
        self.error.as_ref()
    }

    /// Record a failure.
    ///
    /// The first one wins: it is the one with the most context, and a later
    /// failure is usually a consequence of it. Rendering continues either way —
    /// the partial SQL is still useful in a debug print, and
    /// [`finish`](Self::finish) is what refuses to hand it over.
    pub fn record_error(&mut self, e: Error) {
        if self.error.is_none() {
            self.error = Some(e);
        }
    }

    /// Append raw SQL.
    pub fn push_str(&mut self, s: &str) {
        self.sql.push_str(s);
    }

    /// Append a number without allocating a `String` for it.
    ///
    /// Placeholders are the hot path this exists for: every dialect that
    /// numbers them (`$1`, `?1`) wrote `position.to_string()` and threw the
    /// `String` away, once per bound argument.
    pub fn push_usize(&mut self, n: usize) {
        let mut buf = itoa_buf();
        self.sql.push_str(format_usize(&mut buf, n));
    }

    /// Bind `v` and write its placeholder.
    ///
    /// The single point where the placeholder counter advances.
    pub fn push_arg(&mut self, v: impl ToValue) {
        let (d, pos) = (self.dialect, self.next_arg);
        d.write_arg(self, pos);
        self.args.push(v.to_value());
        self.next_arg += 1;
    }

    /// Write a named argument's placeholder.
    ///
    /// Named arguments exist to prepare a statement whose values are supplied at
    /// bind time, so nothing is added to the argument list and the positional
    /// counter does not move.
    ///
    /// Records [`Error::NoNamedArgs`] if the dialect has no named-argument
    /// syntax.
    pub fn push_named_arg(&mut self, name: &str) {
        let d = self.dialect;
        d.write_named_arg(self, name);
    }

    /// Write a dotted, quoted identifier: `["users", "id"]` becomes
    /// `"users"."id"`.
    ///
    /// Empty parts are skipped, so a caller can pass an unset qualifier without
    /// branching. Generic over `AsRef<str>` so that a clause can hand over its
    /// stored `[Cow<'static, str>]` directly.
    pub fn push_quoted<S: AsRef<str>>(&mut self, parts: &[S]) {
        let d = self.dialect;
        let mut written = 0;
        for part in parts {
            let part = part.as_ref();
            if part.is_empty() {
                continue;
            }
            if written > 0 {
                self.sql.push('.');
            }
            d.write_quoted(self, part);
            written += 1;
        }
    }

    /// Render a nested expression. Replaces bob's `Express`.
    pub fn write_expr<E: Expression + ?Sized>(&mut self, e: &E) {
        e.write_sql(self);
    }

    /// Render `prefix`, the expression, then `suffix` — but only if `cond`.
    /// Replaces bob's `ExpressIf`.
    ///
    /// When `cond` is false nothing at all is written, affixes included, and no
    /// argument is consumed.
    pub fn write_if<E: Expression + ?Sized>(
        &mut self,
        cond: bool,
        prefix: &str,
        e: &E,
        suffix: &str,
    ) {
        if !cond {
            return;
        }
        self.push_str(prefix);
        self.write_expr(e);
        self.push_str(suffix);
    }

    /// [`write_if`](Self::write_if) for an optional clause, which is how most
    /// clauses are stored.
    pub fn write_if_some<E: Expression + ?Sized>(
        &mut self,
        e: Option<&E>,
        prefix: &str,
        suffix: &str,
    ) {
        if let Some(e) = e {
            self.push_str(prefix);
            self.write_expr(e);
            self.push_str(suffix);
        }
    }

    /// Render a slice joined by `sep` and wrapped in `prefix`/`suffix`, writing
    /// nothing at all when the slice is empty. Replaces bob's `ExpressSlice`.
    ///
    /// The empty case is the load-bearing part: it is how a clause omits itself,
    /// keyword and all.
    pub fn write_slice<E: Expression>(
        &mut self,
        items: &[E],
        prefix: &str,
        sep: &str,
        suffix: &str,
    ) {
        if items.is_empty() {
            return;
        }
        self.push_str(prefix);
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                self.push_str(sep);
            }
            self.write_expr(item);
        }
        self.push_str(suffix);
    }

    /// [`write_slice`](Self::write_slice) for anything iterable, so a clause can
    /// map over its own storage without collecting into a `Vec` first.
    pub fn write_iter<E, I>(&mut self, items: I, prefix: &str, sep: &str, suffix: &str)
    where
        E: Expression,
        I: IntoIterator<Item = E>,
    {
        let mut it = items.into_iter().peekable();
        if it.peek().is_none() {
            return;
        }
        self.push_str(prefix);
        for (i, item) in it.enumerate() {
            if i > 0 {
                self.push_str(sep);
            }
            self.write_expr(&item);
        }
        self.push_str(suffix);
    }

    /// Render a nested expression under a different dialect, keeping one shared
    /// argument list, placeholder counter and error slot.
    ///
    /// This is how a sub-query built for one dialect embeds in a query built for
    /// another — bob's `BaseQuery.WriteSQL` ignores the dialect handed to it and
    /// uses its own.
    pub fn write_with_dialect<E: Expression + ?Sized>(&mut self, dialect: &dyn Dialect, e: &E) {
        // The borrowed dialect outlives only this call, so the nested writer gets
        // a shorter lifetime and the buffers are moved through it and back.
        let mut nested = SqlWriter {
            sql: std::mem::take(&mut self.sql),
            args: std::mem::take(&mut self.args),
            dialect,
            next_arg: self.next_arg,
            error: self.error.take(),
        };
        e.write_sql(&mut nested);
        self.sql = nested.sql;
        self.args = nested.args;
        self.next_arg = nested.next_arg;
        self.error = nested.error;
    }

    /// Consume the writer, yielding the SQL and its arguments — or the recorded
    /// failure.
    pub fn finish(self) -> Result<(String, Vec<Value>)> {
        match self.error {
            Some(e) => Err(e),
            None => Ok((self.sql, self.args)),
        }
    }

    /// Consume the writer, yielding everything including any recorded failure.
    ///
    /// For a debug print that wants the partial SQL as well as the reason.
    pub fn into_parts(self) -> (String, Vec<Value>, Option<Error>) {
        (self.sql, self.args, self.error)
    }
}

/// `write!` into the SQL buffer, for the rare fragment that is easier formatted
/// than pushed.
/// A stack buffer wide enough for any `usize` in decimal.
///
/// `usize::MAX` is 20 digits on 64-bit; 20 is the exact bound, and the extra
/// room costs nothing on the stack.
fn itoa_buf() -> [u8; 20] {
    [0; 20]
}

/// Write `n` into `buf` and return the decimal digits.
///
/// `write!` into a `String` is what this replaces, and it allocates; this is
/// the same digits with the allocation removed.
fn format_usize(buf: &mut [u8; 20], mut n: usize) -> &str {
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + u8::try_from(n % 10).expect("a decimal digit");
        n /= 10;
        if n == 0 {
            break;
        }
    }
    // Every byte written is an ASCII digit.
    core::str::from_utf8(&buf[i..]).expect("ASCII digits")
}

impl fmt::Write for SqlWriter<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.sql.push_str(s);
        Ok(())
    }
}

/// Render an expression to SQL and arguments, numbering placeholders from 1.
///
/// This is what a query's `build()` calls, and the only place a recorded failure
/// becomes a `Result`.
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
    e.write_sql(&mut w);
    w.finish()
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use keelson_sqlcheck::testing::assert_frag_sql;

    #[test]
    fn push_usize_writes_the_same_digits_as_to_string() {
        for n in [0usize, 1, 9, 10, 99, 100, 12345, usize::MAX] {
            let mut w = SqlWriter::new(&Numbered);
            w.push_usize(n);
            assert_eq!(w.sql(), n.to_string(), "{n}");
        }
    }

    use super::*;
    use crate::dialect::testing::{Numbered, Positional, TestDialect};

    /// Where a fragment of each shape is legal, for the cases that can be judged
    /// as part of a statement. The rest of this module is about the writer's own
    /// mechanics — a bracketed tree, a half-written buffer, an offset placeholder
    /// run — and those are not SQL in any position; each says so where it stands.
    const COND: &str = r#"SELECT "id" FROM users WHERE {}"#;
    const VALUE: &str = r#"SELECT {} FROM users"#;

    /// `"col" = $n`
    #[derive(Debug)]
    struct Eq(&'static str, i32);

    impl Expression for Eq {
        fn write_sql(&self, w: &mut SqlWriter<'_>) {
            w.push_quoted(&[self.0]);
            w.push_str(" = ");
            w.push_arg(self.1);
        }
    }

    /// A sub-select, to prove nesting re-indexes.
    #[derive(Debug)]
    struct Sub(Vec<Eq>);

    impl Expression for Sub {
        fn write_sql(&self, w: &mut SqlWriter<'_>) {
            w.push_str("(SELECT 1 FROM users WHERE ");
            w.write_slice(&self.0, "", " AND ", "");
            w.push_str(")");
        }
    }

    #[test]
    fn placeholders_are_numbered_in_write_order() {
        let (sql, args) = build(
            &Numbered,
            &Sub(vec![Eq("age", 10), Eq("id", 20), Eq("name", 30)]),
        )
        .unwrap();
        assert_frag_sql(
            r#"SELECT "id" FROM users WHERE "id" IN {}"#,
            &sql,
            r#"(SELECT 1 FROM users WHERE "age" = $1 AND "id" = $2 AND "name" = $3)"#,
        );
        assert_eq!(args, vec![Value::I32(10), Value::I32(20), Value::I32(30)]);
    }

    #[test]
    fn nesting_continues_the_outer_numbering() {
        #[derive(Debug)]
        struct Outer;

        impl Expression for Outer {
            fn write_sql(&self, w: &mut SqlWriter<'_>) {
                w.write_expr(&Eq("age", 1));
                // `EXISTS`, because a sub-select in the middle of a conjunction has
                // to be a condition rather than the single value it returns.
                w.push_str(" AND EXISTS ");
                w.write_expr(&Sub(vec![Eq("id", 2), Eq("name", 3)]));
                w.push_str(" AND ");
                w.write_expr(&Eq("email", 4));
            }
        }

        let (sql, args) = build(&Numbered, &Outer).unwrap();
        assert_frag_sql(
            COND,
            &sql,
            concat!(
                r#""age" = $1 AND EXISTS (SELECT 1 FROM users WHERE "id" = $2 AND "name" = $3)"#,
                r#" AND "email" = $4"#
            ),
        );
        assert_eq!(args.len(), 4);
        assert_eq!(args[3], Value::I32(4));
    }

    /// Not judged: `($1 IN ($2 IN ($3)))` is a placeholder soup with nothing for
    /// PostgreSQL to infer a type from, and the nesting is the point rather than
    /// the SQL.
    #[test]
    fn a_subquery_three_levels_deep_never_restarts_numbering() {
        // The bug this guards against is a nested expression building its own
        // writer and starting from 1 again. Only push_arg moves the counter, and
        // there is one counter, so it cannot happen by construction.
        #[derive(Debug)]
        struct Nest(usize);

        impl Expression for Nest {
            fn write_sql(&self, w: &mut SqlWriter<'_>) {
                w.push_str("(");
                w.push_arg(self.0 as i32);
                if self.0 > 1 {
                    w.push_str(" IN ");
                    w.write_expr(&Nest(self.0 - 1));
                }
                w.push_str(")");
            }
        }

        let (sql, args) = build(&Numbered, &Nest(3)).unwrap();
        assert_eq!(sql, "($1 IN ($2 IN ($3)))");
        assert_eq!(args, vec![Value::I32(3), Value::I32(2), Value::I32(1)]);
    }

    /// Not judged: the brackets are the test's own notation for tree shape, not
    /// SQL syntax.
    #[test]
    fn interleaved_siblings_and_children_stay_in_write_order() {
        #[derive(Debug)]
        struct Pair(Box<dyn Expression>, Box<dyn Expression>);

        impl Expression for Pair {
            fn write_sql(&self, w: &mut SqlWriter<'_>) {
                w.push_str("[");
                w.write_expr(&self.0);
                w.push_str(" ");
                w.write_expr(&self.1);
                w.push_str("]");
            }
        }

        let tree = Pair(
            Box::new(Pair(Box::new(Eq("a", 1)), Box::new(Eq("b", 2)))),
            Box::new(Pair(Box::new(Eq("c", 3)), Box::new(Eq("d", 4)))),
        );
        let (sql, args) = build(&Numbered, &tree).unwrap();
        assert_eq!(sql, r#"[["a" = $1 "b" = $2] ["c" = $3 "d" = $4]]"#);
        assert_eq!(
            args,
            vec![Value::I32(1), Value::I32(2), Value::I32(3), Value::I32(4)]
        );
    }

    /// Not judged: a fragment whose lowest placeholder is `$3` has no `$1`, and no
    /// server will prepare that. Which is what `build_from` is for — splicing into
    /// a statement that already has two arguments.
    #[test]
    fn build_from_offsets_the_first_placeholder() {
        let (sql, args) = build_from(&Numbered, 3, &Sub(vec![Eq("age", 1), Eq("id", 2)])).unwrap();
        assert_eq!(
            sql,
            r#"(SELECT 1 FROM users WHERE "age" = $3 AND "id" = $4)"#
        );
        assert_eq!(args.len(), 2, "args are still returned from the start");
    }

    #[test]
    #[should_panic(expected = "1-based")]
    fn start_zero_is_rejected() {
        let _ = build_from(&Numbered, 0, &Eq("a", 1));
    }

    /// Not judged: `?` and backticks are MySQL's, and the judge reachable from
    /// this crate is PostgreSQL's. `keelson-mysql` is where that dialect answers.
    #[test]
    fn positional_dialects_ignore_the_index_but_still_order_args() {
        let (sql, args) = build(&Positional, &Sub(vec![Eq("age", 7), Eq("id", 8)])).unwrap();
        assert_eq!(sql, "(SELECT 1 FROM users WHERE `age` = ? AND `id` = ?)");
        assert_eq!(args, vec![Value::I32(7), Value::I32(8)]);
    }

    #[test]
    fn arg_position_tracks_the_next_placeholder() {
        let mut w = SqlWriter::new(&Numbered);
        assert_eq!(w.arg_position(), 1);
        w.push_arg(1i32);
        assert_eq!(w.arg_position(), 2);
        w.push_str(" -- not an arg");
        assert_eq!(w.arg_position(), 2);
        w.push_arg("two");
        assert_eq!(w.arg_position(), 3);
    }

    #[test]
    fn raw_strings_of_every_stored_form_are_expressions() {
        let (sql, args) = build(&Numbered, "id = 1").unwrap();
        assert_frag_sql(COND, &sql, "id = 1");
        assert!(args.is_empty());

        let (sql, _) = build(&Numbered, &String::from("id = 2")).unwrap();
        assert_frag_sql(COND, &sql, "id = 2");

        let borrowed: Cow<'static, str> = Cow::Borrowed("id = 3");
        let (sql, _) = build(&Numbered, &borrowed).unwrap();
        assert_frag_sql(COND, &sql, "id = 3");

        let owned: Cow<'static, str> = Cow::Owned(String::from("id = 4"));
        let (sql, _) = build(&Numbered, &owned).unwrap();
        assert_frag_sql(COND, &sql, "id = 4");

        let boxed: Box<dyn Expression> = Box::new(Eq("age", 1));
        let (sql, _) = build(&Numbered, &boxed).unwrap();
        assert_frag_sql(COND, &sql, r#""age" = $1"#);

        let shared: DynExpr = dyn_expr(Eq("id", 2));
        let (sql, _) = build(&Numbered, &shared).unwrap();
        assert_frag_sql(COND, &sql, r#""id" = $1"#);
    }

    #[test]
    fn numbers_render_as_literals_not_placeholders() {
        let (sql, args) = build(&Numbered, &20i64).unwrap();
        assert_frag_sql(VALUE, &sql, "20");
        assert!(args.is_empty(), "a literal binds nothing");
    }

    #[test]
    fn expr_fn_wraps_a_closure() {
        let e = expr_fn(|w: &mut SqlWriter<'_>| {
            w.push_str("LIMIT ");
            w.push_arg(5i64);
        });
        let (sql, args) = build(&Numbered, &e).unwrap();
        assert_frag_sql(r#"SELECT "id" FROM users {}"#, &sql, "LIMIT $1");
        assert_eq!(args, vec![Value::I64(5)]);
    }

    #[test]
    fn write_if_skips_everything_including_the_affixes() {
        let mut w = SqlWriter::new(&Numbered);
        w.write_if(false, " WHERE ", &Eq("a", 1), "!");
        assert_eq!(w.sql(), "");
        assert_eq!(w.arg_position(), 1, "a skipped arg must not advance");
        w.write_if(true, " WHERE ", &Eq("a", 1), "!");
        let (sql, args) = w.finish().unwrap();
        assert_eq!(sql, r#" WHERE "a" = $1!"#);
        assert_eq!(args.len(), 1);
    }

    #[test]
    fn write_if_some_follows_the_option() {
        let mut w = SqlWriter::new(&Numbered);
        w.write_if_some(None::<&Eq>, " LIMIT ", "");
        assert_eq!(w.sql(), "");
        w.write_if_some(Some(&Eq("a", 1)), " WHERE ", ";");
        assert_eq!(w.sql(), r#" WHERE "a" = $1;"#);
    }

    #[test]
    fn write_slice_is_a_no_op_when_empty() {
        let mut w = SqlWriter::new(&Numbered);
        w.write_slice::<Eq>(&[], " WHERE ", " AND ", ";");
        assert_eq!(w.sql(), "");

        w.write_iter(Vec::<String>::new(), "(", ", ", ")");
        assert_eq!(w.sql(), "");

        w.write_iter(vec!["a", "b"], "(", ", ", ")");
        assert_eq!(w.sql(), "(a, b)");
    }

    #[test]
    fn push_quoted_joins_with_dots_and_drops_empty_parts() {
        let mut w = SqlWriter::new(&Numbered);
        w.push_quoted(&["users", "id"]);
        w.push_str(" ");
        w.push_quoted(&["", "id"]);
        w.push_str(" ");
        w.push_quoted::<&str>(&[]);
        w.push_str(" ");
        // The form a clause actually stores.
        w.push_quoted(&[Cow::Borrowed("a"), Cow::Owned("b".to_owned())]);
        assert_eq!(w.sql(), r#""users"."id" "id"  "a"."b""#);
    }

    #[test]
    fn named_args_do_not_consume_an_arg_slot() {
        let mut w = SqlWriter::new(&TestDialect);
        w.push_arg(1i32);
        w.push_str(", ");
        w.push_named_arg("name");
        w.push_str(", ");
        w.push_arg(2i32);
        let (sql, args) = w.finish().unwrap();
        assert_eq!(sql, "?1, :name, ?2");
        assert_eq!(args, vec![Value::I32(1), Value::I32(2)]);
    }

    #[test]
    fn a_nested_dialect_shares_the_arg_list_and_counter() {
        #[derive(Debug)]
        struct Mixed;

        impl Expression for Mixed {
            fn write_sql(&self, w: &mut SqlWriter<'_>) {
                w.write_expr(&Eq("a", 1));
                w.push_str(" AND ");
                w.write_with_dialect(&Positional, &Eq("b", 2));
                w.push_str(" AND ");
                w.write_expr(&Eq("c", 3));
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
    fn a_recorded_error_is_surfaced_by_build_and_only_by_build() {
        #[derive(Debug)]
        struct Bad;

        impl Expression for Bad {
            fn write_sql(&self, w: &mut SqlWriter<'_>) {
                w.push_str("x = ");
                w.push_named_arg("nope");
            }
        }

        // write_sql itself cannot fail, so the SQL is still there...
        let mut w = SqlWriter::new(&Numbered);
        w.write_expr(&Bad);
        assert_eq!(w.sql(), "x = ");
        assert!(matches!(w.error(), Some(Error::NoNamedArgs)));

        // ...and build is what refuses to hand it over.
        assert!(matches!(build(&Numbered, &Bad), Err(Error::NoNamedArgs)));

        // A failure inside a helper still propagates out of the whole build.
        let mut w = SqlWriter::new(&Numbered);
        w.write_slice(&[Bad], "(", ", ", ")");
        assert!(w.finish().is_err());
    }

    #[test]
    fn the_first_recorded_error_wins() {
        let mut w = SqlWriter::new(&Numbered);
        w.record_error(Error::Incomplete("a table"));
        w.record_error(Error::NoNamedArgs);
        let (_, _, err) = w.into_parts();
        assert!(matches!(err, Some(Error::Incomplete("a table"))));
    }

    #[test]
    fn fmt_write_appends_to_the_same_buffer() {
        let mut w = SqlWriter::new(&Numbered);
        write!(w, "OFFSET {}", 4).unwrap();
        assert_eq!(w.sql(), "OFFSET 4");
    }

    #[test]
    fn the_writer_exposes_its_dialect_to_nested_expressions() {
        #[derive(Debug)]
        struct UsesDialect;

        impl Expression for UsesDialect {
            fn write_sql(&self, w: &mut SqlWriter<'_>) {
                let d = w.dialect();
                d.write_quoted(w, "col");
            }
        }

        let (sql, _) = build(&Positional, &UsesDialect).unwrap();
        assert_eq!(sql, "`col`");
    }
}
