//! The SQL clauses the three dialects share, as data.
//!
//! One struct per clause — [`With`], [`Where`], [`Frame`], … — each an
//! [`Expression`] that renders **only itself**. A dialect's
//! query type composes them as named fields and decides the order and the
//! separation between them; nothing here knows what statement it is part of.
//!
//! # Four conventions the dialect crates depend on
//!
//! 1. **[`Default`] means "clause absent", and an absent clause renders nothing
//!    at all** — not a bare keyword, not a stray space. That is what lets a query
//!    write `w.write_if(!q.where_.is_empty(), " ", &q.where_, "")` and never think
//!    about it again. Every clause also has an `is_empty`, for the cases where the
//!    query needs a separator in front.
//! 2. **A clause writes its own keyword**, so `Where` renders `WHERE a AND b` and
//!    `Limit` renders `LIMIT 10`. The single exception is [`Set`], and the reason
//!    is grammatical: MySQL's `ON DUPLICATE KEY UPDATE` takes the same assignment
//!    list with no `SET` in front of it, so the keyword has to belong to whatever
//!    contains the list. Clauses that are lists of independent items ([`Locks`],
//!    [`Combines`]) write no keyword either, because each item carries its own.
//! 3. **Anything the caller supplies is an [`Expr`](crate::expr::Expr)**, never a
//!    `String` or a number: a `LIMIT` may be a bound argument, a `VALUES` cell may
//!    be a sub-select, a frame bound may be `$1 PRECEDING`. Fields typed
//!    `Cow<'static, str>` are *identifiers* and are quoted with the dialect's own
//!    quoting; fields typed as a keyword enum are fixed SQL.
//! 4. **A nested context implements the same `Has*` trait.** `ON CONFLICT … DO
//!    UPDATE` has a `WHERE` of its own, and so does the index-inference target in
//!    front of it, so [`HasWhere`] is implemented by [`ConflictClause`] and
//!    [`ConflictTarget`] as well as by whatever statements a dialect gives one to.
//!    A mod written once therefore works in all of them.
//!
//! # Keywords are enums, not strings
//!
//! bob stores `Type string` / `Direction string` and exports `const` vocabularies.
//! Here a closed keyword set is an enum ([`FrameMode`], [`LockStrength`],
//! [`SetOp`], …), because the set really is closed by the grammar and a typo in
//! one is otherwise a runtime syntax error. The two genuinely open-ended ones keep
//! an escape hatch shaped like the grammar that opened them:
//! [`JoinKind::Custom`] for MySQL's `STRAIGHT_JOIN`, and
//! [`OrderDirection::Using`] for PostgreSQL's `ORDER BY … USING <operator>`.
//!
//! # Sub-queries
//!
//! Where a clause holds one — [`Cte::query`], [`Combine::query`],
//! [`Values::query`] — it holds it as an [`Expr`](crate::expr::Expr), which a
//! dialect's query type reaches through
//! [`Expr::Custom`](crate::expr::Expr::Custom). bob has a `Query` interface in
//! those slots whose only extra promise is "renders with its own dialect rather
//! than the one handed to it", and a query type keeps that promise for itself by
//! calling
//! [`SqlWriter::write_with_dialect`](crate::SqlWriter::write_with_dialect) inside
//! its own `write_sql`.
//!
//! # Rendering choices made here
//!
//! Formatting is not part of the contract — the golden comparison collapses runs
//! of whitespace — but two choices are visible through it and are deliberate:
//!
//! - **Identifiers are quoted.** A CTE name, a `USING` column, a window name, a
//!   locked table: bob writes all of these verbatim, which breaks the moment one
//!   of them is a reserved word or mixed-case. They go through
//!   [`SqlWriter::push_quoted`](crate::SqlWriter::push_quoted) here.
//! - **No trailing or doubled separators.** bob emits `FOR KEY SHARE ` and
//!   `USING("id") ` with trailing spaces and `PARTITION BY a  ORDER BY b` with
//!   two, because each fragment guesses at its own padding. Every clause here
//!   writes separators only between things that are actually present.

mod combine;
mod conflict;
mod cte;
mod fetch;
mod frame;
mod from;
mod group_by;
mod having;
mod join;
mod limit;
mod lock;
mod offset;
mod order_by;
mod returning;
mod select;
mod set;
mod values;
mod where_;
mod window;
mod with;

pub use combine::{Combine, Combines, HasCombines, SetOp};
pub use conflict::{
    Conflict, ConflictAction, ConflictClause, ConflictTarget, HasConflict, HasConflictClause,
};
pub use cte::{Cte, CteCycle, CteSearch, SearchOrder};
pub use fetch::{Fetch, FirstOrNext, HasFetch};
pub use frame::{Frame, FrameExclusion, FrameMode, HasFrame};
pub use from::{
    HasTableRef, IndexHint, IndexHintKind, IndexHintScope, IndexedBy, TableFunctions, TableRef,
};
pub use group_by::{GroupBy, GroupByWith, GroupingSet, GroupingSetKind, HasGroupBy};
pub use having::{HasHaving, Having};
pub use join::{HasJoins, Join, JoinKind};
pub use limit::{HasLimit, Limit};
pub use lock::{HasLocks, Lock, LockStrength, LockWait, Locks};
pub use offset::{HasOffset, Offset, RowsKeyword};
pub use order_by::{HasOrderBy, NullsPosition, OrderBy, OrderDef, OrderDirection};
pub use returning::{HasReturning, Returning};
pub use select::{HasSelectList, SelectList};
pub use set::{HasSet, Set};
pub use values::{HasValues, Values, ValuesRow};
pub use where_::{HasWhere, Where};
pub use window::{HasWindow, HasWindows, NamedWindow, Window, Windows};
pub use with::{HasWith, With};

use std::borrow::Cow;

use crate::writer::{Expression, SqlWriter};

/// An item of a clause list that may itself be absent.
///
/// The lists in this module hold structs rather than expressions, and an absent
/// struct renders nothing — so [`SqlWriter::write_slice`] would put a separator
/// between two things one of which is not there. For a list of independent items
/// that is a stray space; for `WITH a AS (…), <absent>` it is a syntax error.
trait MaybeAbsent {
    /// Whether this item renders nothing.
    fn is_absent(&self) -> bool;
}

/// [`SqlWriter::write_slice`] over items that may be absent: separators go only
/// between items that are actually written, and if every item is absent then
/// nothing at all is — affixes included.
fn write_present<E: Expression + MaybeAbsent>(
    w: &mut SqlWriter<'_>,
    items: &[E],
    prefix: &str,
    sep: &str,
    suffix: &str,
) {
    let mut written = false;
    for item in items.iter().filter(|i| !i.is_absent()) {
        w.push_str(if written { sep } else { prefix });
        w.write_expr(item);
        written = true;
    }
    if written {
        w.push_str(suffix);
    }
}

/// Write a list of identifiers, each quoted, wrapped in `prefix`/`suffix`.
///
/// Nothing at all is written when the list is empty — the same omission rule as
/// [`SqlWriter::write_slice`], which cannot be used here because a
/// `Cow<'static, str>` renders as raw SQL rather than as a quoted name.
fn write_quoted_list(
    w: &mut SqlWriter<'_>,
    names: &[Cow<'static, str>],
    prefix: &str,
    sep: &str,
    suffix: &str,
) {
    if names.is_empty() {
        return;
    }
    w.push_str(prefix);
    for (i, name) in names.iter().enumerate() {
        if i > 0 {
            w.push_str(sep);
        }
        w.push_quoted(&[name]);
    }
    w.push_str(suffix);
}

/// Implement the `Has*` clause-accessor traits for a statement, one line each.
///
/// Every one of them is the same five lines — `impl HasX for Q { fn x_mut(&mut
/// self) -> &mut X { &mut self.field } }` — and the three dialect crates
/// between them had a hundred and fifty. They carry no reasoning: the whole
/// content of one is *which field of this statement is that clause*, which is
/// exactly what a line here says.
///
/// ```
/// use keelson_core::clause::{HasLimit, HasWhere, Limit, Where};
///
/// #[derive(Default)]
/// struct MyQuery {
///     where_: Where,
///     limit: Limit,
/// }
///
/// keelson_core::impl_clause_accessors!(MyQuery {
///     HasWhere => where_mut: Where = where_,
///     HasLimit => limit_mut: Limit = limit,
/// });
///
/// let mut q = MyQuery::default();
/// let _: &mut Where = q.where_mut();
/// ```
///
/// The trait and type names resolve in the calling module, so a dialect's own
/// accessor trait — `HasExtraTables`, `HasHints` — goes in the same list as
/// the ones from here, and nothing has to be imported that was not already.
/// A nested field path works too (`from.joins`), which is how a statement
/// whose joins hang off its `FROM` item reaches them.
///
/// The alternative was a derive macro reading `#[clause]` attributes on the
/// fields. It was not taken: it would put keelson-macros between every
/// dialect crate and its statements, to say the same thing in a place where
/// the field's *type* no longer names the trait it satisfies. Here the pair
/// is written out, and a wrong one does not compile.
#[macro_export]
macro_rules! impl_clause_accessors {
    ($q:ty { $($trait_:ident => $method:ident: $ty:ty = $($field:ident).+),+ $(,)? }) => {
        $(
            impl $trait_ for $q {
                fn $method(&mut self) -> &mut $ty {
                    &mut self.$($field).+
                }
            }
        )+
    };
}

#[cfg(test)]
mod tests {
    use keelson_sqlcheck::testing::{assert_frag_sql, assert_stmt_sql};

    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::expr::{Chain, Expr, arg, quote};
    use crate::value::Value;
    use crate::writer::{Expression, SqlWriter, build};

    /// Enough of a PostgreSQL `SELECT` to prove the clauses compose, with the
    /// write order taken from
    /// <https://www.postgresql.org/docs/17/sql-select.html>: `WITH`, select list,
    /// `FROM`, `WHERE`, `GROUP BY`, `HAVING`, `WINDOW`, then the set operations
    /// and their trailing `ORDER BY`/`LIMIT`.
    ///
    /// The real query type lives in `keelson-psql`; this one exists so the clause
    /// shapes can be checked as a group from inside this crate. Because it renders
    /// a whole statement, the cases below go to the judge rather than to
    /// `assert_eq!` alone.
    #[derive(Debug, Default)]
    struct Select {
        with: With,
        select: SelectList,
        from: TableRef,
        where_: Where,
        group_by: GroupBy,
        having: Having,
        windows: Windows,
        order_by: OrderBy,
        limit: Limit,
        locks: Locks,
        combines: Combines,
    }

    impl Expression for Select {
        fn write_sql(&self, w: &mut SqlWriter<'_>) {
            w.write_if(!self.with.is_empty(), "", &self.with, " ");

            // The leading query is parenthesised exactly when it carries a tail
            // clause of its own *and* something is combined onto it, so that
            // `(SELECT … LIMIT 1) UNION (…)` cannot be read as the LIMIT applying
            // to the union. See `Combines::parenthesises_leading_query`.
            let tail =
                !self.order_by.is_empty() || !self.limit.is_empty() || !self.locks.is_empty();
            let parens = self.combines.parenthesises_leading_query(tail);
            if parens {
                w.push_str("(");
            }

            w.push_str("SELECT ");
            w.write_expr(&self.select);
            w.write_if(!self.from.is_empty(), " FROM ", &self.from, "");
            w.write_if(!self.where_.is_empty(), " ", &self.where_, "");
            w.write_if(!self.group_by.is_empty(), " ", &self.group_by, "");
            w.write_if(!self.having.is_empty(), " ", &self.having, "");
            w.write_if(!self.windows.is_empty(), " ", &self.windows, "");
            w.write_if(!self.order_by.is_empty(), " ", &self.order_by, "");
            w.write_if(!self.limit.is_empty(), " ", &self.limit, "");
            w.write_if(!self.locks.is_empty(), " ", &self.locks, "");

            if parens {
                w.push_str(")");
            }
            w.write_if(!self.combines.is_empty(), " ", &self.combines, "");
        }
    }

    fn from(table: &'static str) -> TableRef {
        TableRef::new(quote(table))
    }

    #[test]
    fn an_all_default_select_is_the_shortest_legal_statement() {
        // Every clause absent contributes nothing, so only the projection's `*`
        // survives. This is the property the whole module is built around.
        //
        // Not judged: `SELECT *` is as short as the clauses can make a statement,
        // and PostgreSQL rejects it — `*` needs something to expand against
        // ("SELECT * with no tables specified is not valid"). So what an
        // all-default `Select` renders is the assertion, and the case below is
        // where the same composition is judged as SQL.
        let (sql, args) = build(&Numbered, &Select::default()).unwrap();
        assert_eq!(sql, "SELECT *");
        assert!(args.is_empty());
    }

    #[test]
    fn a_select_over_a_cte_joined_and_filtered() {
        // Expectation from the PostgreSQL 17 grammar:
        //   WITH with_query [, ...] SELECT select_list FROM from_item
        //   with_query: name [(col, ...)] AS (select)
        //   from_item:  table_name [[AS] alias] join_type from_item USING (col)
        let inner = Select {
            select: SelectList {
                columns: vec![quote("id")],
                ..SelectList::default()
            },
            from: from("posts"),
            ..Select::default()
        };

        let mut with = With::default();
        with.append_cte(Cte {
            columns: vec!["id".into()],
            ..Cte::new("recent", Expr::custom(inner))
        });

        let mut from_users = from("users");
        from_users.set_alias("u");
        from_users.append_join(Join {
            kind: JoinKind::Left,
            to: from("recent"),
            using: vec!["id".into()],
            ..Join::default()
        });

        let q = Select {
            with,
            select: SelectList {
                columns: vec![quote(("u", "id"))],
                ..SelectList::default()
            },
            from: from_users,
            where_: Where {
                conditions: vec![quote(("u", "id")).eq(arg(7i32))],
            },
            ..Select::default()
        };

        let (sql, args) = build(&Numbered, &q).unwrap();
        assert_stmt_sql(
            &sql,
            concat!(
                r#"WITH "recent" ("id") AS (SELECT "id" FROM "posts") "#,
                r#"SELECT "u"."id" FROM "users" AS "u" LEFT JOIN "recent" USING ("id") "#,
                r#"WHERE ("u"."id" = $1)"#
            ),
        );
        assert_eq!(args, vec![Value::I32(7)]);
    }

    #[test]
    fn a_combined_select_keeps_its_own_limit_inside_the_parentheses() {
        // PostgreSQL 17, sql-select: "If ORDER BY / LIMIT is to apply to only one
        // of the operands, that operand must be parenthesised", and the trailing
        // ORDER BY / LIMIT belong to the whole set operation.
        let mut combines = Combines::default();
        combines.append_combine(Combine {
            op: Some(SetOp::Union),
            query: Some(Expr::raw("SELECT 2")),
            all: true,
        });
        combines.order_by.append_order(Expr::raw("1"));
        combines.limit.set_limit(5);

        let mut limit = Limit::default();
        limit.set_limit(1);

        let q = Select {
            select: SelectList {
                columns: vec![quote("id")],
                ..SelectList::default()
            },
            from: from("users"),
            limit,
            combines,
            ..Select::default()
        };

        assert_stmt_sql(
            &build(&Numbered, &q).unwrap().0,
            r#"(SELECT "id" FROM "users" LIMIT 1) UNION ALL (SELECT 2) ORDER BY 1 LIMIT 5"#,
        );
    }

    #[test]
    fn the_same_where_mod_reaches_a_statement_and_a_conflict_clause() {
        // The point of the Has* traits: one function, three receivers.
        // Qualified, because in an `ON CONFLICT DO UPDATE ... WHERE` an
        // unqualified column is ambiguous between the target row and EXCLUDED —
        // and a mod written once has to be legal in every receiver.
        fn recent<Q: HasWhere>(q: &mut Q) {
            q.where_mut().append_where(Expr::raw(r#""users"."id" > 1"#));
        }

        let mut select = Select {
            from: from("users"),
            ..Select::default()
        };
        recent(&mut select.where_);
        assert_stmt_sql(
            &build(&Numbered, &select).unwrap().0,
            r#"SELECT * FROM "users" WHERE "users"."id" > 1"#,
        );

        // The target needs its column list for the predicate to attach to; that is
        // the grammar, not this test's convenience.
        let mut conflict = ConflictClause {
            target: ConflictTarget::on_columns(quote("id")),
            ..ConflictClause::do_update()
        };
        conflict
            .set
            .append_set(Expr::raw(r#""name" = EXCLUDED."name""#));
        recent(&mut conflict);
        recent(&mut conflict.target);
        // The action's WHERE is framed; the *target's* is an index predicate, and
        // the shared schema has no partial unique index for it to match — see
        // `conflict::tests::a_partial_index_target_carries_the_indexs_own_predicate`.
        // So this one pins the rendering and the framed case below pins the SQL.
        assert_eq!(
            build(&Numbered, &conflict).unwrap().0,
            concat!(
                r#"ON CONFLICT ("id") WHERE "users"."id" > 1 "#,
                r#"DO UPDATE SET "name" = EXCLUDED."name" WHERE "users"."id" > 1"#
            )
        );

        let mut action_only = ConflictClause {
            target: ConflictTarget::on_columns(quote("id")),
            ..ConflictClause::do_update()
        };
        action_only
            .set
            .append_set(Expr::raw(r#""name" = EXCLUDED."name""#));
        recent(&mut action_only);
        assert_frag_sql(
            r#"INSERT INTO users ("id", "name") VALUES (1, 'kubo') {}"#,
            &build(&Numbered, &action_only).unwrap().0,
            r#"ON CONFLICT ("id") DO UPDATE SET "name" = EXCLUDED."name" WHERE "users"."id" > 1"#,
        );
    }

    /// `write_quoted_list` is the shared helper every identifier list goes
    /// through, so its empty case is load-bearing in six clauses.
    ///
    /// An identifier list is not a statement and belongs to no single one of them —
    /// six clauses put it in six places — so this is one of the cases that stays a
    /// string comparison.
    #[test]
    fn a_quoted_list_omits_its_affixes_when_empty() {
        let mut w = SqlWriter::new(&Numbered);
        write_quoted_list(&mut w, &[], " (", ", ", ")");
        assert_eq!(w.sql(), "");
        write_quoted_list(&mut w, &["a".into(), "b".into()], " (", ", ", ")");
        assert_eq!(w.sql(), r#" ("a", "b")"#);
    }
}
