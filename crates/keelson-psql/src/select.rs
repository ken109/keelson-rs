use std::sync::Arc;

use keelson_core::clause::{
    Combines, Fetch, GroupBy, HasCombines, HasFetch, HasGroupBy, HasHaving, HasJoins, HasLimit,
    HasLocks, HasOffset, HasOrderBy, HasSelectList, HasTableRef, HasWhere, HasWindows, HasWith,
    Having, Join, Limit, Locks, Offset, OrderBy, SelectList, TableRef, Where, Windows, With,
};
use keelson_core::{BuildMod, DynExpr, Expression, Result, SqlWriter};

/// The PostgreSQL `SELECT` statement, clause by clause.
///
/// The field order is the order the clauses are written in, which is the order
/// the [grammar](https://www.postgresql.org/docs/current/sql-select.html) lists
/// them. The four `combined_*` clauses apply to the *result* of a `UNION` /
/// `INTERSECT` / `EXCEPT` rather than to this query, which is why they are
/// separate from `order_by`, `limit`, `offset` and `fetch`.
#[derive(Debug, Clone, Default)]
pub struct SelectQuery {
    pub with: With,
    pub select: SelectList,
    pub distinct: Distinct,
    pub from: TableRef,
    pub where_: Where,
    pub group_by: GroupBy,
    pub having: Having,
    pub windows: Windows,
    pub combines: Combines,
    pub order_by: OrderBy,
    pub limit: Limit,
    pub offset: Offset,
    pub fetch: Fetch,
    pub locks: Locks,

    pub combined_order: OrderBy,
    pub combined_limit: Limit,
    pub combined_offset: Offset,
    pub combined_fetch: Fetch,

    /// Mods deferred to build time — bob's contextual mods.
    pub build_mods: Vec<Arc<dyn BuildMod<SelectQuery>>>,
}

impl SelectQuery {
    /// Register a mod that runs on every build rather than now.
    pub fn append_build_mod(&mut self, m: Arc<dyn BuildMod<SelectQuery>>) {
        self.build_mods.push(m);
    }

    /// Whether the query's own tail clauses have to be bracketed off from the
    /// set operations that follow them.
    ///
    /// `SELECT … LIMIT 10 UNION SELECT …` would attach the `LIMIT` to the union,
    /// so the operand gets parentheses of its own as soon as it has a tail.
    fn needs_parens(&self) -> bool {
        !self.combines.is_empty()
            && (!self.order_by.is_empty()
                || !self.limit.is_empty()
                || !self.offset.is_empty()
                || !self.fetch.is_empty()
                || !self.locks.is_empty())
    }
}

impl Expression for SelectQuery {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        // Build mods run against a clone so that rendering stays `&self`.
        if !self.build_mods.is_empty() {
            let mut applied = self.clone();
            applied.build_mods.clear();
            for m in &self.build_mods {
                m.apply(&mut applied)?;
            }
            return applied.write_sql(w);
        }

        w.write_if(!self.with.is_empty(), "\n", &self.with, "")?;

        let parens = self.needs_parens();
        if parens {
            w.push_str("(");
        }

        w.push_str("SELECT ");
        w.write_if(self.distinct.is_set(), "", &self.distinct, " ")?;
        w.write_if(true, "\n", &self.select, "")?;
        w.write_if(!self.from.is_empty(), "\nFROM ", &self.from, "")?;
        w.write_if(!self.where_.is_empty(), "\n", &self.where_, "")?;
        w.write_if(!self.group_by.is_empty(), "\n", &self.group_by, "")?;
        w.write_if(!self.having.is_empty(), "\n", &self.having, "")?;
        w.write_if(!self.windows.is_empty(), "\n", &self.windows, "")?;
        w.write_if(!self.order_by.is_empty(), "\n", &self.order_by, "")?;
        w.write_if(!self.limit.is_empty(), "\n", &self.limit, "")?;
        w.write_if(!self.offset.is_empty(), "\n", &self.offset, "")?;
        w.write_if(!self.fetch.is_empty(), "\n", &self.fetch, "")?;
        w.write_slice(&self.locks.locks, "\n", "\n", "")?;

        if parens {
            w.push_str(")");
        }

        w.write_slice(&self.combines.queries, "\n", "\n", "")?;
        w.write_if(
            !self.combined_order.is_empty(),
            "\n",
            &self.combined_order,
            "",
        )?;
        w.write_if(
            !self.combined_limit.is_empty(),
            "\n",
            &self.combined_limit,
            "",
        )?;
        w.write_if(
            !self.combined_offset.is_empty(),
            "\n",
            &self.combined_offset,
            "",
        )?;
        w.write_if(
            !self.combined_fetch.is_empty(),
            "\n",
            &self.combined_fetch,
            "",
        )?;

        w.push_str("\n");
        Ok(())
    }
}

/// `DISTINCT`, optionally `DISTINCT ON (…)`.
///
/// PostgreSQL-only, so it is not one of the shared clauses. `None` is no
/// `DISTINCT` at all; `Some([])` is a bare `DISTINCT`, which is what
/// `sm::distinct(())` sets.
#[derive(Debug, Clone, Default)]
pub struct Distinct {
    pub on: Option<Vec<DynExpr>>,
}

impl Distinct {
    /// Whether the query has a `DISTINCT` to write.
    pub fn is_set(&self) -> bool {
        self.on.is_some()
    }
}

impl Expression for Distinct {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        w.push_str("DISTINCT");
        if let Some(on) = &self.on {
            w.write_slice(on, " ON (", ", ", ")")?;
        }
        Ok(())
    }
}

/// A query with a `DISTINCT` clause.
pub trait HasDistinct {
    fn distinct_mut(&mut self) -> &mut Distinct;
}

/// A query whose `ORDER BY` applies to the result of a set operation.
pub trait HasCombinedOrder {
    fn combined_order_mut(&mut self) -> &mut OrderBy;
}

/// A query whose `LIMIT` applies to the result of a set operation.
pub trait HasCombinedLimit {
    fn combined_limit_mut(&mut self) -> &mut Limit;
}

/// A query whose `OFFSET` applies to the result of a set operation.
pub trait HasCombinedOffset {
    fn combined_offset_mut(&mut self) -> &mut Offset;
}

/// A query whose `FETCH` applies to the result of a set operation.
pub trait HasCombinedFetch {
    fn combined_fetch_mut(&mut self) -> &mut Fetch;
}

macro_rules! has_clause {
    ($($trait_:ident, $method:ident -> $ty:ty { $($field:tt)* })+) => {
        $(
            impl $trait_ for SelectQuery {
                fn $method(&mut self) -> &mut $ty {
                    &mut self.$($field)*
                }
            }
        )+
    };
}

has_clause! {
    HasWith, with_mut -> With { with }
    HasSelectList, select_list_mut -> SelectList { select }
    HasDistinct, distinct_mut -> Distinct { distinct }
    HasTableRef, table_ref_mut -> TableRef { from }
    HasJoins, joins_mut -> Vec<Join> { from.joins }
    HasWhere, where_mut -> Where { where_ }
    HasGroupBy, group_by_mut -> GroupBy { group_by }
    HasHaving, having_mut -> Having { having }
    HasWindows, windows_mut -> Windows { windows }
    HasCombines, combines_mut -> Combines { combines }
    HasOrderBy, order_by_mut -> OrderBy { order_by }
    HasLimit, limit_mut -> Limit { limit }
    HasOffset, offset_mut -> Offset { offset }
    HasFetch, fetch_mut -> Fetch { fetch }
    HasLocks, locks_mut -> Locks { locks }
    HasCombinedOrder, combined_order_mut -> OrderBy { combined_order }
    HasCombinedLimit, combined_limit_mut -> Limit { combined_limit }
    HasCombinedOffset, combined_offset_mut -> Offset { combined_offset }
    HasCombinedFetch, combined_fetch_mut -> Fetch { combined_fetch }
}
