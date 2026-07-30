# Notices

## bob

keelson's design is strongly inspired by [bob](https://github.com/stephenafamo/bob)
by Stephen Afam-Osemene (MIT licensed), a SQL access toolkit for Go.

What is borrowed is its architecture rather than its code: hand-crafting each
dialect to its own grammar instead of a shared AST, treating query mods as
first-class values whose types constrain where they apply, accepting raw SQL
anywhere an expression is expected, and rendering in a single pass while arguments
accumulate. keelson departs from bob's implementation deliberately and throughout —
a single expression enum instead of boxed trait objects, `Cow<'static, str>` instead
of owned strings, infallible rendering, and clauses shared across dialects through
traits.

No bob source is included or redistributed here.
