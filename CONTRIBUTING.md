# Contributing to keelson

The gates are strict and the reasons are written down. That combination is the
point: nothing here should require you to guess what the project wants, and
nothing should be enforced only by review.

Start with the two documents that explain the shape of the thing you are about
to change:

- **[docs/testing-tiers.md](docs/testing-tiers.md)** — what each testing tier
  proves, and why a construct is not "tested" until the coverage gate sees it.
- **the crate's own `src/lib.rs`** — each crate's architecture documentation
  lives at the top of its `lib.rs`, decisions and rejected alternatives
  included. It is the intended thing to read before changing that crate, and
  the intended place to record a decision you make.

## The gates

Everything CI runs, you can run. These four are the ones that fail most often;
run them before pushing.

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

Then the ones that are cheap but easy to forget:

```sh
./scripts/run-examples.sh              # the examples assert their own output
./scripts/feature-matrix.sh            # every feature combination the facade advertises
./scripts/check-version-consistency.sh # one version, everywhere it is written
cargo doc --workspace --no-deps        # with RUSTDOCFLAGS="-D warnings"
cargo deny check                       # advisories and licences (cargo install cargo-deny)
```

And the two that need more than a toolchain:

```sh
# Tier D: prove the suites actually exercised every declared construct.
KEELSON_SQLCHECK_RECORD="$PWD/target/sqlcheck-record" cargo test --workspace
cargo run -p keelson-sqlcheck --bin coverage-gate -- target/sqlcheck-record

# The engine tier: real PostgreSQL 17 and MySQL 8.4 in containers. Needs Docker,
# takes several minutes, and is what MySQL's correctness actually rests on.
# A cargo alias from .cargo/config.toml, where the feature list it passes is
# written once — every crate that gates engine tests on a feature has to be
# named, because a feature does not propagate from a dependency to its
# dependents, and naming only the judge's compiles the rest out.
cargo test-engine

# Or one crate at a time, where its own feature is the whole story:
cargo test -p keelson-sqlx --features live-docker
```

`cargo test --workspace` already runs the grammar judges and a real SQLite —
`pg_query`, `sqlite3-parser` and bundled `rusqlite` compile in — so a plain test
run is the full non-engine verification, with no services.

## What a change is expected to carry

**A rendering change carries a Tier A case derived from the manual.** Expected
SQL is written from the engine's own reference material, never pasted from the
builder's output: a test that compares the code to itself proves nothing. If
the construct is new, it also goes in the dialect's coverage manifest, or
Tier D will not know to look for it.

**A dialect gets what its grammar has, and nothing else.** There is no shared
AST here on purpose. A construct only PostgreSQL has belongs only in
`keelson-psql`; a construct MySQL lacks must be *absent* from `keelson-mysql`,
or must fail with an error that names the engine's rule. Silent fallbacks and
plausible guesses are the one thing this library refuses to ship.

**A generated-code change re-blesses its fixtures.** The generator's output is
checked in and compiled:

```sh
KEELSON_GEN_BLESS=1 cargo test -p keelson-gen        # the tests/generated fixtures
cd examples && cargo run -p keelson-gen -- --config keelson.toml --url sqlite://blog.db
```

The examples' committed models, queries and `schema.snapshot.json` are gated by
`examples/tests/generated_is_fresh.rs`, which calls the same `keelson_gen::check`
the CLI's `--check` does.

**A compile-time refusal is a test.** `tests/compile_fail/` holds the mistakes
keelson will not compile, next to the exact error each must produce. After an
intentional change to a message:

```sh
TRYBUILD=overwrite cargo test -p keelson-examples --test compile_fail
```

**A public item carries documentation, and a decision carries its rejected
alternative.** `missing_docs` is a warning that CI denies, so the first half is
mechanical. The second half is the house style: when you choose between two
designs, the one you did not take belongs in the doc comment, in a sentence.
That is what makes the next person's disagreement productive.

**A user-visible change carries a CHANGELOG entry.** `CHANGELOG.md` follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); the eleven published
crates share one version, so one entry covers them all. Write it for someone
upgrading: what changed, and what they should do about it.

## Commits and pull requests

Commits are `type(scope): summary`, lowercase, in the imperative, and the
summary says what the change *achieves* rather than which files moved:

```
feat(exec): atomic — a unit of work that nests where it is called
fix(gen): refuse a table name that singularises to itself
docs(examples): lead raw_sql with sql!, and show what it makes impossible
```

Types in use: `feat`, `fix`, `docs`, `test`, `refactor`, `perf`, `ci`, `deps`,
`chore`. Scopes are crate names without the `keelson-` prefix (`exec`, `gen`,
`psql`, `models`, …), or omitted when the change is workspace-wide.

Pull requests run every gate above except the release-only ones. The engine
tier gates merges too — engine acceptance is part of this repo's definition of
correct, not a thing to check afterwards.

## Adding a dependency

Rarely, and with a sentence saying why in the workspace manifest next to it —
that file is a series of such sentences already. New dependencies must pass
`cargo deny check`: a permissive licence from the list in `deny.toml`, from
crates.io, with no open advisory. `default-features = false` unless the
defaults are actually wanted; several entries there exist purely to keep a
transitive feature from arriving uninvited.

## Reporting something

- **A bug:** the smallest query or schema that reproduces it, the dialect, and
  what the engine said. A failing test case is better than a description, and
  a failing test case in a pull request is better still.
- **A security issue:** not here — see [SECURITY.md](SECURITY.md).

## Licence

By contributing you agree that your contributions are licensed under the MIT
licence, the same as the rest of the repository.
