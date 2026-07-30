# Deliberate rendering choices

Where keelson formats SQL differently from the obvious or the traditional, the
reason is recorded here. Semantics are never at stake — every entry is about the
bytes we hand a database being tidier, not different.

This file exists because "why is there a space there" is otherwise unanswerable a
year later, and because a reviewer needs to be able to tell a considered choice
from a bug.

## `OVER` is separated from its function call

```sql
avg(salary) OVER (w)     -- keelson
avg(salary)OVER (w )     -- the traditional builder rendering
```

Go's `bob`, which keelson is modelled on, writes `OVER` with no preceding space
while its window clause contributes a trailing one, so the keyword ends up welded
to the closing parenthesis and padded on the wrong side. Both parse identically.
Ours is what a person would write, and log output is read by people.

## `CAST` is not wrapped in redundant parentheses

```sql
CAST(x AS int)      -- keelson
(CAST(x AS int))    -- a builder that wraps every expression
```

keelson parenthesises a sub-expression when it might otherwise bind wrongly. A
`CAST(…)` is already self-delimiting, so a wrapping pair can never disambiguate
anything — it is pure noise. `expr::cast` therefore does not apply the
parenthesisation rule to its own result. `expr::group` is available when
parentheses are genuinely wanted.

## Formatting is not part of the contract

Tests compare with `keelson_sqlcheck::normalize`, which trims and collapses runs of
whitespace. Line breaks and indentation are therefore free to change without
breaking anything, while tokens and their order are pinned. If a change alters
tokens, that is a real change and a test should say so.
