//! The test-data factory layer — the runtime the factory generator will emit
//! against.
//!
//! bob took this layer from Ruby's FactoryBot; Rust has almost no equivalent,
//! so it is designed the way Layer 2 was: **nothing is generated yet** — this
//! crate is the machinery ([`Source`] per-column value sources, [`Parent`]/
//! [`OptionalParent`] reference states, the [`Sequence`] and the [`Faker`]),
//! and the hand-written users/posts/comments factory in `tests/` is the
//! generator's specification, written once by hand and run against real
//! engines. The call-site shape being served:
//!
//! ```ignore
//! use factories as fac;
//!
//! // Mods are values, keelson's house style throughout.
//! let u = fac::users::factory((fac::users::id(10), fac::users::name("Ada")))
//!     .create(&db)
//!     .await?;
//!
//! // The schema-aware win: a comment needs a post needs a user, and
//! // create_many makes the whole chain exist.
//! let cs = fac::comments::factory(()).create_many(&db, 10).await?;
//! ```
//!
//! # The decisions, recorded
//!
//! **Factories fire model hooks.** `create`/`create_many` insert through
//! Layer 2's `ModelInsert` path — the same `table().insert(setter).one(db)`
//! production writes take — so `before_insert` rewrites the setter and
//! `after_insert` runs on the caller's executor, exactly as they would for any
//! other write. FactoryBot fires callbacks for the same reason: a factory that
//! bypassed hooks would manufacture rows the application could never have
//! written, and tests against such rows test a database that does not exist.
//! `build()` runs no hooks — it produces a plain setter with no executor in
//! sight, and is the raw-data escape hatch.
//!
//! **Non-null FKs auto-create their parents.** A required parent reference is
//! a [`Parent`] field on the template, defaulting to `Auto`: at create time
//! the parent's own default template is created first (its parents
//! recursively, so a comment chains a post chains a user) and the FK takes the
//! created row's key. Each created row gets its **own** parent chain —
//! FactoryBot's association semantics; to share a parent, create it once and
//! pass it back in via the existing-row mod (`post(&p)`) or shape it with a
//! template mod (`for_post(…)`). A *nullable* FK is an [`OptionalParent`]
//! defaulting to `Absent` — the column stays NULL unless a mod opts in, so a
//! factory never invents rows the schema does not require.
//!
//! **Uniqueness is sequence-based.** Primary-key and unique columns default to
//! [`Sequence`] values: a process-unique, time-derived base plus an atomic
//! counter (the same shape the Layer 2 spec's `key()` pinned), so
//! `create_many(&db, 100)` cannot collide in-process and does not collide with
//! earlier runs against a shared persistent server.
//!
//! **Random values: in-crate SplitMix64, no dependency.** The evaluation:
//!
//! - `fake` — rejected. Its realistic-looking data (names, addresses,
//!   locales) is cosmetic for schema-level test rows, its dependency tree is
//!   the largest of the three options, and its output for a given seed is not
//!   a stability contract, which fights the determinism switch below.
//! - `rand` alone — nearly right, but `StdRng` documents that its algorithm
//!   may change between major versions, so "seeded runs reproduce" would be a
//!   promise held by a dependency's semver policy — and the actual need is a
//!   few dozen lines of uniform integers and short strings.
//! - **Chosen: an in-crate SplitMix64** ([`Faker`]) — zero dependencies, and
//!   the exact output sequence is pinned by test *in this crate*, so
//!   reproducibility is keelson's own tested contract rather than an upstream
//!   accident. Reopening condition: if factories ever need realistic data,
//!   add `fake` behind an off-by-default feature; the [`Source::Gen`] seam is
//!   where it would plug in.
//!
//! **The determinism switch, and its honest scope.** Every random default
//! draws from the [`Faker`] threaded through `build`/`create_with`;
//! `Faker::seeded(n)` makes two runs draw identical values, and the spec pins
//! that. [`Sequence`] values are deliberately **outside** the seed: sequences
//! are uniqueness machinery, and reproducing a primary key against a shared
//! server would reproduce a collision. Seeded runs therefore reproduce every
//! random-sourced column while unique columns stay unique — which is the only
//! version of "reproducible test data" that survives contact with a real
//! database.
//!
//! **`build()` touches no database — by signature.** It takes no executor, so
//! the guarantee is compile-time, not behavioural. Consequence, recorded: a
//! required FK whose parent is `Auto` or a template cannot be filled without a
//! database, so `build()` leaves it unset — the caller either provides the key
//! (`user_id(k)` / `user(&u)`) or uses `create`, where the chain is made.

#![warn(missing_docs)]

mod faker;
mod parent;
mod sequence;
mod source;

pub use faker::Faker;
pub use parent::{OptionalParent, Parent};
pub use sequence::Sequence;
pub use source::Source;
