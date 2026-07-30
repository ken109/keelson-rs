//! Reusable SQL clauses.
//!
//! One type per clause — [`With`], [`Where`], [`Limit`], [`OrderBy`], … — each an
//! [`Expression`](crate::Expression) that renders only itself. Query structs in
//! the dialect crates compose them as named fields, and `#[derive(Clauses)]`
//! generates the `Has*` accessor traits that clause mods are generic over.
//!
//! Three conventions run through the whole module, and the dialect crates depend
//! on all three:
//!
//! - **A clause writes its own keyword but not its own separation from the
//!   previous clause.** `Where` renders `WHERE a AND b`; the newline in front of
//!   it comes from the query. Clauses that are lists of independent items
//!   ([`Locks`], [`Combines`]) write neither, since each item carries its keyword.
//! - **An empty clause renders as the empty string**, so a query can write it
//!   unconditionally when it has no prefix to suppress. Where a prefix is
//!   involved the query guards it with `is_empty()`:
//!   `w.write_if(!q.where_.is_empty(), "\n", &q.where_, "")`.
//! - **Anything the caller supplies is an erased
//!   [`DynExpr`](crate::DynExpr)**, never a `String` or a number — a limit may be
//!   a bound argument, a `VALUES` cell may be a sub-select. Fields typed
//!   `String`/`Vec<String>` are identifiers or fixed SQL keywords, and are the
//!   only things written verbatim.
//!
//! Keyword-valued fields such as [`Join::kind`] or [`OrderDef::direction`] are
//! `String` rather than enums. PostgreSQL's `ORDER BY … USING <operator>` and
//! MySQL's `STRAIGHT_JOIN` make those sets open-ended, and generated code
//! composes them; the `const &str` vocabularies next to each clause are what the
//! dialect crates use.
//!
//! Where a clause holds a sub-query — [`Cte::query`], [`Combine::query`],
//! [`Values::query`] — it holds it as a `DynExpr`. bob has a `Query` interface
//! there whose only extra promise is "renders with its own dialect, not the one
//! handed to it", and a query type keeps that promise by calling
//! [`SqlWriter::write_with_dialect`](crate::SqlWriter::write_with_dialect) inside
//! its own `write_sql`.

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

pub use combine::{Combine, Combines, EXCEPT, HasCombines, INTERSECT, UNION};
pub use conflict::{
    CONFLICT_DO_NOTHING, CONFLICT_DO_UPDATE, Conflict, ConflictClause, ConflictTarget, HasConflict,
};
pub use cte::{Cte, CteCycle, CteSearch, SEARCH_BREADTH, SEARCH_DEPTH};
pub use fetch::{Fetch, HasFetch};
pub use frame::{FRAME_MODE_GROUPS, FRAME_MODE_RANGE, FRAME_MODE_ROWS, Frame, HasFrame};
pub use from::{HasTableRef, IndexHint, TableRef};
pub use group_by::{GroupBy, GroupingSet, HasGroupBy};
pub use having::{HasHaving, Having};
pub use join::{
    CROSS_JOIN, FULL_JOIN, HasJoins, INNER_JOIN, Join, LEFT_JOIN, RIGHT_JOIN, STRAIGHT_JOIN,
};
pub use limit::{HasLimit, Limit};
pub use lock::{
    HasLocks, LOCK_STRENGTH_KEY_SHARE, LOCK_STRENGTH_NO_KEY_UPDATE, LOCK_STRENGTH_SHARE,
    LOCK_STRENGTH_UPDATE, LOCK_WAIT_NO_WAIT, LOCK_WAIT_SKIP_LOCKED, Lock, Locks,
};
pub use offset::{HasOffset, Offset};
pub use order_by::{HasOrderBy, OrderBy, OrderDef};
pub use returning::{HasReturning, Returning};
pub use select::{HasSelectList, SelectList};
pub use set::{HasSet, Set};
pub use values::{HasValues, Values, ValuesRow};
pub use where_::{HasWhere, Where};
pub use window::{HasWindows, NamedWindow, Window, Windows};
pub use with::{HasWith, With};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::error::Result;
    use crate::writer::{DynExpr, Expression, SqlWriter, build, dyn_expr, expr_fn};

    /// Enough of a `SELECT` to check that the clauses compose.
    ///
    /// The write order and the separators are PostgreSQL's, transcribed from
    /// bob's `dialect/psql`. The real query type lives in `keelson-psql`; this
    /// exists so that the clause shapes can be checked against SQL bob actually
    /// emitted before that crate exists.
    #[derive(Debug, Default)]
    struct Select {
        with: With,
        select: SelectList,
        from: TableRef,
        where_: Where,
    }

    impl Expression for Select {
        fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
            w.write_if(!self.with.is_empty(), "\n", &self.with, "")?;
            w.push_str("SELECT ");
            w.write_if(true, "\n", &self.select, "")?;
            w.write_if(!self.from.is_empty(), "\nFROM ", &self.from, "")?;
            w.write_if(!self.where_.is_empty(), "\n", &self.where_, "")?;
            w.push_str("\n");
            Ok(())
        }
    }

    fn eq(col: &'static str, v: i32) -> DynExpr {
        dyn_expr(expr_fn(move |w: &mut SqlWriter<'_>| {
            w.push_str("(");
            w.push_quoted(&[col]);
            w.push_str(" = ");
            w.push_arg(v);
            w.push_str(")");
            Ok(())
        }))
    }

    /// bob's `psql` "with cross join" fixture, byte for byte.
    #[test]
    fn a_select_joined_to_a_sub_select() {
        let inner = Select {
            select: SelectList {
                columns: vec![dyn_expr("id"), dyn_expr("type")],
                ..SelectList::default()
            },
            from: TableRef::new(dyn_expr("clients")),
            where_: Where {
                conditions: vec![eq("client_id", 100)],
            },
            ..Select::default()
        };
        let parenthesised = dyn_expr(expr_fn(move |w: &mut SqlWriter<'_>| {
            w.push_str("(");
            w.write_expr(&inner)?;
            w.push_str(")");
            Ok(())
        }));

        let mut from = TableRef::new(dyn_expr("users"));
        from.set_table_alias("u", []);
        from.append_join(Join {
            kind: CROSS_JOIN.into(),
            to: TableRef {
                alias: "clients".into(),
                ..TableRef::new(parenthesised)
            },
            ..Join::default()
        });

        let q = Select {
            select: SelectList {
                columns: vec![dyn_expr("id"), dyn_expr("name"), dyn_expr("type")],
                ..SelectList::default()
            },
            from,
            where_: Where {
                conditions: vec![eq("id", 100)],
            },
            ..Select::default()
        };

        let (sql, args) = build(&Numbered, &q).unwrap();
        assert_eq!(
            sql,
            "SELECT \nid, name, type\nFROM users AS \"u\"\nCROSS JOIN (SELECT \nid, type\nFROM clients\nWHERE (\"client_id\" = $1)\n) AS \"clients\"\nWHERE (\"id\" = $2)\n"
        );
        assert_eq!(
            args.len(),
            2,
            "the sub-select's argument is numbered before the outer one"
        );
    }

    /// bob's `psql` "CTE with column aliases" fixture, byte for byte. The `USING`
    /// clause's trailing space and the missing separator between `)` and `SELECT`
    /// are both bob's, and both matter.
    #[test]
    fn a_select_over_a_cte() {
        let mut cte_from = TableRef::new(dyn_expr("test1"));
        cte_from.append_join(Join {
            kind: LEFT_JOIN.into(),
            to: TableRef::new(dyn_expr("test2")),
            using: vec!["id".into()],
            ..Join::default()
        });

        let inner = Select {
            select: SelectList {
                columns: vec![dyn_expr("id")],
                ..SelectList::default()
            },
            from: cte_from,
            ..Select::default()
        };

        let mut with = With::default();
        with.append_cte(dyn_expr(Cte {
            query: Some(dyn_expr(inner)),
            columns: vec!["id".into(), "data".into()],
            ..Cte::new("c")
        }));

        let q = Select {
            with,
            from: TableRef::new(dyn_expr("c")),
            ..Select::default()
        };

        let (sql, _) = build(&Numbered, &q).unwrap();
        assert_eq!(
            sql,
            "\nWITH\nc(id, data) AS (SELECT \nid\nFROM test1\nLEFT JOIN test2 USING(\"id\") \n)SELECT \n*\nFROM c\n"
        );
    }
}
