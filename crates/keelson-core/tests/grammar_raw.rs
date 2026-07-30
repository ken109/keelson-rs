//! Raw SQL and placeholder handling, from outside the crate.
//!
//! This is where silent corruption lives. An off-by-one in the placeholder
//! counter binds the wrong value to the wrong column and every layer downstream
//! reports success, so the cases here assert the *argument list* as hard as they
//! assert the SQL — and they render under `$N` (PostgreSQL-shaped) wherever
//! numbering is the point, because [`Positional`] (`?`, MySQL-shaped) renders the
//! same string no matter how badly the counter is wrong.
//!
//! # Where the expectations come from
//!
//! Every expected string is derived from the documented rendering rules in
//! `keelson_core::expr` (which are themselves bob's `WriteSQL` behaviour, arm by
//! arm) or from bob's `expr/raw.go` — named in a comment wherever the construct is
//! not obvious. None of it was copied out of a program's output.
//!
//! # What is judged, and what cannot be
//!
//! `keelson-core` has no dialect of its own, so rendering here goes through
//! [`PgLike`] from `keelson-sqlcheck` — `$N` and `"id"`, close enough to
//! PostgreSQL that libpg_query and (under `--features live-docker`) a real
//! PostgreSQL 17 can judge the result. Anything that is or can be phrased as a
//! whole statement therefore goes through [`assert_stmt`] or [`assert_frag`],
//! which put the grammar and the engine in front of the string comparison.
//!
//! Three kinds of case stay a bare `assert_eq!`, and say so where they are:
//!
//! - **A template scan.** `\?` handling operates on arbitrary text; `a\?b?c` is
//!   not SQL and pretending otherwise would test the frame instead.
//! - **A deliberately foreign dialect.** The cross-dialect sub-query cases render
//!   MySQL backticks or SQLite `:name` *inside* a `$N` statement — that is the
//!   property under test, and no single parser accepts it.
//! - **A fragment with no legal home.** `"data" ? 'key'` needs a jsonb column the
//!   shared schema has not got, and `$1 ARRAY[…] @> ARRAY[$2] $3` — three
//!   placeholders with an operator between two of them — is a deliberate soup that
//!   no position accepts.
//!
//! Every name in a judged case comes from `tests/schema/psql.sql` — `users`,
//! `posts`, `comments`, `tags`, `post_tags` — because an engine resolves names.

use keelson_core::expr::{Chain, Expr, IntoExpr, RawArg, arg, arg_group, f, quote};
use keelson_core::testing::{Numbered, Positional, TestDialect};
use keelson_core::{
    Dialect, DynExpr, Error, Expression, Query, QueryType, SqlWriter, Value, build, build_from,
    dyn_expr,
};
use keelson_sqlcheck::testing::{PgLike, assert_frag, assert_stmt};

/// Render under the `$N` dialect — the one that shows a numbering bug, and the one
/// the psql judges understand.
fn pg(e: &Expr) -> (String, Vec<Value>) {
    build(&PgLike, e).expect("render")
}

/// Render and take the SQL only.
fn pg_sql(e: &Expr) -> String {
    pg(e).0
}

/// Render without surfacing the failure, for the cases where the partial SQL and
/// the bound arguments are themselves the thing under test.
fn parts(d: &dyn Dialect, e: &Expr) -> (String, Vec<Value>, Option<Error>) {
    let mut w = SqlWriter::new(d);
    w.write_expr(e);
    w.into_parts()
}

/// Where a fragment of each shape is legal. A lone placeholder is fine in a select
/// list — PostgreSQL resolves a parameter it has nothing else to go on to `text` —
/// but an operator whose *every* operand is one cannot be resolved at all, which is
/// why one or two frames below supply a `CAST`.
const COND: &str = r#"SELECT "id" FROM users WHERE {}"#;
const POST_VALUE: &str = "SELECT {} FROM posts";
const TAIL: &str = r#"SELECT "id" FROM users {}"#;

fn text(s: &str) -> Value {
    Value::Text(s.to_owned())
}

/// `ARRAY['rust'] @> ARRAY[$n]` — a dialect-specific operator, as a dialect crate
/// would write one: an ordinary `Expression` that reaches core through
/// `Expr::Custom`. Array literals rather than a column, because `@>` needs an
/// operand type the shared schema does not have a column of.
#[derive(Debug, Clone)]
struct Contains(Value);

impl Expression for Contains {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.push_str("ARRAY['rust'] @> ARRAY[");
        w.push_arg(self.0.clone());
        w.push_str("]");
    }
}

// ---------------------------------------------------------------------------
// `?` is data in some nodes and syntax in exactly one
// ---------------------------------------------------------------------------

/// `Expr::Raw` is documented as verbatim: "`?` is *not* rewritten". PostgreSQL's
/// jsonb key-existence operator is spelled `?`, so this is the only way to write
/// it without escaping.
#[test]
fn raw_sql_never_has_its_question_marks_rewritten() {
    let e = Expr::raw("\"data\" ? 'key'");
    let (sql, args) = pg(&e);
    assert_eq!(sql, "\"data\" ? 'key'");
    assert!(args.is_empty(), "raw SQL binds nothing");
}

/// A `?` between single quotes is still a hole. `write_template` is a byte scan
/// that does not track quoting — bob's `convertQuestionMarks` does not either —
/// so the string literal is rewritten along with the real placeholder. With the
/// arguments the author actually meant, the count check catches it.
#[test]
fn a_question_mark_inside_a_string_literal_in_a_template_is_a_hole() {
    let e = Expr::template("a = '?' AND b = ?", [RawArg::value(1i32)]);
    let err = build(&PgLike, &e).unwrap_err();
    assert_eq!(
        err.to_string(),
        "Bad Statement: has 2 placeholders but 1 args: a = '?' AND b = ?"
    );
}

/// The dangerous variant of the same mistake: supply an argument for the quoted
/// `?` as well and there is nothing left to complain about. This test exists to
/// pin the failure mode, not to bless it — the remedy is the next case.
#[test]
fn a_quoted_question_mark_with_a_matching_arg_corrupts_the_literal_silently() {
    let e = Expr::template(
        "a = '?' AND b = ?",
        [RawArg::value("x"), RawArg::value(1i32)],
    );
    let (sql, args) = pg(&e);
    assert_eq!(sql, "a = '$1' AND b = $2");
    assert_eq!(args, vec![text("x"), Value::I32(1)]);
}

/// `\?` is the fix, and the escape is removed before the SQL exists, so the
/// literal reaching the server is a plain `'?'`.
#[test]
fn escaping_is_how_a_question_mark_survives_inside_a_literal() {
    let e = Expr::template(r"a = '\?' AND b = ?", [RawArg::value(1i32)]);
    let (sql, args) = pg(&e);
    assert_eq!(sql, "a = '?' AND b = $1");
    assert_eq!(args, vec![Value::I32(1)], "only the real hole binds");
}

/// `Expr::Literal` is not a template: it renders `'` + the text + `'` with no
/// scanning at all.
#[test]
fn a_question_mark_in_a_literal_node_is_never_scanned() {
    let e = Expr::binary(quote("a"), "=", Expr::literal("?"));
    let (sql, args) = pg(&e);
    assert_eq!(sql, r#""a" = '?'"#);
    assert!(args.is_empty());
}

/// On a `?`-placeholder dialect the escaped `?` and the bound argument are
/// indistinguishable in the SQL — the argument list is the only thing that says
/// which is which. This is exactly why the numbering cases below use `PgLike`.
#[test]
fn on_a_positional_dialect_a_literal_and_a_placeholder_look_identical() {
    // Two escapes and two holes, alternating: `a ? $1 AND b ? $2` in intent.
    let e = Expr::template(
        r"a \? ? AND b \? ?",
        [RawArg::value(1i32), RawArg::value(2i32)],
    );
    let (sql, args) = build(&Positional, &e).expect("render");
    assert_eq!(sql, "a ? ? AND b ? ?");
    assert_eq!(args, vec![Value::I32(1), Value::I32(2)]);
    // Under `$N` the two roles separate again.
    assert_eq!(pg_sql(&e), "a ? $1 AND b ? $2");
}

/// The same fragment under `$N`, where the two roles are visible.
#[test]
fn the_json_existence_operators_need_escaping_but_keep_their_meaning() {
    let e = Expr::template(
        r#""tags" \?| ? AND "tags" \?& ?"#,
        [RawArg::value("a"), RawArg::value("b")],
    );
    let (sql, args) = pg(&e);
    assert_eq!(sql, r#""tags" ?| $1 AND "tags" ?& $2"#);
    assert_eq!(args, vec![text("a"), text("b")]);
}

// ---------------------------------------------------------------------------
// The escape rules, decided and documented
// ---------------------------------------------------------------------------

/// `\?` alone: one literal question mark, no hole, no argument, no mismatch.
#[test]
fn a_lone_escape_is_a_literal_question_mark_and_not_a_hole() {
    let e = Expr::template(r"\?", []);
    let (sql, args) = pg(&e);
    assert_eq!(sql, "?");
    assert!(args.is_empty());
}

/// Escapes on both sides of literal text, so the scan has to resume in the middle
/// rather than at a boundary.
#[test]
fn escapes_survive_on_both_sides_of_ordinary_text() {
    assert_eq!(pg_sql(&Expr::template(r"\?x\?", [])), "?x?");
}

/// A hole then an escape: the escape is found on a later pass, after `rest` has
/// already moved past the hole.
#[test]
fn an_escape_after_a_hole_is_still_an_escape() {
    let e = Expr::template(r"? \?", [RawArg::value(1i32)]);
    assert_eq!(pg_sql(&e), "$1 ?");
}

/// An escape then a hole: the first `?` in the string is the escaped one, so the
/// hole that follows must still take position 1.
#[test]
fn an_escape_before_a_hole_does_not_consume_a_position() {
    let e = Expr::template(r"\? ?", [RawArg::value(1i32)]);
    let (sql, args) = pg(&e);
    assert_eq!(sql, "? $1");
    assert_eq!(args, vec![Value::I32(1)]);
}

/// Only `\?` is an escape sequence. A backslash in front of anything else is
/// ordinary text — nothing is stripped and nothing is doubled.
#[test]
fn a_backslash_that_is_not_followed_by_a_question_mark_is_ordinary_text() {
    let e = Expr::template(r"a\b = ? AND c \ d", [RawArg::value(1i32)]);
    assert_eq!(pg_sql(&e), r"a\b = $1 AND c \ d");
}

/// There is no escape for the escape. `\\?` is a literal backslash followed by an
/// escaped `?`, because the scan recognises exactly one two-byte sequence and the
/// second backslash is the one adjacent to the `?`.
#[test]
fn a_doubled_backslash_does_not_un_escape_the_question_mark() {
    let e = Expr::template(r"a \\? AND b = ?", [RawArg::value(1i32)]);
    let (sql, args) = pg(&e);
    assert_eq!(sql, r"a \? AND b = $1");
    assert_eq!(args, vec![Value::I32(1)]);
}

/// The consequence of the rule above: a backslash immediately before a *hole*
/// cannot be expressed in one template at all, since it would be eaten. Two
/// fragments give it.
#[test]
fn a_backslash_directly_before_a_hole_needs_two_fragments() {
    assert_eq!(pg_sql(&Expr::template(r"\?", [])), "?");
    let two_parts = Expr::join_with("", (Expr::raw(r"\"), arg(1i32)));
    assert_eq!(pg_sql(&two_parts), r"\$1");
}

/// The scan slices on byte offsets. Both `?` and `\` are ASCII, so the offsets
/// always land on character boundaries — but only if the arithmetic is done on
/// bytes throughout. Non-ASCII text on both sides of an escape *and* a hole is
/// what would panic if it were not.
#[test]
fn multibyte_text_around_an_escape_is_not_sliced_mid_character() {
    let e = Expr::template(
        r"名前 \? ? AND 年齢 > ?",
        [RawArg::value("さくら"), RawArg::value(20i32)],
    );
    let (sql, args) = pg(&e);
    assert_eq!(sql, "名前 ? $1 AND 年齢 > $2");
    assert_eq!(args, vec![text("さくら"), Value::I32(20)]);
}

/// A trailing backslash with nothing after it ends the scan harmlessly.
#[test]
fn a_trailing_backslash_is_written_out_unchanged() {
    let e = Expr::template(r"a = ? \", [RawArg::value(1i32)]);
    assert_eq!(pg_sql(&e), r"a = $1 \");
}

/// `\?` in a template is the *only* escaping core performs. A quote character
/// inside an identifier or a string literal is passed through untouched: doubling
/// it is a `Dialect::write_quoted` decision (real psql doubles the `"`), and
/// `Expr::Literal` is documented as escaping nothing because it is for SQL the
/// program itself wrote. Text from outside belongs in `arg`, which binds.
#[test]
fn core_escapes_nothing_except_a_template_question_mark() {
    // The stand-in dialects quote without doubling, which is what makes the
    // delegation visible here.
    assert_eq!(pg_sql(&quote(r#"we"ird"#)), r#""we"ird""#);
    assert_eq!(pg_sql(&Expr::literal("O'Brien")), "'O'Brien'");
    // The safe spelling of the same value: bound, not interpolated.
    let (sql, args) = pg(&arg("O'Brien"));
    assert_eq!(sql, "$1");
    assert_eq!(args, vec![text("O'Brien")]);
}

/// A template is atomic, so an enclosing operator will not parenthesise it: the
/// author wrote the SQL and gets it back unedited, precedence included. Pinned
/// because it is a footgun that a future "helpful" change might try to fix.
#[test]
fn a_template_is_never_parenthesised_by_its_surroundings() {
    let e = keelson_core::expr::not(Expr::template(
        "age = ? OR is_active",
        [RawArg::value(1i32)],
    ));
    // Judged, and the judge is the point: this parses, and it does not mean what
    // the `NOT` looks like it means — `NOT age = $1 OR is_active` is
    // `(NOT age = $1) OR is_active`, because the template is atomic and keeps the
    // author's precedence, parentheses and all.
    assert_frag(COND, &e, "NOT age = $1 OR is_active");
}

// ---------------------------------------------------------------------------
// A placeholder in every position a value can appear
// ---------------------------------------------------------------------------

/// Select-list position, with an alias. `Chain::as_` is documented as *not*
/// parenthesising, because `($1 AS "one")` is a syntax error in a select list — so
/// the judge is what distinguishes this from the shape a "helpful" parenthesis
/// would produce.
#[test]
fn a_placeholder_in_the_select_list_with_an_alias() {
    let e = arg(1i32).as_("one");
    let args = assert_frag("SELECT {} FROM users", &e, r#"$1 AS "one""#);
    assert_eq!(args, vec![Value::I32(1)]);
}

/// Function-argument position.
#[test]
fn a_placeholder_as_a_function_argument() {
    let e = Expr::func("coalesce", (quote("title"), arg("untitled")));
    let args = assert_frag(POST_VALUE, &e, r#"coalesce("title", $1)"#);
    assert_eq!(args, vec![text("untitled")]);
}

/// `IN` list position. `Chain::in_` always parenthesises its operands, and
/// `Expr::args` is the unparenthesised list that fills them.
#[test]
fn placeholders_as_an_in_list() {
    let e = quote("id").in_(Expr::args([1i32, 2, 3]));
    let args = assert_frag(COND, &e, r#"("id" IN ($1, $2, $3))"#);
    assert_eq!(args, vec![Value::I32(1), Value::I32(2), Value::I32(3)]);
}

/// Both operands of `BETWEEN`, which is a `Join` of five parts.
#[test]
fn placeholders_on_both_sides_of_between() {
    let e = quote("age").between(arg(18i32), arg(65i32));
    let args = assert_frag(COND, &e, r#"("age" BETWEEN $1 AND $2)"#);
    assert_eq!(args, vec![Value::I32(18), Value::I32(65)]);
}

/// Every slot of a `CASE`: the condition, each result and the `ELSE`.
/// `CaseBuilder::else_` applies the parenthesisation rule, so the whole `CASE` is
/// self-delimiting.
#[test]
fn placeholders_in_every_slot_of_a_case() {
    let e = keelson_core::expr::case()
        .when(quote("status").eq(arg("new")), arg(1i32))
        .when(quote("status").eq(arg("old")), arg(2i32))
        .else_(arg(0i32));
    // The cast is the frame's: every result is a placeholder, so the CASE has no
    // branch of known type for PostgreSQL to infer one from.
    let args = assert_frag(
        "SELECT CAST({} AS integer) FROM posts",
        &e,
        r#"(CASE WHEN ("status" = $1) THEN $2 WHEN ("status" = $3) THEN $4 ELSE $5 END)"#,
    );
    assert_eq!(
        args,
        vec![
            text("new"),
            Value::I32(1),
            text("old"),
            Value::I32(2),
            Value::I32(0)
        ]
    );
}

/// A cast's operand.
#[test]
fn a_placeholder_as_a_cast_operand() {
    let e = Expr::cast(arg("2026-07-30"), "date");
    let args = assert_frag("SELECT {} FROM users", &e, "CAST($1 AS date)");
    assert_eq!(args, vec![text("2026-07-30")]);
}

/// `LIMIT`/`OFFSET` position. Numbers reaching a clause through `IntoExpr` are
/// literals; a bound row count has to be asked for with `arg`.
#[test]
fn placeholders_in_the_limit_and_offset_slots() {
    let bound = Expr::join((
        Expr::raw("LIMIT"),
        arg(10i64),
        Expr::raw("OFFSET"),
        arg(20i64),
    ));
    let args = assert_frag(TAIL, &bound, "LIMIT $1 OFFSET $2");
    assert_eq!(args, vec![Value::I64(10), Value::I64(20)]);

    let literal = Expr::join((Expr::raw("LIMIT"), 10i64.into_expr()));
    assert!(assert_frag(TAIL, &literal, "LIMIT 10").is_empty());
}

/// A multi-row `VALUES` tail. Two rows of two is the shape where a
/// column/argument mismatch would be invisible in the SQL and fatal in the
/// database.
#[test]
fn placeholders_in_several_value_rows_stay_in_row_order() {
    let e = Expr::join_with(", ", (arg_group(["a", "b"]), arg_group(["c", "d"])));
    let args = assert_frag(
        r#"INSERT INTO tags ("id", "name") VALUES {}"#,
        &e,
        "($1, $2), ($3, $4)",
    );
    assert_eq!(args, vec![text("a"), text("b"), text("c"), text("d")]);
}

/// Zero placeholders in a position that must hold a value. bob's `Arg()` with no
/// values writes `NULL`, and `Placeholder(0)` is that call, so an empty `IN` list
/// becomes `IN (NULL)` — never true, but a statement the server will accept, where
/// `IN ()` is a syntax error.
#[test]
fn an_empty_placeholder_list_renders_null_rather_than_nothing() {
    assert_frag(
        "SELECT {} FROM users",
        &keelson_core::expr::placeholders(0),
        "NULL",
    );
    let args = assert_frag(
        r#"SELECT "id" FROM users WHERE "id" IN {}"#,
        &arg_group(Vec::<i32>::new()),
        "(NULL)",
    );
    assert!(args.is_empty());
    assert_frag(
        COND,
        &quote("id").in_(Expr::args(Vec::<i32>::new())),
        r#"("id" IN (NULL))"#,
    );
}

/// The left-hand side of an operator, which is the position a builder tends to
/// assume is always a column.
#[test]
fn a_placeholder_on_the_left_of_an_operator() {
    let e = arg(1i32).eq(quote("id"));
    let args = assert_frag(COND, &e, r#"($1 = "id")"#);
    assert_eq!(args, vec![Value::I32(1)]);
}

/// Inside a function call that also carries a window: the argument is written
/// before the `OVER`, so it takes the earlier position.
#[test]
fn a_placeholder_inside_a_windowed_function_call() {
    let e = f("lag", (quote("views"), arg(1i32))).over(Expr::raw("\"w\""));
    let args = assert_frag(
        r#"SELECT {} FROM posts WINDOW "w" AS ()"#,
        &e,
        r#"lag("views", $1) OVER ("w")"#,
    );
    assert_eq!(args, vec![Value::I32(1)]);
}

/// Every clause of a statement at once. Assembled by hand out of fragments —
/// core has no query builder — so what it pins is strictly the left-to-right
/// numbering across positions, which is the property a clause layer must not
/// break.
///
/// It is a whole statement, so the claim that it is "valid PostgreSQL against the
/// shared test schema" is now checked rather than asserted: a placeholder in the
/// select list, in `GROUP BY`, in `ORDER BY`, in `HAVING`, in `LIMIT` and in
/// `OFFSET` all at once is exactly the kind of thing a hand-written expected string
/// is happy to agree with either way.
#[test]
fn placeholders_are_numbered_left_to_right_across_a_whole_statement() {
    let e = Expr::join((
        Expr::raw("SELECT"),
        arg(1i32).as_("tag"),
        Expr::raw(", count(*) FROM posts WHERE"),
        quote("title").like(arg("%rust%")),
        Expr::raw("GROUP BY"),
        arg(1i32),
        Expr::raw("HAVING count(*) >"),
        arg(2i64),
        Expr::raw("ORDER BY"),
        arg(1i32),
        Expr::raw("LIMIT"),
        arg(10i64),
        Expr::raw("OFFSET"),
        arg(20i64),
    ));
    let args = assert_stmt(
        &e,
        concat!(
            r#"SELECT $1 AS "tag" , count(*) FROM posts WHERE ("title" LIKE $2) "#,
            r#"GROUP BY $3 HAVING count(*) > $4 ORDER BY $5 LIMIT $6 OFFSET $7"#
        ),
    );
    assert_eq!(args.len(), 7);
    assert_eq!(
        args,
        vec![
            Value::I32(1),
            text("%rust%"),
            Value::I32(1),
            Value::I64(2),
            Value::I32(1),
            Value::I64(10),
            Value::I64(20),
        ],
        "the same value bound three times occupies three positions"
    );
}

/// The two stand-in dialects have to write the same SQL, because the in-module
/// tests in `src/**` render with `keelson_core::testing::Numbered` and judge the
/// string with `keelson_sqlcheck::testing::assert_frag_sql` — they cannot name
/// `PgLike`, since the unit-test target compiles this crate a second time and the
/// `Expression` bound would not hold. That indirection is only sound while the two
/// agree, so this is where it is checked. An integration test can see both.
#[test]
fn numbered_and_pg_like_render_the_same_sql() {
    let cases = [
        Expr::join((Expr::raw("SELECT"), arg(1i32), Expr::raw("FROM users"))),
        quote(("users", "age")).between(arg(18i32), arg(65i32)),
        Expr::args((1..=12i32).collect::<Vec<_>>()),
        Expr::template("f(?, \\?, ?)", [RawArg::value(1i32), RawArg::value(2i32)]),
        Expr::cast(arg("2026-07-30"), "date"),
    ];
    for e in cases {
        assert_eq!(
            build(&Numbered, &e).expect("render"),
            build(&PgLike, &e).expect("render"),
            "the two stand-in dialects disagree, which invalidates every \
             assert_frag_sql in src/**: {e:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Numbering across the raw/built boundary
// ---------------------------------------------------------------------------

/// A built expression spliced into a template through `RawArg::expr`, with values
/// on either side. The spliced expression binds two arguments from one hole, so
/// the hole after it must jump by two.
#[test]
fn a_built_expression_spliced_into_a_template_consumes_as_many_positions_as_it_binds() {
    let inner = quote("age").between(arg(18i32), arg(65i32));
    let e = Expr::template(
        "id = ? AND ? AND id <> ?",
        [
            RawArg::value(1i32),
            RawArg::expr(inner),
            RawArg::value(2i32),
        ],
    );
    let args = assert_frag(
        COND,
        &e,
        r#"id = $1 AND ("age" BETWEEN $2 AND $3) AND id <> $4"#,
    );
    assert_eq!(
        args,
        vec![Value::I32(1), Value::I32(18), Value::I32(65), Value::I32(2)]
    );
}

/// A template inside a template. Two independent scans, one counter.
#[test]
fn a_template_nested_in_a_template_continues_the_numbering() {
    let inner = Expr::template("g(?, ?)", [RawArg::value(2i32), RawArg::value(3i32)]);
    let e = Expr::template(
        "f(?, ?, ?)",
        [
            RawArg::value(1i32),
            RawArg::expr(inner),
            RawArg::value(4i32),
        ],
    );
    let (sql, args) = pg(&e);
    assert_eq!(sql, "f($1, g($2, $3), $4)");
    assert_eq!(args.len(), 4);
    assert_eq!(
        args[3],
        Value::I32(4),
        "the last hole is still the last arg"
    );
}

/// A template in the middle of a built tree — the boundary crossed in the other
/// direction.
#[test]
fn a_template_inside_a_built_expression_continues_the_numbering() {
    let e = Expr::join_with(
        " AND ",
        (
            quote("age").eq(arg(1i32)),
            Expr::template(
                r#"coalesce("id", ?) = ?"#,
                [RawArg::value(2i32), RawArg::value(3i32)],
            ),
            quote("id").eq(arg(4i32)),
        ),
    );
    let args = assert_frag(
        COND,
        &e,
        r#"("age" = $1) AND coalesce("id", $2) = $3 AND ("id" = $4)"#,
    );
    assert_eq!(
        args,
        vec![Value::I32(1), Value::I32(2), Value::I32(3), Value::I32(4)]
    );
}

/// A `Custom` expression from a dialect crate binds through the same writer, so
/// it neither restarts nor skips positions.
#[test]
fn a_custom_expression_shares_the_placeholder_counter() {
    let e = Expr::join((arg(1i32), Expr::custom(Contains(text("rust"))), arg(2i32)));
    let (sql, args) = pg(&e);
    // Not judged: three placeholders in a row with an operator between two of them
    // is a deliberate soup, not a fragment any statement position accepts. What it
    // pins is that `Custom` draws from the same counter.
    assert_eq!(sql, r#"$1 ARRAY['rust'] @> ARRAY[$2] $3"#);
    assert_eq!(args, vec![Value::I32(1), text("rust"), Value::I32(2)]);
}

/// All three kinds of boundary at once: a built node containing a template
/// containing a dialect-specific `Custom`.
#[test]
fn built_template_and_custom_nest_three_deep_with_one_counter() {
    let custom = Expr::custom(Contains(text("rust")));
    let tpl = Expr::template(
        "(? OR ?)",
        [RawArg::expr(custom), RawArg::value("fallback")],
    );
    let e = Expr::binary(arg(0i32), "AND", tpl);
    let (sql, args) = pg(&e);
    // Not judged, for the same reason: `$1 AND (…)` has nothing to type `$1` from.
    assert_eq!(sql, r#"$1 AND (ARRAY['rust'] @> ARRAY[$2] OR $3)"#);
    assert_eq!(args, vec![Value::I32(0), text("rust"), text("fallback")]);
}

/// `build_from` — bob's `BuildN` — offsets a template's holes too, which is what
/// makes a hand-written fragment safe to splice into a statement that already has
/// arguments.
#[test]
fn build_from_offsets_a_templates_holes() {
    let e = Expr::template(
        "a = ? AND b = ?",
        [RawArg::value(1i32), RawArg::value(2i32)],
    );
    let (sql, args) = build_from(&PgLike, 5, &e).expect("render");
    assert_eq!(sql, "a = $5 AND b = $6");
    assert_eq!(
        args,
        vec![Value::I32(1), Value::I32(2)],
        "args are still returned from the start"
    );
}

/// The same fragment on a positional dialect: identical SQL for any start, and
/// the argument order is the only carrier of meaning.
#[test]
fn a_positional_dialect_hides_the_numbering_so_the_arg_order_is_the_contract() {
    let e = Expr::join((
        arg("a"),
        Expr::template("f(?)", [RawArg::value("b")]),
        arg("c"),
    ));
    let (sql, args) = build(&Positional, &e).expect("render");
    assert_eq!(sql, "? f(?) ?");
    assert_eq!(args, vec![text("a"), text("b"), text("c")]);

    let (offset_sql, offset_args) = build_from(&Positional, 9, &e).expect("render");
    assert_eq!(offset_sql, sql, "the position is dropped");
    assert_eq!(offset_args, args);
}

/// Two-digit positions. `$10` must be one token, not `$1` followed by a `0` — the
/// classic formatting off-by-one that only shows up past nine arguments.
#[test]
fn positions_past_nine_are_written_as_one_token() {
    let e = Expr::args((1..=12i32).collect::<Vec<_>>());
    let args = assert_frag(
        r#"SELECT "id" FROM users WHERE "id" IN ({})"#,
        &e,
        "$1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12",
    );
    assert_eq!(args.len(), 12);
    assert_eq!(args[9], Value::I32(10));
}

/// The same, driven by a template's holes rather than by an argument list.
#[test]
fn a_template_with_more_than_nine_holes_numbers_them_all() {
    let sql_text = "f(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)";
    let e = Expr::template(sql_text, (1..=11i32).map(RawArg::value));
    let (sql, args) = pg(&e);
    assert_eq!(sql, "f($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)");
    assert_eq!(args.len(), 11);
    assert_eq!(args[10], Value::I32(11));
}

// ---------------------------------------------------------------------------
// Count mismatches
// ---------------------------------------------------------------------------

/// Fewer arguments than holes. bob's behaviour, reproduced deliberately: the SQL
/// is written in full before the count is checked so the error can quote the
/// clause, and each missing argument binds `NULL` so that the positions *after*
/// it are still right.
#[test]
fn fewer_args_than_holes_binds_null_and_keeps_the_later_positions() {
    let e = Expr::template("a = ? AND b = ? AND c = ?", [RawArg::value(1i32)]);
    let (sql, args, err) = parts(&PgLike, &e);
    assert_eq!(sql, "a = $1 AND b = $2 AND c = $3");
    assert_eq!(args, vec![Value::I32(1), Value::Null, Value::Null]);
    assert_eq!(
        err.expect("a mismatch is recorded").to_string(),
        "Bad Statement: has 3 placeholders but 1 args: a = ? AND b = ? AND c = ?"
    );
}

/// More arguments than holes. The surplus is never bound, so a caller that
/// ignored the error would send too few values rather than the wrong ones.
#[test]
fn more_args_than_holes_leaves_the_surplus_unbound() {
    let e = Expr::template("a = ?", [RawArg::value(1i32), RawArg::value(2i32)]);
    let (sql, args, err) = parts(&PgLike, &e);
    assert_eq!(sql, "a = $1");
    assert_eq!(args, vec![Value::I32(1)]);
    assert_eq!(
        err.expect("a mismatch is recorded").to_string(),
        "Bad Statement: has 1 placeholders but 2 args: a = ?"
    );
}

/// No holes at all but arguments supplied — the shape of "someone deleted a `?`".
#[test]
fn arguments_with_no_holes_at_all_are_a_mismatch() {
    let e = Expr::template("SELECT 1", [RawArg::value(1i32)]);
    let err = build(&PgLike, &e).unwrap_err();
    assert!(matches!(
        err,
        Error::RawArgCount {
            placeholders: 0,
            args: 1,
            ..
        }
    ));
    assert_eq!(
        err.to_string(),
        "Bad Statement: has 0 placeholders but 1 args: SELECT 1"
    );
}

/// An escaped `?` is not a hole, so it does not count towards the total — and the
/// message quotes the template as written, escape included.
#[test]
fn an_escaped_question_mark_does_not_count_towards_the_total() {
    let ok = Expr::template(r"a \? b", []);
    assert_eq!(pg_sql(&ok), "a ? b");

    let not_ok = Expr::template(r"a \? b", [RawArg::value(1i32)]);
    let err = build(&PgLike, &not_ok).unwrap_err();
    assert_eq!(
        err.to_string(),
        r"Bad Statement: has 0 placeholders but 1 args: a \? b"
    );
}

/// A mismatch deep inside a larger expression fails the whole build, and the
/// error names the offending fragment rather than the statement.
#[test]
fn a_mismatch_inside_a_larger_expression_names_the_fragment() {
    let e = Expr::join((arg(1i32), Expr::template("f(?)", [])));
    let err = build(&PgLike, &e).unwrap_err();
    assert_eq!(
        err.to_string(),
        "Bad Statement: has 1 placeholders but 0 args: f(?)"
    );
}

/// Two broken fragments still produce one error, and it is the first — the one
/// with the most context, since a later failure is usually a consequence.
#[test]
fn two_mismatches_surface_as_the_first_one_only() {
    let e = Expr::join((Expr::template("f(?)", []), Expr::template("g(?, ?)", [])));
    let err = build(&PgLike, &e).unwrap_err();
    assert_eq!(
        err.to_string(),
        "Bad Statement: has 1 placeholders but 0 args: f(?)"
    );
}

// ---------------------------------------------------------------------------
// Named arguments, on a dialect that has them and on one that does not
// ---------------------------------------------------------------------------

/// SQLite-shaped: `:name`. A named argument binds nothing and consumes no
/// position, so the positional placeholders either side are 1 and 2, not 1 and 3.
#[test]
fn a_named_argument_binds_nothing_and_skips_no_position() {
    let e = Expr::join((arg(1i32), Expr::named_arg("cutoff"), arg(2i32)));
    let (sql, args) = build(&TestDialect, &e).expect("render");
    assert_eq!(sql, "?1 :cutoff ?2");
    assert_eq!(args, vec![Value::I32(1), Value::I32(2)]);
}

/// The same through a template's holes, with an offset start: the hole after the
/// named one takes the *next* position, not the one after next.
#[test]
fn a_named_hole_consumes_a_hole_but_no_position_even_from_an_offset_start() {
    let e = Expr::template(
        "a = ? AND b = ? AND c = ?",
        [RawArg::value(1i32), RawArg::named("b"), RawArg::value(3i32)],
    );
    let (sql, args) = build_from(&TestDialect, 7, &e).expect("render");
    assert_eq!(sql, "a = ?7 AND b = :b AND c = ?8");
    assert_eq!(args, vec![Value::I32(1), Value::I32(3)]);
}

/// A dialect with no named-argument syntax records `NoNamedArgs`. Two of them
/// still give one error — the writer holds a single slot and the first wins — and
/// the failure surfaces from `build` and nowhere else.
#[test]
fn a_dialect_without_named_arguments_reports_it_once_from_build() {
    let e = Expr::join((Expr::named_arg("a"), Expr::named_arg("b")));

    // Rendering is infallible, so the partial SQL is still there: neither named
    // argument wrote anything, and the separator remains.
    let (sql, args, err) = parts(&PgLike, &e);
    assert_eq!(sql, " ");
    assert!(args.is_empty());
    assert!(matches!(err, Some(Error::NoNamedArgs)));

    // `build` is the one place it becomes a `Result`, and a `Result` carries
    // exactly one error however many were recorded.
    assert!(matches!(build(&PgLike, &e), Err(Error::NoNamedArgs)));
    assert!(matches!(build(&Positional, &e), Err(Error::NoNamedArgs)));
}

/// A named hole on a dialect without named arguments also leaves a count
/// mismatch behind it (the hole consumed an argument that bound nothing). The
/// `NoNamedArgs` failure is recorded during the scan and therefore wins, so the
/// author is told the real cause rather than the symptom.
#[test]
fn the_named_arg_failure_wins_over_the_mismatch_it_causes() {
    let e = Expr::template("a = ? AND b = ?", [RawArg::named("a")]);
    let (sql, args, err) = parts(&PgLike, &e);
    // Nothing was written for the named hole; the second hole is short of an
    // argument and binds NULL at position 1.
    assert_eq!(sql, "a =  AND b = $1");
    assert_eq!(args, vec![Value::Null]);
    assert!(matches!(err, Some(Error::NoNamedArgs)));
    assert!(matches!(build(&PgLike, &e), Err(Error::NoNamedArgs)));
}

/// A named argument buried in a built tree fails the same way — there is no depth
/// at which the dialect's answer changes.
#[test]
fn a_named_argument_nested_deep_still_fails_the_build() {
    let e = quote("a").eq(Expr::group(Expr::join((
        arg(1i32),
        Expr::named_arg("deep"),
    ))));
    assert!(matches!(build(&PgLike, &e), Err(Error::NoNamedArgs)));
    // And renders on a dialect that has the syntax.
    let (sql, args) = build(&TestDialect, &e).expect("render");
    assert_eq!(sql, r#"("a" = (?1 :deep))"#);
    assert_eq!(args, vec![Value::I32(1)]);
}

// ---------------------------------------------------------------------------
// `Expr::Custom` round-tripping
// ---------------------------------------------------------------------------

/// An `Expr` wrapped as `Custom` renders byte-for-byte as itself and binds the
/// same arguments: the escape hatch is transparent to the writer.
#[test]
fn an_expr_round_trips_through_custom_unchanged() {
    let inner = quote("a").eq(arg(1i32));
    assert_eq!(pg(&inner.clone()), pg(&Expr::custom(inner)));
}

/// What the round trip *does* cost: `Custom` is opaque, so the parenthesisation
/// rule can no longer see that the inside is atomic and wraps it. This is
/// documented ("core cannot see inside it") and is the reason a dialect wraps
/// only finished, self-delimiting shapes in `Custom`.
#[test]
fn wrapping_in_custom_makes_an_atomic_expression_look_unatomic() {
    let bare = quote("a");
    assert!(bare.is_atomic());
    assert_eq!(pg_sql(&bare.clone().grouped()), r#""a""#);

    let wrapped = Expr::custom(bare);
    assert!(!wrapped.is_atomic());
    assert_eq!(pg_sql(&wrapped.grouped()), r#"("a")"#);
}

/// A `DynExpr` converts to `Expr::Custom` through `IntoExpr`, which is how a
/// clause that stored an erased expression hands it back to the enum.
#[test]
fn a_dyn_expr_converts_into_custom_and_renders() {
    let erased: DynExpr = dyn_expr(Contains(text("rust")));
    let e = erased.into_expr();
    assert!(matches!(e, Expr::Custom(_)));
    let args = assert_frag(COND, &e, "ARRAY['rust'] @> ARRAY[$1]");
    assert_eq!(args, vec![text("rust")]);
}

/// A template survives erasure: the `?` rewriting happens at render time, so it
/// still sees the outer writer's dialect and counter.
#[test]
fn a_template_inside_custom_is_still_rewritten_by_the_outer_dialect() {
    let tpl = Expr::template("f(?)", [RawArg::value(1i32)]);
    let e = Expr::join((arg(0i32), Expr::custom(tpl)));
    assert_eq!(pg_sql(&e), "$1 f($2)");
    assert_eq!(build(&Positional, &e).expect("render").0, "? f(?)");
}

/// A failure recorded inside a `Custom` propagates out of the whole build — the
/// error slot belongs to the writer, not to the expression.
#[test]
fn a_failure_inside_custom_propagates() {
    let e = Expr::custom(Expr::named_arg("x"));
    assert!(matches!(build(&PgLike, &e), Err(Error::NoNamedArgs)));

    let mismatch = Expr::custom(Expr::template("f(?)", []));
    assert!(matches!(
        build(&PgLike, &mismatch),
        Err(Error::RawArgCount { .. })
    ));
}

// ---------------------------------------------------------------------------
// A sub-query built for one dialect, embedded in another
// ---------------------------------------------------------------------------

/// A query in a foreign dialect, the way a dialect crate's `query()` mod writes
/// one: `write_with_dialect` swaps the syntax and keeps the buffer, the argument
/// list, the counter and the error slot.
#[derive(Debug)]
struct Foreign<Q>(Q);

impl<Q: Query> Expression for Foreign<Q> {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.write_with_dialect(self.0.dialect(), &self.0);
    }
}

/// `SELECT "id" FROM posts WHERE "views" > ?` — a MySQL-shaped sub-query.
#[derive(Debug)]
struct MysqlSubquery(i32);

impl Expression for MysqlSubquery {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.push_str("SELECT ");
        w.push_quoted(&["id"]);
        w.push_str(" FROM posts WHERE ");
        w.push_quoted(&["views"]);
        w.push_str(" > ");
        w.push_arg(self.0);
    }
}

impl Query for MysqlSubquery {
    fn query_type(&self) -> QueryType {
        QueryType::Select
    }

    fn dialect(&self) -> &dyn Dialect {
        &Positional
    }
}

/// The corruption this guards against is the worst kind: the embedded query
/// writes `?` while the enclosing one writes `$N`, so if the shared counter did
/// not advance across the boundary the arguments after it would bind one position
/// too low and the SQL would still look perfectly reasonable.
#[test]
fn a_foreign_dialect_subquery_shares_the_counter_and_the_arg_list() {
    let e = Expr::join((
        Expr::raw("SELECT * FROM comments WHERE"),
        quote("post_id").eq(arg(7i32)),
        Expr::raw("AND"),
        quote("id").in_(Expr::custom(Foreign(MysqlSubquery(100)))),
        Expr::raw("AND"),
        quote("body").like(arg("%rust%")),
    ));
    let (sql, args) = pg(&e);
    // `Chain::in_` supplies the sub-select's parentheses itself.
    assert_eq!(
        sql,
        concat!(
            r#"SELECT * FROM comments WHERE ("post_id" = $1) AND "#,
            r#"("id" IN (SELECT `id` FROM posts WHERE `views` > ?)) AND "#,
            r#"("body" LIKE $3)"#
        )
    );
    assert_eq!(
        args,
        vec![Value::I32(7), Value::I32(100), text("%rust%")],
        "the sub-query's argument still occupies position 2"
    );
}

/// Embedding does not change the sub-query: built on its own it numbers from 1,
/// which is what makes it reusable.
#[test]
fn embedding_leaves_the_subquery_itself_untouched() {
    let q = MysqlSubquery(100);
    let (sql, args) = q.build().expect("render");
    assert_eq!(sql, "SELECT `id` FROM posts WHERE `views` > ?");
    assert_eq!(args, vec![Value::I32(100)]);
    assert_eq!(q.query_type(), QueryType::Select);

    let embedded = Expr::join((arg(1i32), Expr::custom(Foreign(MysqlSubquery(100)))));
    assert_eq!(
        pg(&embedded).1,
        vec![Value::I32(1), Value::I32(100)],
        "the same query, embedded, contributes the same value at a later position"
    );
}

/// The nested writer carries the *inner* dialect, so a named argument is legal
/// inside a SQLite-shaped sub-query even though the enclosing PostgreSQL-shaped
/// statement could not write one. This is the whole point of a query rendering
/// itself in its own dialect.
#[test]
fn a_named_argument_is_judged_by_the_dialect_it_is_written_in() {
    #[derive(Debug)]
    struct SqliteSubquery;

    impl Expression for SqliteSubquery {
        fn write_sql(&self, w: &mut SqlWriter<'_>) {
            w.push_str("SELECT ");
            w.push_quoted(&["id"]);
            w.push_str(" FROM posts WHERE ");
            w.push_quoted(&["title"]);
            w.push_str(" = ");
            w.push_named_arg("title");
        }
    }

    impl Query for SqliteSubquery {
        fn query_type(&self) -> QueryType {
            QueryType::Select
        }

        fn dialect(&self) -> &dyn Dialect {
            &TestDialect
        }
    }

    let e = Expr::join((
        Expr::raw("SELECT * FROM comments WHERE"),
        quote("post_id").in_(Expr::custom(Foreign(SqliteSubquery))),
        Expr::raw("AND"),
        quote("id").eq(arg(1i32)),
    ));
    let (sql, args) = pg(&e);
    assert_eq!(
        sql,
        concat!(
            r#"SELECT * FROM comments WHERE "#,
            r#"("post_id" IN (SELECT "id" FROM posts WHERE "title" = :title)) AND "#,
            r#"("id" = $1)"#
        )
    );
    assert_eq!(
        args,
        vec![Value::I32(1)],
        "the named argument bound nothing, so the outer arg is still position 1"
    );
}

/// And the reverse: a failure recorded under the inner dialect comes back out
/// with the outer build, because the error slot is shared too.
#[test]
fn a_failure_under_the_inner_dialect_surfaces_from_the_outer_build() {
    #[derive(Debug)]
    struct StrictSubquery;

    impl Expression for StrictSubquery {
        fn write_sql(&self, w: &mut SqlWriter<'_>) {
            w.push_str("SELECT ");
            w.push_named_arg("nope");
        }
    }

    impl Query for StrictSubquery {
        fn query_type(&self) -> QueryType {
            QueryType::Select
        }

        // `$N`, and no named arguments.
        fn dialect(&self) -> &dyn Dialect {
            &PgLike
        }
    }

    let e = Expr::join((arg(1i32), Expr::custom(Foreign(StrictSubquery))));
    // The outer dialect *does* have named arguments; the inner one does not, and
    // the inner one is what decides.
    assert!(matches!(build(&TestDialect, &e), Err(Error::NoNamedArgs)));
}
