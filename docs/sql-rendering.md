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
- A whole hand-written statement (`RawQuery`, reached as the dialect's
  `raw_query`) is a template, not a raw: its `?` are rewritten by this same
  scan. A statement a caller typed is exactly where a bound value belongs,
  and rewriting is what lets the same text move between engines.

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

## `OFFSET` and `FETCH` default to `OFFSET n` and `FETCH NEXT n ROWS`

```sql
OFFSET $1                     -- the default; OFFSET $1 ROWS on request
FETCH NEXT $1 ROWS ONLY       -- the default; FETCH FIRST $1 ROW ONLY reachable too
```

`ROW`/`ROWS` and `FIRST`/`NEXT` are pure synonyms in the grammar, so which one a
statement says can never change what it does — but unlike `SELECT ALL` or
`UNION DISTINCT`, which are the grammar's defaults and add nothing when written,
these keywords are part of a spelling a human reads and diffs against existing
queries. So every spelling is representable (`Offset::rows`,
`Fetch::first_or_next`, `Fetch::rows`), and the defaults are the tersest common
form: bare `OFFSET n`, which every dialect accepts, and `FETCH NEXT … ROWS`,
which is what the clause rendered before the other spellings existed.

## `MERGE` writes `WHEN NOT MATCHED` bare; `BY TARGET` only on request

```sql
WHEN NOT MATCHED THEN INSERT …            -- the default
WHEN NOT MATCHED BY TARGET THEN INSERT …  -- NotMatchedChain::by_target()
```

PostgreSQL 17 added `BY TARGET` as an explicit spelling of what `WHEN NOT
MATCHED` already means, so it exists to read well next to `WHEN NOT MATCHED BY
SOURCE`. Same policy as `OFFSET n ROWS` and `FETCH FIRST` above: the synonym is
representable because a human diffs SQL against existing queries, and the
default is the tersest form — which here is also the only one PostgreSQL 15 and
16 accept.

Two smaller `MERGE`-adjacent spellings, decided the same way by their grammars
rather than by taste: MySQL's standalone `VALUES` statement writes each row as
`ROW(…)` with the keyword welded to the parenthesis, because that is the
`row_constructor` production's own spelling (an `INSERT`'s `VALUES (…)` list
has no `ROW`); and a `MERGE` arm's `THEN UPDATE SET` list is the ordinary
`UPDATE` assignment list, keyword supplied by the arm, for the same reason
`Set` never writes its own `SET`.

## A clause with nowhere to go is a `build()` error, never a guess

```sql
SELECT "id" FROM "users" LIMIT $1 ORDER BY 1    -- what these shapes used to render
UPDATE "posts" SET "views" = $1                  -- …or worse: valid SQL missing the join
```

Four shapes found by the combinatorial suite had the builder
hand back garbage — or, worse, valid SQL that silently dropped a clause —
with `build()` reporting `Ok`. All four are now recorded on the writer and
surfaced once by `build()`, extending the rule that rendering is infallible
and errors travel through the writer:

- **A combined tail clause without a set operation** (`order_by_combined`
  et al. with no `union`/`intersect`/`except`) is
  `Error::Incomplete`: the combined clauses exist to apply to the result of
  the combination, and with none they would render after the query's own tail
  clauses, which no grammar accepts.
- **`LIMIT` and `FETCH` together** is `Error::ConflictingClauses`: gram.y's
  `select_limit` makes them two spellings of one production. Deliberately not
  last-write-wins — like the MySQL modifier ordering above, mod application
  order must never change what a query means.
- **Join mods with no `FROM`/`USING` item to attach to** (PostgreSQL/SQLite
  `UPDATE … FROM`, `DELETE … USING`, and `SELECT`) is `Error::Incomplete`.
  This was the worst of the four: the SQL built *valid* and simply omitted
  the join the caller asked for. MySQL's `UPDATE` differs legitimately — its
  target list *is* a `table_references`, joins and all — and keeps rendering.
  The same rule covers **extra from-items** (`from_also`/`using_also`): with
  no leading item to open the list, they used to vanish the same silent way,
  and are now the same `Error::Incomplete`. MySQL's `UPDATE` is exempt again,
  for the same reason with a different outcome: an absent target is already
  its own `Incomplete` before the extras are reached.
- **`.lateral()` on a bare table or CTE name** records an error at the call
  (keelson-psql and keelson-mysql wrap the item): `LATERAL` is grammatical
  only before a sub-query or function item in PostgreSQL's grammar, and only
  before a derived table in MySQL's (8.4 manual, *15.2.15.9 Lateral Derived
  Tables*). Raw fragments stay trusted —
  progressive enhancement means hand-written SQL is never judged.

## A function from-item's alias and its column definitions share one `AS`

```sql
json_to_recordset($1) AS "r" ("a" int)           -- gram.y's func_alias_clause
json_to_recordset($1) AS ("a" int) AS "r"        -- a syntax error
```

PostgreSQL's `func_alias_clause` is one production: `[ AS ] [ alias ]
( column_definition [, ...] )`. The alias sits *inside* it, between the single
`AS` and the column definitions — there is no second `AS` for a select-list
alias to use. `Function::columns` and `Function::as_table` therefore return a
`TableFunction`, a type on which the expression-position enders (`as_`,
`over`, `over_name`) do not exist, so the colliding combination
`f(..).columns(..).as_("r")` — which used to compile and render the
unparseable second line — cannot be written at all. This is the
typestate arm of the "never a guess" rule above: where the collision is
visible in the types, it is refused at compile time rather than recorded at
`build()`.

## Formatting is not part of the contract

Tests compare with `keelson_sqlcheck::normalize`, which trims and collapses runs of
whitespace. Line breaks and indentation are therefore free to change without
breaking anything, while tokens and their order are pinned. If a change alters
tokens, that is a real change and a test should say so.
