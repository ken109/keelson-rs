# Changelog

All notable changes to this workspace are recorded here. The eleven published
crates share one version and are released together, so one entry covers them
all.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
— with the pre-1.0 caveat that a `0.x` minor bump is allowed to break.

## [0.2.0] — 2026-09-03

A `0.x` minor, and it breaks: `ThenLoad::new` takes a `Relation` rather than
three closures, and `C::Row: Clone` is a bound where the runtime groups a
to-one relation. Both are in the **Changed** section with what to do about
them. `ExecError::Conflict` is additive — the enum is `#[non_exhaustive]` —
but code that reached a concurrency conflict through `ExecError::Driver` and
a `downcast_ref` will stop finding it there; `TxConflict::of` still answers.

### Added

- **`keelson_exec::Header`, a result set's column header with the name lookup
  prepared once.** Resolving a column by name was a scan of the header on
  every read, so decoding a row cost O(columns) name comparisons for each of
  its own columns — quadratic in the width. Measured over 5,000 rows, that
  scan was 23% of the time spent decoding a 30-column row and 66% of an
  80-column one. A backend now builds one `Header` from the first row of a
  result set and calls `Row::with_header` for the rest; wide headers get a
  prepared map, narrow ones keep the scan, which is still faster there.
  `Row::new` is unchanged and still builds a row that carries its own header,
  which is what a test or a hand-built fixture wants.

- **`keelson-gen --check`.** The same run with the writing removed: it renders,
  compares against what is committed, prints what differs and exits `2`.
  Committed generated code that no longer matches its schema still compiles, so
  `cargo build` cannot be the thing that notices — this can. Backed by
  `keelson_gen::check`, `keelson_gen::queries::check` and
  `keelson_gen::check_files`, so a build script or a test can ask the same
  question in-process; `examples/tests/generated_is_fresh.rs` now does exactly
  that instead of hand-rolling the comparison.

- **Schema snapshots — `keelson-gen` without a database.** With
  `snapshot = "schema.snapshot.json"` in the config, a run against a live
  database writes the introspected schema beside the generated code, and a run
  with no `--url` reads it back. A CI job can then answer `--check`, and a
  contributor can regenerate, with no database in reach. The snapshot is a
  generated file like the `.rs` files are: refreshed by the same run, reviewed
  in the same diff, and reported as drift by a `--check` against a live
  database if somebody forgot to commit it. `keelson_gen::schema::{Schema,
  Snapshot}` are now `serde::Serialize`/`Deserialize`, and a snapshot from
  another format version or another engine is refused by name rather than
  surfacing as a pile of unmapped types.

- **`keelson-gen --version`.**

- **Property tests for SQLite and MySQL** (`keelson-sqlite/tests/property.rs`,
  `keelson-mysql/tests/property.rs`). Tier C was psql-only; it is now one lane
  per dialect. The SQLite lane checks `?n` numbering against a real in-process
  SQLite with no Docker; the MySQL lane checks that the bound values arrive in
  render order, which is the invariant a bare positional `?` makes invisible
  everywhere else. `docs/testing-tiers.md` records what each lane can and
  cannot say.

- **`benches/`, a criterion benchmark of `Query::build()`.** Building is
  synchronous and driver-free, which is a claim about a hot path that nothing
  else here measured: the test tiers prove the SQL is right, and a rendering
  change that doubled the allocations would pass all of them. Not a merge gate
  — a shared runner cannot tell a regression from a noisy neighbour — and not a
  comparison against other builders. `publish = false`, and a member of its own
  so criterion never enters a published crate's dev-dependency graph.

- **`keelson-tokio-postgres`, a second Layer 2 backend.** `publish = false`,
  and its job is to make "the execution layer is driver-free" a thing the
  compiler checks rather than a thing the README says. It implements the whole
  surface — `Executor`, `StreamExecutor`, `Begin`, `BeginWith`,
  `RawConnection`, and both halves of the type map — over `tokio-postgres`,
  which shares nothing with sqlx but the wire protocol, and its engine-tier
  tests are written against `&dyn Executor` and `&dyn Begin` without naming the
  crate. No pool, no feature matrix, no compatibility promise; a production
  backend would need all three, and that is why it is not published.

- **`CONTRIBUTING.md` and `SECURITY.md`.** The gates were enforced and
  explained but never collected; the security claims keelson actually makes
  (a bound value is never interpolated; `quote()` escapes for its dialect) and
  the ones it does not (raw SQL is raw SQL) are now written down where someone
  reporting an issue will look.

- **`cargo deny` and Dependabot.** `deny.toml` gates advisories, licences and
  the registry a dependency may come from, and runs as its own CI job;
  `.github/dependabot.yml` is what makes the tree move once a fix exists
  upstream. A `cargo llvm-cov` job now reports Rust line coverage alongside the
  construct-coverage gate — reported, never gated, because a threshold is a
  number people write tests to move.

### Changed

- **A then-load level takes what a relation *is*, not three closures.**
  `ThenLoad::new(Relation { parent_key, child_key, filter, attach })`, where
  `attach` is `Attach::One` or `Attach::Many`. The old shape asked generated
  code for the parent keys, the child-query filter, and an attachment that
  then called `attach_to_one`/`attach_to_many` with two *more* key closures —
  so each key was written twice and keeping the pair in agreement was the
  generator's problem, along with a four-way match over map/`filter_map` and
  a helper that bridged the two sides' nullability. A key is `Option<K>` on
  both sides now, so a `None` matches nothing and none of that is needed. Five
  hundred lines lighter across the generated code, and each line names what it
  is. `C::Row: Clone` is a bound where the runtime groups.

- **`Query::build()` stops allocating per placeholder and per function
  render.** `position.to_string()` built and threw away a `String` for every
  bound argument in all three numbering dialects; `FuncExpr::write_sql` cloned
  itself into an `Expr` to render. Cumulative with the writer's buffers, on
  the `benches/` suite: an eight-element `IN` list is 37% faster, a
  512-element one 44%, a single-row `INSERT` 31%, a 64-deep nested predicate
  22%. All p < 0.05. `SqlWriter::push_usize` is the new method a dialect uses
  to write a placeholder's number, and is what a custom `Dialect` should call
  instead of formatting the number itself.

- **Six hundred lines of `impl Has* for XQuery` became a line each.**
  `keelson_core::impl_clause_accessors!` takes the pair those five-line impls
  actually carried — which trait, which field — and the three dialect crates
  had a hundred and forty-three of them. `keelson-models` gained the same
  treatment for its clause delegations. Nothing that was not exactly that
  shape was touched.

- **Tier C's three property lanes share one random-query generator**
  (`keelson_sqlcheck::property`). The typed AST and its strategies were
  written out three times, identically, and one copy had already drifted.
  Each lane still owns its rendering and its invariants.

- **`transaction.rs`, `value.rs` and `coverage.rs` are modules, not files.**
  No API changed in any of them: `keelson_exec::{Begin, TxOptions,
  TxConflict, …}` and `keelson_core::{Value, ToValue, FromValue}` resolve
  exactly as before, and the coverage gate still reports 207/207, 153/153 and
  117/117.

- **A concurrency conflict is `ExecError::Conflict`, not a boxed driver
  error.** Whether to retry is the most consequential question a caller asks
  of an execution error, and the answer was behind `downcast_ref` — the type
  it downcast to being keelson's own, wrapped in `ExecError::Driver` on the
  way out and unwrapped by `TxConflict::of` on the way back in. It is a
  variant now, so `Err(ExecError::Conflict(c))` is a plain `match` arm.
  `TxConflict::of` still answers the same question for code that does not want
  to match, and the driver error it was classified out of is still underneath
  as the `source`.

- **One conformance suite, run by every execution backend.**
  `keelson_sqlcheck::conformance` holds the round-trip floor — every mapped
  type out and back, with the edges of each — written against `&dyn Executor`,
  which is all a backend is. It used to live in keelson-sqlx's tests and be
  shared by that crate's three engines and nothing else, so the second
  PostgreSQL backend had written its own: seven types with one value apiece
  against twelve types and some forty values, with `f64`, `bytea`, `date`,
  `time`, `timestamp` and `numeric` having no round-trip coverage there at
  all. Nothing said so; the two files simply had no relationship. A backend
  now gets the whole suite from one call and cannot be thinner than the others
  by accident — the one type it may decline, `Decimal`, is declined by naming
  a feature in its manifest, which is a line a reviewer sees.

- **The `SQLSTATE`s that mean "retry" are named once**, in
  `TxConflict::from_postgres_sqlstate`, rather than once per PostgreSQL
  backend with a comment in each saying it matched the other. Two backends
  disagreeing there would mean the same workload retried on one and given up
  on on the other.

- **`Query::build()` allocates its buffers once.** `SqlWriter` started with an
  empty `String` and an empty `Vec`, so every statement grew both from nothing
  through a handful of reallocations. Pre-sizing them costs no more per
  allocation and removes the copying: on the `benches/` suite, a typical
  `SELECT` is 8–12% faster, an eight-element `IN` list 24%, and a single-row
  `INSERT` 21%.

- **The engine tier's feature list is a cargo alias.** `cargo test-engine`,
  defined in `.cargo/config.toml`. The list of `--features` it passes had been
  written out in four places — the CI workflow, twice in
  `docs/testing-tiers.md`, and `CONTRIBUTING.md` — and the failure mode of
  updating one copy and not the others is a green run that tested less than it
  claimed, which is exactly what had already happened once.

- **`unsafe` is denied workspace-wide, and forbidden in every published
  crate.** `#![forbid(unsafe_code)]` in each shipped crate's `lib.rs`, with the
  one exception — `keelson-sqlcheck`'s `atexit` container sweep, which is
  `publish = false` — taking a local `#[allow]` at the site.

- **The dialect crates carry docs.rs metadata.** `sql!` is behind the `macros`
  feature, so the docs.rs pages for `keelson-psql`, `keelson-mysql` and
  `keelson-sqlite` were rendering without the one item the README leads with.

### Fixed

- **A cast inferred a different type from the column it was cast to.** Layer
  4's query analysers each carried their own copy of the database-type table,
  while `queries/psql.rs` documented itself as going "through the same table
  the model generator uses for columns". It did not, and the copies had
  drifted: the SQLite analyser had no `numeric`/`decimal` rule at all, so
  `CAST(x AS NUMERIC)` inferred nothing where a `NUMERIC` *column* inferred
  `rust_decimal::Decimal`, and the PostgreSQL analyser had never listed
  `name`. Casts now resolve through `typemap`, the one table.

- **The engine tier was not running most of its tests.** CI ran
  `cargo test --workspace --features keelson-sqlcheck/live-docker`, which
  starts the containers and switches on the dialect crates' engine judging —
  they ask `live::available()` at run time — but leaves compiled out every test
  gated on its *own* crate's `live-docker` feature, because a feature does not
  propagate from a dependency to its dependents. keelson-sqlx's nine
  PostgreSQL and MySQL transaction tests, and keelson-models', keelson-factory'
  s and keelson-gen's live suites, were being skipped in every run. The engine
  job now names each crate's feature, through the `cargo test-engine` alias
  that `docs/testing-tiers.md` and `CONTRIBUTING.md` also point at.

- **`h2` moved past RUSTSEC-2026-0258** in the lockfile (a dev-only path,
  through `testcontainers`). The one advisory that remains — RUSTSEC-2023-0071,
  the Marvin attack against `rsa`, reached through `sqlx-mysql`'s
  `caching_sha2_password` support — has no patched release to move to and is
  ignored with its exposure and its mitigation written out in `deny.toml`.

- **`chacha20` moved off a yanked release** (`0.10.1` → `0.10.2`), reached
  through `rand` under `postgres-protocol`. Found by the new supply-chain job
  on its first run, which is the sort of thing `cargo build` has no opinion
  about.

## [0.1.1] — 2026-08-05

### Fixed

- **`#[derive(FromRow)]` and `#[derive(Bind)]` now compile for a crate that
  depends only on the `keelson` facade.** Both expanded to `::keelson_exec` and
  `::keelson_core`, which do not resolve when those crates are transitive — so
  the one-line dependency the README advertises, together with the derives that
  same README demonstrates, did not build. The derives now resolve the path
  from the caller's own manifest (`proc-macro-crate`): `::keelson_exec` for a
  crate that depends on it directly, `::keelson::exec` for one that took the
  facade, and the caller's own name for a renamed dependency.

  If you worked around this by adding `keelson-core` or `keelson-exec` to your
  `Cargo.toml`, you can drop them again — though keeping them is harmless, and
  generated code still names them directly on purpose.

### Added

- `tests/facade-consumer`, an unpublished workspace member that depends on
  `keelson` and nothing else. Every other compilation context here has the
  inner crates for reasons of its own — dev-dependencies in the macro crate,
  generated code in the examples crate, the package's own dependencies in an
  integration test — so none of them could see the bug above. This one is the
  seat a new reader sits in, and it is now part of `cargo test --workspace`.

## [0.1.0] — 2026-08-04

The first release. Everything below already existed and was tested; this
version is the point at which it became installable.

### Added

- **Layer 1 — query builder.** Statement starters (`select`, `insert`,
  `update`, `delete`, `merge`) filled in by *mods*: values that modify a
  statement. A tuple of mods is itself one mod, so composition never runs out
  of arity, and a raw `&str` is a first-class expression anywhere one is
  accepted. Three dialect crates, each written to its own engine's grammar
  rather than to a shared AST: `keelson-psql`, `keelson-mysql`,
  `keelson-sqlite`, over `keelson-core`.
- **Layer 2 — execution.** An object-safe `Executor` (a pool, a connection, a
  transaction, a `&dyn Executor`), the `Execute` verbs on every query
  (`fetch_all`, `fetch_one`, `fetch_optional`, `fetch_scalar`, `fetch_scalars`,
  `execute`), lifetime-free transactions with closure-scoped savepoints and
  per-transaction isolation levels, and row decoding that names the column that
  failed. `keelson-exec` holds the traits; `keelson-sqlx` is the backend over
  sqlx's PostgreSQL, MySQL and SQLite drivers.
- **Layer 3 — models and factories.** A typed shell per table or view, where
  `users::age()` is one `Column<i64>` that is the expression, the filter origin
  and the alias carrier at once; a three-state `Setter` that distinguishes
  *unset* from `NULL`; hooks as trait methods; relations loaded by same-query
  preload or by chained, batched then-loads. Plus factories with auto-created
  parent chains, sequence-based uniqueness and a seedable faker.
  `keelson-models`, `keelson-factory`.
- **Layer 4 — generation.** `keelson-gen`, a CLI that introspects a live schema
  and writes the models and factories as `.rs` files you commit and diff. It
  also compiles hand-written `.sql` files into typed modules, with each query
  usable either as a query of its own or as a mod merged flat into a model
  query.
- **`keelson`**, the facade crate: it re-exports the individual crates behind
  features and nothing else, so `keelson::psql` *is* `keelson_psql`. No feature
  is on by default.
- **`keelson-macros`**, reached through `keelson-core`'s `macros` feature:
  `#[derive(Bind)]`, `#[derive(FromRow)]`, and each dialect's `sql!`.

### Notes

- **MSRV is Rust 1.90**, edition 2024.
- **The API will change.** Layer 1's shape is the most settled; Layer 3 and
  Layer 4's generated surfaces are the most likely to move. Pin exactly and
  read the diff of regenerated files.
- **Nothing here has run in production.** The four layers are heavily tested —
  against the engines' own parsers, and in the engine tier against
  containerised PostgreSQL and MySQL — but testing is not deployment.
- `keelson-sqlcheck` is not published. It judges keelson's own output, needs a
  container runtime, and reads the repository's `tests/schema/` through
  `include_str!`.

[0.2.0]: https://github.com/ken109/keelson-rs/releases/tag/v0.2.0
[0.1.1]: https://github.com/ken109/keelson-rs/releases/tag/v0.1.1
[0.1.0]: https://github.com/ken109/keelson-rs/releases/tag/v0.1.0
