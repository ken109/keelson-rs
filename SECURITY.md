# Security policy

## Reporting a vulnerability

Report it privately through GitHub's advisory form:

**<https://github.com/ken109/keelson-rs/security/advisories/new>**

Please do not open a public issue for something that is exploitable. If GitHub
advisories are not available to you, email the address on the author's GitHub
profile with `keelson` in the subject.

What helps most, in order: the smallest input that demonstrates it (a query, a
schema, a connection string shape), which crate and version, and what an
attacker gets that they should not. If the input is generated, the generator is
better than the output.

Expect an acknowledgement within a week. Fixes for a confirmed issue go out as
a patch release, with an advisory naming the affected versions once the fix is
published.

## Supported versions

keelson is pre-1.0 and every published crate shares one version. Only the
**latest published version** receives fixes; there are no maintenance branches
for older `0.x` releases. Upgrading is the supported remedy, and the CHANGELOG
records what an upgrade costs.

## What is in scope

keelson builds SQL from values, binds those values as parameters, and hands
both to a driver. The security-relevant claims it makes are narrow, and these
are them:

- **A bound value is never interpolated into SQL.** `arg(x)` becomes a
  placeholder and an entry in the argument vector, on every dialect. A value
  that reaches the statement text is a vulnerability, not a formatting bug.
- **An identifier passed through `quote()` is escaped for the dialect it is
  rendered by.** A quoting rule that lets an identifier close its own quoting
  is in scope.
- **The generator emits code, not queries against your data.** `keelson-gen`
  reads a catalog with a connection you give it; an injection through a
  *schema* object's name — a table or column named to break out of the emitted
  file — is in scope.

## What is not

- **Raw SQL is raw SQL.** `select::where_("…")`, `raw_query`, and `sql!`'s
  literal text are deliberately not escaped or parsed: they exist so you can
  write SQL your dialect supports and keelson does not model. Interpolating
  untrusted input into one of those strings is the caller's bug, and no amount
  of library design can distinguish it from the intended use. `sql!`'s `{holes}`
  bind as parameters and *are* in scope; the text around them is not.
- **The connection string.** Where it comes from and who can read it is your
  deployment's business; keelson passes it to the driver.
- **Denial of service from a query you asked for.** keelson will build the
  cross join you described.
- **Vulnerabilities in dependencies**, which belong upstream — though the
  workspace tracks them (`cargo deny check advisories`, on every pull request)
  and a fix reachable by a version bump will be released as one.

## Known dependency exposure

One advisory in the tree has no patched release to move to, so it is recorded
here rather than only in `deny.toml`:

**RUSTSEC-2023-0071 — the Marvin attack against `rsa`.** Reached through
`sqlx-mysql`, which uses `rsa` for MySQL's `caching_sha2_password` /
`sha256_password` public-key password exchange. It is live for a MySQL
connection that authenticates over a network an attacker can time. **Connect to
MySQL over TLS**, which is what stops the timing from being observable. It
affects only the `sqlx-mysql` feature; a PostgreSQL or SQLite build never
resolves `rsa` at all.
