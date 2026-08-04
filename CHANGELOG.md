# Changelog

All notable changes to this workspace are recorded here. The eleven published
crates share one version and are released together, so one entry covers them
all.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
— with the pre-1.0 caveat that a `0.x` minor bump is allowed to break.

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

[0.1.1]: https://github.com/ken109/keelson-rs/releases/tag/v0.1.1
[0.1.0]: https://github.com/ken109/keelson-rs/releases/tag/v0.1.0
