use std::fmt;

use keelson_core::Mod;
use keelson_exec::{ExecError, ExecResult, Execute as _, Executor, FromRow, Row};

use crate::Table;
use crate::delegate::delegate_clauses;

fn decode_rows<T: FromRow>(rows: Vec<Row>) -> Result<Vec<T>, ExecError> {
    rows.into_iter().map(|mut r| T::from_row(&mut r)).collect()
}

/// A pending model `INSERT`: the three-state setter, held **unbuilt** so
/// [`Table::before_insert`] can still rewrite it once an executor is in hand.
///
/// That deferral is why this wrapper, unlike the others, stores extra mods as
/// closures ([`with`](ModelInsert::with)) instead of applying them eagerly:
/// there is no statement to apply them to until the verb runs.
pub struct ModelInsert<M: Table> {
    setter: M::Setter,
    #[allow(clippy::type_complexity)] // a list of deferred mods, spelled out
    mods: Vec<Box<dyn FnOnce(&mut M::Insert) + Send>>,
}

impl<M: Table> fmt::Debug for ModelInsert<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModelInsert")
            .field("mods", &self.mods.len())
            .finish_non_exhaustive()
    }
}

impl<M: Table> ModelInsert<M> {
    pub(crate) fn new(setter: M::Setter) -> Self {
        ModelInsert {
            setter,
            mods: Vec::new(),
        }
    }

    /// Defer Layer 1 mods onto the eventual `INSERT` statement —
    /// `.with(insert::on_conflict(…).do_nothing())` is how an upsert or any
    /// other dialect feature mixes into a typed insert.
    #[must_use]
    pub fn with(mut self, mods: impl Mod<M::Insert> + Send + 'static) -> Self {
        self.mods.push(Box::new(move |q| mods.apply(q)));
        self
    }

    async fn build(self, db: &dyn Executor) -> Result<M::Insert, ExecError> {
        let ModelInsert { mut setter, mods } = self;
        M::before_insert(db, &mut setter).await?;
        let mut q = M::insert_query(setter);
        for m in mods {
            m(&mut q);
        }
        Ok(q)
    }

    /// Insert and hand back the one inserted row, via the statement's
    /// `RETURNING`. Zero returned rows is [`ExecError::RowNotFound`] — on a
    /// dialect without `RETURNING` (MySQL) the generated model supplies its
    /// own read-back instead of this verb; see the crate docs.
    pub async fn one(self, db: &dyn Executor) -> Result<M::Row, ExecError> {
        let q = self.build(db).await?;
        let mut models: Vec<M::Row> = decode_rows(q.fetch_rows(db).await?)?;
        match models.len() {
            0 => Err(ExecError::RowNotFound),
            1 => {
                M::after_insert(db, &models).await?;
                Ok(models.pop().expect("len checked"))
            }
            _ => Err(ExecError::TooManyRows),
        }
    }

    /// Insert for the side effect. [`Table::after_insert`] still runs, with an
    /// empty row slice.
    pub async fn exec(self, db: &dyn Executor) -> Result<ExecResult, ExecError> {
        let q = self.build(db).await?;
        let done = q.execute(db).await?;
        M::after_insert(db, &[]).await?;
        Ok(done)
    }
}

/// A pending model `UPDATE`: the statement (which filters and mods have
/// already landed on) plus the setter, joined together only at verb time so
/// [`Table::before_update`] sees the setter first.
pub struct ModelUpdate<M: Table> {
    setter: M::Setter,
    query: M::Update,
}

impl<M: Table> fmt::Debug for ModelUpdate<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModelUpdate")
            .field("query", &self.query)
            .finish_non_exhaustive()
    }
}

impl<M: Table> ModelUpdate<M> {
    pub(crate) fn new(setter: M::Setter, mods: impl Mod<Self>) -> Self {
        let mut u = ModelUpdate {
            setter,
            query: M::update_query(),
        };
        mods.apply(&mut u);
        u
    }

    /// Apply mods written against the concrete dialect statement — the same
    /// escape hatch as [`ModelSelect::apply`](crate::ModelSelect::apply).
    pub fn apply(&mut self, mods: impl Mod<M::Update>) {
        mods.apply(&mut self.query);
    }

    async fn build(self, db: &dyn Executor) -> Result<M::Update, ExecError> {
        let ModelUpdate {
            mut setter,
            mut query,
        } = self;
        M::before_update(db, &mut setter).await?;
        M::apply_setter(setter, &mut query);
        Ok(query)
    }

    /// Update for the side effect; answers how many rows changed.
    pub async fn exec(self, db: &dyn Executor) -> Result<ExecResult, ExecError> {
        let q = self.build(db).await?;
        let done = q.execute(db).await?;
        M::after_update(db, done.rows_affected).await?;
        Ok(done)
    }

    /// Update and decode whatever the statement's `RETURNING` produced —
    /// which is nothing unless a `returning` mod (or the generated model)
    /// put one on. The rows come back as this model's row struct.
    pub async fn all(self, db: &dyn Executor) -> Result<Vec<M::Row>, ExecError> {
        let q = self.build(db).await?;
        let models: Vec<M::Row> = decode_rows(q.fetch_rows(db).await?)?;
        M::after_update(db, models.len() as u64).await?;
        Ok(models)
    }
}

/// A pending model `DELETE`.
pub struct ModelDelete<M: Table> {
    query: M::Delete,
}

impl<M: Table> fmt::Debug for ModelDelete<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModelDelete")
            .field("query", &self.query)
            .finish()
    }
}

impl<M: Table> ModelDelete<M> {
    pub(crate) fn new(mods: impl Mod<Self>) -> Self {
        let mut d = ModelDelete {
            query: M::delete_query(),
        };
        mods.apply(&mut d);
        d
    }

    /// Apply mods written against the concrete dialect statement.
    pub fn apply(&mut self, mods: impl Mod<M::Delete>) {
        mods.apply(&mut self.query);
    }

    /// Delete for the side effect; answers how many rows went.
    pub async fn exec(self, db: &dyn Executor) -> Result<ExecResult, ExecError> {
        M::before_delete(db).await?;
        let done = self.query.execute(db).await?;
        M::after_delete(db, done.rows_affected).await?;
        Ok(done)
    }

    /// Delete and decode the statement's `RETURNING`, if a mod put one on.
    pub async fn all(self, db: &dyn Executor) -> Result<Vec<M::Row>, ExecError> {
        M::before_delete(db).await?;
        let models: Vec<M::Row> = decode_rows(self.query.fetch_rows(db).await?)?;
        M::after_delete(db, models.len() as u64).await?;
        Ok(models)
    }
}

// UPDATE: everything an `UPDATE` can carry across the three dialects.
delegate_clauses!(ModelUpdate, Table, Update, {
    HasWith      => with_mut:      keelson_core::clause::With,
    HasTableRef  => table_ref_mut: keelson_core::clause::TableRef,
    HasJoins     => joins_mut:     Vec<keelson_core::clause::Join>,
    HasWhere     => where_mut:     keelson_core::clause::Where,
    HasOrderBy   => order_by_mut:  keelson_core::clause::OrderBy,
    HasLimit     => limit_mut:     keelson_core::clause::Limit,
    HasSet       => set_mut:       keelson_core::clause::Set,
    HasReturning => returning_mut: keelson_core::clause::Returning,
});

// DELETE.
delegate_clauses!(ModelDelete, Table, Delete, {
    HasWith      => with_mut:      keelson_core::clause::With,
    HasTableRef  => table_ref_mut: keelson_core::clause::TableRef,
    HasJoins     => joins_mut:     Vec<keelson_core::clause::Join>,
    HasWhere     => where_mut:     keelson_core::clause::Where,
    HasOrderBy   => order_by_mut:  keelson_core::clause::OrderBy,
    HasLimit     => limit_mut:     keelson_core::clause::Limit,
    HasReturning => returning_mut: keelson_core::clause::Returning,
});
