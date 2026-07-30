# Standard type mappings

How the common Rust application types — chrono's temporal types, `uuid::Uuid`,
`rust_decimal::Decimal`, `serde_json::Value` — map onto `keelson_core::Value`,
onto each dialect's column types, and onto the serialised argument list. This
file is the single table; the execution layer binds by it and the code
generator picks column types by it. A cell that says "no mapping" is a
decision, not an omission.

The table is a **default, not a cage**: the code generator will accept
per-column-type and per-column overrides in its configuration (map `numeric`
to your own wrapper type, or `users.metadata` to a domain type), the way
bob's type replacements do. What an override must satisfy is a trait bound in
the generated code, so a replacement type that cannot actually bind on a
backend is a compile error rather than a runtime surprise. The override
machinery lives with the generator; what this file fixes is only what you get
when you say nothing.

## Where the types live: feature-gated variants in keelson-core

The three candidate homes, and why the first won:

- **Feature-gated variants in core's `Value`** (chosen — the sqlx model).
  `Value` is an enum, and that is decisive: variants cannot be added from
  outside the crate that owns the enum, so any external home degrades to
  wrapper types. keelson-core is also the crate everything else already
  depends on, so a feature enabled anywhere unifies cleanly across the
  workspace instead of introducing a new node in the dependency graph.
- **A separate `keelson-types` crate.** Could only offer `CustomValue`
  wrappers around the same four crates — the downcasting these mappings exist
  to remove — while adding a crate boundary between `Value` and its own
  variants. Rejected.
- **Backend-only conversions.** Keeps core clean, but the query *builder* is
  where `arg(uuid)` is written; if the conversion lives in the backend, that
  line does not compile and every call site wraps by hand. The ergonomic
  point of first-class types is lost. Rejected.

The features are `chrono`, `uuid`, `decimal` (crate `rust_decimal`) and
`json` (crate `serde_json`). None is a default feature, and each gates its
optional dependency with default features off, so the no-features build of
keelson-core is exactly what it was before these types existed.

## Contract common to every mapped type

- **Binding only — no inline literal rendering.** A `Value` reaches SQL text
  in exactly one way: `Expr::Arg` renders the dialect's placeholder and the
  value goes into the argument list. That was already true of every existing
  variant — keelson has no code path that inlines a bound value, and
  `expr::literal` is a separate, text-only, nothing-escaped path for SQL the
  program wrote. The mapped types inherit this: there is deliberately no
  "render a `Uuid` as `'…'::uuid`" facility, because a literal syntax per
  type per dialect is a quoting/escaping surface with no benefit over a
  placeholder.
- **Serialisation is the pinned text form below.** `Value` serialises as a
  bare scalar (never a tagged enum) so a bound argument list compares as a
  plain JSON array. Each mapped type serialises as its standard text form —
  the same form the execution layer may use as its wire fallback — and the
  exact strings are pinned by test in `value.rs` and
  `tests/type_mappings.rs`.
- **Equality is the type's own semantic equality** (`NaiveDate == NaiveDate`,
  numeric for `Decimal`, structural for JSON), consistent with the existing
  per-variant equality and with `_ => false` across variants.
- **Reading back accepts the variant or its text form.** `FromValue` for each
  type accepts the matching `Value` variant, or `Value::Text` in the type's
  standard text form — because SQLite has no native storage class for most of
  these and MySQL drivers routinely hand them back as text. Details per type
  below.

## The table

Column types are what the code generator emits and what the execution layer
assumes when it prepares a statement. "Binds as" is the wire representation
the execution layer must use per backend; where a driver offers a native
parameter type, that is preferred, with the pinned text form as the fallback.

### `chrono::NaiveDate` → `Value::Date`

| dialect | column type | binds as | notes |
|---|---|---|---|
| PostgreSQL | `date` | native `DATE` param, or text `2026-07-30` | |
| MySQL | `DATE` | native `DATE` param, or text `2026-07-30` | |
| SQLite | `TEXT` | text `2026-07-30` | SQLite has no date storage class; ISO 8601 text is what its date functions consume and what sorts correctly |

Serialises as `"2026-07-30"` (ISO 8601 calendar date).

### `chrono::NaiveTime` → `Value::Time`

| dialect | column type | binds as | notes |
|---|---|---|---|
| PostgreSQL | `time` (without time zone) | native `TIME` param, or text `12:34:56[.fff]` | |
| MySQL | `TIME` | native `TIME` param, or text | MySQL `TIME` also holds durations up to ±838h; keelson maps only the wall-clock range, a duration type is out of scope |
| SQLite | `TEXT` | text `12:34:56[.fff]` | |

Serialises as `"12:34:56"`, with fractional seconds only when non-zero, in
3/6/9-digit groups (`"12:34:56.789"`).

**No mapping** to PostgreSQL `timetz`: a time-of-day with an offset but no
date does not name an instant, PostgreSQL's own documentation discourages the
type, and chrono has no matching type to map from.

### `chrono::NaiveDateTime` → `Value::DateTime`

| dialect | column type | binds as | notes |
|---|---|---|---|
| PostgreSQL | `timestamp` (without time zone) | native param, or text `2026-07-30T12:34:56[.fff]` | |
| MySQL | `DATETIME` | native param, or text with a space (`2026-07-30 12:34:56`) | MySQL's literal grammar wants the space form; drivers accept it universally |
| SQLite | `TEXT` | text `2026-07-30T12:34:56[.fff]` | SQLite's date functions accept both `T` and space |

Serialises as `"2026-07-30T12:34:56"` (ISO 8601, `T` separator), fractional
seconds only when non-zero. `FromValue` additionally accepts the
space-separated form, since that is what SQLite and MySQL conventionally
store.

A naive datetime is **not** interchangeable with a zoned one: `Value` keeps
them as distinct variants and no implicit conversion exists in either
direction, because attaching or stripping a zone is a semantic claim only the
application can make.

### `chrono::DateTime<Utc>` (and any `DateTime<Tz>`) → `Value::TimestampTz`

| dialect | column type | binds as | notes |
|---|---|---|---|
| PostgreSQL | `timestamptz` | native param, or text `2026-07-30T12:34:56Z` | PostgreSQL stores UTC and renders in the session zone — the offset in a bound value is consumed, never stored |
| MySQL | `TIMESTAMP` | native param, or text `2026-07-30 12:34:56` **with the session `time_zone` set to `+00:00`** | MySQL converts through the session zone on write; the execution layer owns pinning the session zone, which this table makes a requirement |
| SQLite | `TEXT` | text `2026-07-30T12:34:56Z` | RFC 3339 with `Z`; sorts correctly and SQLite's date functions parse it |

Serialises as `"2026-07-30T12:34:56Z"` (RFC 3339, `Z` suffix), fractional
seconds only when non-zero.

**The `FixedOffset` decision:** there is one zoned variant, carried in UTC.
`ToValue` is implemented generically for `DateTime<Tz>` — `Utc`,
`FixedOffset`, `Local` all bind — and normalises to UTC at conversion. The
offset is dropped deliberately: none of the three targets round-trips an
offset (see the notes column), so an offset-preserving variant would promise
what no backend keeps, and each backend would silently break the promise
differently. An application that needs the original offset stores it in its
own column. `FromValue` exists only for `DateTime<Utc>` for the same reason:
read-back can only ever produce UTC honestly.

### `uuid::Uuid` → `Value::Uuid`

| dialect | column type | binds as | notes |
|---|---|---|---|
| PostgreSQL | `uuid` | native `UUID` param, or hyphenated text | |
| MySQL | `CHAR(36)` | hyphenated lowercase text | `BINARY(16)` is the compact alternative but is opaque in logs and needs `BIN_TO_UUID`/`UUID_TO_BIN` at every touch point, so it is not the standard mapping; `FromValue` still accepts 16 raw bytes so such a column reads back |
| SQLite | `TEXT` | hyphenated lowercase text | |

Serialises as the hyphenated lowercase RFC 9562 form
(`"550e8400-e29b-41d4-a716-446655440000"`).

### `rust_decimal::Decimal` → `Value::Decimal`

| dialect | column type | binds as | notes |
|---|---|---|---|
| PostgreSQL | `numeric(p,s)` | text `19.99` (the lossless wire form), or native numeric param | |
| MySQL | `DECIMAL(p,s)` | text `19.99` | the MySQL protocol sends decimals as text anyway |
| SQLite | `TEXT` | text `19.99` | **deliberately not `REAL`**: storing a decimal in a binary float loses exactly the precision the type exists to keep. `TEXT` round-trips; the cost is that SQL-side arithmetic needs a cast, which is the honest trade |

Serialises as a **string** (`"19.99"`), never a JSON number: a JSON number is
a float to most readers, and `1.10` would collapse to `1.1`. Trailing zeros
survive (scale is preserved, as `NUMERIC` preserves it); equality is numeric
(`1.10 == 1.100`), matching the database's. `FromValue` accepts the variant,
text, and exact integers; **floats are rejected** — a binary fraction has no
faithful decimal scale, and inventing one silently is the bug `Decimal`
exists to prevent.

### `serde_json::Value` → `Value::Json`

| dialect | column type | binds as | notes |
|---|---|---|---|
| PostgreSQL | `jsonb` | serialised text (drivers send json/jsonb as text) | `jsonb`, not `json`: binary storage, indexable, and duplicate-key/whitespace normalisation is what applications expect. A column that must preserve key order or duplicates is the rare case and can be declared `json` by hand |
| MySQL | `JSON` | serialised text | |
| SQLite | `TEXT` | serialised text | SQLite's `json_*` functions operate on text; its newer internal `JSONB` format is an implementation detail not exposed as a column type |

Serialises **structurally** — the document itself, not a string containing
it — consistent with how `Value::Array` already serialises. This means a JSON
argument in a serialised arg list looks like the object, which is what a
human reading a log wants.

## Deliberate non-mappings

- **No inline literal rendering for any mapped type** — binding only, as
  above.
- **No `DateTime<FixedOffset>` variant** — normalised to UTC, as above.
- **No `timetz` mapping** — as above.
- **No duration/interval mapping** — chrono's `Duration`, PostgreSQL's
  `interval` and MySQL's out-of-range `TIME` values are a separate design
  (intervals are not a scalar in any portable sense) and are left to
  `CustomValue` until designed properly.
- **No `time` / `jiff` crate support yet** — one temporal vocabulary keeps
  the table single; a `time`-crate feature would be additive and can follow
  the same pattern if demanded.
- **No `Vec<T>` → `Array` blanket conversion for the new types** — arrays
  remain explicit via `Value::array`, for the `Vec<u8>`/`Bytes` collision
  reason recorded on `Value::array`.
- **`CustomValue` stays.** The escape hatch is still the right home for
  genuinely dialect-specific types (PostgreSQL ranges, geometric types, …).
  These mappings remove the four types that were abusing it, not the hatch.

## What the later phases inherit

- **Execution layer:** the "binds as" column per backend, including the
  MySQL session-zone requirement for `TIMESTAMP`; the text forms here are the
  canonical fallback wire encoding, and `FromValue`'s text acceptance means
  reading a column bound through keelson always round-trips.
- **Codegen:** the column-type column verbatim, including the SQLite `TEXT`
  choices and `numeric`/`DECIMAL` with explicit `(p,s)` taken from the
  declared schema.
