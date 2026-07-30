use keelson_core::Query;
use keelson_exec::{ExecError, ExecFuture, Executor, FromRow};

/// The no-op every hook defaults to.
fn done<'a>() -> ExecFuture<'a, Result<(), ExecError>> {
    Box::pin(std::future::ready(Ok(())))
}

/// A readable model: enough to `SELECT` and map rows. No primary key
/// required — a database view, a reporting projection, a read-only slice of a
/// table are all `View`s. [`Table`] adds the mutations.
///
/// Implemented by the generated model *marker* type (`users::Users`), not by
/// the row struct: the marker carries the associated types and the hooks, the
/// row struct stays plain data.
///
/// # Hooks
///
/// [`after_select`](View::after_select) is a trait default method — **static
/// dispatch, no downcasting**, the deliberate departure from bob's runtime
/// type-assertion opt-in: the generator emits nothing for a model without
/// hooks, an application overrides the method on its model, and the call
/// resolves at compile time. The hook receives `&dyn Executor` — exactly the
/// executor the caller passed in — so it runs *inside the caller's
/// transaction* when there is one, and cannot end a transaction it did not
/// open (the execution layer was shaped for precisely this; see
/// `docs/execution.md` §Q2).
///
/// There is deliberately no `before_select`: everything a before-select hook
/// could do to the query, a query mod already does at the same call site, and
/// ad-hoc pre-query work rides the [`QueryExtensions`] hook channel
/// ([`ModelSelect::add_hook`](crate::ModelSelect::add_hook)).
///
/// [`QueryExtensions`]: keelson_core::QueryExtensions
pub trait View: Sized + Send + Sync + 'static {
    /// The row struct rows decode into, `rel` field included.
    type Row: FromRow + Send + Sync + 'static;

    /// The dialect's `SELECT` statement type. Generated models are tied to
    /// one dialect here — which is what makes a dialect/backend mismatch a
    /// compile-time impossibility rather than a runtime check.
    type Select: Query + Send + Sync + 'static;

    /// The seeded `SELECT`: this model's columns, `FROM` this model's table.
    /// Everything else — filters, mods, preloads — is applied on top by
    /// [`ModelTable::query`](crate::ModelTable::query).
    fn base_select() -> Self::Select;

    /// Runs after rows are mapped and loaders have finished, on the caller's
    /// executor. `&mut` so a hook may massage the result set.
    fn after_select<'a>(
        db: &'a dyn Executor,
        rows: &'a mut Vec<Self::Row>,
    ) -> ExecFuture<'a, Result<(), ExecError>> {
        let _ = (db, rows);
        done()
    }
}

/// A writable model: a [`View`] with a primary key and the three mutations.
///
/// The `View`/`Table` split is the surface contract: `SELECT`-only models
/// implement `View` alone, and `insert`/`update`/`delete` simply do not exist
/// on them — misuse-resistance by trait bound, not by runtime error.
///
/// # What the generator emits per method
///
/// The `*_query` methods are the codegen seam: each returns (or completes) a
/// plain Layer 1 statement of this model's dialect, so everything the machinery
/// runs is an ordinary [`Query`] that raw mods can keep modifying. The
/// hand-written model in `keelson-models/tests/` is the byte-for-byte
/// specification of what the generator will write.
///
/// # Hooks
///
/// Same design as [`View::after_select`]: trait default methods, statically
/// dispatched, `&dyn Executor` in. The before-mutation hooks additionally
/// receive the `Setter` **mutably** — stamping a timestamp or normalising a
/// value before it is written is the canonical before-hook, and giving the
/// hook the same three-state `Setter` the caller used means it can also tell
/// "not mentioned" from "set to NULL".
pub trait Table: View {
    /// The primary key's Rust type. A composite key is a tuple.
    type Pk: Send + 'static;

    /// The generated three-state setter struct.
    type Setter: Default + Send + 'static;

    /// The dialect's `INSERT` statement type.
    type Insert: Query + Send + Sync + 'static;

    /// The dialect's `UPDATE` statement type.
    type Update: Query + Send + Sync + 'static;

    /// The dialect's `DELETE` statement type.
    type Delete: Query + Send + Sync + 'static;

    /// An `INSERT` of exactly the set fields, `RETURNING` this model's
    /// columns (on dialects that have `RETURNING`; see the per-dialect notes
    /// in the crate docs). An all-unset setter inserts the row the schema's
    /// defaults describe.
    fn insert_query(setter: Self::Setter) -> Self::Insert;

    /// The bare `UPDATE` of this model's table, with no assignments yet:
    /// filters and mods apply to this, and the assignments arrive at run time
    /// via [`apply_setter`](Table::apply_setter) — *after*
    /// [`before_update`](Table::before_update) has had its chance to touch
    /// the setter.
    fn update_query() -> Self::Update;

    /// Turn the set fields into `SET` assignments on `q`. Unset fields do not
    /// appear.
    fn apply_setter(setter: Self::Setter, q: &mut Self::Update);

    /// The bare `DELETE FROM` this model's table.
    fn delete_query() -> Self::Delete;

    /// This row's primary key — what a keyed loader groups by.
    fn pk(row: &Self::Row) -> Self::Pk;

    /// Runs before the `INSERT` is built; may rewrite the setter.
    fn before_insert<'a>(
        db: &'a dyn Executor,
        setter: &'a mut Self::Setter,
    ) -> ExecFuture<'a, Result<(), ExecError>> {
        let _ = (db, setter);
        done()
    }

    /// Runs after the `INSERT`, with the returned rows (empty when the insert
    /// ran for its side effect only), on the caller's executor — inside the
    /// caller's transaction when there is one.
    fn after_insert<'a>(
        db: &'a dyn Executor,
        rows: &'a [Self::Row],
    ) -> ExecFuture<'a, Result<(), ExecError>> {
        let _ = (db, rows);
        done()
    }

    /// Runs before the assignments are built; may rewrite the setter.
    fn before_update<'a>(
        db: &'a dyn Executor,
        setter: &'a mut Self::Setter,
    ) -> ExecFuture<'a, Result<(), ExecError>> {
        let _ = (db, setter);
        done()
    }

    /// Runs after the `UPDATE`, with how many rows it touched (or returned).
    fn after_update<'a>(
        db: &'a dyn Executor,
        affected: u64,
    ) -> ExecFuture<'a, Result<(), ExecError>> {
        let _ = (db, affected);
        done()
    }

    /// Runs before the `DELETE`.
    fn before_delete(db: &dyn Executor) -> ExecFuture<'_, Result<(), ExecError>> {
        let _ = db;
        done()
    }

    /// Runs after the `DELETE`, with how many rows it removed (or returned).
    fn after_delete<'a>(
        db: &'a dyn Executor,
        affected: u64,
    ) -> ExecFuture<'a, Result<(), ExecError>> {
        let _ = (db, affected);
        done()
    }
}
