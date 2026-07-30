use std::fmt;
use std::marker::PhantomData;

use keelson_core::Mod;

use crate::mutate::{ModelDelete, ModelInsert, ModelUpdate};
use crate::select::ModelSelect;
use crate::{Table, View};

/// The model's entry point — what `users::table()` (or, for a `SELECT`-only
/// model, `reports::view()`) returns.
///
/// One zero-sized type for both: the query side needs only [`View`], and the
/// mutations are bounded on [`Table`], so on a view model
/// `insert`/`update`/`delete` simply do not exist — the `View`/`Table` split
/// enforced where it is felt, at the call site.
pub struct ModelTable<M> {
    _model: PhantomData<M>,
}

impl<M> fmt::Debug for ModelTable<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ModelTable")
    }
}

impl<M> Clone for ModelTable<M> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<M> Copy for ModelTable<M> {}

impl<M> Default for ModelTable<M> {
    fn default() -> Self {
        ModelTable::new()
    }
}

impl<M> ModelTable<M> {
    /// The entry point value. Generated `table()`/`view()` functions return
    /// this.
    pub const fn new() -> Self {
        ModelTable {
            _model: PhantomData,
        }
    }
}

impl<M: View> ModelTable<M> {
    /// A `SELECT` over this model: the base select plus `mods` — typed
    /// filters, Layer 1 mods, preloads and then-loads, all in one tuple.
    ///
    /// ```ignore
    /// let adults = users::table().query((
    ///     users::age().gte(21),   // typed: a &str here is a compile error
    ///     select::limit(20),      // Layer 1 mods mix in directly
    /// )).all(&db).await?;
    /// ```
    pub fn query(self, mods: impl Mod<ModelSelect<M>>) -> ModelSelect<M> {
        let mut q = ModelSelect::new(M::base_select());
        mods.apply(&mut q);
        q
    }
}

impl<M: Table> ModelTable<M> {
    /// An `INSERT` of the setter's set fields.
    ///
    /// ```ignore
    /// let u = users::table().insert(users::Setter {
    ///     name: set("Stephen"),
    ///     ..Default::default()
    /// }).one(&db).await?;
    /// ```
    pub fn insert(self, setter: M::Setter) -> ModelInsert<M> {
        ModelInsert::new(setter)
    }

    /// An `UPDATE` of the setter's set fields, filtered and modified by
    /// `mods` (an unfiltered update really is the whole table, exactly as in
    /// SQL).
    pub fn update(self, setter: M::Setter, mods: impl Mod<ModelUpdate<M>>) -> ModelUpdate<M> {
        ModelUpdate::new(setter, mods)
    }

    /// A `DELETE`, filtered and modified by `mods`.
    pub fn delete(self, mods: impl Mod<ModelDelete<M>>) -> ModelDelete<M> {
        ModelDelete::new(mods)
    }
}
