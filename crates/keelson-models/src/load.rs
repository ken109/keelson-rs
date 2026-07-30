//! Then-loading: the second query, and the third, and the fourth.
//!
//! A one-level then-load fetches a relation with one keyed query instead of
//! one query per parent row. The level below it — the relation *of* that
//! relation — is where the N+1 problem actually bites, so a then-load is not a
//! closure here but a value, [`ThenLoad`], that another then-load can be
//! attached to.
//!
//! # The decisions, recorded
//!
//! **Path syntax: chained values, not strings.** A path is written by hanging
//! one generated relation mod off another:
//!
//! ```ignore
//! posts::table().query(
//!     posts::then_load::user()                     // posts → author
//!         .then(users::then_load::posts()),        //       → the author's posts
//! )
//! ```
//!
//! There is no `"user.team"` string anywhere. `then` takes an
//! [`IntoLoader<C::Row>`](IntoLoader) — a loader for *this* level's child
//! model — so `posts::then_load::user().then(posts::then_load::user())` does
//! not compile: the inner one loads onto `Post`s, and this level hands it
//! `User`s. A misspelt or mis-rooted path is a type error at the call site,
//! and each level is still an ordinary mod that can be used alone.
//!
//! Several relations at one level are several `then` calls
//! (`.then(a).then(b)`); they run in order, each over the same child set.
//!
//! **One batched `IN` query per level, not a join.** Level *n* runs after
//! level *n-1* has its rows, keyed by exactly the keys those rows carry:
//! `SELECT … WHERE "users"."id" IN ($1, …)`. The alternative — widening the
//! parent's `LEFT JOIN` chain — was rejected for to-many relations because a
//! join multiplies the parent rows by every child (and by every grandchild
//! again at the next level), so the wire cost of a three-level path grows as
//! the product of the fan-outs while the batched form stays additive. It is
//! also the only shape that works uniformly: a to-many level cannot be
//! decoded out of a widened row set without a group-by pass, while a batched
//! level is the same code for both cardinalities. The cost of the choice is
//! one round trip per level (plus one per batch, below) — an explicit,
//! bounded number of queries, which the specs assert exactly so a regression
//! to N+1 fails the test.
//!
//! Same-query `preload` (the to-one `LEFT JOIN`) is the exception that keeps
//! its shape: it costs no query at all. It has no `then`, deliberately — its
//! children exist only as per-parent copies inside the parent rows, so a
//! level below it would have to re-derive the distinct child set that the
//! join had already dissolved. Spell that path `then_load::user().then(…)`
//! instead; it is one query, and it is the query the level below needs
//! anyway. The compile error is that `preload::user()` has no `then` method.
//!
//! **Batching: [`KEY_BATCH`] keys per query.** An unbounded `IN` list is a
//! real failure mode — PostgreSQL's and MySQL's wire protocols both cap a
//! statement at 65535 bind parameters, and SQLite built before 3.32 caps
//! `SQLITE_MAX_VARIABLE_NUMBER` at 999 — so the distinct keys of a level are
//! chunked and one query runs per chunk. [`KEY_BATCH`] is 900: under the
//! oldest of those limits, with room left for whatever arguments the caller's
//! own mods put in the same statement. [`ThenLoad::batch`] overrides it per
//! level. The children of every chunk are concatenated before the next level
//! runs, so batching costs queries at *this* level only — it does not
//! multiply the levels below it.
//!
//! **Deduplication.** Keys are sorted and deduplicated before the query, so
//! two posts by the same author put that author in the `IN` list once, fetch
//! it once, and — because the next level runs over the fetched child set
//! rather than over the parents — load *its* relations once. The author
//! arrives in both posts by [`attach_to_one`]'s clone, grandchildren
//! included. Sorting is not only for the `dedup`: it makes the argument list
//! deterministic, so the emitted SQL of a given result set is stable enough
//! to judge.
//!
//! **Cycles terminate because a path is a finite value.** Nothing here walks
//! a relation graph; `then` builds a list, and the list has the length the
//! caller wrote. A cyclic path is legal and terminates at the depth it was
//! written to:
//!
//! ```ignore
//! posts::then_load::user()                          // 1 query: the users
//!     .then(users::then_load::posts()               // 1 query: their posts
//!         .then(posts::then_load::user()))          // 1 query: those posts' users
//! ```
//!
//! — four queries in total including the caller's own, then it stops. The
//! same is true of a self-referential relation (`posts.parent_id → posts`):
//! `parent().then(parent())` is two levels because it says two levels. There
//! is no "load until it stops changing" mode to run away, and no depth
//! counter is needed to stop one.
//!
//! **Ordering within a level.** The child model's `after_select` runs per
//! batch, as part of the batch's own `all()`, and therefore *before* the
//! deeper levels load; a hook on the child model sees its own rows with
//! `rel` still empty. The alternative — deferring it until the whole subtree
//! is loaded — would make a two-level query run the hook at a different point
//! than a one-level query does, which is the worse surprise.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;

use keelson_core::Mod;
use keelson_exec::{ExecError, Executor};

use crate::select::{Loader, ModelSelect};
use crate::{ModelTable, View};

/// How many keys one level's keyed query may carry.
///
/// The binding constraint is the oldest SQLite default
/// (`SQLITE_MAX_VARIABLE_NUMBER` = 999 before 3.32); PostgreSQL and MySQL cap
/// a statement at 65535 parameters. 900 sits under all three with room for
/// the caller's own arguments in the same statement. Override per level with
/// [`ThenLoad::batch`].
pub const KEY_BATCH: usize = 900;

/// A re-appliable query shaper ([`ThenLoad::with`]). Not a [`Mod`]: a mod is
/// consumed when it is applied, and a level applies its shape to every batch
/// query, every time the parent query runs.
type Shape<C> = Arc<dyn Fn(&mut ModelSelect<C>) + Send + Sync>;

/// Anything that can act as one level of a load path over `T`.
///
/// Implemented by [`ThenLoad`] — which is what the generated relation mods
/// return — and by a bare [`Loader<T>`], so a hand-written loader nests
/// exactly like a generated one.
pub trait IntoLoader<T> {
    /// The loader payload this level runs.
    fn into_loader(self) -> Loader<T>;
}

impl<T> IntoLoader<T> for Loader<T> {
    fn into_loader(self) -> Loader<T> {
        self
    }
}

/// One level of a load path: fetch `C` for a set of `P`, keyed, batched and
/// deduplicated — plus the levels hanging off it.
///
/// Generated `then_load::…()` functions return one of these. It is a
/// [`Mod`] over the parent's [`ModelSelect`], so it drops into a query tuple
/// like any other mod; [`then`](ThenLoad::then) is what makes it a path
/// rather than a single level.
///
/// The three function pointers are the model-specific half — which keys to
/// take off the parents, how to filter the child query by them, how to
/// attach the results — and they are function pointers rather than closures
/// because generated code has nothing to capture, which keeps this type
/// `Send + Sync` without a bound in sight.
pub struct ThenLoad<P: View, C: View, K> {
    keys: fn(&[P::Row]) -> Vec<K>,
    key_filter: fn(Vec<K>, &mut ModelSelect<C>),
    attach: fn(&mut [P::Row], Vec<C::Row>),
    shape: Vec<Shape<C>>,
    nested: Vec<Loader<C::Row>>,
    batch: usize,
}

impl<P: View, C: View, K> std::fmt::Debug for ThenLoad<P, C, K> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThenLoad")
            .field("shape", &self.shape.len())
            .field("nested", &self.nested.len())
            .field("batch", &self.batch)
            .finish()
    }
}

impl<P, C, K> ThenLoad<P, C, K>
where
    P: View,
    C: View,
    K: Ord + Clone + Send + Sync + 'static,
{
    /// Assemble a level. Generated code calls this; the three arguments are
    /// the parent keys, the child-query filter and the attachment.
    pub fn new(
        keys: fn(&[P::Row]) -> Vec<K>,
        key_filter: fn(Vec<K>, &mut ModelSelect<C>),
        attach: fn(&mut [P::Row], Vec<C::Row>),
    ) -> Self {
        ThenLoad {
            keys,
            key_filter,
            attach,
            shape: Vec::new(),
            nested: Vec::new(),
            batch: KEY_BATCH,
        }
    }

    /// Hang another level off this one: load `deeper` over the children this
    /// level fetched, before they are attached to their parents.
    ///
    /// Typed by the child model — only a loader over `C::Row` fits — so the
    /// path is checked at the call site rather than spelled in a string.
    #[must_use]
    pub fn then(mut self, deeper: impl IntoLoader<C::Row>) -> Self {
        self.nested.push(deeper.into_loader());
        self
    }

    /// Shape this level's query: the closure runs on **every batch query**,
    /// after the key filter.
    ///
    /// A closure rather than a mod because a mod is consumed when it is
    /// applied and there may be many batches — and because a query is
    /// re-issued every time the parent query runs. Anything that applies to a
    /// `ModelSelect` goes in it: a filter, an order, a Layer 1 mod, or a
    /// `preload` of the child's own to-one relation.
    ///
    /// ```ignore
    /// posts::then_load::user()
    ///     .with(|q| users::is_active().eq(true).apply(q))
    /// ```
    #[must_use]
    pub fn with(mut self, shape: impl Fn(&mut ModelSelect<C>) + Send + Sync + 'static) -> Self {
        self.shape.push(Arc::new(shape));
        self
    }

    /// Override [`KEY_BATCH`] for this level.
    ///
    /// # Panics
    ///
    /// If `keys` is zero: a batch of no keys makes no progress, and silently
    /// substituting a size would hide the caller's mistake.
    #[must_use]
    #[track_caller]
    pub fn batch(mut self, keys: usize) -> Self {
        assert!(keys > 0, "then-load batch size must be at least 1");
        self.batch = keys;
        self
    }

    async fn run(&self, db: &dyn Executor, parents: &mut [P::Row]) -> Result<(), ExecError> {
        let keys = distinct((self.keys)(parents));
        if keys.is_empty() {
            return Ok(());
        }
        let mut children: Vec<C::Row> = Vec::new();
        for chunk in keys.chunks(self.batch) {
            let mut q = ModelTable::<C>::new().query(());
            (self.key_filter)(chunk.to_vec(), &mut q);
            for shape in &self.shape {
                shape(&mut q);
            }
            children.extend(q.all(db).await?);
        }
        // The deeper levels run over the distinct children, once — not per
        // batch and not per parent, which is what makes a shared child load
        // its own relations exactly once.
        for deeper in &self.nested {
            deeper(db, &mut children).await?;
        }
        (self.attach)(parents, children);
        Ok(())
    }
}

/// The keys of one level, deduplicated and ordered: each key appears in the
/// `IN` list once, and the list is deterministic so the emitted SQL of a
/// given result set is stable.
fn distinct<K: Ord>(mut keys: Vec<K>) -> Vec<K> {
    keys.sort_unstable();
    keys.dedup();
    keys
}

impl<P, C, K> IntoLoader<P::Row> for ThenLoad<P, C, K>
where
    P: View,
    C: View,
    K: Ord + Clone + Send + Sync + 'static,
{
    fn into_loader(self) -> Loader<P::Row> {
        let level = Arc::new(self);
        Arc::new(move |db, rows: &mut Vec<P::Row>| {
            let level = Arc::clone(&level);
            Box::pin(async move { level.run(db, rows).await })
        })
    }
}

impl<P, C, K> Mod<ModelSelect<P>> for ThenLoad<P, C, K>
where
    P: View,
    C: View,
    K: Ord + Clone + Send + Sync + 'static,
{
    fn apply(self, q: &mut ModelSelect<P>) {
        q.add_loader(self.into_loader());
    }
}

/// Attach a to-one relation: each parent gets the child whose key matches, or
/// `None`. Children are cloned only where several parents share one.
pub fn attach_to_one<P, C, K>(
    parents: &mut [P],
    children: Vec<C>,
    parent_key: impl Fn(&P) -> K,
    child_key: impl Fn(&C) -> K,
    mut attach: impl FnMut(&mut P, Option<C>),
) where
    K: Eq + Hash,
    C: Clone,
{
    let by_key: HashMap<K, C> = children.into_iter().map(|c| (child_key(&c), c)).collect();
    for p in parents {
        let child = by_key.get(&parent_key(p)).cloned();
        attach(p, child);
    }
}

/// Attach a to-many relation: each parent gets every child whose key matches.
///
/// Each child is attached exactly once — parents are assumed key-distinct,
/// which they are when the key is the parent's primary key (the shape every
/// generated then-load has).
pub fn attach_to_many<P, C, K>(
    parents: &mut [P],
    children: Vec<C>,
    parent_key: impl Fn(&P) -> K,
    child_key: impl Fn(&C) -> K,
    mut attach: impl FnMut(&mut P, Vec<C>),
) where
    K: Eq + Hash,
{
    let mut by_key: HashMap<K, Vec<C>> = HashMap::new();
    for c in children {
        by_key.entry(child_key(&c)).or_default().push(c);
    }
    for p in parents {
        let own = by_key.remove(&parent_key(p)).unwrap_or_default();
        attach(p, own);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    struct Parent {
        id: i32,
        children: Vec<i32>,
        one: Option<i32>,
    }

    fn parents() -> Vec<Parent> {
        (1..=3)
            .map(|id| Parent {
                id,
                children: Vec::new(),
                one: None,
            })
            .collect()
    }

    #[test]
    fn to_many_groups_by_key_and_leaves_misses_empty() {
        let mut ps = parents();
        // (child id, parent id): parent 1 has two, parent 3 none.
        let children = vec![(10, 1), (11, 1), (20, 2)];
        attach_to_many(
            &mut ps,
            children,
            |p| p.id,
            |c| c.1,
            |p, cs| p.children = cs.into_iter().map(|c| c.0).collect(),
        );
        assert_eq!(ps[0].children, vec![10, 11]);
        assert_eq!(ps[1].children, vec![20]);
        assert_eq!(ps[2].children, Vec::<i32>::new());
    }

    #[test]
    fn to_one_attaches_a_match_or_none_and_shares_children() {
        let mut ps = parents();
        ps[1].id = 1; // two parents share the same key
        let children = vec![(100, 1)];
        attach_to_one(
            &mut ps,
            children,
            |p| p.id,
            |c| c.1,
            |p, c| p.one = c.map(|c| c.0),
        );
        assert_eq!(ps[0].one, Some(100));
        assert_eq!(ps[1].one, Some(100), "a shared child is cloned, not stolen");
        assert_eq!(ps[2].one, None);
    }

    /// The deduplication contract: the `IN` list carries each key once, in a
    /// deterministic order.
    #[test]
    fn keys_are_deduplicated_and_ordered() {
        assert_eq!(distinct(vec![3, 1, 3, 2, 1, 1]), vec![1, 2, 3]);
        assert_eq!(distinct(Vec::<i32>::new()), Vec::<i32>::new());
    }

    /// The batching boundary — `distinct(keys).chunks(batch)` is exactly what
    /// `run` iterates, so this counts the queries one level will issue. The
    /// live specs prove a real engine sees the same number.
    #[test]
    fn keys_batch_at_the_boundary() {
        let queries = |n: usize, batch: usize| {
            distinct((0..n as i32).collect::<Vec<_>>())
                .chunks(batch)
                .count()
        };
        assert_eq!(queries(KEY_BATCH - 1, KEY_BATCH), 1);
        assert_eq!(
            queries(KEY_BATCH, KEY_BATCH),
            1,
            "the cap itself is one query"
        );
        assert_eq!(
            queries(KEY_BATCH + 1, KEY_BATCH),
            2,
            "one over the cap is two"
        );
        assert_eq!(queries(2 * KEY_BATCH, KEY_BATCH), 2);
        assert_eq!(queries(2 * KEY_BATCH + 1, KEY_BATCH), 3);
        // An overridden batch behaves the same way — which is what makes the
        // boundary cheap to test against a live engine.
        assert_eq!(queries(4, 2), 2);
        assert_eq!(queries(5, 2), 3);
        // Duplicates cost nothing: 900 rows sharing 3 keys are one query.
        let mut dup = vec![1; KEY_BATCH * 2];
        dup.extend([2, 3]);
        assert_eq!(distinct(dup).chunks(KEY_BATCH).count(), 1);
    }
}
