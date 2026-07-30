# Golden tests extracted from Go's `bob`

These fixtures are a correctness oracle. They are extracted by running the test
suite of [`bob`](https://github.com/stephenafamo/bob) — the Go library keelson is
inspired by — and capturing the SQL it actually generates for each of its test
cases. bob's SQL is spec-correct and battle-tested, so matching it is strong
evidence that we are correct too.

They are *not* a specification of keelson's API, internals, or formatting.
keelson is a Rust-native design, not a transliteration, and where bob's output is
needlessly verbose we are free to emit something tidier — see
[Deliberate deviations](#deliberate-deviations).

Extracted from **bob v0.42.0**. 80 cases: 3 dialects × 4 statement types, plus
standalone expression cases.

| dialect | select | insert | update | delete |
| ------- | ------ | ------ | ------ | ------ |
| psql    | 20     | 6      | 3      | 2      |
| mysql   | 8      | 7      | 4      | 4      |
| sqlite  | 9      | 6      | 3      | 1      |

Plus 7 expression-level cases from `expr/raw_test.go`.

## Format

`bob-v0.42.0.jsonl` — one JSON object per line:

| field            | meaning                                                              |
| ---------------- | -------------------------------------------------------------------- |
| `kind`           | `query` (a full statement) or `expression` (a fragment)               |
| `dialect`        | `psql` / `mysql` / `sqlite`, empty for dialect-agnostic cases         |
| `source`         | the bob test file the case came from                                 |
| `name`           | the test case key in bob                                             |
| `doc`            | bob's own description of the case, when present                       |
| `expected_sql`   | SQL as literally written in bob's test — loosely formatted, informational only |
| `generated_sql`  | **what bob actually produced**, byte for byte                        |
| `clean_sql`      | `generated_sql` with whitespace normalised — **compare against this** |
| `normalized_sql` | `generated_sql` parsed and deparsed by the dialect's real parser      |
| `args`           | bound arguments: Go type, Go literal, and JSON value                 |
| `build_error`    | set when bob returned an error instead of SQL                        |
| `expected_error` | expression cases only: the error bob's test expected                 |

## Compare against `clean_sql`, not the others

`clean_sql` applies bob's own normalisation (collapse whitespace runs to a single
space, pad brackets with spaces). It therefore pins **token content and order
while leaving formatting free** — this library can indent and break lines however
it likes and still match.

The other two are not suitable as the comparison target:

- `generated_sql` bakes in bob's exact newline placement (`SELECT \nstatus, ...`),
  which is an implementation detail, not a contract.
- `normalized_sql` **loses information**. The dialect parser rewrites `"status"`
  to `status` and `LEAD` to `lead`, so quoting and casing bugs would slip through.
  Keep it only as a secondary semantic sanity check.

## Deliberate deviations

Emitting different SQL from bob is allowed when ours is genuinely better — dropping
a redundant pair of parentheses, for instance. It is never allowed to differ in
meaning. Tidier, yes; different, no.

When a case deviates, record it in `deviations.md` alongside this file with the
case name, what bob emits, what keelson emits, and why ours is better, then assert
keelson's string in the test with a comment pointing at that entry. A deviation
that cannot be justified in one sentence is a bug in the builder, not an
improvement — and bending the assertion instead of writing the entry defeats the
whole point of having an oracle.

## Regenerating

The extractor patches bob's own test harness (`test/utils`) to append each case to
a JSONL file, then runs the dialect tests.

```sh
# 1. get a writable copy of the version you want
git clone --depth 1 --branch v0.42.0 https://github.com/stephenafamo/bob /tmp/bob
cd /tmp/bob

# 2. add the dumper and patch the harness
cp <this-dir>/extract/golden.go test/utils/golden.go
patch -p0 test/utils/utils.go < <this-dir>/extract/utils.go.patch

# 3. run the dialect tests with the output path set
KEELSON_GOLDEN_OUT=/tmp/golden.jsonl go test ./dialect/... ./expr/... .
```

Cases are emitted whether or not the assertion passes, so a failing upstream test
still yields a fixture (with `build_error` set if the build itself failed).

Note: bob's `dialect/*/table_test.go` spins up a real database with testcontainers
and needs Docker running. Those are integration tests and produce no golden SQL —
`view_test.go` and `table_test.go` assert through their own helpers, not the shared
harness, so their cases are not captured here and must be ported by hand.

## Licence

bob is MIT licensed, Copyright (c) 2022 Stephen Afam-Osemene. Its licence text is
kept alongside these fixtures in `BOB-LICENCE`, since the cases here are derived
from bob's test suite.
