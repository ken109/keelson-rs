# Deliberate SQL deviations from bob

Every row here is a place where keelson emits different SQL from `bob` **on
purpose**. Semantics are always identical — the only licence taken is to be
tidier. A deviation that cannot be justified in one sentence is a bug, not an
improvement, and belongs in the code rather than in this table.

`assert_case` compares [`clean`](../../crates/keelson-golden/src/lib.rs)ed SQL, and
`clean` pads and collapses whitespace around parentheses. A difference that
disappears under `clean` therefore needs no change to any assertion; it is still
recorded here, marked **cleans equal**, because the bytes we hand a database do
differ from bob's.

## Expressions (`keelson-core::expr`)

| case | bob emits | keelson emits | why ours is better |
|---|---|---|---|
| `psql / Window function over window name`, `psql / Window function over empty frame`, `psql / with sub-select and window` — **cleans equal, no assertion changed** | `avg(salary)OVER (w )` | `avg(salary) OVER (w)` | bob's `Function.WriteSQL` writes `OVER` with no preceding space and `clause.Window` leaves a trailing one, so the keyword ends up welded to the closing parenthesis and padded on the wrong side. Legal, but it reads like a typo in a log. |
| no fixture exercises `CAST` | `(CAST(x AS int))` | `CAST(x AS int)` | `CAST(..)` is already self-delimiting, so bob's builder-level wrap adds a pair of parentheses that can never disambiguate anything. `expr::cast` therefore does not apply the parenthesisation rule to its own result; `expr::group` is there if they are wanted. |

Note on what is *not* a deviation: keelson's `Expr::join_with("")` joins with an
empty separator, where bob's `Join{Sep: ""}` silently substitutes a space. That is
an API difference, not an output one — bob's substitution exists because Go gives
you the zero value when you leave a field out, and every keelson call site passes
the separator explicitly (`Expr::join` is the space-separated form).
