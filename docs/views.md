# Views

A view is a query with a name. That is the whole of it, and it is why the
generator treats one differently from a table: the catalog can tell you a
view's columns, and almost nothing else. It has no foreign keys. It usually
has no primary key. On PostgreSQL and SQLite every one of its columns is
reported as nullable, because a view carries no constraints of its own.

So the two questions the model layer needs answered — *how does this relate to
anything?* and *what identifies a row?* — have no catalog answer for a view.
keelson-gen does not infer them. It takes them from the configuration, checks
everything about them that can be checked, and refuses what it cannot cover.

## Relations: declared, then validated

`[[relationships]]` may name a view on either end.

```toml
# The view is the referencing side: one row per post, each naming its author.
[[relationships]]
table      = "post_authors"   # a view
column     = "user_id"
ref_table  = "users"
ref_column = "id"
cardinality = "many_to_one"

# The view is the referenced side, one row per post.
[[relationships]]
table      = "posts"
column     = "id"
ref_table  = "post_authors"   # a view
ref_column = "post_id"
name        = "authorship"
cardinality = "one_to_one"
```

`cardinality` is optional between two base tables — there the referenced
column's key constraint answers it, and a foreign key always means
many-to-one — and **required** as soon as either end is a view. Nothing in the
catalog says whether `post_authors` holds one row per post or many, and the
answer changes the generated types, so it has to be stated.

| `cardinality`  | on the referencing side | on the referenced side              |
| -------------- | ----------------------- | ----------------------------------- |
| `"many_to_one"` | `Option<TargetRow>`    | `Vec<ThisRow>`                      |
| `"one_to_one"`  | `Option<TargetRow>`    | `Option<Box<ThisRow>>`              |

The `Box` on a to-one back-reference is not decoration: the referencing side
already holds an `Option` pointing back, so without indirection the two row
types would be mutually recursive and would not compile.

Every declaration is validated against the introspected schema before anything
is emitted. A mistake is a generation-time error naming the TOML key, never a
compile error in generated code and never a surprise at run time:

- both `table` and `ref_table` must name a table or view the schema holds —
  the message lists what it does hold;
- both `column` and `ref_column` must exist there — likewise;
- the two columns must resolve to the same Rust type, so the join compiles;
- `cardinality` must be present when a view is involved;
- and if the `only`/`except` filters removed an end, the declaration is an
  error rather than a quiet no-op. A *foreign key* that loses an end to the
  filters goes quietly, because that is what the filter asked for; a
  declaration you wrote by hand does not.

## Identity: needed for writes, not for reads

Loading a relation needs a join column, not a row identity. The generated
loaders group children by the declared join column and deduplicate the keys
they fetch by — again — that column. So a keyless view can hold relations, be
the target of relations, preload through a `LEFT JOIN` and then-load through a
keyed second query, without any notion of what identifies one of its rows.

What a keyless view does *not* get is everything that needs an identity:

- no `Pk` associated type and no `Table` impl;
- no `Setter`, and so no `INSERT`/`UPDATE`/`DELETE`;
- no keyed read-back on MySQL (the thing that stands in for `RETURNING`);
- no factory template.

The entry point is `view()` rather than `table()`, and that is the whole
difference at the call site.

## Writing through a view

Two things have to be true, and neither is assumed.

**The engine has to allow it.** The three disagree, so the generator asks each
one its own way rather than applying a rule of thumb:

| engine     | how it is asked                                     | what makes a view writable |
| ---------- | --------------------------------------------------- | -------------------------- |
| PostgreSQL | `pg_relation_is_updatable(oid, true) & 28 = 28`      | auto-updatable (one base table, no aggregate/`DISTINCT`/set operation/`GROUP BY`/window), or `INSTEAD OF` triggers. The mask is `UPDATE｜INSERT｜DELETE`; a materialised view is never updatable. |
| MySQL      | `information_schema.VIEWS.IS_UPDATABLE`              | one flag MySQL computes from the view body. |
| SQLite     | `sqlite_master`, read for `INSTEAD OF` triggers      | a view is read-only unless it carries `INSTEAD OF` triggers for **all three** statements. |

**The configuration has to supply the identity the catalog does not.**

```toml
[tables.editable_users]
key = ["id"]
```

`key` is accepted only on a relation the catalog gives no primary key — a view,
or a table declared without one — and only when the engine above says writes
reach it. On a read-only view it is refused, with that engine's rule in the
message. On a relation that already has a primary key it is refused too: the
catalog's answer is not the configuration's to overrule.

Declaring a column as key **asserts it is never NULL**, and the generated row
field stops being an `Option` accordingly. A key that can be NULL identifies
nothing, and PostgreSQL and SQLite report every view column as nullable, so
without this the feature would be unusable on two engines out of three. The
assertion is yours; a view that really does yield NULL there fails to decode,
by name, at the row.

## What is still refused

- **Factories over a writable view.** A factory draws distinct values from
  auto-increment columns and unique constraints, and a view reports neither.
  `[output] factories = true` together with a declared key on a view is an
  explicit `Unsupported` error rather than a template that looks right and
  collides on the second row. A foreign key *pointing at* a view stays a plain
  value column rather than a `Parent` field, and a back-reference *from* one
  gets no `with_new_…` mod, for the same reason: a factory creates rows, and a
  view has none of its own.
- **Multi-column joins.** `[[relationships]]` declares one column on each
  side; composite foreign keys are introspected faithfully but emit no
  relation, view or not.
- **Same-query preload of a back-reference.** `preload` covers to-one
  relations; a `Vec` or `Option` back-reference is loaded by `then_load`.
  This is not view-specific.

## A caveat worth knowing

MySQL's `IS_UPDATABLE` is a single flag, and it is more generous than the
server's own behaviour for `INSERT`: a view over a join can report `YES` and
still reject an `INSERT`, because inserting has to pick one base table.
PostgreSQL's mask separates the three statements and SQLite's triggers are
per-statement, so only MySQL has this gap. The generator reports what the
catalog says; if you declare a key on a MySQL view over a join, expect the
server to have the last word on `INSERT`.
