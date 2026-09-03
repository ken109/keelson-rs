use std::fmt;
use std::sync::Arc;

use keelson_core::{Dialect, Expression, Mod, Query, QueryExtensions, QueryType, SqlWriter};
use keelson_exec::{ExecError, ExecFuture, ExecHook, Execute as _, Executor, FromRow, Row};

use crate::View;
use crate::delegate::delegate_clauses;

/// The mapper-mod payload, pinned: after the base row struct is decoded, each
/// mapper mod reads *more* of the same [`Row`] into the struct — the prefixed
/// preload columns a same-query `LEFT JOIN` added.
///
/// This completes the wiring core's [`QueryExtensions`] left open: core fixed
/// the mechanism with type-parameter payloads, keelson-exec pinned `Hook`
/// ([`ExecHook`]) and the raw-row `Loader`, and the mapper-mod payload was
/// explicitly deferred to Layer 2 "which owns the row-mapper it would modify"
/// — this is that row-mapper, so this is where the type gets pinned.
pub type MapperMod<T> = Arc<dyn Fn(&mut Row, &mut T) -> Result<(), ExecError> + Send + Sync>;

/// The then-load payload, pinned: runs after the rows are mapped, with the
/// caller's executor and the **decoded** models, so a second query can be
/// keyed by the first's keys and its results attached to the `rel` fields.
///
/// Deliberately typed over the model rather than reusing keelson-exec's
/// row-level `ExecLoader`: a then-loader's whole job is to mutate decoded
/// structs (`post.rel.user = …`), which `&[Row]` cannot express. `ExecLoader`
/// remains the payload for row-level extensions; model queries never use it.
pub type Loader<T> = Arc<
    dyn for<'a> Fn(&'a dyn Executor, &'a mut Vec<T>) -> ExecFuture<'a, Result<(), ExecError>>
        + Send
        + Sync,
>;

/// Wrap a closure as an [`ExecHook`]. The named-function-plus-`Box::pin`
/// shape generated code uses:
///
/// ```text
/// q.add_hook(hook(|db| Box::pin(async move { … })));
/// ```
pub fn hook<F>(f: F) -> ExecHook
where
    F: for<'a> Fn(&'a dyn Executor) -> ExecFuture<'a, Result<(), ExecError>>
        + Send
        + Sync
        + 'static,
{
    Arc::new(f)
}

/// Wrap a closure as a [`MapperMod`].
pub fn mapper_mod<T, F>(f: F) -> MapperMod<T>
where
    F: Fn(&mut Row, &mut T) -> Result<(), ExecError> + Send + Sync + 'static,
{
    Arc::new(f)
}

/// Wrap a closure as a [`Loader`].
pub fn loader<T, F>(f: F) -> Loader<T>
where
    F: for<'a> Fn(&'a dyn Executor, &'a mut Vec<T>) -> ExecFuture<'a, Result<(), ExecError>>
        + Send
        + Sync
        + 'static,
{
    Arc::new(f)
}

/// A model `SELECT`: the dialect statement plus the extension payloads the
/// query carries — hooks, preload mapper mods, then-loaders.
///
/// Still a [`Query`]: `build()` hands back the same `(String, Vec<Value>)`
/// escape hatch as everything else, and the raw `Execute` verbs keep working.
/// The model verbs — [`all`](ModelSelect::all), [`one`](ModelSelect::one),
/// [`optional`](ModelSelect::optional) — are the path that also runs the
/// extensions, reading them back through [`QueryExtensions`], which this type
/// implements with the pinned payload types.
///
/// Layer 1 interop: the wrapper implements every `Has*` clause trait its
/// statement implements (see `delegate.rs`), so shared dialect mods
/// (`select::limit(20)`, `select::where_("raw sql")`, joins, CTEs, …) apply to
/// it directly, in the same tuple as typed filters. Statement-specific mods go
/// through [`apply`](ModelSelect::apply).
pub struct ModelSelect<M: View> {
    query: M::Select,
    hooks: Vec<ExecHook>,
    mapper_mods: Vec<MapperMod<M::Row>>,
    loaders: Vec<Loader<M::Row>>,
}

impl<M: View> fmt::Debug for ModelSelect<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModelSelect")
            .field("query", &self.query)
            .field("hooks", &self.hooks.len())
            .field("mapper_mods", &self.mapper_mods.len())
            .field("loaders", &self.loaders.len())
            .finish()
    }
}

impl<M: View> ModelSelect<M> {
    pub(crate) fn new(query: M::Select) -> Self {
        ModelSelect {
            query,
            hooks: Vec::new(),
            mapper_mods: Vec::new(),
            loaders: Vec::new(),
        }
    }

    /// Apply mods written against the concrete dialect statement — the escape
    /// hatch for the statement-specific ones (psql's `select::distinct()`)
    /// that are not generic over a `Has*` trait and so cannot land on the
    /// wrapper directly.
    pub fn apply(&mut self, mods: impl Mod<M::Select>) {
        mods.apply(&mut self.query);
    }

    /// The dialect statement, for inspection.
    pub fn as_select(&self) -> &M::Select {
        &self.query
    }

    /// Attach a pre-query hook. Runs on the caller's executor, before the
    /// statement, in attachment order.
    pub fn add_hook(&mut self, hook: ExecHook) {
        self.hooks.push(hook);
    }

    /// Attach a mapper mod — generated preload mods call this.
    pub fn add_mapper_mod(&mut self, mapper_mod: MapperMod<M::Row>) {
        self.mapper_mods.push(mapper_mod);
    }

    /// Attach a then-loader — generated then-load mods call this.
    pub fn add_loader(&mut self, loader: Loader<M::Row>) {
        self.loaders.push(loader);
    }

    /// Every row, mapped, loaded, hooked.
    ///
    /// The order is the contract: hooks → the statement (through the same
    /// traced verb funnel as everything else) → base [`FromRow`] plus mapper
    /// mods, per row → then-loaders → [`View::after_select`]. All of it on
    /// `db`, so inside `db`'s transaction when `db` is one.
    pub async fn all(&self, db: &dyn Executor) -> Result<Vec<M::Row>, ExecError> {
        for h in self.hooks() {
            h(db).await?;
        }
        let rows = self.query.fetch_rows(db).await?;
        let mut models = Vec::with_capacity(rows.len());
        for mut row in rows {
            let mut model = M::Row::from_row(&mut row)?;
            for mm in self.mapper_mods() {
                mm(&mut row, &mut model)?;
            }
            models.push(model);
        }
        for l in self.loaders() {
            l(db, &mut models).await?;
        }
        M::after_select(db, &mut models).await?;
        Ok(models)
    }

    /// Exactly one row — zero is [`ExecError::RowNotFound`], two is
    /// [`ExecError::TooManyRows`], matching the execution layer's "one means
    /// one".
    pub async fn one(&self, db: &dyn Executor) -> Result<M::Row, ExecError> {
        let mut models = self.all(db).await?;
        match models.len() {
            0 => Err(ExecError::RowNotFound),
            1 => Ok(models.pop().expect("len checked")),
            _ => Err(ExecError::TooManyRows),
        }
    }

    /// At most one row; a second is still [`ExecError::TooManyRows`].
    pub async fn optional(&self, db: &dyn Executor) -> Result<Option<M::Row>, ExecError> {
        let mut models = self.all(db).await?;
        match models.len() {
            0 => Ok(None),
            1 => Ok(models.pop()),
            _ => Err(ExecError::TooManyRows),
        }
    }
}

impl<M: View> Expression for ModelSelect<M> {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        self.query.write_sql(w);
    }
}

impl<M: View> Query for ModelSelect<M> {
    fn query_type(&self) -> QueryType {
        self.query.query_type()
    }

    fn dialect(&self) -> &dyn Dialect {
        self.query.dialect()
    }
}

/// The extension points, answered with the pinned payload types — the
/// completion of the `QueryExtensions` wiring core left with type parameters.
impl<M: View> QueryExtensions<ExecHook, Loader<M::Row>, MapperMod<M::Row>> for ModelSelect<M> {
    fn hooks(&self) -> &[ExecHook] {
        &self.hooks
    }

    fn loaders(&self) -> &[Loader<M::Row>] {
        &self.loaders
    }

    fn mapper_mods(&self) -> &[MapperMod<M::Row>] {
        &self.mapper_mods
    }
}

delegate_clauses!(ModelSelect, View, Select, {
    HasWith       => with_mut:        keelson_core::clause::With,
    HasSelectList => select_list_mut: keelson_core::clause::SelectList,
    HasTableRef   => table_ref_mut:   keelson_core::clause::TableRef,
    HasJoins      => joins_mut:       Vec<keelson_core::clause::Join>,
    HasWhere      => where_mut:       keelson_core::clause::Where,
    HasGroupBy    => group_by_mut:    keelson_core::clause::GroupBy,
    HasHaving     => having_mut:      keelson_core::clause::Having,
    HasWindows    => windows_mut:     keelson_core::clause::Windows,
    HasOrderBy    => order_by_mut:    keelson_core::clause::OrderBy,
    HasLimit      => limit_mut:       keelson_core::clause::Limit,
    HasOffset     => offset_mut:      keelson_core::clause::Offset,
    HasFetch      => fetch_mut:       keelson_core::clause::Fetch,
    HasLocks      => locks_mut:       keelson_core::clause::Locks,
    HasCombines   => combines_mut:    keelson_core::clause::Combines,
});
