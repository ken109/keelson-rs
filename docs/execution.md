# The execution layer

How a built statement — `Query::build()`'s `(String, Vec<Value>)` — reaches a
real database and comes back as mapped Rust values. This document is the
synthesis of two independent designs (A, written user-seat-first from
thirteen call sites; B, written maintainer-seat-first against a paper "second
backend"), one decision per question, **with the rejected alternative recorded
next to each choice**. Where the two designs agreed, that agreement is noted
as signal; where they clashed, the clash was a real tradeoff and is named.

The result ships as two crates:

- **keelson-exec** — the traits, driver-free. What Layer 2 (generated models)
  depends on.
- **keelson-sqlx** — the first backend: sqlx's PostgreSQL, MySQL and SQLite
  drivers behind per-database features.

Ground rules inherited from Layer 1 and kept throughout: async is the one
lane and keelson-core stays sync and driver-free; no lifetime parameter on
any public type (the only lifetimes are transient `'_`s on futures borrowed
from `&self` for one call, the same class as `SqlWriter<'_>`);
misuse-resistance beats flexibility wherever they collide; everything binds
by `docs/type-mappings.md`.

---

## Q1. The Executor trait

**Chosen: an object-safe three-method core trait, `&self`, boxed futures;
the ergonomic verbs live on the *query* as a blanket `Execute` trait.**

```rust
pub trait Executor: Send + Sync + fmt::Debug {
    fn family(&self) -> Family;   // Postgres | MySql | Sqlite — metadata, not dispatch
    fn fetch(&self, stmt: Statement) -> ExecFuture<'_, Result<Vec<Row>, ExecError>>;
    fn execute(&self, stmt: Statement) -> ExecFuture<'_, Result<ExecResult, ExecError>>;
}
```

Both designs agreed on the core (that agreement is the strongest signal in
either document): **object-safe, so `&dyn Executor` is the currency** every
layer trades in — application code, generated `save()`s, hooks. sqlx's
generic `Executor<'c>` with its lifetime and associated types is the
inverse choice, and "how do I write a function that takes a pool *or* a
transaction" being sqlx's most-asked question is why it is not copied. The
cost is one boxed future per call — noise against a network round-trip.

**`&self`, not `&mut self`** (both designs, independently, with different
arguments — kept both): exclusivity is enforced *inside* the executor (a
pool checks out per call; a transaction serialises behind an async mutex),
because with `&mut` every hook signature and generated call site inherits
reborrow plumbing forever, and because the natural second backend
(`tokio_postgres::Client`, an actor-thread rusqlite) is `&self`-shaped
anyway. Given up: the compiler no longer proves "no concurrent statements on
one connection" — the backend absorbs the mistake instead of representing it.

**Where the verbs live — the one real clash between the designs.** A hangs
them on the query (`q.fetch_all(&db)`); B hangs them on the executor
(`db.fetch_all(&q)`). Chosen: **A, the query side** — it matches the sqlx
muscle memory users arrive with, and it keeps exactly one name (`Executor`)
on the executor side instead of a trait pair. B's argument for its shape was
that the extension trait is where tracing lives so no backend can forget it;
that argument survives the move — the `Execute` verbs funnel through one
`run_fetch`/`run_execute` pair in keelson-exec, which is where the spans are
(§Q5). Rejected: B's `ExecutorExt`, as a second user-facing vocabulary for
the same calls.

**Query vs execute split**: two methods, sqlx's split, because users know it.
Both are always available on every statement — `DELETE … RETURNING` is
fetched, an unread `SELECT` may be executed. `QueryType` feeds tracing only.
Rejected (both designs, same reason): a typestate split (`SelectQuery` gets
fetch, `UpdateQuery` gets execute) — `RETURNING` makes that partition a lie
in both directions. `fetch_one`/`fetch_optional` error with `TooManyRows` on
a second row where sqlx silently drops it: "one" means one.

**Streaming — the second clash.** A made `fetch` return a `RowStream` and
built `fetch_all` as a collect over it, which forces every backend *and every
transaction* to support an owned stream (A's own design needed an
`OwnedMutexGuard` riding inside the stream to make that true — its most
fragile part). Chosen: **B — `fetch` returns `Vec<Row>`** (the shape every
backend can provide and the shape Layer 2's `find_all` wants), and streaming
is a separate **opt-in `StreamExecutor` trait** returning an owned
`RowStream` (a bounded channel fed by a producer the backend spawns; dropping
the stream cancels the producer and releases its connection). keelson-sqlx
implements it for its pools. A concrete struct, not `impl Stream`, so the
futures crate stays out of the public API; a `Stream` impl is additive later.

**Dialect/backend mismatch — the third clash.** A wanted a pre-flight check
(`Dialect::kind()` added to core, compared against `Executor::family()`).
Chosen: **B — no runtime check and no core change.** Core's surface is
frozen ("keep the no-new-features build byte-identical" is a stated gate),
`Dialect` is a behaviour rather than an identity, the server rejects a
mismatched placeholder syntax at prepare time with a clear error, and Layer 2
will prevent the mismatch statically by tying generated queries to a family.
`family()` exists for observability and the test harness, not gatekeeping.

**Prepared-statement caching**: backend-private, keyed on SQL text, invisible
in the trait (both designs). The documented contract is only that the SQL
text is a valid cache key — which keelson can promise because building is
deterministic (Tier C). sqlx caches per-connection automatically; a
tokio-postgres backend would carry its own LRU. No `persistent(bool)` knob in
v1; `Statement` is `#[non_exhaustive]` so one can be added without breakage.

**How `(String, Vec<Value>)` reaches driver bind types**: each backend owns
one total function `Value → driver parameter`, written against the "binds
as" column of the mappings table — native parameter where the driver has
one, pinned text form as the fallback. `Value::Custom` is downcast for the
customs a backend knows; an unknown custom is
`ExecError::UnsupportedValue { type_name, family }` at bind time — loud,
never silently `to_plain()`d (both designs, verbatim agreement). Two
implementation facts discovered on the way, now part of the backend
contract:

- **Untyped NULL on PostgreSQL** binds with the `unknown` (OID 705)
  parameter type so the server infers from context — a typed null (text,
  say) is refused where an `int` is expected. (`keelson_sqlx::psql`'s
  `UnknownNull`.)
- **Zero-argument side-effect statements run over the driver's plain
  (unprepared) path** — MySQL refuses `BEGIN`/`SAVEPOINT` in the
  prepared-statement protocol, and nothing is gained by preparing an
  argument-less statement.

## Q2. Transactions

**Chosen: one owned, type-erased `Transaction` in keelson-exec, implementing
`Executor`; commit/rollback consume `self`; savepoints are closures; the
transaction SQL is written once in keelson-exec.**

`Transaction: Executor` is the whole point and both designs said so in the
same words: every query-taking function, every generated `save`, every hook
accepts pool and transaction alike as `&dyn Executor`.

**Who issues the transaction SQL — a quiet clash, resolved toward A.** B
gave each backend a `TxBackend` with its own commit/rollback; A had
keelson-exec own the vocabulary (`BEGIN`, `COMMIT`, `ROLLBACK`,
`SAVEPOINT keelson_sp_n`, `RELEASE …`, `ROLLBACK TO …`) over a minimal
`RawConnection` seam. A wins: the vocabulary is identical across PostgreSQL,
MySQL and SQLite, so writing it once means transaction semantics *cannot*
drift per backend — B's own "second backend" audit even observed that no
driver's native transaction type is needed, which is the argument for
hoisting the SQL out of backends entirely. The seam a backend implements is:

```rust
pub trait RawConnection: Send + fmt::Debug {
    fn family(&self) -> Family;
    fn fetch<'a>(&'a mut self, sql: &'a str, args: Vec<Value>) -> ExecFuture<'a, …>;
    fn execute<'a>(&'a mut self, sql: &'a str, args: Vec<Value>) -> ExecFuture<'a, …>;
    fn abandon(self: Box<Self>);   // dispose; must NOT return to a pool as reusable
}
```

**Misuse made unrepresentable**: `commit(self)`/`rollback(self)` consume, so
use-after-finish is a compile error (both designs; sqlx's shape, kept).

**Savepoints — the fourth clash, and the misuse-resistance rule decides
it.** A spelled nesting `tx.begin() → Transaction`, with a runtime
`SavepointOpen` error for "parent finished while child alive" — a compile
guarantee traded away to stay lifetime-free. B spelled it
`tx.savepoint(async |sp| …)`: the savepoint has no handle to leak, `Ok` ⇒
`RELEASE`, `Err` ⇒ `ROLLBACK TO`, and the ordering misuse A had to detect at
runtime is *inexpressible*. Chosen: **B's closure.** Given up: holding two
sibling savepoints open and interleaving them — which no supported engine
offers on one connection anyway. The closure receives `&Transaction` (the
same one, thanks to `&self` executors), so `&dyn Executor`-taking helpers
work unchanged at any depth. `Begin` is deliberately *not* implemented by
`Transaction`: "did I open a transaction or a savepoint?" cannot be confused
at a call site.

**Drop without commit** (designs agreed): the connection is **abandoned** —
detached from the pool and closed, never returned as reusable — and the
server discards the open transaction. No async runtime is assumed in `Drop`.
The lazy path is safe and merely expensive, which is the right way round, and
is exactly why the API pushes toward the closure form:

**`within` — the closure transaction** (both designs): `db.within(async |tx|
{ … })` on any `Begin` (pools, connections), commit on `Ok`, rollback on
`Err`. The closure receives `&Transaction` and so cannot commit or consume
it; neither a dropped transaction nor a forgotten commit is expressible.
Limitation, recorded: the future `within` returns is not provably `Send`
(naming `AsyncFnOnce::CallOnceFuture` to bound it is unstable); explicit
`begin`/`commit` covers the multi-threaded-executor case that needs `Send`.

**What Layer 2 hooks require** (asked explicitly; both designs converged):
(a) `Transaction: Executor` — satisfied; (b) `&self` methods so one
transaction can be lent to N hooks in sequence — satisfied; (c) hooks
receive `&dyn Executor`, not `&Transaction`, so a hook cannot end the
caller's transaction — misuse-resistance by type erasure. Core's
`QueryExtensions` payloads are pinned in keelson-exec as:

```rust
pub type ExecHook   = Arc<dyn for<'a> Fn(&'a dyn Executor) -> ExecFuture<'a, Result<(), ExecError>> + Send + Sync>;
pub type ExecLoader = Arc<dyn for<'a> Fn(&'a dyn Executor, &'a [Row]) -> ExecFuture<'a, Result<(), ExecError>> + Send + Sync>;
```

`MapperMod` is deferred to Layer 2, which owns the row-mapper it would
modify.

## Q2b. Isolation levels and access modes

**Chosen: an opt-in `BeginWith` trait carrying a `TxOptions` value, whose
per-engine rendering lives in keelson-exec — and which *refuses* any option
the engine would only appear to honour.**

This is the first place the three engines stop agreeing, so the deliverable is
handling the disagreement honestly rather than presenting a uniform API that
quietly means different things per backend.

**The rule, one sentence: a level is accepted only when the engine runs the
transaction at that level.** No substitution, no downgrade, no no-op. The SQL
standard permits an implementation to run *stricter* than asked, so
substituting would be conformant — and would still be a lie to a caller who
asked for a level in order to get particular behaviour. Rejected alternative:
"accept everything everywhere, document the differences", which is what most
toolkits do and what makes `READ UNCOMMITTED` on PostgreSQL a silent no-op.

| | PostgreSQL | MySQL / InnoDB | SQLite |
|---|---|---|---|
| READ UNCOMMITTED | **refused** — accepted by the server, run as READ COMMITTED | yes (a real dirty read) | **refused** |
| READ COMMITTED | yes (default) | yes | **refused** |
| REPEATABLE READ | yes | yes (default) | **refused** |
| SERIALIZABLE | yes | yes | yes — SQLite's only level |
| `READ ONLY` | yes | yes | **refused** |
| `SqliteBegin::{Deferred,Immediate,Exclusive}` | **refused** | **refused** | yes |

Same name, different semantics, recorded rather than smoothed over:
PostgreSQL's REPEATABLE READ raises a serialization failure when a transaction
writes a row that changed since its snapshot; InnoDB's coexists with locking
reads and an `UPDATE` that takes the *current* row. Nothing in keelson tries
to reconcile those — `Isolation` is a request, and each engine's meaning is
documented on the variant.

**SQLite — the decision that had to be made, and why.** SQLite has no
per-transaction isolation levels in the standard sense; it has begin modes and
a connection-level `read_uncommitted` pragma. Three options were on the table:
map the standard levels onto begin modes, reject them, or expose the begin
modes as their own vocabulary. **Chosen: reject the standard levels, and
expose the begin modes as their own type (`SqliteBegin`), except
`Serializable`, which is accepted because it is literally what SQLite runs.**
Mapping was rejected because the two axes are not the same axis — a begin mode
says *when locks are taken*, not *which anomalies are permitted*, so
`ReadCommitted → BEGIN DEFERRED` would be a coincidence dressed as a
translation. Blanket rejection including `Serializable` was rejected too: it
would force every portable call site to branch on `Family` to ask for a level
SQLite genuinely provides. `SqliteBegin` is named for its engine precisely so
that no reader mistakes it for a portable knob; asking for it on PostgreSQL or
MySQL is an error, not an ignored field. The `read_uncommitted` pragma is not
exposed at all: it is connection-level, it only does anything in shared-cache
mode, and keelson does not set connection state behind a pooled connection's
back (the same rule that keeps `PRAGMA query_only` out — which is why SQLite
refuses `READ ONLY` rather than emulating it).

**Read-only is in the same entry point**, because on the two engines that have
it, it is spelled in the same place the level is (`BEGIN … READ ONLY`,
`START TRANSACTION READ ONLY`) — a second entry point would have been a second
way to say one thing. `Access::ReadWrite` is accepted everywhere it is the
default, so stating the default explicitly is allowed.

**Where the SQL is composed: keelson-exec, per family, in `TxOptions::plan`**
— the same argument as §Q2's "the vocabulary is written once so semantics
cannot drift per backend", except that here the vocabulary is genuinely three
vocabularies, so what is written once is the *decision table*. A backend's
`begin_with` is three lines: `opts.check(family)?`, take a connection, hand it
to `Transaction::begin_on_with`. `plan` is public, so "what does keelson
actually send?" is answerable without a packet capture:

- PostgreSQL — one statement: `BEGIN [ISOLATION LEVEL …] [READ ONLY|READ WRITE]`.
- MySQL — two: `SET TRANSACTION ISOLATION LEVEL …` then `START TRANSACTION …`.
  InnoDB cannot carry a level on `START TRANSACTION` and refuses
  `SET TRANSACTION` once a transaction is open. Unqualified — no `SESSION`, no
  `GLOBAL` — the `SET` scopes to the *next* transaction on that connection,
  which is the scope wanted: it expires with the transaction instead of riding
  the connection back into the pool.
- SQLite — one: `BEGIN [DEFERRED|IMMEDIATE|EXCLUSIVE]`.

With no options set, all three plans are exactly `BEGIN` — `begin_with` with
defaults is byte-identical to `begin`.

**A new trait, not a new method on `Begin`** — §Q7's semver rule
("`Executor`, `Begin`, `RawConnection`, `StreamExecutor` never grow methods;
new capabilities arrive as new opt-in traits") applied to its first real
customer. `BeginWith: Begin` follows the `StreamExecutor` template, and
`BeginWithExt::within_with` mirrors `within`.

**Two failure modes, handled where they are invisible.** (1) An option this
engine cannot honour is refused *before* a connection is taken out of the
pool, so a rejected configuration disturbs nothing. (2) If a plan statement
fails halfway — MySQL's `SET` landing without its `START TRANSACTION` — the
connection is **abandoned**, not returned: it carries a pending
next-transaction characteristic nobody can see, and reusing it would apply a
level to a transaction that never asked for one.

**Serialization failures are a matchable value, not a message.**
`TxConflict::{Serialization, Deadlock, LockTimeout, Busy}` with
`TxConflict::of(&err) -> Option<TxConflict>`; a backend that recognises a
conflict reports it as a `TxConflictError` carrying the engine's own code and
the driver error as its `source`. keelson-sqlx classifies by code, never by
message text: PostgreSQL SQLSTATE `40001`/`40P01`/`55P03`, MySQL error numbers
`1213`/`1205` (SQLSTATE is only a category there), SQLite's `SQLITE_BUSY`/
`SQLITE_LOCKED` primary result codes. Every variant means the same thing to a
caller — retry the transaction from the top. Rejected: a new `ExecError`
variant, which is where this belongs long-term; the reopening condition is a
consumer that wants to `match` rather than call `TxConflict::of`, at which
point the variant lands in `ExecError` (it is `#[non_exhaustive]` for exactly
this) and `of` becomes its accessor.

**Tested as behaviour, not as strings.** The statement text is pinned by unit
tests in keelson-exec (derived from each engine's `BEGIN`/`SET TRANSACTION`
grammar, never from builder output); what the levels *do* is proved in
`keelson-sqlx/tests/transactions.rs` against real engines, always with two
concurrent transactions:

- PostgreSQL: REPEATABLE READ keeps its snapshot while a concurrently-opened
  default-level transaction on another pooled connection sees the commit —
  which is simultaneously the proof that the level landed on the transaction's
  own connection and nowhere else; a write-write conflict surfacing as
  `TxConflict::Serialization`; a `READ ONLY` transaction refusing a write.
- MySQL: the same per-connection proof, plus six subsequent transactions on
  the returned connection all still at the default (a `SET SESSION` would fail
  every round); READ COMMITTED showing a non-repeatable read and REPEATABLE
  READ not; a genuine dirty read under READ UNCOMMITTED (which is what makes
  refusing PostgreSQL's a considered decision rather than a blanket one); a
  deadlock surfacing as `TxConflict::Deadlock`; `READ ONLY` refusing a write.
- SQLite: a plain transaction *cannot* be made to show a non-repeatable read
  while a concurrent writer commits — the behavioural half of refusing READ
  COMMITTED — and `BEGIN IMMEDIATE` taking the write lock at begin time, so a
  second one loses immediately with `TxConflict::Busy` where two `DEFERRED`
  transactions coexist.

## Q3. Row mapping

**Chosen (designs agreed on every load-bearing point): rows decode once, at
the driver seam, into keelson's own `Row` — `Arc<[Column]>` header shared
per result set, one `Value` per cell.** The deliberate deviation from sqlx
(which decodes driver bytes directly into user types, per database): one
`FromRow` impl per type is correct on every backend, because `FromValue`'s
documented text acceptance absorbs MySQL handing a datetime back as text and
SQLite having no date type *below* `FromRow`; the impl is driver-free (what
lets the derive/codegen live beside keelson-exec, not beside any driver);
and `Row` is owned, cloneable, testable without a database. The copy cost is
real but bounded — one `Value` per cell, header shared — and for the hot
path that truly cannot afford it, `Pool::inner()` exposes the sqlx pool:
keelson is a layer, not a jail. (A per-backend `FastFromRow<B>` opt-in
remains possible later; rejected for v1 because it multiplies the mapping
surface by the backend count.)

**By name for structs, by position for tuples** (agreed): name-based
survives column reordering and JOIN-prefixed projections and is what
generated models want; tuples `(A, B, …)` map positionally for ad-hoc reads.
Duplicate names resolve to the first (documented, matching sqlx); reach the
rest by position. From B: `from_row(&mut Row)` with `take`/`take_at`, so
`String`/`Vec<u8>`/JSON *move* out of the row rather than clone; `get`
variants exist for non-consuming reads.

**Errors name the column, always** (agreed; the requirement). `FromValue`
cannot know a column name, so the `Row` boundary weaves it in:

- NULL in a non-`Option` field → `column "email": cannot read NULL as String`
- type mismatch → `column "name": cannot read text as i64`
- missing column → `no column "emial" in result set (columns: id, name,
  email)` — listing what *was* there, because a typo is the bug nine times
  out of ten.

`ExecError::Decode { column, source }` carries the column structurally, not
just in the message.

**How generated models will derive it**: the derive is deferred with the
macro crate (§Q4); the documented hand-written pattern — one
`row.take("col")?` per field — is byte-for-byte the shape codegen will emit,
and is what the test suites use, so the product path stays exercised.

## Q4. The binding trait the codegen override hangs on

**Chosen: reuse core's `ToValue`/`FromValue` as the two halves, with a
blanket umbrella in keelson-exec** (design A):

```rust
pub trait Bind: ToValue + FromValue + Send + 'static {}   // blanket impl
pub const fn assert_bind<T: Bind>() {}
```

Codegen emits, per overridden column type, `const _: () =
keelson_exec::assert_bind::<UserId>();` — plus generated code that *uses*
both halves — so a non-binding override fails to compile in one line naming
the type, not in an inference swamp. Because the contract is defined against
`Value` rather than any driver, an override binds on every backend or on
none — backend-count-invariant, which was B's stated goal too.

**Rejected: B's separate `IntoArg`/`FromColumn` pair.** B's three arguments,
answered: (1) *ownership direction* — `ToValue` consumes where codegen binds
from `&self`; but the generated `self.col.clone().to_value()` costs exactly
what `IntoArg::into_arg(&self)`'s internal clone costs, so the borrowing
trait buys nothing but a third name for the same concept. (2) *contract
width* — core's pair is expression-shaped and loose (`()` binds, `&str`
binds); true, and accepted: the loose end sits on `ToValue` impls for
borrow-types, none of which a codegen config would name as a column type.
(3) *semver isolation* — a real point, recorded as the reopening condition:
if the override contract ever needs to grow methods (a `sql_type_hint()`),
mint the narrower pair then, in keelson-exec, without touching core.

**Derivable for newtypes — without a proc-macro crate.** The task allowed
weighing the macro crate; the decision is to **not ship one yet**. A
declarative macro covers the newtype case completely:

```rust
pub struct UserId(pub i64);
keelson_exec::bind_newtype!(UserId(i64));   // ToValue + FromValue by delegation
```

Anything richer (validated strings, enums-as-text) is six hand-written lines
where the domain rule belongs. A `#[derive(FromRow, ToValue, FromValue)]`
proc-macro crate becomes worth its build cost when Layer 2's generator
arrives and emits derives at scale; the macro names are reserved for it.
Cost of the choice: `bind_newtype!` handles single-field tuple structs only
— refusing multi-field types is honest (they have no single column shape).

## Q5. Observability

**Chosen: per-statement spans in the shared verb funnel (`run_fetch`/
`run_execute` in keelson-exec) — the one code path every backend flows
through — so no backend can ship uninstrumented and no two backends can
drift** (both designs put spans in the shared layer; they differed only on
which side of the API that layer sits, resolved in §Q1). Callers of raw
`Executor::fetch` on a `dyn` object bypass sugar and spans together; that is
the escape hatch and is documented as such.

Span `keelson.query`, OTel-database-semconv field names so existing
dashboards light up: `db.system`, `db.query.text` (**full SQL, untruncated**
— it is parameterized text, safe by construction: placeholders, never
values; the sole path user data can take into SQL text is `expr::literal`,
and a truncated query is the one you cannot paste into `EXPLAIN`),
`keelson.query_type`, `keelson.args.count` (the **count**), and on close
`keelson.rows` / `keelson.rows_affected` or `error`.

**Args are never recorded — at any level, on any field, ever.** They are the
PII channel. Pinned by test (`keelson-exec/tests/tracing.rs`): a capturing
subscriber runs a query with a distinctive text argument and asserts its
bytes appear nowhere in the telemetry. That test *is* the policy.

**Transaction lifecycle: events, not a spanning span — a deliberate
deviation from both designs.** Both sketched a `keelson.transaction` span
enclosing statement spans; entering a span across `.await`s from behind a
`dyn` boundary is exactly the misuse the tracing documentation warns about,
and explicit parenting would thread span handles through the `Executor`
trait itself. v1 emits `tracing::debug!` events at begin/commit/rollback/
abandon with the outcome; a linking span is future work if real tracing
consumers ask for it.

**Feature-gated, default off** (A; B wanted default-on). keelson-exec's
dependency floor with the feature off is core + tokio/sync, zero tracing
code compiled — the same "the no-features build is what it was" discipline
core's type features follow. No metrics, no logs: subscribers derive both.

## Q6. Pooling

**The Executor abstraction assumes nothing about pooling** — verbatim
agreement between the designs, kept verbatim. No `PoolExecutor`, no
`acquire()` in the vocabulary, no pool configuration surface: pool tuning is
where pool libraries genuinely differ, and an abstraction over them would be
a lowest common denominator or a re-implementation. The one weak promise
`Executor` makes: *each call runs on some connection*; only a connection or
a `Transaction` strengthens that to *the same connection*. Sharp edge,
documented on the trait: session state (`SET`, temp tables, advisory locks)
through a bare pool is a bug — take a transaction.

The one piece of session state keelson itself owns — MySQL
`time_zone = '+00:00'` (the type-mappings requirement) — is pinned by
`keelson_sqlx::mysql::Pool::connect` in the pool's `after_connect` hook, on
**every** connection, so no user-visible surface depends on remembering it.
(`from_pool` documents that the caller inherits the duty; the round-trip
suite tests the pin rather than trusting it.)

**How non-sqlx backends slot in**: implement `RawConnection` per connection
and `Executor + Begin` on the pool handle (wrap deadpool/bb8 privately;
`begin` moves an owned checkout into the `RawConnection`), optionally
`StreamExecutor`. That paragraph nowhere mentions sqlx — which is the "not a
port" receipt.

## Q7. Crate layout

```
keelson-core        unchanged (frozen surface; zero async vocabulary)
keelson-{psql,mysql,sqlite}   unchanged
keelson-exec   NEW  Executor/Execute, Statement, ExecResult, Family, Row/Column/
                    FromRow, Bind/assert_bind/bind_newtype!, Transaction/
                    RawConnection/Begin/BeginExt, ExecHook/ExecLoader,
                    StreamExecutor/RowStream, ExecError.
                    Deps: keelson-core + tokio(sync). Optional: tracing.
keelson-sqlx   NEW  one crate, per-db features psql/mysql/sqlite mapping onto
                    sqlx's own; type features chrono/uuid/decimal/json in
                    lockstep with core's. Deps: keelson-exec, sqlx,
                    tokio(rt,sync), futures-util.
keelson             DEFERRED facade (re-exports behind matching features);
                    not needed to ship the execution layer.
keelson-macros      DEFERRED until Layer 2's generator (§Q4).
```

**The traits live in keelson-exec, pointedly not keelson-core** — verbatim
agreement. Core's opening doc stakes "building is entirely synchronous and
driver-independent"; its 1052 tests mean something because its scope is
closed, and welding the executor on would couple the most-depended crate's
release cadence to the churniest layer. Layer 2's generated models depend on
`keelson-core + keelson-exec` — trait-complete, driver-free — and the
application's binary picks the backend crate; swapping backends is a
Cargo.toml + constructor-site change, which is the measurable meaning of
"driver-agnostic".

**Per-database sqlx drivers; `sqlx::Any` rejected** — verbatim agreement:
`Any` erases exactly what the mappings table requires kept (native
`uuid`/temporal/decimal binds). One keelson-sqlx crate with per-db features
(B) rather than three crates (considered in A): the bind/decode functions
share a skeleton, users' feature lists line up with sqlx's own, and the
release train grows one car, not three.

**Semver rules, written down** (B): `Executor`, `Begin`, `RawConnection`,
`StreamExecutor` never grow methods — new capabilities arrive as new opt-in
traits (`StreamExecutor` is the template). `Statement`, `ExecResult`,
`ExecError`, `Family` are `#[non_exhaustive]` from day one.

**A workspace consequence, recorded honestly**: `links = "sqlite3"` permits
one `libsqlite3-sys` in the graph; sqlx 0.8's SQLite driver requires ^0.30,
so the workspace's rusqlite (sqlcheck's live SQLite judge, dev-only) is held
at 0.32 — the release sharing that requirement. The judge uses only
`open_in_memory`/`execute_batch`/`prepare`, identical across the versions.

## Q8. Round-trip testing

**Where the tests live: `crates/keelson-sqlx/tests/roundtrip.rs` (plus
`transactions.rs`, `query_path.rs`), reusing keelson-sqlcheck's containers**
— agreement, kept: sqlcheck's `live` module grew two accessors (`psql_url()`,
`mysql_url()`) exposing the running container/`KEELSON_LIVE_*_URL` server,
schema-lock and atexit cleanup included; the execution suite adds its own
`CREATE TABLE IF NOT EXISTS keelson_roundtrip` and nothing else to the
infrastructure. SQLite runs unconditionally (in-process), so plain
`cargo test` exercises the whole harness; the server engines run under
`cargo test -p keelson-sqlx --features live-docker`.

**One deviation from B, with its reopening condition**: B wanted the case
corpus shipped in keelson-exec behind a `testing` feature so a second
backend's suite is twenty lines. Deferred — a shared harness designed before
its second consumer exists is speculation; the suite here is data-driven
(one generic `store`/`rt` pair over `&dyn Executor` + per-family DDL), so
promoting it into `keelson_exec::testing` when keelson-tokio-postgres
appears is mechanical. That moment is the reopening condition.

**The matrix**, per family × mapped type, all through placeholders (the
binding-only contract end to end), compared by each type's own semantic
equality:

- integers at `i64::MIN/MAX` (and the full signed/unsigned ladder where the
  family types it natively — `u64::MAX` round-trips on MySQL, is *refused
  loudly* where only signed 64-bit exists, and the refusal is the
  assertion); floats at exactly-representable values
- text: empty, non-BMP unicode, 32 KiB; bytes: empty, embedded NUL
- dates at epoch/leap-day/9999-12-31; times at midnight and 3/6-digit
  fractions; naive datetimes with fractions **plus read-back of the
  space-separated form** SQLite/MySQL conventionally store
- `TimestampTz`: bind `DateTime<FixedOffset>` (+09:00) → same instant in UTC
- `Uuid`: nil/max/fixed; MySQL `BINARY(16)` read-back (the 16-raw-bytes
  acceptance)
- `Decimal`: numeric equality, negatives, 28 digits, and **scale asserted on
  text** — SQLite stores `1.10` as the literal text, PostgreSQL
  unconstrained `numeric` renders `1.10` back via `CAST(… AS text)`
- JSON: object/array/nested-unicode/the `"null"` document
- NULL through every mapped type as `Option<T>` = `None` → `None` (on
  PostgreSQL this is also the test of the untyped-NULL bind)
- arrays (psql only): `int8[]`/`text[]` with NULL elements; empty array
  (binds as `text[]` — the documented inference limit)

**Contract tests, the doc's sharpest claims**: (1) MySQL
`SELECT @@session.time_zone` answers `+00:00` on pooled connections and a
zoned bind stores the instant — the pin is *tested*, not trusted. (A's
stronger variant — flip the server's global zone and re-read — was rejected:
mutating server-global state violates the shared-`KEELSON_LIVE_*_URL`
contract.) (2) Text-form fallback: the pinned text forms, inserted as text
(through an explicit cast on strictly-typed PostgreSQL; by string coercion
on MySQL; natively on SQLite), read back equal to the native binds. (3)
Cross-backend agreement, literally: one canonical value per mapped type
round-tripped on all three engines and asserted equal everywhere.

**Plus transaction semantics** (`transactions.rs`): one suite written
against `&dyn Begin` alone — commit persists, drop-without-commit rolls
back, explicit rollback discards, savepoints nest and partially roll back,
`within` commits on `Ok` and rolls back on `Err` — run against all three
engines, **plus** the deliberately non-generic isolation-level tests (§Q2b),
which are per engine because that is where the engines stop agreeing. And the **full-path suite** (`query_path.rs`): dialect-built
queries through the `Execute` verbs, `FromRow` structs, `RETURNING` as a
fetch, column-named decode errors end to end, and the streaming path
including mid-stream cancellation.

**Gate**: the SQLite lane runs in the ordinary workspace test run; the
engine lane runs wherever sqlcheck's live-docker lane runs. This suite
covers *values* (did the value survive the trip); Tier D keeps covering *SQL
constructs* — the two gates stay separate on purpose.

---

## Deviations ledger (the "not a port" receipts)

| from | kept | deviated |
|---|---|---|
| sqlx | `fetch_one/optional/all` verb names; consuming commit; per-connection statement cache; per-db drivers | object-safe `&self` `Executor` (no `'c`, no `&mut`); rows decode via `Value`, one `FromRow` for all backends; `fetch_one`/`fetch_optional` reject extra rows; keelson-exec speaks its own BEGIN/SAVEPOINT over `RawConnection` instead of driver tx machinery; isolation levels an engine would only appear to honour are refused rather than passed through (§Q2b); `Any` driver rejected; streaming opt-in rather than the core contract |
| SeaORM | shared-ref executor a transaction also implements; closure transaction (`within`, and closure savepoints) | no runtime backend enum — backends are crates, `Family` is metadata; no ActiveValue machinery at this layer |
| bob (Go) | hooks resolve through the query's own extension points (`QueryExtensions`), run on the caller's executor | hook payloads are typed (`ExecHook`), not runtime type-asserts |

## What was explicitly deferred, and what reopens each

- **`keelson` facade crate** — reopen when Layer 2 ships and the one-import
  experience matters.
- **`keelson-macros` (derives)** — reopen with the code generator; until
  then `bind_newtype!` + the documented `FromRow` pattern are the product
  path.
- **Corpus in `keelson_exec::testing`** — reopen with the second backend.
- **`persistent(bool)`, typed-stream sugar (`fetch_stream::<T>` on
  `Execute`), `Stream` impl for `RowStream`, transaction-spanning trace
  span** — all additive; each waits for a concrete consumer.
- **A typed `ExecError` variant for refused transaction options and for
  conflicts** — refusals are `ExecError::Other` with a described message and
  conflicts are `TxConflict::of`; reopen when a consumer needs to `match`
  (§Q2b).
