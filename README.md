# keelson

A SQL access toolkit for Rust: per-dialect query builders, a driver-free
execution layer, generated models and test factories, and hand-written `.sql`
files turned into typed code. Four layers, adopted one at a time or all at once.

> **Maturity: pre-1.0, and honest about it.** keelson is new. The four layers
> are implemented and heavily tested against real PostgreSQL, MySQL and SQLite,
> but nothing here has run in production yet and the API *will* change. `0.1.0`
> is the first release. Read the [Stability](#stability) section before
> depending on it.

- **License:** MIT
- **MSRV:** Rust 1.90 (edition 2024)
- **Engines:** PostgreSQL, MySQL, SQLite

## What keelson is

keelson is the toolkit half of what people usually call an ORM. It builds SQL,
runs it, and maps rows back — and it generates as much of that as it can read
from your database. What it deliberately does not do is hide SQL from you: a
generated model query and a raw `&str` fragment go into the same call, in the
same tuple, because the query builder was designed so that they could — and a
whole statement you wrote yourself runs through the same verbs as one keelson
built.

The design is strongly inspired by [bob](https://github.com/stephenafamo/bob),
a SQL access toolkit for Go, and departs from it throughout wherever Rust wants
something else. It is not a port and shares no code; see [NOTICE.md](https://github.com/ken109/keelson-rs/blob/main/NOTICE.md).

Four properties hold everywhere:

- **Each dialect is written to its own grammar.** There is no shared AST that
  every database is squeezed through. `keelson-psql`'s `SELECT` is shaped by
  PostgreSQL's own reference manual, `keelson-sqlite`'s by SQLite's railroad
  diagrams, `keelson-mysql`'s by MySQL's. Clauses that genuinely coincide are
  shared through traits, so the mods are written once — but a construct only
  one engine has is only on that engine, and a construct it lacks is not
  offered.
- **Everything that renders SQL is judged by a real parser.** Expected SQL in
  keelson's tests is derived from the official grammar and checked with the
  engine's own parser (PostgreSQL's `libpg_query`, SQLite's `lemon` grammar),
  and — in the engine tier — `PREPARE`d by a real containerised server. A
  coverage gate then proves that every construct the library declares was
  actually exercised. See [docs/testing-tiers.md](https://github.com/ken109/keelson-rs/blob/main/docs/testing-tiers.md).
- **Anything unsupported is an explicit error.** No silent fallback, no
  plausible guess. If MySQL cannot honour a read-only transaction the way you
  asked, you get a refusal that names the engine's rule — not a downgrade.
- **Generated code is meant to be read.** keelson-gen is a CLI that writes
  `.rs` files you commit and diff, not a proc macro that expands somewhere you
  cannot step through.

## Why keelson exists next to sqlx, diesel, SeaORM, sea-query and cornucopia

The Rust SQL ecosystem is good. keelson is not a replacement for any of these,
and for most of them there is a clear question that picks the other one.

**[sqlx](https://github.com/launchbadge/sqlx)** is the driver everyone,
including keelson, ends up standing on — `keelson-sqlx` is a backend over its
PostgreSQL, MySQL and SQLite drivers. What sqlx does better: `query!` verifies
your SQL and its types *against a live database at compile time*, which is a
stronger guarantee than anything keelson offers; it ships migrations; it is
mature and enormously deployed. What it does not try to do is build queries —
`query!` needs the SQL written out, so a filter that is only sometimes applied
means string assembly or a second macro invocation. That is the gap Layer 1
fills, and the reason keelson is a *layer over* sqlx rather than a competitor
to it.

**[diesel](https://diesel.rs)** has the strongest static guarantees in the
ecosystem: its schema DSL makes a column/table mismatch a type error with no
database in the loop, its migrations are excellent, and it is battle-tested.
The trade is that queries live inside diesel's type system, so SQL a dialect
supports but the DSL does not is reached through `sql_query`/`sql_function`
escape hatches, and complex query composition is where its type errors get
hard. keelson takes the opposite position on that trade: a raw `&str` is a
first-class expression *everywhere* an expression is accepted, a whole
hand-written statement (`sqlite::sql!("… WHERE age >= {min}")`) is an ordinary
query with the same verbs and the same row mapping, and the type
system is spent on the model layer (typed columns, typed setters) rather than
on proving whole statements. If you want the compiler to reject a malformed
query at all costs, use diesel.

**[SeaORM](https://www.sea-ql.org/SeaORM/)** is an actual ORM, and does the
ORM things better: `ActiveModel` change tracking, entity relations,
`sea-orm-cli` codegen, a large documented surface, an async story built on
sqlx. keelson has no ActiveModel and no entity graph. Its Layer 2 is a thin
typed shell over Layer 1 — `users::table().query((users::age().gte(21),
select::limit(20)))` is a `SELECT` you can predict from the call site, and
relation loading is two explicit strategies (a same-query `LEFT JOIN`, or one
batched `IN` per level) rather than a lazy-loading policy. If you want an ORM,
SeaORM is the mature one.

**[sea-query](https://github.com/SeaQL/sea-query)** is the closest neighbour:
a driver-agnostic dynamic query builder. It does one thing keelson refuses to:
build one AST and render it for MySQL, PostgreSQL or SQLite as needed, which
is exactly right if your product must run on whichever database the customer
brings. keelson's per-dialect crates make that impossible by construction —
your statement type *is* `keelson_psql::SelectQuery` — and buys, with that
restriction, per-dialect constructs that a common denominator cannot express
(`MERGE`, `DISTINCT ON`, `RETURNING`, `ON CONFLICT`, `INSERT OR REPLACE`,
locking clauses) plus the guarantee that what compiles is grammatical for the
engine you compiled it for.

**[cornucopia](https://github.com/cornucopia-rs/cornucopia)** (and Go's
[sqlc](https://sqlc.dev), the idea's origin) turns `.sql` files into typed Rust
functions. It is PostgreSQL-only, and within PostgreSQL it is a sharper tool
than keelson's Layer 4: it derives its types from the server's own prepared
statement description, so its nullability is the server's answer, not an
analysis. keelson's Layer 4 does the same job for PostgreSQL and SQLite from a
parse tree plus the introspected schema, with every nullability rule numbered
and written into the generated file — and adds one thing cornucopia does not
have: the same query file also compiles to a *mod*, so a hand-written `WHERE`
can be merged flat into a generated model query instead of nesting as a
sub-select.

**So: pick keelson if** you want to write SQL that looks like the SQL of your
specific database, compose it from values rather than strings, and have the
boring parts (models, factories, row mapping) generated from the schema you
already migrated — and you can live with a young library.

## The four layers

Each layer depends only on the ones below it, and each is usable on its own.

| # | what it is | crates |
|---|---|---|
| 1 | **Query builder.** Statement starters (`select`, `insert`, `update`, `delete`, `merge`) filled in by *mods* — values that modify a statement. A tuple of mods is a mod, so composition never runs out of arity. Raw SQL is an expression anywhere. | `keelson-core`, `keelson-psql`, `keelson-mysql`, `keelson-sqlite` |
| 2 | **Execution.** An object-safe `Executor` (a pool, a connection, a transaction, `&dyn Executor`), the `Execute` verbs on every query (`fetch_all`, `fetch_one`, `fetch_scalar`, `execute`), lifetime-free transactions with savepoints and per-transaction isolation levels, and row decoding that names the column that failed. | `keelson-exec` + a backend: `keelson-sqlx` |
| 3 | **Models.** A typed shell per table or view: `users::age()` is one `Column<i64>` that is the expression, the filter origin and the alias carrier at once; a three-state `Setter` distinguishes *unset* from `NULL`; hooks are trait methods; relations load by same-query preload or by chained, batched then-loads. Plus the test-data half: factories with auto-created parent chains, sequence-based uniqueness and a seedable faker. | `keelson-models`, `keelson-factory` |
| 4 | **Generation.** `keelson-gen` introspects a live schema and writes the models and the factories as `.rs` files you commit. It also compiles hand-written `.sql` files into typed modules — with each query usable *either* as a query of its own *or* as a mod merged flat into a model query. | `keelson-gen` |

The layer numbering is bob's, and so is the idea that you can stop at any of
them. Layer 1 alone is a legitimate way to use keelson.

## A worked example

Layers 1 and 2, against a real SQLite database. This block is the crate-level
documentation of the `keelson` facade crate and runs as a doctest, so it cannot
drift from the API.

```rust
use keelson::exec::{Execute as _, Executor as _, Statement};
use keelson::sqlite::{self, Chain as _, Query as _, arg, insert, quote, select};
use keelson::sqlx::sqlite::Pool;
use keelson::FromRow;

// Row mapping is by field name; `#[keelson(rename = "...")]` and
// `#[keelson(flatten)]` are there for when the names disagree.
#[derive(Debug, PartialEq, FromRow)]
struct Crew {
    id: i64,
    name: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = Pool::connect("sqlite::memory:").await?;
    db.execute(Statement::new(
        "CREATE TABLE crew (id INTEGER PRIMARY KEY, name TEXT NOT NULL, age INTEGER)",
        vec![],
    ))
    .await?;

    // A statement is a starter plus mods. A tuple of mods is itself one mod,
    // so composition never runs out of arity.
    sqlite::insert((
        insert::into("crew").columns(["id", "name", "age"]),
        insert::values((arg(1), arg("Ada"), arg(36))),
        insert::values((arg(2), arg("Kid"), arg(12))),
    ))
    .execute(&db)
    .await?;

    // Mods are values, so a filter can be decided at runtime and dropped in:
    // `Option<M>` is a mod, and `None` contributes nothing.
    let only_adults = Some(select::where_(quote("age").gte(arg(21))));

    let q = sqlite::select((
        select::columns((quote("id"), quote("name"))),
        select::from(quote("crew")),
        only_adults,
        select::where_("name IS NOT NULL"), // raw SQL, same tuple
        select::order_by(quote("id")),
    ));

    // Building is synchronous and driver-free — inspect the SQL if you like.
    let (sql, args) = q.build()?;
    assert_eq!(
        sql,
        r#"SELECT "id", "name" FROM "crew" WHERE ("age" >= ?1) AND name IS NOT NULL ORDER BY "id""#
    );
    assert_eq!(args.len(), 1);

    let crew: Vec<Crew> = q.fetch_all(&db).await?;
    assert_eq!(crew, vec![Crew { id: 1, name: "Ada".into() }]);
    Ok(())
}
```

With Layer 3 generated on top, the same query is typed and the raw fragment
still fits in the tuple:

```rust,ignore
let adults = users::table()
    .query((
        users::age().gte(21),                     // typed: `.gte("x")` will not compile
        select::where_(r#""users"."name" <> 'bob'"#), // raw SQL, same tuple
        select::order_by(users::age()).desc(),    // Layer 1 mod, same tuple
        users::then_load::posts(),                // one batched IN query per level
    ))
    .all(&db)
    .await?;
```

## Getting started

keelson is one dependency line, with the engine and the layers chosen by
feature:

```toml
[dependencies]
keelson = { version = "0.1.0", features = ["sqlx-psql", "models", "macros"] }
```

`keelson` is a facade: it re-exports the individual crates and nothing else, so
`keelson::psql` *is* `keelson_psql`. Depending on those crates directly is
equally supported and is what generated code does.

| feature | brings in | for |
|---|---|---|
| `psql`, `mysql`, `sqlite` | the dialect crate | Layer 1 |
| `exec` | `keelson-exec` | Layer 2 traits (no driver) |
| `sqlx-psql`, `sqlx-mysql`, `sqlx-sqlite` | `keelson-sqlx` + `exec` + the matching dialect | Layer 2 with a driver |
| `models` | `keelson-models` (+ `exec`) | Layer 3 |
| `factory` | `keelson-factory` (+ `models`) | test data |
| `macros` | `#[derive(Bind)]`, `#[derive(FromRow)]`, each dialect's `sql!` | row mapping, column type overrides, hand-written statements |
| `chrono`, `uuid`, `decimal`, `json` | the matching `Value` variant, wired through the backend | typed columns beyond the scalars |
| `tracing` | per-statement spans in the execution funnel | observability |

**No feature is on by default.** There is no dialect that could be the right
default and no backend that could be, and every optional dependency here is one
a build would otherwise not pay for — including the proc-macro toolchain behind
`macros`.

Generation is a separate, build-time tool:

```sh
cargo install keelson-gen
keelson-gen --config keelson.toml --url "$DATABASE_URL"
```

## Examples

[`examples/`](https://github.com/ken109/keelson-rs/tree/main/examples) holds
fourteen runnable programs, one topic each — from a first `SELECT` to
generated models, relation loading, factories, `.sql` files, and the layering
question of who owns the transaction. They need no
server (SQLite in a temporary file) and each asserts its own output, so CI
runs them:

```sh
cargo run -p keelson-examples --example builder_basics
./scripts/run-examples.sh            # all of them
```

The directory is also a worked application: a schema, a `keelson.toml`, the
committed generated code, hand-written hooks, and a test that fails when the
generated files stop matching their sources.

## Migrations are not keelson's job

**keelson is DML-only.** It generates and runs `SELECT`/`INSERT`/`UPDATE`/
`DELETE`/`MERGE`; it does not emit DDL, does not diff schemas, and does not
track migration history. keelson-gen introspects whatever database you point it
at and generates from what is there.

Use a migration tool you already trust — [`sqlx migrate`](https://github.com/launchbadge/sqlx/blob/main/sqlx-cli/README.md),
[Atlas](https://atlasgo.io), [refinery](https://github.com/rust-db/refinery),
Flyway, or your framework's — then re-run `keelson-gen`. The intended loop is:
*migrate → regenerate → compile*, where the compiler is what tells you which
call sites the schema change broke.

This is a deliberate scope decision, not a gap waiting to be filled: schema
migration is a solved problem with mature tools whose value is in their history
tracking and their team workflow, none of which a query builder improves by
owning.

## Stability

- **Version 0.1.0, the first release.** Published to crates.io; nothing has run
  in production yet.
- **The API will change.** Layer 1's shape is the most settled; Layer 3 and
  Layer 4's generated surfaces are the most likely to move. Pin exactly, read
  the diff of regenerated files.
- **What is already load-bearing:** the workspace gates are a green `cargo
  build`/`cargo test`/`cargo clippy -- -D warnings`/`cargo fmt --check`, the
  grammar judges, the construct-coverage gate, and an engine tier that runs
  every dialect's suite against containerised PostgreSQL and MySQL. CI runs all
  of them on every pull request, plus an MSRV check on the declared
  `rust-version`.
- **Unsupported is loud.** Where keelson cannot do something honestly it says
  so at build time or in a typed error — MySQL has no `RETURNING`, so a
  generated MySQL model has no `update(...).all()`; SQLite cannot run
  `READ COMMITTED`, so it refuses that isolation level rather than pretending.

## Documentation

| document | what is in it |
|---|---|
| [docs/sql-rendering.md](https://github.com/ken109/keelson-rs/blob/main/docs/sql-rendering.md) | how a statement becomes SQL: the writer, placeholders, quoting, the one-pass rule |
| [docs/execution.md](https://github.com/ken109/keelson-rs/blob/main/docs/execution.md) | the execution layer's design, one decision per question with the rejected alternative recorded |
| [docs/type-mappings.md](https://github.com/ken109/keelson-rs/blob/main/docs/type-mappings.md) | the type × dialect table every backend binds by |
| [docs/testing-tiers.md](https://github.com/ken109/keelson-rs/blob/main/docs/testing-tiers.md) | the four testing tiers and what each proves |
| [docs/views.md](https://github.com/ken109/keelson-rs/blob/main/docs/views.md) | views: declared relations, keyless models, per-engine updatability |
| [docs/publishing.md](https://github.com/ken109/keelson-rs/blob/main/docs/publishing.md) | the crate graph, the publish order, and what a release needs |

Each crate's `src/lib.rs` carries its own architecture documentation — the
decisions and the alternatives that were rejected — and that is the intended
place to read before changing one.

## License

MIT — see [LICENSE](https://github.com/ken109/keelson-rs/blob/main/LICENSE). Copyright (c) 2026 Kensuke Kubo.

Third-party attribution is in [NOTICE.md](https://github.com/ken109/keelson-rs/blob/main/NOTICE.md).
