# keelson examples

Thirteen runnable programs, one topic each. Every one of them asserts its own
output, so they are also a test suite — `./scripts/run-examples.sh` runs the
lot, and CI does too.

```sh
cargo run -p keelson-examples --example builder_basics
```

Nothing here needs a server. The examples that touch a database use SQLite in
a temporary file that is deleted when they finish.

## In reading order

| | example | what it shows |
|---|---|---|
| **Layer 1** | [`builder_basics`](builder_basics.rs) | statements are a starter plus *mods*; `SELECT`/`INSERT`/`UPDATE`/`DELETE`; identifiers, literals and bound values |
| | [`dynamic_queries`](dynamic_queries.rs) | the filter that is only sometimes applied — `Option<M>`, `Vec<M>` and `()` are all mods, so composition needs no string assembly |
| | [`joins_and_ctes`](joins_and_ctes.rs) | joins, aggregates, CTEs (recursive included), sub-queries, window functions, `DISTINCT ON` |
| | [`dialects`](dialects.rs) | one upsert, three engines; `MERGE`, `REPLACE`, `INSERT OR REPLACE`; and why MySQL has no `returning` to call |
| | [`raw_sql`](raw_sql.rs) | hand-written SQL as a first-class expression, with bound arguments — and what a refusal looks like |
| **Layer 2** | [`execute`](execute.rs) | `fetch_all`/`one`/`optional`/`scalar`/`execute`, `#[derive(FromRow)]`, `&dyn Executor` |
| | [`transactions`](transactions.rs) | the closure form, savepoints, isolation levels, the refusals, and `TxOptions::plan` |
| | [`streaming`](streaming.rs) | a result set one row at a time, and what holds the connection |
| | [`errors`](errors.rs) | every failure mode, provoked on purpose and printed |
| **Layers 3–4** | [`models`](models.rs) | generated models: typed columns, the three-state `Setter`, hooks, a view model |
| | [`relations`](relations.rs) | preload (one `LEFT JOIN`) vs then-load (one keyed query per level), and chaining levels |
| | [`factories`](factories.rs) | test data: auto-created parent chains, sequences, the seedable faker |
| | [`sql_files`](sql_files.rs) | `.sql` files compiled to typed Rust, and the two faces each query has |

## What is in this directory

| path | |
|---|---|
| `schema.sql` | the schema everything runs against — a small blog. keelson does not own this file; your migration tool does. |
| `keelson.toml` | the generator configuration, commented |
| `queries/blog.sql` | hand-written SQL, the source of truth for Layer 4 |
| `src/models/` | **generated.** Models and factories, committed so you can read and diff them. |
| `src/queries/` | **generated.** One module per `.sql` file. |
| `src/hooks.rs` | hand-written hooks the generated models delegate to |
| `src/lib.rs` | `Sandbox`, the throwaway database the examples share |
| `tests/generated_is_fresh.rs` | fails if the generated files no longer match their sources |

## Regenerating

The loop is *migrate → regenerate → compile*, where the compiler tells you
which call sites the schema change broke. keelson does not do the migrating —
use `sqlx migrate`, Atlas, refinery or whatever you already trust — but after
it has run:

```sh
cd examples
sqlite3 /tmp/blog.db < schema.sql          # stand in for "your migrated database"
cargo run -p keelson-gen -- --config keelson.toml --url sqlite:///tmp/blog.db
```

`cargo test -p keelson-examples` fails if you forget.

## The dependency line

`examples/Cargo.toml` depends on the `keelson` facade *and* on the individual
crates. That is not two ways of doing the same thing:

- The examples themselves use the facade (`keelson::sqlite`, `keelson::exec`),
  which is the one-dependency-line story from the root README.
- The **generated** files under `src/` name the crates directly
  (`keelson_models::Column`, `keelson_sqlite::select`), because generated code
  must not assume the application took the facade.

They are the same crates either way, and cargo unifies the features.

## Using another engine

The Layer 1 examples already build PostgreSQL and MySQL statements. To *run*
against one, the change is a feature and a pool type:

```toml
keelson = { version = "0.0.0", features = ["sqlx-psql", "models"] }
```

```rust
let db = keelson::sqlx::psql::Pool::connect(&std::env::var("DATABASE_URL")?).await?;
```

and `dialect = "psql"` in `keelson.toml` before regenerating. The verbs, the
traits and the model surface are identical; what changes is the grammar you
are writing to — and, where an engine cannot do something, an explicit refusal
rather than a quiet substitution.
