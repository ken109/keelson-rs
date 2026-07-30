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

## Clauses are separated by a single space, not a newline

```sql
SELECT "id" FROM "users" WHERE ("age" >= $1)   -- keelson
SELECT\n"id"\nFROM "users"\nWHERE ("age" >= $1)  -- bob
```

`bob` writes a newline in front of every clause and a trailing one after the
statement, so a query is a paragraph even when it is a dozen tokens long. A
statement is one logical line, and one line is what a log entry, an error message
and a `Debug` print all want. Nothing is lost: whitespace is not part of the
contract (below), so a pretty-printer can be added later without touching a test.

## `OVER "w"` is written without parentheses when it names a window

```sql
avg("views") OVER "w"                 -- a reference to a WINDOW-clause entry
avg("views") OVER ("w" ORDER BY "id") -- a definition that copies "w"
```

These are two different grammar productions, and PostgreSQL refuses the second
when `"w"` has a frame clause: *cannot copy window "w" because it has a frame
clause / HINT: Omit the parentheses in this OVER clause.* `bob` only ever emits
the parenthesised form, which makes a named framed window unreachable. keelson-psql
has `Function::over_name` for the reference and `Function::over` for the
definition, and this is the one case where the parentheses carry meaning rather
than shape.

## `\?` in a template is the only escape keelson performs

```sql
-- expr::template(r"\?| ? AND '\?'", [..])
"tags" ?| $1 AND '?'
```

A template's `?` holes are rewritten by a single byte scan, and `\?` is how a
question mark survives it — needed for PostgreSQL's `?`, `?|` and `?&` jsonb
operators, and for any `?` inside a string literal. The scan does not track
quoting (neither does `bob`), so a `?` between single quotes *is* a hole; when the
argument counts happen to match, the corruption is silent. Write `\?`.

Three consequences, all deliberate:

- There is no escape for the escape. `\\?` is a literal backslash followed by an
  escaped `?`, so a backslash immediately preceding a hole cannot be written in
  one template — use two fragments.
- Nothing else is escaped anywhere. A `'` inside `expr::literal` and a `"` inside
  `expr::quote` are passed through untouched: doubling the quote character is a
  `Dialect::write_quoted` decision, and `literal` is documented as being for SQL
  the program itself wrote. Text from outside belongs in `expr::arg`, which binds.
- `expr::raw` never scans at all, so a `?` in it is always literal.

## MySQL's statement modifiers are written in grammar order, not call order

```sql
SELECT DISTINCT HIGH_PRIORITY SQL_CALC_FOUND_ROWS "id" FROM …  -- keelson
SELECT SQL_CALC_FOUND_ROWS HIGH_PRIORITY DISTINCT "id" FROM …  -- bob, mods as written
```

MySQL fixes the order of `DISTINCT`, `HIGH_PRIORITY`, `IGNORE`, `STRAIGHT_JOIN` and
the `SQL_*` keywords, and rejects any other. `bob` keeps them in a `[]string` and
appends as the mods are applied, so `im.Ignore(), im.HighPriority()` emits
`INSERT IGNORE HIGH_PRIORITY`, which does not parse. keelson-mysql stores them as a
`Modifier` enum whose declaration order *is* the grammar's, kept sorted on insert.
Mod order therefore cannot affect validity, and a repeated modifier is written once.

This is the one place where a rendering decision is about validity rather than
tidiness, which is why it is recorded here rather than only in the code.

## `INSERT … SET` drops the insert column list

```sql
INSERT INTO `t` SET `b` = ?          -- keelson, when a SET assignment is present
INSERT INTO `t` (`a`) SET `b` = ?    -- a syntax error
```

MySQL's `INSERT` has three row sources and they are separate productions. The
`VALUES` and `SELECT` forms carry `[(col_name, …)]`; the `SET` form does not. Since
`SET` already wins when it is combined with `VALUES` — a half-and-half rendering is
not a statement — it takes the column list with it, so choosing a row source chooses
one whole production. `PARTITION` is in both and stays. Real MySQL is what caught
this: the grammar backend accepts the invalid form.

## Formatting is not part of the contract

Tests compare with `keelson_sqlcheck::normalize`, which trims and collapses runs of
whitespace. Line breaks and indentation are therefore free to change without
breaking anything, while tokens and their order are pinned. If a change alters
tokens, that is a real change and a test should say so.
